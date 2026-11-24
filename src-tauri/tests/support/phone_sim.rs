//! Test-only stand-in for the phone's browser: a str0m *sender* over real UDP on this box.
//! Adapted from the receiver spike's `tools/loopback-sender`.
//!
//! * builds an offer (video sendonly + an audio m-line, **no candidates** in the offer),
//! * takes the desktop's answer, then trickles its own host candidate and end-of-candidates
//!   (so the desktop's trickle path is exercised),
//! * plays the fixture at 30 fps in a loop, answering keyframe requests with the next
//!   keyframe like a real encoder would.
#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use camlooper_lib::pipeline::EncodedCodec;
use camlooper_lib::remote::stream::media::{annexb_nals, h264_is_keyframe, vp8_is_keyframe};
use str0m::change::SdpAnswer;
use str0m::format::Codec as SCodec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

/// Split an x264 Annex-B file into access units (one slice per frame: pending non-VCL NALs
/// plus one VCL NAL).
pub fn h264_access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut aus = vec![];
    let mut cur = vec![];
    for n in annexb_nals(data) {
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(n);
        if matches!(n[0] & 0x1f, 1 | 5) {
            aus.push(std::mem::take(&mut cur));
        }
    }
    aus
}

pub fn ivf_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut v = vec![];
    let mut i = u16::from_le_bytes([data[6], data[7]]) as usize;
    while i + 12 <= data.len() {
        let n = u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        v.push(data[i + 12..i + 12 + n].to_vec());
        i += 12 + n;
    }
    v
}

#[derive(Debug, Default)]
pub struct SimReport {
    pub connected: bool,
    pub frames_sent: u64,
    pub keyframe_requests: u32,
}

pub struct PhoneSim {
    /// The offer SDP (available immediately).
    pub offer: String,
    answer_tx: mpsc::Sender<String>,
    /// The phone's trickled candidate lines (`candidate:…`), then `""`.
    pub candidates: mpsc::Receiver<String>,
    join: std::thread::JoinHandle<SimReport>,
}

impl PhoneSim {
    /// Start a phone that sends `frames` (`codec`) for `seconds` after it connects, then
    /// vanishes without a word (the desktop sees the media stop).
    pub fn start(codec: EncodedCodec, frames: Vec<Vec<u8>>, seconds: u64) -> PhoneSim {
        Self::start_with(codec, frames, seconds, false)
    }

    /// Like [`Self::start`]; with `close` the phone ends like a browser's
    /// `RTCPeerConnection.close()`: a DTLS close_notify to the desktop.
    pub fn start_with(codec: EncodedCodec, frames: Vec<Vec<u8>>, seconds: u64, close: bool) -> PhoneSim {
        assert!(!frames.is_empty());
        let mut cfg = RtcConfig::new()
            .set_crypto_provider(Arc::new(str0m_purecrypto::default_provider()))
            .clear_codecs()
            .enable_opus(true, false);
        match codec {
            EncodedCodec::Vp8 => cfg = cfg.enable_vp8(true),
            EncodedCodec::H264 => cfg.codec_config().add_h264(102.into(), Some(103.into()), true, 0x42e01f),
        }
        let mut rtc: Rtc = cfg.build(Instant::now());
        let mut change = rtc.sdp_api();
        let vmid: Mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let _amid = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().expect("offer");
        let offer = offer.to_sdp_string();

        let (answer_tx, answer_rx) = mpsc::channel::<String>();
        let (cand_tx, cand_rx) = mpsc::channel::<String>();
        let join = std::thread::spawn(move || {
            let answer = answer_rx.recv_timeout(Duration::from_secs(20)).expect("answer");
            let answer = SdpAnswer::from_sdp_string(&answer).expect("parse answer");
            rtc.sdp_api().accept_answer(pending, answer).expect("accept answer");
            let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let local = sock.local_addr().unwrap();
            let c = Candidate::host(local, "udp").unwrap();
            let _ = cand_tx.send(c.to_sdp_string());
            let _ = cand_tx.send(String::new());
            rtc.add_local_candidate(c);
            play(rtc, sock, local, vmid, codec, frames, seconds, close)
        });
        PhoneSim { offer, answer_tx, candidates: cand_rx, join }
    }

