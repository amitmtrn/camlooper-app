//! The WebRTC receiver: one tokio task per phone session owning a str0m [`Rtc`] (sans-IO)
//! and its UDP sockets.
//!
//! * **Candidates.** str0m does no gathering. We bind one UDP socket per usable IPv4
//!   interface and announce each as a host candidate, plus at most one server-reflexive
//!   candidate learned from one STUN binding (≤ 800 ms, best effort). One socket per
//!   interface rather than one `0.0.0.0` socket because str0m's ICE agent matches every
//!   incoming packet's *destination* against its host candidates and silently drops
//!   anything else; a wildcard socket cannot report the destination address without
//!   `IP_PKTINFO`, which std/tokio do not expose.
//! * **Answer.** The desktop does not trickle: all candidates are in the answer SDP. The
//!   phone's candidates may trickle in at any time.
//! * **Frames.** str0m depacketizes, reorders and NACK/RTX-repairs; we forward complete
//!   frames with a keyframe flag. Keyframes (PLI) are requested when the track starts, after
//!   loss, when the consumer asks (decoder (re)start, writer out of sync) and when no frame
//!   arrived for >1 s — the last one works around str0m#1033, where the reorder queue can
//!   wait forever on an unrecoverable frame. All PLIs share one 1/s limiter.
//! * **Bandwidth.** Chrome adapts resolution to its bandwidth estimate. The answer keeps
//!   `transport-cc` + the transport-wide sequence number extension, so str0m sends TWCC
//!   feedback and Chrome's send-side estimator can ramp up; we also send REMB with a
//!   generous ceiling for senders that still look at it.
//! * **Timeouts.** No ICE connection within 15 s → `ice_failed`. Once connected, no media
//!   for 10 s → `timeout`.
//!
//! Privacy: SDP and candidate addresses are logged at debug level only.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use str0m::bwe::Bitrate;
use str0m::change::SdpOffer;
use str0m::format::Codec as SCodec;
use str0m::media::{KeyframeRequestKind, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::media::{h264_is_keyframe, vp8_is_keyframe, PliLimiter};
use super::signal::{StopReason, MAX_SDP_BYTES};
use crate::pipeline::EncodedCodec;

const MAX_HOST_IPS: usize = 8;
const STUN_BUDGET: Duration = Duration::from_millis(800);
const FRAME_QUEUE: usize = 90;

#[derive(Debug, Clone)]
pub struct RtcOptions {
    /// Bind exactly these addresses instead of enumerating interfaces (tests use loopback).
    pub bind_ips: Option<Vec<Ipv4Addr>>,
    pub ice_timeout: Duration,
    pub media_timeout: Duration,
    /// No frame for this long → request a keyframe (rate-limited to `pli_interval`).
    pub stall: Duration,
    pub pli_interval: Duration,
    /// REMB ceiling sent to the phone once a second; `None` disables REMB.
    pub remb_bps: Option<u64>,
}

impl Default for RtcOptions {
    fn default() -> Self {
        Self {
            bind_ips: None,
            ice_timeout: Duration::from_secs(15),
            media_timeout: Duration::from_secs(10),
            stall: Duration::from_secs(1),
            pli_interval: Duration::from_secs(1),
            remb_bps: Some(8_000_000),
        }
    }
}

/// A depacketized frame for the decoder.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub codec: EncodedCodec,
    pub keyframe: bool,
    /// str0m saw no loss between the previous frame and this one.
    pub contiguous: bool,
    pub rtp_ts: u32,
    pub data: Arc<[u8]>,
}

#[derive(Debug)]
pub enum RtcCommand {
    /// A remote trickle candidate (`candidate:…`); empty = end-of-candidates.
    RemoteCandidate(String),
    RequestKeyframe,
    /// The consumer dropped frames; treat the stream as discontinuous.
    FramesDropped,
    Stop,
}

