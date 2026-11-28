//! A meeting session end to end on this box, no browser, no relay, no portal: a str0m
//! *viewer* standing in for the phone (it offers `recvonly` audio + video and the transcript
//! data channel) ← the real stream manager, meeting session, ffmpeg encoders (libopus,
//! libx264) and sender, fed by the fixture source (ffmpeg's test pattern and a tone).
//!
//! What is checked on the viewer's side is what a phone would get: Opus packets at the
//! packet rate, H.264 access units that ffprobe decodes as 1280x720, and the "captions come
//! later" note on the data channel.
//!
//! Skipped (with a message) when ffmpeg/ffprobe are not on PATH or lack libx264/libopus.
#![cfg(unix)]

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use camlooper_lib::pipeline::{CameraSink, Geometry};
use camlooper_lib::remote::stream::media::h264_is_keyframe;
use camlooper_lib::remote::stream::meeting::{MediaInput, CAPTIONS_UNAVAILABLE};
use camlooper_lib::remote::stream::rtc::{RtcOptions, TRANSCRIPT_CHANNEL_LABEL};
use camlooper_lib::remote::stream::signal::{LinkState, OutboundFrame, StopReason, StreamMode, StreamSignal};
use camlooper_lib::remote::stream::{
    CameraTarget, ChannelDecisions, DecisionEvent, FixedCamera, FixtureSource, MeetingSource, NoStun, Phase, SessionDeps, StreamManager,
};
use futures_util::future::BoxFuture;
use serde_json::json;
use str0m::change::SdpAnswer;
use str0m::media::{Direction, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::sync::mpsc;

fn encoders_available() -> bool {
    let has = |tool: &str, args: &[&str], needle: &str| {
        Command::new(tool).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(needle)).unwrap_or(false)
    };
    has("ffmpeg", &["-hide_banner", "-encoders"], "libx264")
        && has("ffmpeg", &["-hide_banner", "-encoders"], "libopus")
        && Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success())
}

#[derive(Debug, Default)]
struct ViewerReport {
    connected: bool,
    audio_packets: u64,
    /// Every received access unit, in order.
    video: Vec<Vec<u8>>,
    channel_texts: Vec<String>,
}

/// The phone, as far as a meeting session is concerned.
struct Viewer {
    offer: String,
    answer_tx: std::sync::mpsc::Sender<String>,
    candidates: std::sync::mpsc::Receiver<String>,
    join: std::thread::JoinHandle<ViewerReport>,
}

impl Viewer {
    /// Watch for `seconds` after connecting, then close like a browser (`close`: a DTLS
    /// close_notify) or vanish without a word.
    fn start(seconds: u64, close: bool) -> Viewer {
        let mut cfg = RtcConfig::new().set_crypto_provider(Arc::new(str0m_purecrypto::default_provider())).clear_codecs().enable_opus(true, false);
        cfg.codec_config().add_h264(102.into(), Some(103.into()), true, 0x42e01f);
        let mut rtc: Rtc = cfg.build(Instant::now());
        let mut change = rtc.sdp_api();
        let amid = change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
        let vmid = change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        change.add_channel(TRANSCRIPT_CHANNEL_LABEL.to_string());
        let (offer, pending) = change.apply().expect("offer");
        let offer = offer.to_sdp_string();
        let (answer_tx, answer_rx) = std::sync::mpsc::channel::<String>();
        let (cand_tx, candidates) = std::sync::mpsc::channel::<String>();
        let join = std::thread::spawn(move || {
            let answer = answer_rx.recv_timeout(Duration::from_secs(20)).expect("answer");
            rtc.sdp_api().accept_answer(pending, SdpAnswer::from_sdp_string(&answer).expect("parse answer")).expect("accept answer");
            let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let local = sock.local_addr().unwrap();
            let c = Candidate::host(local, "udp").unwrap();
            let _ = cand_tx.send(c.to_sdp_string());
            let _ = cand_tx.send(String::new());
            rtc.add_local_candidate(c);
            watch(rtc, sock, local, amid, vmid, seconds, close)
        });
        Viewer { offer, answer_tx, candidates, join }
    }
}