    pub fn answer(&self, sdp: String) {
        self.answer_tx.send(sdp).expect("sim thread alive");
    }

    pub fn finish(self) -> SimReport {
        self.join.join().expect("sim thread")
    }
}

#[allow(clippy::too_many_arguments)]
fn play(
    mut rtc: Rtc,
    sock: UdpSocket,
    local: SocketAddr,
    vmid: Mid,
    codec: EncodedCodec,
    frames: Vec<Vec<u8>>,
    seconds: u64,
    close: bool,
) -> SimReport {
    let is_key = |f: &[u8]| match codec {
        EncodedCodec::H264 => h264_is_keyframe(f),
        EncodedCodec::Vp8 => vp8_is_keyframe(f),
    };
    let keyidx: Vec<usize> = (0..frames.len()).filter(|&i| is_key(&frames[i])).collect();
    assert!(!keyidx.is_empty(), "fixture has no keyframe");
    sock.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
    let mut report = SimReport::default();
    let connect_deadline = Instant::now() + Duration::from_secs(15);
    let mut end: Option<Instant> = None;
    let mut idx = 0usize;
    let mut force_kf = false;
    let mut pt = None;
    let mut next_frame = Instant::now();
    let mut buf = vec![0u8; 2048];

    loop {
        let now = Instant::now();
        if end.is_some_and(|e| now >= e) || (end.is_none() && now >= connect_deadline) || !rtc.is_alive() {
            break;
        }
        loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(_)) => break,
                Ok(Output::Transmit(t)) => {
                    let _ = sock.send_to(&t.contents, t.destination);
                }
                Ok(Output::Event(e)) => match e {
                    // On loopback ICE can go Checking → Completed without a Connected event.
                    Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed)
                        if !report.connected =>
                    {
                        report.connected = true;
                        end = Some(Instant::now() + Duration::from_secs(seconds));
                    }
                    Event::KeyframeRequest(_) => {
                        report.keyframe_requests += 1;
                        force_kf = true;
                    }
                    _ => {}
                },
                Err(_) => return report,
            }
        }
        let now = Instant::now();
        if report.connected && now >= next_frame {
            if let Some(w) = rtc.writer(vmid) {
                if pt.is_none() {
                    pt = w
                        .payload_params()
                        .find(|p| matches!(p.spec().codec, SCodec::H264 | SCodec::Vp8))
                        .map(|p| p.pt());
                }
                if let Some(pt) = pt {
                    if force_kf {
                        idx = keyidx.iter().copied().find(|&k| k >= idx).unwrap_or(keyidx[0]);
                        force_kf = false;
                    }
                    let mt = MediaTime::new(report.frames_sent * 3000, Frequency::NINETY_KHZ);
                    if w.write(pt, now, mt, frames[idx].clone()).is_ok() {
                        report.frames_sent += 1;
                    }
                    idx = (idx + 1) % frames.len();
                }
            }
            next_frame += Duration::from_micros(33_333);
            continue;
        }
        let input = match sock.recv_from(&mut buf) {
            Ok((n, source)) => match buf[..n].try_into() {
                Ok(contents) => Input::Receive(
                    Instant::now(),
                    Receive { proto: Protocol::Udp, source, destination: local, contents },
                ),
                Err(_) => continue,
            },
            Err(_) => Input::Timeout(Instant::now()),
        };
        if rtc.handle_input(input).is_err() {
            break;
        }
    }
    if close && report.connected && rtc.close().is_ok() {
        // Drain the close (RTCP BYE, DTLS close_notify) onto the wire.
        let until = Instant::now() + Duration::from_millis(300);
        while rtc.is_alive() && Instant::now() < until {
            match rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    let _ = sock.send_to(&t.contents, t.destination);
                }
                Ok(Output::Timeout(_)) => {
                    std::thread::sleep(Duration::from_millis(5));
                    let _ = rtc.handle_input(Input::Timeout(Instant::now()));
                }
                Ok(Output::Event(_)) => {}
                Err(_) => break,
            }
        }
    }
    rtc.disconnect();
    report
}
