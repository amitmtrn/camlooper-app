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
//! * **Hostile input.** The UDP ports are reachable by anything on the LAN. A datagram is only
//!   handed to str0m when [`Rtc::accepts`] says it belongs to this session (STUN with our
//!   ufrag, or a source that is a validated ICE peer), and an error from a received datagram
//!   is logged and ignored — it never ends the session. Remote candidates are filtered (UDP,
//!   IPv4, a real unicast address) and capped per session; oversized frames are dropped.
//! * **Peer close.** When the phone closes its `RTCPeerConnection` the DTLS close_notify
//!   arrives here before (or without) the relay's `stream_stop`. That is reported as
//!   [`RtcClose::PeerClosed`], a normal ending, not an error.
//!
//! * **File sessions** ([`RtcMode::File`]). The same task, sockets and ICE/DTLS, but the offer
//!   has only an `m=application` section and the phone opens one reliable data channel. Its
//!   messages are forwarded *in order and without loss*: when the consumer (which writes them
//!   to disk) falls behind, the task stops reading the network until it has caught up, and
//!   SCTP's flow control slows the phone down. No media timeout applies — the consumer has
//!   its own idle timeout.
//!
//! Privacy: SDP and candidate addresses are logged at debug level only.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use str0m::bwe::Bitrate;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::format::Codec as SCodec;
use str0m::media::{KeyframeRequestKind, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::file_receive::{DATA_CHANNEL_LABEL, MAX_CHUNK_BYTES};
use super::media::{h264_is_keyframe, vp8_is_keyframe, PliLimiter};
use super::signal::{StopReason, MAX_REMOTE_CANDIDATES, MAX_SDP_BYTES};
use crate::pipeline::EncodedCodec;

const MAX_HOST_IPS: usize = 4;
/// A depacketized frame larger than this is dropped (a 1080p keyframe is well under 1 MB).
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const STUN_BUDGET: Duration = Duration::from_millis(800);
const FRAME_QUEUE: usize = 90;
/// `progress` reports waiting for the data channel beyond this many are not queued.
const MAX_PENDING_TEXT: usize = 64;
/// While the consumer of a file session is behind, str0m's timers run this often.
const OUTBOX_TIMER: Duration = Duration::from_millis(100);

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

/// What the session is for, which decides the offer it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcMode {
    /// The phone's camera: the offer must carry video.
    Live,
    /// One video file over a data channel: the offer must carry no media at all.
    File,
}

#[derive(Debug)]
pub enum RtcCommand {
    /// A remote trickle candidate (`candidate:…`); empty = end-of-candidates.
    RemoteCandidate(String),
    RequestKeyframe,
    /// The consumer dropped frames; treat the stream as discontinuous.
    FramesDropped,
    /// Send a text message on the file data channel (queued until the channel can take it).
    /// Never dropped: these are the few messages a transfer ends with.
    ChannelText(String),
    /// Like [`Self::ChannelText`], for a `progress` report: one that is still waiting when
    /// the next arrives is replaced by it, and none is queued once the queue is long.
    ChannelProgress(String),
    /// Answered once every [`Self::ChannelText`] sent before it has been acknowledged by the
    /// phone (SCTP), or at once when there is no channel to deliver on. Dropped unanswered
    /// when the task ends first.
    Delivered(tokio::sync::oneshot::Sender<()>),
    Stop,
}