fn watch(mut rtc: Rtc, sock: UdpSocket, local: SocketAddr, amid: Mid, vmid: Mid, seconds: u64, close: bool) -> ViewerReport {
    sock.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
    let mut report = ViewerReport::default();
    let connect_deadline = Instant::now() + Duration::from_secs(15);
    let mut end: Option<Instant> = None;
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
                    Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed) if !report.connected => {
                        report.connected = true;
                        end = Some(Instant::now() + Duration::from_secs(seconds));
                    }
                    Event::MediaData(d) if d.mid == amid => report.audio_packets += 1,
                    Event::MediaData(d) if d.mid == vmid => report.video.push(d.data.to_vec()),
                    Event::ChannelData(d) => report.channel_texts.push(String::from_utf8_lossy(&d.data).into_owned()),
                    _ => {}
                },
                Err(_) => return report,
            }
        }
        let input = match sock.recv_from(&mut buf) {
            Ok((n, source)) => match buf[..n].try_into() {
                Ok(contents) => Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents }),
                Err(_) => continue,
            },
            Err(_) => Input::Timeout(Instant::now()),
        };
        if rtc.handle_input(input).is_err() {
            break;
        }
    }
    if close && report.connected && rtc.close().is_ok() {
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

async fn next_out(out: &mut mpsc::UnboundedReceiver<OutboundFrame>) -> OutboundFrame {
    tokio::time::timeout(Duration::from_secs(20), out.recv()).await.expect("frame for the phone in time").unwrap()
}

/// What ffprobe decodes from an Annex B file: frames, and `width,height,frames` plus errors.
fn decode_count(path: &Path) -> (u64, String) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-f", "h264", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames,width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let frames = text.trim().split(',').next_back().and_then(|s| s.parse().ok()).unwrap_or(0);
    (frames, text.trim().to_string() + &String::from_utf8_lossy(&out.stderr))
}

/// Kills the process when dropped (the session drops it with the encoder it feeds).
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A capture that delivers six seconds of raw 1280x720 frames in real time and then goes
/// quiet without ending — what a shared window does when it is closed and the capture
/// process stays up.
struct StallingWindow;

impl MeetingSource for StallingWindow {
    fn audio(&self) -> BoxFuture<'_, anyhow::Result<MediaInput>> {
        FixtureSource { video: false }.audio()
    }

    fn video(&self) -> BoxFuture<'_, anyhow::Result<Option<MediaInput>>> {
        Box::pin(async {
            // Raw I420 at 30 fps for 6 s, then silence on a pipe that stays open.
            let frames = "ffmpeg -hide_banner -loglevel error -nostdin -re -f lavfi -i testsrc2=size=1280x720:rate=30 -t 6 -pix_fmt yuv420p -f rawvideo pipe:1";
            let mut child = Command::new("sh")
                .args(["-c", &format!("{frames}; exec sleep 121")])
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            let stdout = child.stdout.take().expect("piped");
            let mut input = MediaInput::args(&["-f", "rawvideo", "-pixel_format", "yuv420p", "-video_size", "1280x720", "-framerate", "30", "-i", "pipe:0"]);
            input.stdin = Some(stdout.into());
            input.keep = Some(Box::new(KillOnDrop(child)));
            Ok(Some(input))
        })
    }
}

struct Up {
    manager: StreamManager,
    out: mpsc::UnboundedReceiver<OutboundFrame>,
    viewer: Viewer,
}

/// Request, accept, offer/answer, candidates: a meeting session with a viewer attached.
async fn start(video: bool, viewer_seconds: u64, viewer_closes: bool) -> Up {
    start_with(Arc::new(FixtureSource { video }), viewer_seconds, viewer_closes).await
}