#[derive(Debug)]
pub enum RtcEvent {
    IceConnected,
    Frame(EncodedFrame),
    /// The task has ended. `Stopped` when asked to.
    Closed(RtcClose),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RtcClose {
    Stopped,
    Failed { reason: StopReason, detail: String },
}

/// Sockets bound and candidates gathered, ready for an offer.
pub struct LocalTransport {
    sockets: Vec<Arc<UdpSocket>>,
    candidates: Vec<Candidate>,
}

impl LocalTransport {
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

/// Usable IPv4 addresses: not loopback / link-local / unspecified; container and VM
/// bridges are skipped when anything else is available (they are never on the phone's LAN).
pub fn host_ipv4s() -> Vec<Ipv4Addr> {
    let ifaces = if_addrs::get_if_addrs().unwrap_or_default();
    let usable = |ip: &Ipv4Addr| !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_broadcast();
    let virtual_name = |name: &str| {
        ["docker", "br-", "veth", "virbr", "vboxnet", "vmnet", "cni", "flannel", "podman", "lxcbr", "lxdbr"]
            .iter()
            .any(|p| name.starts_with(p))
    };
    let mut all: Vec<(bool, Ipv4Addr)> = ifaces
        .iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) if usable(&ip) => Some((virtual_name(&i.name), ip)),
            _ => None,
        })
        .collect();
    all.sort();
    all.dedup();
    let real: Vec<Ipv4Addr> = all.iter().filter(|(v, _)| !v).map(|(_, ip)| *ip).collect();
    let mut ips = if real.is_empty() { all.into_iter().map(|(_, ip)| ip).collect() } else { real };
    ips.truncate(MAX_HOST_IPS);
    ips
}

/// Bind sockets and gather candidates. `stun` is the first STUN server from
/// `/v1/ice-servers` (already resolved), if any.
pub async fn gather(opts: &RtcOptions, stun: Option<SocketAddr>) -> Result<LocalTransport> {
    let ips = opts.bind_ips.clone().unwrap_or_else(host_ipv4s);
    let mut sockets = Vec::new();
    let mut candidates = Vec::new();
    for ip in ips {
        let sock = match UdpSocket::bind((ip, 0)).await {
            Ok(s) => s,
            Err(e) => {
                log::debug!("phone stream: cannot bind {ip}: {e}");
                continue;
            }
        };
        let local = sock.local_addr()?;
        match Candidate::host(local, "udp") {
            Ok(c) => {
                candidates.push(c);
                sockets.push(Arc::new(sock));
            }
            Err(e) => log::debug!("phone stream: no host candidate for {local}: {e}"),
        }
    }
    if sockets.is_empty() {
        return Err(anyhow!("no usable network interface for the phone stream"));
    }
    if let Some(server) = stun {
        if let Some((local, mapped)) = stun_srflx(&sockets, server).await {
            if mapped != local {
                if let Ok(c) = Candidate::server_reflexive(mapped, local, "udp") {
                    candidates.push(c);
                }
            }
        }
    }
    eprintln!("phone stream: {} local candidate(s) gathered", candidates.len());
    Ok(LocalTransport { sockets, candidates })
}