#[derive(Debug)]
pub enum RtcEvent {
    IceConnected,
    Frame(EncodedFrame),
    /// The phone opened the file data channel.
    ChannelOpen,
    /// A text / binary message on the file data channel, in order, none dropped.
    ChannelText(String),
    ChannelBinary(Vec<u8>),
    /// The phone closed the file data channel (but not the connection).
    ChannelClosed,
    /// The task has ended. `Stopped` when asked to.
    Closed(RtcClose),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RtcClose {
    Stopped,
    /// The phone closed the connection (DTLS close_notify). A normal ending.
    PeerClosed,
    Failed { reason: StopReason, detail: String },
}

/// Sockets bound and candidates gathered, ready for an offer.
pub struct LocalTransport {
    /// Each socket with its bound address (looked up once, not per packet).
    sockets: Vec<(SocketAddr, Arc<UdpSocket>)>,
    candidates: Vec<Candidate>,
}

impl LocalTransport {
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
}

/// Interface name prefixes that are never on the phone's LAN: container and VM bridges, and
/// VPN / tunnel interfaces (whose addresses would also be disclosed to the relay in the answer).
const SKIPPED_INTERFACES: &[&str] = &[
    "docker", "br-", "veth", "virbr", "vboxnet", "vmnet", "cni", "flannel", "podman", "lxcbr", "lxdbr", "tun", "tap", "wg",
    "utun", "tailscale", "zt", "ipsec", "ppp",
];

/// Pick the host candidates from `(interface name, address)` pairs: no loopback / link-local /
/// unspecified / broadcast; bridges and tunnels are skipped when anything else is available;
/// the default-route interface's address comes first; at most [`MAX_HOST_IPS`].
pub fn select_host_ips(ifaces: &[(String, Ipv4Addr)], default_route: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let usable = |ip: &Ipv4Addr| !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_broadcast();
    let skipped = |name: &str| {
        let name = name.to_ascii_lowercase();
        SKIPPED_INTERFACES.iter().any(|p| name.starts_with(p))
    };
    let mut all: Vec<(bool, Ipv4Addr)> =
        ifaces.iter().filter(|(_, ip)| usable(ip)).map(|(name, ip)| (skipped(name), *ip)).collect();
    all.sort();
    all.dedup();
    let real: Vec<Ipv4Addr> = all.iter().filter(|(v, _)| !v).map(|(_, ip)| *ip).collect();
    let mut ips = if real.is_empty() { all.into_iter().map(|(_, ip)| ip).collect() } else { real };
    if let Some(pos) = default_route.and_then(|d| ips.iter().position(|ip| *ip == d)) {
        let d = ips.remove(pos);
        ips.insert(0, d);
    }
    ips.truncate(MAX_HOST_IPS);
    ips
}

/// The local address the OS would use to reach the internet (no packet is sent: connecting a
/// UDP socket only selects a route).
fn default_route_ipv4() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

/// Usable IPv4 addresses of this machine for host candidates (see [`select_host_ips`]).
pub fn host_ipv4s() -> Vec<Ipv4Addr> {
    let ifaces: Vec<(String, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) => Some((i.name, ip)),
            IpAddr::V6(_) => None,
        })
        .collect();
    select_host_ips(&ifaces, default_route_ipv4())
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
                sockets.push((local, Arc::new(sock)));
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
    mode: RtcMode,
) -> Result<(String, mpsc::UnboundedSender<RtcCommand>, mpsc::Receiver<RtcEvent>)> {
    let start = Instant::now();
    let mut rtc = build_rtc(start, mode);
    for c in &transport.candidates {
        rtc.add_local_candidate(c.clone());
    }
    log::debug!("phone stream: offer sdp:\n{offer_sdp}");
    let allow_loopback = transport.sockets.iter().any(|(a, _)| a.ip().is_loopback());
    let (offer_sdp, offer_candidates) = filter_offer_candidates(offer_sdp, allow_loopback, MAX_REMOTE_CANDIDATES);
    // str0m's errors can quote SDP lines (addresses, ufrag). The detail stays at debug level;
    // what reaches stderr and the UI is a fixed sentence.
    let offer = SdpOffer::from_sdp_string(&offer_sdp).map_err(|e| {
        log::debug!("phone stream: unreadable offer SDP: {e}");
        anyhow!("the phone's offer could not be read")
    })?;
    if !offer_matches_mode(&offer_sdp, mode) {
        log::debug!("phone stream: the offer's sections do not fit a {mode:?} session");
        return Err(anyhow!("the phone's offer does not match the request"));
    }
    let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| {
        log::debug!("phone stream: offer not acceptable: {e}");
        anyhow!("the phone's offer is not supported")
    })?;
    let answer = answer.to_sdp_string();
    log::debug!("phone stream: answer sdp:\n{answer}");
    if answer.len() > MAX_SDP_BYTES {
        return Err(anyhow!("answer SDP is {} bytes, over the relay budget", answer.len()));
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::channel(FRAME_QUEUE);
    let limits = CandidateLimits { used: offer_candidates, allow_loopback };
    tokio::spawn(run(rtc, transport.sockets, RunSetup { opts, mode, start, limits }, cmd_rx, ev_tx));
    Ok((answer, cmd_tx, ev_rx))
}

/// Whether the offer's media sections are what `mode` allows. A file session is a data
/// channel and nothing else (no camera or microphone rides along with a file transfer); a
/// live session must bring video, and a data-channel-only offer is not one.
fn offer_matches_mode(sdp: &str, mode: RtcMode) -> bool {
    let (mut media, mut video, mut application) = (0, 0, 0);
    for line in sdp.lines() {
        let Some(m) = line.trim().strip_prefix("m=") else { continue };
        match m.split_whitespace().next().unwrap_or_default() {
            "application" => application += 1,
            "video" => {
                video += 1;
                media += 1;
            }
            _ => media += 1,
        }
    }
    match mode {
        RtcMode::Live => video >= 1,
        RtcMode::File => application == 1 && media == 0,
    }
}