async fn start_with(source: Arc<dyn MeetingSource>, viewer_seconds: u64, viewer_closes: bool) -> Up {
    // The camera must never be touched: a sink nobody may write, which the test would see.
    let target = CameraTarget { sink: CameraSink::Null, geometry: Geometry { width: 1280, height: 720, fps: 30 }, preview: false };
    let mut deps = SessionDeps::new("ffmpeg".into(), Arc::new(FixedCamera(target)), Arc::new(NoStun));
    deps.rtc = RtcOptions { bind_ips: Some(vec![Ipv4Addr::LOCALHOST]), ..RtcOptions::default() };
    deps.meeting = Some(source);
    let (dtx, mut decisions) = mpsc::unbounded_channel();
    let (manager, mut out) = StreamManager::spawn(Arc::new(Arc::new(deps)), Arc::new(ChannelDecisions(dtx)));

    manager.on_relay_signal(json!({"kind":"hello","sessionId":"h1","appProtocol":3}));
    match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::HelloOk { features, .. }) => assert!(features.contains(&"meeting".to_string()), "{features:?}"),
        other => panic!("expected hello_ok, got {other:?}"),
    }
    manager.on_relay_signal(json!({"kind":"stream_request","sessionId":"meet-1","deviceLabel":"Sim","mode":"meeting"}));
    let Some(DecisionEvent::Request(req)) = decisions.recv().await else { panic!("no decision request") };
    assert_eq!(req.mode, StreamMode::Meeting);
    manager.accept(&req.session_id).await.unwrap();
    assert!(matches!(next_out(&mut out).await, OutboundFrame::Signal(StreamSignal::StreamAccept { .. })));
    assert!(matches!(
        next_out(&mut out).await,
        OutboundFrame::Signal(StreamSignal::Status { state: LinkState::Connecting, mode: Some(StreamMode::Meeting), .. })
    ));

    let viewer = Viewer::start(viewer_seconds, viewer_closes);
    manager.on_relay_signal(json!({"kind":"offer","sessionId":"meet-1","sdp": viewer.offer}));
    let answer = match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::Answer { sdp, .. }) => sdp,
        other => panic!("expected the answer, got {other:?}"),
    };
    assert_eq!(answer.matches("a=sendonly").count(), 2, "the desktop sends both tracks: {answer}");
    viewer.answer_tx.send(answer).unwrap();
    for _ in 0..2 {
        let c = viewer.candidates.recv_timeout(Duration::from_secs(10)).expect("viewer candidate");
        manager.on_relay_signal(json!({"kind":"candidate","sessionId":"meet-1","candidate":c,"sdpMid":"0","sdpMLineIndex":0}));
    }
    // Live as soon as sound flows — sound only at first, whatever comes later.
    match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::Status { state: LinkState::Live, mode: Some(StreamMode::Meeting), .. }) => {}
        other => panic!("expected status live, got {other:?}"),
    }
    Up { manager, out, viewer }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_viewer_gets_sound_a_decodable_picture_and_the_captions_note() {
    if !encoders_available() {
        eprintln!("skipping: ffmpeg with libx264 and libopus (and ffprobe) is needed");
        return;
    }
    let Up { manager, mut out, viewer } = start(true, 14, false).await;
    let mut status = manager.subscribe();
    // The picture follows the sound: the phone is told its size once it is being sent.
    let with_picture = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let OutboundFrame::Signal(StreamSignal::Status { width, height, codec, mode, .. }) = next_out(&mut out).await {
                if width.is_some() {
                    return (width, height, codec, mode);
                }
            }
        }
    })
    .await
    .expect("a status with the picture's size");
    assert_eq!(with_picture, (Some(1280), Some(720), Some("H264".to_string()), Some(StreamMode::Meeting)));
    tokio::time::sleep(Duration::from_secs(4)).await;
    let st = manager.status();
    assert_eq!((st.state, st.mode, st.width, st.height), (Phase::Live, StreamMode::Meeting, Some(1280), Some(720)), "{st:?}");
    assert!(st.frames > 200, "frames sent: {st:?}");
    assert!(st.fps.unwrap_or(0.0) >= 20.0, "{st:?}");

    manager.stop(StopReason::User).await;
    assert!(matches!(next_out(&mut out).await, OutboundFrame::StreamStop { reason: StopReason::User, .. }));
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state == Phase::Idle)).await.unwrap().unwrap();
    assert_eq!(manager.status().last_error, None);

    // The desktop's stop closes the connection; the viewer notices and ends.
    let report = tokio::task::spawn_blocking(move || viewer.join.join().expect("viewer thread")).await.unwrap();
    assert!(report.connected);
    // 50 Opus packets a second.
    assert!(report.audio_packets >= 150, "only {} audio packets", report.audio_packets);
    assert_eq!(report.channel_texts, vec![CAPTIONS_UNAVAILABLE.to_string()]);
    assert!(report.video.len() >= 60, "only {} video frames", report.video.len());
    assert!(h264_is_keyframe(&report.video[0]), "the picture starts on a keyframe");
    let keyframes = report.video.iter().filter(|f| h264_is_keyframe(f)).count();
    assert!(keyframes >= 3, "a keyframe every second: {keyframes} in {} frames", report.video.len());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("received.h264");
    std::fs::write(&path, report.video.concat()).unwrap();
    let (decoded, info) = decode_count(&path);
    eprintln!("meeting: {} audio packets, {} video frames, {decoded} decoded ({info})", report.audio_packets, report.video.len());
    assert!(decoded >= 60, "only {decoded} decodable frames: {info}");
    assert!(info.starts_with("1280,720,"), "{info}");
    assert!(!info.to_lowercase().contains("error"), "decode errors: {info}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_picture_it_is_a_sound_only_session_and_a_vanished_viewer_ends_it() {
    if !encoders_available() {
        eprintln!("skipping: ffmpeg with libx264 and libopus (and ffprobe) is needed");
        return;
    }
    // No window was picked (the fixture has none). The viewer vanishes after 3 s.
    let Up { manager, mut out, viewer } = start(false, 3, false).await;
    let mut status = manager.subscribe();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let st = manager.status();
    assert_eq!((st.state, st.mode, st.width, st.codec.clone()), (Phase::Live, StreamMode::Meeting, None, None), "{st:?}");
    assert!(st.frames >= 30, "audio packets sent: {st:?}");

    let report = tokio::task::spawn_blocking(move || viewer.join.join().expect("viewer thread")).await.unwrap();
    // 50 packets a second once the connection carries media; the viewer watched for 3 s,
    // part of which went to the DTLS handshake.
    assert!(report.audio_packets >= 50, "only {} audio packets", report.audio_packets);
    assert!(report.video.is_empty(), "no picture was sent");

    // Nothing is received in a meeting session, so nothing times out on media: ICE notices
    // that the phone is gone (its checks go unanswered for some 25 s), and the session ends
    // by itself.
    let stop = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match out.recv().await {
                Some(OutboundFrame::StreamStop { reason, .. }) => return reason,
                Some(_) => {}
                None => panic!("the manager is gone"),
            }
        }
    })
    .await
    .expect("the session ends once the viewer is gone");
    assert_eq!(stop, StopReason::Timeout);
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state == Phase::Error)).await.unwrap().unwrap();
    assert_eq!(manager.status().last_error_code.as_deref(), Some("timeout"));
}