/// Build the `Rtc`, accept the offer and start the session task.
/// Returns the answer SDP (candidates embedded), a command sender and the event receiver.
pub fn accept_offer(
    transport: LocalTransport,
    offer_sdp: &str,
    opts: RtcOptions,
) -> Result<(String, mpsc::UnboundedSender<RtcCommand>, mpsc::Receiver<RtcEvent>)> {
    let start = Instant::now();
    let mut rtc = build_rtc(start);
    for c in &transport.candidates {
        rtc.add_local_candidate(c.clone());
    }
    log::debug!("phone stream: offer sdp:\n{offer_sdp}");
    let offer = SdpOffer::from_sdp_string(offer_sdp).context("unreadable offer SDP")?;
    let answer = rtc.sdp_api().accept_offer(offer).context("offer not acceptable")?;
    let answer = answer.to_sdp_string();
    log::debug!("phone stream: answer sdp:\n{answer}");
    if answer.len() > MAX_SDP_BYTES {
        return Err(anyhow!("answer SDP is {} bytes, over the relay budget", answer.len()));
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::channel(FRAME_QUEUE);
    tokio::spawn(run(rtc, transport.sockets, opts, start, cmd_rx, ev_tx));
    Ok((answer, cmd_tx, ev_rx))
}

fn build_rtc(start: Instant) -> Rtc {
    let mut cfg = RtcConfig::new()
        .set_crypto_provider(Arc::new(str0m_purecrypto::default_provider()))
        .clear_codecs()
        .set_stats_interval(None);
    // H.264 first: str0m answers with the first local codec the offer also has, and with
    // VP8 configured first it picked VP8 even when Chrome preferred H.264 (spike finding).
    // Android phones usually have a hardware H.264 encoder, which costs them less battery.
    // packetization-mode=1 only; constrained baseline first, then baseline, main, high.
    {
        let cc = cfg.codec_config();
        cc.add_h264(102.into(), Some(103.into()), true, 0x42e01f);
        cc.add_h264(104.into(), Some(105.into()), true, 0x42001f);
        cc.add_h264(106.into(), Some(107.into()), true, 0x4d001f);
        cc.add_h264(108.into(), Some(109.into()), true, 0x64001f);
    }
    // VP8 as the fallback; Opus only so an audio m-line in the offer is accepted (its data is
    // dropped — phase 1 is video only).
    cfg = cfg.enable_vp8(true).enable_opus(true, false);
    cfg.build(start)
}

struct Udp {
    local: SocketAddr,
    source: SocketAddr,
    data: Vec<u8>,
}

async fn run(
    mut rtc: Rtc,
    sockets: Vec<Arc<UdpSocket>>,
    opts: RtcOptions,
    start: Instant,
    mut cmd_rx: mpsc::UnboundedReceiver<RtcCommand>,
    ev_tx: mpsc::Sender<RtcEvent>,
) {
    let (udp_tx, mut udp_rx) = mpsc::channel::<Udp>(1024);
    let readers: Vec<_> = sockets.iter().map(|s| spawn_reader(s.clone(), udp_tx.clone())).collect();
    drop(udp_tx);

    let mut st = LoopState {
        video_mid: None,
        connected_at: None,
        last_media: None,
        pli: PliLimiter::new(opts.pli_interval),
        next_remb: start,
        discontinuity: false,
        frames: 0,
        dropped: 0,
        plis: 0,
    };
    let close = loop {
        // 1. Drain everything str0m wants to do.
        let timeout = match drain(&mut rtc, &sockets, &mut st, &ev_tx) {
            Ok(t) => t,
            Err(close) => break close,
        };
        // 2. Our own timers.
        let now = Instant::now();
        if st.connected_at.is_none() && now.duration_since(start) >= opts.ice_timeout {
            break RtcClose::Failed { reason: StopReason::IceFailed, detail: "no ICE connection".into() };
        }
        if let Some(c) = st.connected_at {
            let since = st.last_media.unwrap_or(c);
            if now.duration_since(since) >= opts.media_timeout {
                break RtcClose::Failed { reason: StopReason::Timeout, detail: "no video from the phone".into() };
            }
        }
        if st.last_media.is_some_and(|t| now.duration_since(t) > opts.stall) {
            st.pli.want();
        }
        if let (Some(mid), true) = (st.video_mid, st.pli.ready(now)) {
            if let Some(mut w) = rtc.writer(mid) {
                // NoReceiverSource until the first RTP packet; retried on the next pass.
                if w.request_keyframe(None, KeyframeRequestKind::Pli).is_ok() {
                    st.pli.sent(now);
                    st.plis += 1;
                    continue; // poll again so the RTCP goes out now
                }
            }
        }
        if let (Some(mid), Some(bps), true) = (st.video_mid, opts.remb_bps, st.last_media.is_some()) {
            if now >= st.next_remb {
                if let Some(rx) = rtc.direct_api().stream_rx_by_mid(mid, None) {
                    rx.request_remb(Bitrate::bps(bps));
                }
                st.next_remb = now + Duration::from_secs(1);
                continue;
            }
        }
        if !rtc.is_alive() {
            break RtcClose::Failed { reason: StopReason::Error, detail: "the connection was closed".into() };
        }

        // 3. Wait for the next input or timer.
        let mut wake = timeout.min(now + Duration::from_millis(250));
        if st.connected_at.is_none() {
            wake = wake.min(start + opts.ice_timeout);
        }
        if let Some(t) = st.pli.next_at() {
            wake = wake.min(t.max(now + Duration::from_millis(10)));
        }
        let input_result = tokio::select! {
            pkt = udp_rx.recv() => match pkt {
                Some(p) => handle_udp(&mut rtc, p),
                None => break RtcClose::Failed { reason: StopReason::Error, detail: "network sockets closed".into() },
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(RtcCommand::RemoteCandidate(c)) => {
                    add_remote_candidate(&mut rtc, &c);
                    Ok(())
                }
                Some(RtcCommand::RequestKeyframe) => {
                    st.pli.want();
                    Ok(())
                }
                Some(RtcCommand::FramesDropped) => {
                    st.discontinuity = true;
                    st.pli.want();
                    Ok(())
                }
                Some(RtcCommand::Stop) | None => break RtcClose::Stopped,
            },
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {
                rtc.handle_input(Input::Timeout(Instant::now())).map_err(|e| e.to_string())
            }
        };
        if let Err(e) = input_result {
            break RtcClose::Failed { reason: StopReason::Error, detail: e };
        }
    };

    rtc.disconnect();
    // Flush whatever the disconnect produced (DTLS close alert) best effort.
    let _ = drain(&mut rtc, &sockets, &mut st, &ev_tx);
    for r in readers {
        r.abort();
    }
    eprintln!(
        "phone stream: receiver closed ({}) after {} frames, {} dropped, {} keyframe requests",
        match &close {
            RtcClose::Stopped => "stopped".to_string(),
            RtcClose::Failed { reason, detail } => format!("{reason}: {detail}"),
        },
        st.frames,
        st.dropped,
        st.plis
    );
    let _ = ev_tx.send(RtcEvent::Closed(close)).await;
}

struct LoopState {
    video_mid: Option<Mid>,
    connected_at: Option<Instant>,
    last_media: Option<Instant>,
    pli: PliLimiter,
    next_remb: Instant,
    /// Frames were dropped since the last delivered one; the next is marked non-contiguous.
    discontinuity: bool,
    frames: u64,
    dropped: u64,
    plis: u64,
}

/// Poll str0m until it asks to be woken at a time. Transmits go out immediately.
fn drain(
    rtc: &mut Rtc,
    sockets: &[Arc<UdpSocket>],
    st: &mut LoopState,
    ev_tx: &mpsc::Sender<RtcEvent>,
) -> Result<Instant, RtcClose> {
    loop {
        let out = rtc
            .poll_output()
            .map_err(|e| RtcClose::Failed { reason: StopReason::Error, detail: e.to_string() })?;
        match out {
            Output::Timeout(t) => return Ok(t),
            Output::Transmit(t) => {
                if let Some(s) = sockets.iter().find(|s| s.local_addr().ok() == Some(t.source)) {
                    // UDP: a full send buffer is the same as loss; never block the loop on it.
                    let _ = s.try_send_to(&t.contents, t.destination);
                }
            }
            Output::Event(ev) => match ev {
                Event::IceConnectionStateChange(state) => {
                    log::debug!("phone stream: ICE {state:?}");
                    if matches!(state, IceConnectionState::Connected | IceConnectionState::Completed)
                        && st.connected_at.is_none()
                    {
                        st.connected_at = Some(Instant::now());
                        let _ = ev_tx.try_send(RtcEvent::IceConnected);
                    }
                }
                Event::MediaAdded(m) if m.kind == MediaKind::Video && st.video_mid.is_none() => {
                    st.video_mid = Some(m.mid);
                    st.pli.want(); // keyframe as soon as the track starts
                }
                Event::MediaData(d) => {
                    if Some(d.mid) != st.video_mid {
                        continue; // audio: discarded in phase 1
                    }
                    let codec = match d.params.spec().codec {
                        SCodec::H264 => EncodedCodec::H264,
                        SCodec::Vp8 => EncodedCodec::Vp8,
                        _ => continue,
                    };
                    st.last_media = Some(Instant::now());
                    let keyframe = match codec {
                        EncodedCodec::H264 => h264_is_keyframe(&d.data),
                        EncodedCodec::Vp8 => vp8_is_keyframe(&d.data),
                    };
                    let contiguous = d.contiguous && !st.discontinuity;
                    if !d.contiguous {
                        st.pli.want();
                    }
                    let frame = EncodedFrame { codec, keyframe, contiguous, rtp_ts: d.time.numer() as u32, data: d.data };
                    match ev_tx.try_send(RtcEvent::Frame(frame)) {
                        Ok(()) => {
                            st.frames += 1;
                            st.discontinuity = false;
                        }
                        Err(_) => {
                            // The decoder is behind. Dropping is the only bounded option;
                            // the next delivered frame is marked discontinuous and a
                            // keyframe re-syncs the stream.
                            st.dropped += 1;
                            st.discontinuity = true;
                            st.pli.want();
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

fn handle_udp(rtc: &mut Rtc, p: Udp) -> Result<(), String> {
    let Ok(contents) = p.data.as_slice().try_into() else {
        return Ok(()); // not STUN/DTLS/RTP
    };
    let input = Input::Receive(
        Instant::now(),
        Receive { proto: Protocol::Udp, source: p.source, destination: p.local, contents },
    );
    rtc.handle_input(input).map_err(|e| e.to_string())
}

fn add_remote_candidate(rtc: &mut Rtc, c: &str) {
    if c.is_empty() {
        return; // end-of-candidates: nothing to do for str0m
    }
    match Candidate::from_sdp_string(c) {
        // str0m is BUNDLE-only, so sdpMid / sdpMLineIndex do not matter.
        Ok(cand) => {
            log::debug!("phone stream: remote candidate {c}");
            rtc.add_remote_candidate(cand)
        }
        // mDNS (.local) host candidates land here: str0m cannot resolve them. The phone's
        // srflx/prflx path still works on the same LAN.
        Err(e) => log::debug!("phone stream: ignoring candidate ({e})"),
    }
}

fn spawn_reader(sock: Arc<UdpSocket>, tx: mpsc::Sender<Udp>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(local) = sock.local_addr() else { return };
        let mut buf = vec![0u8; 2048];
        loop {
            match sock.recv_from(&mut buf).await {
                Ok((n, source)) => {
                    if tx.send(Udp { local, source, data: buf[..n].to_vec() }).await.is_err() {
                        return;
                    }
                }
                // Windows reports an ICMP port-unreachable as ConnectionReset on UDP.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                Err(e) => {
                    log::debug!("phone stream: socket {local} closed: {e}");
                    return;
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------------------
// STUN: one binding request for one server-reflexive candidate.
// ---------------------------------------------------------------------------------------

/// Parse `stun:host:port` / `stun:host` (port 3478). `turn:`/`turns:` and `stuns:` are not
/// supported (no TURN client) and return `None`.
pub fn parse_stun_url(url: &str) -> Option<(String, u16)> {
    let rest = url.trim().strip_prefix("stun:")?;
    let rest = rest.split('?').next()?;
    if rest.is_empty() {
        return None;
    }
    if let Some(v6) = rest.strip_prefix('[') {
        let (host, tail) = v6.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => 3478,
        };
        return Some((host.to_string(), port));
    }
    match rest.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((rest.to_string(), 3478)),
    }
}

/// Resolve the first usable STUN URL to an IPv4 address, within `budget`.
pub async fn resolve_stun(urls: &[String], budget: Duration) -> Option<SocketAddr> {
    let (host, port) = urls.iter().find_map(|u| parse_stun_url(u))?;
    let lookup = tokio::net::lookup_host(format!("{host}:{port}"));
    let mut addrs = tokio::time::timeout(budget, lookup).await.ok()?.ok()?;
    addrs.find(|a| a.is_ipv4())
}

/// One STUN binding from the socket that routes to `server` (or the first socket),
/// two transmissions 400 ms apart. `(local, mapped)` on success.
async fn stun_srflx(sockets: &[Arc<UdpSocket>], server: SocketAddr) -> Option<(SocketAddr, SocketAddr)> {
    let route_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(server)?;
            s.local_addr()
        })
        .ok()
        .map(|a| a.ip());
    let sock = sockets
        .iter()
        .find(|s| s.local_addr().ok().map(|a| a.ip()) == route_ip)
        .or_else(|| sockets.first())?
        .clone();
    let local = sock.local_addr().ok()?;
    let mut txid = [0u8; 12];
    rand::Rng::fill(&mut rand::thread_rng(), &mut txid);
    let mut req = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xa4, 0x42];
    req.extend_from_slice(&txid);
    let deadline = tokio::time::Instant::now() + STUN_BUDGET;
    let mut buf = [0u8; 576];
    for attempt in 0..2 {
        if sock.send_to(&req, server).await.is_err() {
            return None;
        }
        let until = (tokio::time::Instant::now() + Duration::from_millis(400)).min(deadline);
        loop {
            match tokio::time::timeout_at(until, sock.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) => {
                    if from == server && n >= 20 && buf[8..20] == txid {
                        let mapped = parse_xor_mapped(&buf[..n])?;
                        log::debug!("phone stream: srflx {mapped} via {local}");
                        return Some((local, mapped));
                    }
                }
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                _ => break,
            }
        }
        if attempt == 1 || tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    eprintln!("phone stream: STUN gave no answer; offering host candidates only");
    None
}

/// XOR-MAPPED-ADDRESS (IPv4) from a STUN binding success response.
pub fn parse_xor_mapped(msg: &[u8]) -> Option<SocketAddr> {
    if msg.len() < 20 || msg[0..2] != [0x01, 0x01] {
        return None;
    }
    let mut i = 20;
    while i + 4 <= msg.len() {
        let typ = u16::from_be_bytes([msg[i], msg[i + 1]]);
        let len = u16::from_be_bytes([msg[i + 2], msg[i + 3]]) as usize;
        let v = msg.get(i + 4..i + 4 + len)?;
        if typ == 0x0020 && len >= 8 && v[1] == 0x01 {
            let port = u16::from_be_bytes([v[2], v[3]]) ^ 0x2112;
            let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ 0x2112_a442;
            return Some(SocketAddr::from((Ipv4Addr::from(ip), port)));
        }
        i += 4 + len.div_ceil(4) * 4;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_mapped_address() {
        let mut m = vec![0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42];
        m.extend_from_slice(&[0; 12]);
        m.extend_from_slice(&[0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43]);
        assert_eq!(parse_xor_mapped(&m).unwrap().to_string(), "192.0.2.1:32853");
        // Not a success response.
        m[1] = 0x11;
        assert_eq!(parse_xor_mapped(&m), None);
        assert_eq!(parse_xor_mapped(&[1, 1]), None);
    }

    #[test]
    fn stun_urls() {
        assert_eq!(parse_stun_url("stun:stun.cloudflare.com:3478"), Some(("stun.cloudflare.com".into(), 3478)));
        assert_eq!(parse_stun_url("stun:stun.l.google.com:19302"), Some(("stun.l.google.com".into(), 19302)));
        assert_eq!(parse_stun_url("stun:example.org"), Some(("example.org".into(), 3478)));
        assert_eq!(parse_stun_url("stun:[2001:db8::1]:3478"), Some(("2001:db8::1".into(), 3478)));
        assert_eq!(parse_stun_url("stun:example.org:3478?transport=udp"), Some(("example.org".into(), 3478)));
        assert_eq!(parse_stun_url("turn:turn.example.org:3478"), None);
        assert_eq!(parse_stun_url("stuns:example.org"), None);
        assert_eq!(parse_stun_url("stun:"), None);
        assert_eq!(parse_stun_url("stun:host:notaport"), None);
    }

    #[test]
    fn host_ips_are_never_loopback_or_link_local() {
        for ip in host_ipv4s() {
            assert!(!ip.is_loopback() && !ip.is_link_local(), "{ip}");
        }
        assert!(host_ipv4s().len() <= MAX_HOST_IPS);
    }

    #[test]
    fn answer_negotiates_h264_first_and_bandwidth_feedback() {
        // A Chrome-like offer listing VP8 *before* H.264 must still get H.264, and the answer
        // must keep transport-cc (TWCC) and goog-remb so the phone can ramp its bitrate.
        let offer = chrome_like_offer();
        let mut rtc = build_rtc(Instant::now());
        rtc.add_local_candidate(Candidate::host("192.0.2.10:50000".parse().unwrap(), "udp").unwrap());
        let answer = rtc
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(&offer).unwrap())
            .unwrap()
            .to_sdp_string();
        let video = answer.split("m=video").nth(1).expect("video m-line");
        let mline = video.lines().next().unwrap();
        let first_pt = mline.split_whitespace().nth(2).unwrap(); // " 9 UDP/TLS/RTP/SAVPF <pt> …"
        assert!(
            video.contains(&format!("a=rtpmap:{first_pt} H264/90000")),
            "first answered video payload must be H.264: {mline}\n{answer}"
        );
        assert!(video.contains("transport-cc"), "{answer}");
        assert!(video.contains("goog-remb"), "{answer}");
        assert!(answer.contains("transport-wide-cc"), "{answer}");
        assert!(answer.contains("a=candidate:"), "candidates embedded in the answer");
        assert!(answer.len() < MAX_SDP_BYTES);
    }

    fn chrome_like_offer() -> String {
        [
            "v=0",
            "o=- 4611731400430051336 2 IN IP4 127.0.0.1",
            "s=-",
            "t=0 0",
            "a=group:BUNDLE 0",
            "a=extmap-allow-mixed",
            "a=msid-semantic: WMS stream",
            "m=video 9 UDP/TLS/RTP/SAVPF 96 97 102 103",
            "c=IN IP4 0.0.0.0",
            "a=rtcp:9 IN IP4 0.0.0.0",
            "a=ice-ufrag:abcd",
            "a=ice-pwd:abcdefghijklmnopqrstuvwx",
            "a=ice-options:trickle",
            "a=fingerprint:sha-256 7B:8B:F0:65:5F:78:E2:51:3B:AC:6F:F3:3F:46:1B:35:DC:B8:5F:64:1A:24:C2:43:F0:A1:58:D0:A1:2C:19:08",
            "a=setup:actpass",
            "a=mid:0",
            "a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01",
            "a=sendonly",
            "a=msid:stream track",
            "a=rtcp-mux",
            "a=rtcp-rsize",
            "a=rtpmap:96 VP8/90000",
            "a=rtcp-fb:96 goog-remb",
            "a=rtcp-fb:96 transport-cc",
            "a=rtcp-fb:96 ccm fir",
            "a=rtcp-fb:96 nack",
            "a=rtcp-fb:96 nack pli",
            "a=rtpmap:97 rtx/90000",
            "a=fmtp:97 apt=96",
            "a=rtpmap:102 H264/90000",
            "a=rtcp-fb:102 goog-remb",
            "a=rtcp-fb:102 transport-cc",
            "a=rtcp-fb:102 ccm fir",
            "a=rtcp-fb:102 nack",
            "a=rtcp-fb:102 nack pli",
            "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
            "a=rtpmap:103 rtx/90000",
            "a=fmtp:103 apt=102",
            "a=ssrc-group:FID 1111 2222",
            "a=ssrc:1111 cname:phone",
            "a=ssrc:2222 cname:phone",
            "",
        ]
        .join("\r\n")
    }
}