fn build_rtc(start: Instant, mode: RtcMode) -> Rtc {
    let mut cfg = RtcConfig::new()
        .set_crypto_provider(Arc::new(str0m_purecrypto::default_provider()))
        .clear_codecs()
        .set_stats_interval(None);
    if mode == RtcMode::File {
        // No codecs: only the SCTP association (data channels) is negotiated. The phone
        // sends pieces of at most 64 KiB; say so (`a=max-message-size`) and let SCTP refuse
        // anything larger instead of reassembling it first (str0m's default is 256 KiB).
        return cfg.set_sctp_max_message_size(MAX_CHUNK_BYTES as u32).build(start);
    }
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

/// Whether a remote candidate is worth pairing with: UDP, IPv4 (we only bind IPv4), a real
/// unicast address and port. Loopback only when we bound loopback ourselves (tests).
fn candidate_allowed(c: &Candidate, allow_loopback: bool) -> bool {
    let addr = c.addr();
    let IpAddr::V4(ip) = addr.ip() else { return false };
    c.proto() == Protocol::Udp
        && addr.port() != 0
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_broadcast()
        && (allow_loopback || !ip.is_loopback())
}

/// Drop `a=candidate:` lines from the offer that fail [`candidate_allowed`], cannot be parsed
/// (mDNS names, which str0m cannot use anyway) or exceed `max`. Returns the SDP and how many
/// candidates were kept.
fn filter_offer_candidates(sdp: &str, allow_loopback: bool, max: usize) -> (String, usize) {
    let mut out = String::with_capacity(sdp.len());
    let mut kept = 0;
    for line in sdp.split_inclusive('\n') {
        if let Some(c) = line.trim_end().strip_prefix("a=candidate:") {
            let ok = kept < max
                && Candidate::from_sdp_string(&format!("candidate:{c}")).is_ok_and(|c| candidate_allowed(&c, allow_loopback));
            if !ok {
                continue;
            }
            kept += 1;
        }
        out.push_str(line);
    }
    (out, kept)
}

struct CandidateLimits {
    /// Remote candidates taken so far this session (offer + trickle).
    used: usize,
    allow_loopback: bool,
}

struct Udp {
    local: SocketAddr,
    source: SocketAddr,
    data: Vec<u8>,
}

/// What the session task is started with, besides the `Rtc` and its sockets.
struct RunSetup {
    opts: RtcOptions,
    mode: RtcMode,
    start: Instant,
    limits: CandidateLimits,
}

async fn run(
    mut rtc: Rtc,
    sockets: Vec<(SocketAddr, Arc<UdpSocket>)>,
    setup: RunSetup,
    mut cmd_rx: mpsc::UnboundedReceiver<RtcCommand>,
    ev_tx: mpsc::Sender<RtcEvent>,
) {
    let RunSetup { opts, mode, start, mut limits } = setup;
    let (udp_tx, mut udp_rx) = mpsc::channel::<Udp>(1024);
    let readers: Vec<_> = sockets.iter().map(|(local, s)| spawn_reader(*local, s.clone(), udp_tx.clone())).collect();
    drop(udp_tx);

    let mut st = LoopState {
        mode,
        video_mid: None,
        connected_at: None,
        last_media: None,
        pli: PliLimiter::new(opts.pli_interval),
        next_remb: start,
        discontinuity: false,
        peer_closed: false,
        file_channel: None,
        outbox: std::collections::VecDeque::new(),
        pending_text: std::collections::VecDeque::new(),
        delivery_waiters: Vec::new(),
        ignored: 0,
        frames: 0,
        dropped: 0,
        plis: 0,
    };
    let close = 'main: loop {
        // 0. File sessions: hand over what the data channel delivered, waiting for the
        //    consumer if it is behind. Nothing is read from the network meanwhile, which is
        //    what slows the phone down; commands are still served, so a stop is never stuck.
        while let Some(ev) = st.outbox.pop_front() {
            tokio::select! {
                biased;
                cmd = cmd_rx.recv() => {
                    st.outbox.push_front(ev);
                    match cmd {
                        Some(RtcCommand::Stop) | None => break 'main RtcClose::Stopped,
                        Some(cmd) => handle_command(&mut rtc, cmd, &mut st, &mut limits),
                    }
                }
                permit = ev_tx.reserve() => match permit {
                    Ok(permit) => permit.send(ev),
                    Err(_) => break 'main RtcClose::Stopped,
                },
                // The consumer is behind (a slow disk). Nothing is read from the network
                // meanwhile, but str0m's own timers — ICE consent checks, SACKs,
                // retransmissions — still have to run, or a long wait ends the connection.
                _ = tokio::time::sleep(OUTBOX_TIMER) => {
                    st.outbox.push_front(ev);
                    if rtc.handle_input(Input::Timeout(Instant::now())).is_err() {
                        break 'main RtcClose::Failed { reason: StopReason::Error, detail: "the connection failed".into() };
                    }
                    if let Err(close) = drain(&mut rtc, &sockets, &mut st, &ev_tx) {
                        break 'main close;
                    }
                }
            }
        }
        flush_channel_text(&mut rtc, &mut st);
        // 1. Drain everything str0m wants to do.
        let timeout = match drain(&mut rtc, &sockets, &mut st, &ev_tx) {
            Ok(t) => t,
            Err(close) => break close,
        };
        notify_delivered(&mut rtc, &mut st);
        if !st.outbox.is_empty() && !st.peer_closed {
            continue;
        }
        if st.peer_closed {
            // The close_notify reply went out in the drain above.
            break RtcClose::PeerClosed;
        }
        // 2. Our own timers.
        let now = Instant::now();
        if st.connected_at.is_none() && now.duration_since(start) >= opts.ice_timeout {
            break RtcClose::Failed { reason: StopReason::IceFailed, detail: "no ICE connection".into() };
        }
        if let (Some(c), RtcMode::Live) = (st.connected_at, mode) {
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
            // str0m disconnects itself on a DTLS fingerprint mismatch (and little else).
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
                Some(p) => {
                    handle_udp(&mut rtc, p, &mut st);
                    Ok(())
                }
                None => break RtcClose::Failed { reason: StopReason::Error, detail: "network sockets closed".into() },
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(RtcCommand::Stop) | None => break RtcClose::Stopped,
                Some(cmd) => {
                    handle_command(&mut rtc, cmd, &mut st, &mut limits);
                    Ok(())
                }
            },
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(wake)) => {
                rtc.handle_input(Input::Timeout(Instant::now())).map_err(|e| e.to_string())
            }
        };
        if let Err(e) = input_result {
            if st.peer_closed {
                break RtcClose::PeerClosed;
            }
            log::debug!("phone stream: receiver timer error: {e}");
            break RtcClose::Failed { reason: StopReason::Error, detail: "the connection failed".into() };
        }
    };

    // Last words for the phone (a file session's `looping` / `error`) go out before the close.
    flush_channel_text(&mut rtc, &mut st);
    let _ = drain(&mut rtc, &sockets, &mut st, &ev_tx);
    rtc.disconnect();
    // Flush whatever the disconnect produced (DTLS close alert) best effort.
    let _ = drain(&mut rtc, &sockets, &mut st, &ev_tx);
    for r in readers {
        r.abort();
    }
    eprintln!(
        "phone stream: receiver closed ({}) after {} frames, {} dropped, {} keyframe requests, {} stray datagrams",
        match &close {
            RtcClose::Stopped => "stopped".to_string(),
            RtcClose::PeerClosed => "closed by the phone".to_string(),
            RtcClose::Failed { reason, detail } => format!("{reason}: {detail}"),
        },
        st.frames,
        st.dropped,
        st.plis,
        st.ignored
    );
    let _ = ev_tx.send(RtcEvent::Closed(close)).await;
}