/// The next `status` for the phone that says whether a picture is being sent.
async fn picture_status(out: &mut mpsc::UnboundedReceiver<OutboundFrame>, with_picture: bool, within: Duration) -> Option<u32> {
    tokio::time::timeout(within, async {
        loop {
            if let Some(OutboundFrame::Signal(StreamSignal::Status { width, mode: Some(StreamMode::Meeting), .. })) = out.recv().await {
                if width.is_some() == with_picture {
                    return width;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no status with picture={with_picture} in time"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_picture_that_stops_arriving_leaves_a_sound_only_session() {
    if !encoders_available() {
        eprintln!("skipping: ffmpeg with libx264 and libopus (and ffprobe) is needed");
        return;
    }
    let Up { manager, mut out, viewer } = start_with(Arc::new(StallingWindow), 20, false).await;
    let mut status = manager.subscribe();
    // The picture flows (it may have been there from the first "live" on)…
    tokio::time::timeout(Duration::from_secs(15), status.wait_for(|s| s.state == Phase::Live && s.width == Some(1280))).await.unwrap().unwrap();
    // …then the capture goes quiet, its process still there: sound only, and the phone is told.
    tokio::time::timeout(Duration::from_secs(20), status.wait_for(|s| s.state == Phase::Live && s.width.is_none())).await.unwrap().unwrap();
    assert_eq!(picture_status(&mut out, false, Duration::from_secs(5)).await, None);
    // …and the session goes on, sound and all.
    let before = manager.status();
    assert_eq!((before.state, before.width, before.last_error.clone()), (Phase::Live, None, None), "{before:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = manager.status();
    assert!(after.state == Phase::Live && after.frames > before.frames + 50, "sound stopped with the picture: {before:?} → {after:?}");

    manager.stop(StopReason::User).await;
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state == Phase::Idle)).await.unwrap().unwrap();
    let report = tokio::task::spawn_blocking(move || viewer.join.join().expect("viewer thread")).await.unwrap();
    assert!(!report.video.is_empty() && report.audio_packets >= 200, "{} frames, {} packets", report.video.len(), report.audio_packets);
    // The capture process was ended with its encoder.
    let leftover = Command::new("pgrep").args(["-x", "-f", "sleep 121"]).output().map(|o| o.stdout).unwrap_or_default();
    assert!(leftover.is_empty(), "the capture process is still running: {}", String::from_utf8_lossy(&leftover));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_viewer_closing_the_connection_is_a_normal_ending() {
    if !encoders_available() {
        eprintln!("skipping: ffmpeg with libx264 and libopus (and ffprobe) is needed");
        return;
    }
    let Up { manager, out: _out, viewer } = start(false, 2, true).await;
    let mut status = manager.subscribe();
    tokio::time::timeout(Duration::from_secs(20), status.wait_for(|s| s.state == Phase::Idle)).await.unwrap().unwrap();
    let st = manager.status();
    assert!(st.ended_by_phone && st.last_error.is_none(), "{st:?}");
    let report = tokio::task::spawn_blocking(move || viewer.join.join().expect("viewer thread")).await.unwrap();
    assert!(report.audio_packets >= 20, "only {} audio packets", report.audio_packets);
}