struct LoopState {
    mode: RtcMode,
    video_mid: Option<Mid>,
    connected_at: Option<Instant>,
    last_media: Option<Instant>,
    pli: PliLimiter,
    next_remb: Instant,
    /// Frames were dropped since the last delivered one; the next is marked non-contiguous.
    discontinuity: bool,
    /// The phone closed the connection (str0m's `Event::Closed`).
    peer_closed: bool,
    /// The phone's file data channel, once open.
    file_channel: Option<ChannelId>,
    /// Data channel events not yet handed to the consumer (see step 0 of the loop).
    outbox: std::collections::VecDeque<RtcEvent>,
    /// Text for the phone that the channel could not take yet; `true` marks a `progress`
    /// report, which a newer one may replace.
    pending_text: std::collections::VecDeque<(String, bool)>,
    /// [`RtcCommand::Delivered`] requests waiting for the phone's acknowledgement.
    delivery_waiters: Vec<tokio::sync::oneshot::Sender<()>>,
    /// Datagrams that were not for this session or that str0m rejected.
    ignored: u64,
    frames: u64,
    dropped: u64,
    plis: u64,
}

/// Every command but `Stop`, which the callers handle (it ends the loop).
fn handle_command(rtc: &mut Rtc, cmd: RtcCommand, st: &mut LoopState, limits: &mut CandidateLimits) {
    match cmd {
        RtcCommand::RemoteCandidate(c) => add_remote_candidate(rtc, &c, limits),
        RtcCommand::RequestKeyframe => st.pli.want(),
        RtcCommand::FramesDropped => {
            st.discontinuity = true;
            st.pli.want();
        }
        RtcCommand::ChannelText(text) => {
            // Never dropped (`saved`, `looping`, `error`, the final `progress`): a session
            // sends a handful of these in all, so the queue stays bounded by the protocol.
            st.pending_text.push_back((text, false));
            flush_channel_text(rtc, st);
        }
        RtcCommand::ChannelProgress(text) => {
            queue_progress(&mut st.pending_text, text);
            flush_channel_text(rtc, st);
        }
        RtcCommand::Delivered(done) => st.delivery_waiters.push(done),
        RtcCommand::Stop => {}
    }
}

/// Whether a data channel the phone opened becomes the session's file channel: only in a
/// file session, only under the agreed label, and only the first one.
fn adopts_channel(mode: RtcMode, label: &str, have_one: bool) -> bool {
    mode == RtcMode::File && label == DATA_CHANNEL_LABEL && !have_one
}

/// Queue a `progress` report: it replaces one that is still waiting at the back (only the
/// newest count matters), and is dropped when [`MAX_PENDING_TEXT`] messages already wait.
fn queue_progress(pending: &mut std::collections::VecDeque<(String, bool)>, text: String) {
    if let Some((waiting, true)) = pending.back_mut() {
        *waiting = text;
    } else if pending.len() < MAX_PENDING_TEXT {
        pending.push_back((text, true));
    }
}

/// Answer [`RtcCommand::Delivered`] once nothing is queued here and the phone has
/// acknowledged everything written to the channel (SCTP counts a message as buffered until
/// its SACK arrives). Without a channel nothing can be delivered any more: answered at once.
fn notify_delivered(rtc: &mut Rtc, st: &mut LoopState) {
    if st.delivery_waiters.is_empty() {
        return;
    }
    let outstanding = match st.file_channel {
        Some(id) => !st.pending_text.is_empty() || rtc.channel(id).is_some_and(|mut c| c.buffered_amount() > 0),
        None => false,
    };
    if !outstanding {
        for done in st.delivery_waiters.drain(..) {
            let _ = done.send(());
        }
    }
}

/// Write queued text to the file data channel, in order, as far as its send buffer allows.
fn flush_channel_text(rtc: &mut Rtc, st: &mut LoopState) {
    let Some(id) = st.file_channel else { return };
    while let Some((text, _)) = st.pending_text.front() {
        let Some(mut channel) = rtc.channel(id) else { return };
        match channel.write(false, text.as_bytes()) {
            Ok(true) => {
                st.pending_text.pop_front();
            }
            Ok(false) => return, // send buffer full: retried on the next pass
            Err(e) => {
                log::debug!("phone stream: could not write to the data channel: {e}");
                st.pending_text.clear();
                return;
            }
        }
    }
}

/// Poll str0m until it asks to be woken at a time. Transmits go out immediately.
fn drain(
    rtc: &mut Rtc,
    sockets: &[(SocketAddr, Arc<UdpSocket>)],
    st: &mut LoopState,
    ev_tx: &mpsc::Sender<RtcEvent>,
) -> Result<Instant, RtcClose> {
    loop {
        let out = match rtc.poll_output() {
            Ok(out) => out,
            // Closing down after the phone's close_notify is not a failure, whatever it trips over.
            Err(_) if st.peer_closed => return Err(RtcClose::PeerClosed),
            Err(e) => {
                log::debug!("phone stream: receiver error: {e}");
                return Err(RtcClose::Failed { reason: StopReason::Error, detail: "the connection failed".into() });
            }
        };
        match out {
            Output::Timeout(t) => return Ok(t),
            Output::Transmit(t) => {
                if let Some((_, s)) = sockets.iter().find(|(local, _)| *local == t.source) {
                    // UDP: a full send buffer is the same as loss; never block the loop on it.
                    let _ = s.try_send_to(&t.contents, t.destination);
                }
            }
            Output::Event(ev) => match ev {
                Event::Closed => {
                    log::debug!("phone stream: the phone closed the connection");
                    st.peer_closed = true;
                }
                Event::IceConnectionStateChange(state) => {
                    log::debug!("phone stream: ICE {state:?}");
                    if matches!(state, IceConnectionState::Connected | IceConnectionState::Completed)
                        && st.connected_at.is_none()
                    {
                        st.connected_at = Some(Instant::now());
                        let _ = ev_tx.try_send(RtcEvent::IceConnected);
                    }
                }
                Event::ChannelOpen(id, label) => {
                    // A live session has no use for a data channel, whatever it is called:
                    // adopting one would let the phone queue messages into the path the
                    // video frames take.
                    if adopts_channel(st.mode, &label, st.file_channel.is_some()) {
                        st.file_channel = Some(id);
                        st.outbox.push_back(RtcEvent::ChannelOpen);
                    } else {
                        log::debug!("phone stream: ignoring a data channel that is not the file channel");
                    }
                }
                Event::ChannelData(d) if Some(d.id) == st.file_channel => {
                    st.outbox.push_back(if d.binary {
                        RtcEvent::ChannelBinary(d.data)
                    } else {
                        RtcEvent::ChannelText(String::from_utf8_lossy(&d.data).into_owned())
                    });
                }
                Event::ChannelClose(id) if Some(id) == st.file_channel => {
                    st.file_channel = None;
                    st.pending_text.clear();
                    st.outbox.push_back(RtcEvent::ChannelClosed);
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
                    if d.data.len() > MAX_FRAME_BYTES {
                        st.dropped += 1;
                        st.discontinuity = true;
                        st.pli.want();
                        continue;
                    }
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

/// Feed one datagram to str0m, if it belongs to this session. Never fatal: the socket is
/// reachable by anything on the network, so a datagram str0m rejects (a malformed DTLS record,
/// a stray packet from a port scan) is counted and dropped. A real failure shows up as
/// `!rtc.is_alive()` or as a timeout.
fn handle_udp(rtc: &mut Rtc, p: Udp, st: &mut LoopState) {
    let Ok(contents) = p.data.as_slice().try_into() else {
        return; // not STUN/DTLS/RTP
    };
    let input = Input::Receive(
        Instant::now(),
        Receive { proto: Protocol::Udp, source: p.source, destination: p.local, contents },
    );
    if !rtc.accepts(&input) {
        st.ignored += 1;
        return;
    }
    if let Err(e) = rtc.handle_input(input) {
        st.ignored += 1;
        log::debug!("phone stream: ignoring a datagram str0m rejected: {e}");
    }
}

fn add_remote_candidate(rtc: &mut Rtc, c: &str, limits: &mut CandidateLimits) {
    if c.is_empty() {
        return; // end-of-candidates: nothing to do for str0m
    }
    if limits.used >= MAX_REMOTE_CANDIDATES {
        log::debug!("phone stream: ignoring candidate (over the per-session limit)");
        return;
    }
    match Candidate::from_sdp_string(c) {
        // str0m is BUNDLE-only, so sdpMid / sdpMLineIndex do not matter.
        Ok(cand) if candidate_allowed(&cand, limits.allow_loopback) => {
            log::debug!("phone stream: remote candidate {c}");
            limits.used += 1;
            rtc.add_remote_candidate(cand)
        }
        Ok(_) => log::debug!("phone stream: ignoring candidate (not a usable UDP/IPv4 address)"),
        // mDNS (.local) host candidates land here: str0m cannot resolve them. The phone's
        // srflx/prflx path still works on the same LAN.
        Err(e) => log::debug!("phone stream: ignoring candidate ({e})"),
    }
}

fn spawn_reader(local: SocketAddr, sock: Arc<UdpSocket>, tx: mpsc::Sender<Udp>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
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
async fn stun_srflx(sockets: &[(SocketAddr, Arc<UdpSocket>)], server: SocketAddr) -> Option<(SocketAddr, SocketAddr)> {
    let route_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(server)?;
            s.local_addr()
        })
        .ok()
        .map(|a| a.ip());
    let (local, sock) = sockets.iter().find(|(a, _)| Some(a.ip()) == route_ip).or_else(|| sockets.first())?.clone();
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

/// XOR-MAPPED-ADDRESS (IPv4) from a STUN binding success response. The header must carry the
/// magic cookie and a length that matches the datagram.
pub fn parse_xor_mapped(msg: &[u8]) -> Option<SocketAddr> {
    if msg.len() < 20 || msg[0..2] != [0x01, 0x01] || msg[4..8] != [0x21, 0x12, 0xa4, 0x42] {
        return None;
    }
    let body = u16::from_be_bytes([msg[2], msg[3]]) as usize;
    if !body.is_multiple_of(4) || msg.len() != 20 + body {
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
        // A wrong magic cookie or a length that does not match the datagram is not STUN.
        m[1] = 0x01;
        assert!(parse_xor_mapped(&m).is_some());
        let mut bad = m.clone();
        bad[4] ^= 0xff;
        assert_eq!(parse_xor_mapped(&bad), None);
        let mut bad = m.clone();
        bad[3] = 0x10;
        assert_eq!(parse_xor_mapped(&bad), None);
        let mut bad = m.clone();
        bad.push(0);
        assert_eq!(parse_xor_mapped(&bad), None);
    }

    #[test]
    fn host_candidates_skip_tunnels_and_lead_with_the_default_route() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let ifaces: Vec<(String, Ipv4Addr)> = [
            ("lo", "127.0.0.1"),
            ("docker0", "172.17.0.1"),
            ("tun0", "10.8.0.2"),
            ("wg0", "10.9.0.2"),
            ("utun3", "100.64.0.1"),
            ("tailscale0", "100.100.1.1"),
            ("enp3s0", "192.168.1.20"),
            ("wlp2s0", "192.168.50.7"),
            ("eth9", "169.254.3.4"),
        ]
        .iter()
        .map(|(n, a)| (n.to_string(), ip(a)))
        .collect();
        // Full-tunnel VPN: the default route is the tunnel, which is never advertised.
        assert_eq!(select_host_ips(&ifaces, Some(ip("10.8.0.2"))), vec![ip("192.168.1.20"), ip("192.168.50.7")]);
        // The default-route interface comes first.
        assert_eq!(select_host_ips(&ifaces, Some(ip("192.168.50.7"))), vec![ip("192.168.50.7"), ip("192.168.1.20")]);
        // Only tunnels/bridges available: better than nothing.
        let only_virtual: Vec<_> = ifaces.iter().filter(|(n, _)| n == "tun0" || n == "docker0").cloned().collect();
        assert_eq!(select_host_ips(&only_virtual, None).len(), 2);
        // Capped.
        let many: Vec<_> = (1..=20).map(|i| (format!("eth{i}"), Ipv4Addr::new(10, 0, i, 1))).collect();
        assert_eq!(select_host_ips(&many, None).len(), MAX_HOST_IPS);
    }

    #[test]
    fn remote_candidates_are_filtered() {
        let c = |s: &str| Candidate::from_sdp_string(s).unwrap();
        let ok = "candidate:1 1 udp 2122260223 192.168.1.5 50000 typ host";
        assert!(candidate_allowed(&c(ok), false));
        for bad in [
            "candidate:1 1 tcp 1518280447 192.168.1.5 9 typ host tcptype active",
            "candidate:1 1 udp 2122260223 192.168.1.5 0 typ host",
            "candidate:1 1 udp 2122260223 127.0.0.1 50000 typ host",
            "candidate:1 1 udp 2122260223 0.0.0.0 50000 typ host",
            "candidate:1 1 udp 2122260223 224.0.0.251 5353 typ host",
            "candidate:1 1 udp 2122260223 255.255.255.255 50000 typ host",
            "candidate:1 1 udp 2122260223 2001:db8::1 50000 typ host",
        ] {
            if let Ok(cand) = Candidate::from_sdp_string(bad) {
                assert!(!candidate_allowed(&cand, false), "{bad}");
            }
        }
        // Loopback only when we bound loopback ourselves (the tests' own transport).
        assert!(candidate_allowed(&c("candidate:1 1 udp 2122260223 127.0.0.1 50000 typ host"), true));
    }

    #[test]
    fn offer_candidates_are_filtered_and_counted() {
        let mut offer = chrome_like_offer();
        let mut lines = String::new();
        lines.push_str("a=candidate:1 1 udp 2122260223 192.168.1.5 50000 typ host\r\n");
        lines.push_str("a=candidate:2 1 udp 2122260223 127.0.0.1 50001 typ host\r\n");
        lines.push_str("a=candidate:3 1 udp 2122260223 abcd1234.local 50002 typ host\r\n");
        for i in 0..100 {
            lines.push_str(&format!("a=candidate:{} 1 udp 2122260223 192.168.2.{} 50000 typ host\r\n", 10 + i, 1 + i));
        }
        offer.push_str(&lines);
        let (filtered, kept) = filter_offer_candidates(&offer, false, MAX_REMOTE_CANDIDATES);
        assert_eq!(kept, MAX_REMOTE_CANDIDATES);
        assert_eq!(filtered.matches("a=candidate:").count(), MAX_REMOTE_CANDIDATES);
        assert!(!filtered.contains("127.0.0.1 50001") && !filtered.contains(".local"));
        assert!(filtered.contains("a=fingerprint:") && filtered.ends_with("\r\n"), "everything else is untouched");
        // And the trickle path shares the budget.
        let mut rtc = build_rtc(Instant::now(), RtcMode::Live);
        let mut limits = CandidateLimits { used: kept, allow_loopback: false };
        add_remote_candidate(&mut rtc, "candidate:1 1 udp 2122260223 192.168.9.9 50000 typ host", &mut limits);
        assert_eq!(limits.used, MAX_REMOTE_CANDIDATES);
        let mut limits = CandidateLimits { used: 0, allow_loopback: false };
        add_remote_candidate(&mut rtc, "candidate:1 1 udp 2122260223 192.168.9.9 50000 typ host", &mut limits);
        add_remote_candidate(&mut rtc, "candidate:2 1 udp 2122260223 127.0.0.1 50000 typ host", &mut limits);
        add_remote_candidate(&mut rtc, "", &mut limits);
        assert_eq!(limits.used, 1);
    }

    #[test]
    fn offer_errors_do_not_quote_the_sdp() {
        let t = LocalTransport { sockets: vec![], candidates: vec![] };
        let secret = "v=0\r\no=- 1 2 IN IP4 203.0.113.77\r\nthis is not sdp 203.0.113.77\r\n";
        let err = accept_offer(t, secret, RtcOptions::default(), RtcMode::Live).expect_err("garbage offer");
        let text = format!("{err:#}");
        assert_eq!(text, "the phone's offer could not be read");
    }

    #[test]
    fn dtls_certificates_are_valid_der_with_minimal_positive_serials() {
        use x509_parser::prelude::*;
        let provider = str0m_purecrypto::default_provider();
        let mut serials = std::collections::HashSet::new();
        for _ in 0..300 {
            let cert = provider.dtls_provider.generate_certificate().expect("certificate");
            let (rest, x509) = X509Certificate::from_der(&cert.certificate).expect("strict DER X.509");
            assert!(rest.is_empty(), "no trailing bytes");
            let serial = x509.raw_serial();
            // DER INTEGER: positive, minimal (no redundant leading 0x00), at most 20 octets
            // (RFC 5280 4.1.2.2). 300 certs cover the 1-in-128 leading-zero-byte case twice over.
            assert!(!serial.is_empty() && serial.len() <= 20, "{serial:02x?}");
            assert_eq!(serial[0] & 0x80, 0, "positive: {serial:02x?}");
            assert!(serial.len() == 1 || serial[0] != 0 || serial[1] & 0x80 != 0, "minimal: {serial:02x?}");
            assert!(serials.insert(serial.to_vec()), "serials are unique");
            assert!(x509.validity().is_valid(), "valid now");
            assert_eq!(x509.subject_pki.algorithm.algorithm.to_id_string(), "1.2.840.10045.2.1", "EC key");
            assert!(!cert.private_key.is_empty());
        }
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
        let mut rtc = build_rtc(Instant::now(), RtcMode::Live);
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

    /// What Chrome offers for `pc.createDataChannel(…)` with no tracks.
    fn datachannel_offer() -> String {
        [
            "v=0",
            "o=- 4611731400430051337 2 IN IP4 127.0.0.1",
            "s=-",
            "t=0 0",
            "a=group:BUNDLE 0",
            "a=extmap-allow-mixed",
            "a=msid-semantic: WMS",
            "m=application 9 UDP/DTLS/SCTP webrtc-datachannel",
            "c=IN IP4 0.0.0.0",
            "a=ice-ufrag:abcd",
            "a=ice-pwd:abcdefghijklmnopqrstuvwx",
            "a=ice-options:trickle",
            "a=fingerprint:sha-256 7B:8B:F0:65:5F:78:E2:51:3B:AC:6F:F3:3F:46:1B:35:DC:B8:5F:64:1A:24:C2:43:F0:A1:58:D0:A1:2C:19:08",
            "a=setup:actpass",
            "a=mid:0",
            "a=sctp-port:5000",
            "a=max-message-size:262144",
            "",
        ]
        .join("\r\n")
    }

    #[test]
    fn the_offer_must_fit_the_kind_of_session() {
        let video = chrome_like_offer();
        let data = datachannel_offer();
        let both = format!("{video}{}", data.split_once("m=application").map(|(_, rest)| format!("m=application{rest}")).unwrap());
        assert!(offer_matches_mode(&video, RtcMode::Live));
        assert!(offer_matches_mode(&both, RtcMode::Live), "a live offer may bring a data channel along");
        assert!(!offer_matches_mode(&data, RtcMode::Live), "a data-channel-only offer is not a camera");
        assert!(offer_matches_mode(&data, RtcMode::File));
        assert!(!offer_matches_mode(&video, RtcMode::File), "no media in a file session");
        assert!(!offer_matches_mode(&both, RtcMode::File));
        assert!(!offer_matches_mode("v=0\r\n", RtcMode::File) && !offer_matches_mode("v=0\r\n", RtcMode::Live));
        let audio_only = video.replace("m=video", "m=audio");
        assert!(!offer_matches_mode(&audio_only, RtcMode::Live) && !offer_matches_mode(&audio_only, RtcMode::File));

        // …and `accept_offer` refuses before it builds anything.
        for (offer, mode) in [(&data, RtcMode::Live), (&video, RtcMode::File)] {
            let t = LocalTransport { sockets: vec![], candidates: vec![] };
            let err = accept_offer(t, offer, RtcOptions::default(), mode).expect_err("mismatch");
            assert_eq!(format!("{err:#}"), "the phone's offer does not match the request");
        }
    }

    #[test]
    fn a_file_session_answers_a_datachannel_only_offer() {
        let mut rtc = build_rtc(Instant::now(), RtcMode::File);
        rtc.add_local_candidate(Candidate::host("192.0.2.10:50000".parse().unwrap(), "udp").unwrap());
        let answer = rtc
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(&datachannel_offer()).unwrap())
            .unwrap()
            .to_sdp_string();
        assert!(answer.contains("m=application") && answer.contains("webrtc-datachannel"), "{answer}");
        assert!(answer.contains("a=sctp-port:"), "{answer}");
        assert!(!answer.contains("m=video") && !answer.contains("m=audio"), "{answer}");
        assert!(answer.contains("a=candidate:"), "candidates embedded in the answer");
        // The phone is told the largest message it may send: one 64 KiB piece. (The phone
        // page sizes its pieces by this, and SCTP refuses anything larger.)
        assert!(answer.contains("a=max-message-size:65536"), "{answer}");
        assert_eq!(MAX_CHUNK_BYTES, 65536);
    }

    #[test]
    fn only_a_file_session_adopts_the_file_channel() {
        assert!(adopts_channel(RtcMode::File, DATA_CHANNEL_LABEL, false));
        // A live session ignores data channels altogether, also one with the right label.
        assert!(!adopts_channel(RtcMode::Live, DATA_CHANNEL_LABEL, false));
        assert!(!adopts_channel(RtcMode::File, "something-else", false));
        assert!(!adopts_channel(RtcMode::File, DATA_CHANNEL_LABEL, true), "one channel per session");
    }

    #[test]
    fn progress_reports_are_coalesced_and_nothing_else_is_ever_dropped() {
        let mut pending = std::collections::VecDeque::new();
        let texts = |p: &std::collections::VecDeque<(String, bool)>| p.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>();
        // The channel is not taking anything; progress keeps coming.
        for n in 0..500 {
            queue_progress(&mut pending, format!("progress {n}"));
        }
        assert_eq!(texts(&pending), vec!["progress 499"], "only the newest count waits");
        // A message that must arrive goes behind it and is not replaced by later progress.
        pending.push_back(("saved".into(), false));
        queue_progress(&mut pending, "progress 500".into());
        queue_progress(&mut pending, "progress 501".into());
        assert_eq!(texts(&pending), vec!["progress 499", "saved", "progress 501"]);
        // With the queue long, progress is not queued at all — but the queue itself never
        // refuses a final message (`handle_command` pushes those unconditionally).
        while pending.len() < MAX_PENDING_TEXT {
            pending.push_back(("error".into(), false));
        }
        queue_progress(&mut pending, "progress 502".into());
        assert_eq!(pending.len(), MAX_PENDING_TEXT);
        assert_eq!(pending.back().map(|(t, _)| t.as_str()), Some("error"));
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
