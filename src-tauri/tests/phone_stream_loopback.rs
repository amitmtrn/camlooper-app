//! End to end on this box, no browser and no relay: a str0m sender standing in for the
//! phone → the real stream manager, receiver task, Annex-B/IVF writer and ffmpeg pipeline
//! (null sink) → at least 30 decodable frames.
//!
//! How "decoded" is checked: the session's ffmpeg is a small wrapper script that `tee`s the
//! exact bytes the pipeline writes to ffmpeg's stdin into a file while running the real
//! ffmpeg with the real arguments. Afterwards `ffprobe -count_frames` decodes the capture.
//!
//! Skipped (with a message) when ffmpeg/ffprobe are not on PATH, like the pipeline tests.
#![cfg(unix)]

mod support;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use camlooper_lib::pipeline::{CameraSink, EncodedCodec, Geometry};
use camlooper_lib::remote::stream::rtc::RtcOptions;
use camlooper_lib::remote::stream::record::MIN_TAKE_MS;
use camlooper_lib::remote::stream::signal::{OutboundFrame, StopReason, StreamMode, StreamSignal};
use camlooper_lib::remote::stream::{
    CameraTarget, ChannelDecisions, DecisionEvent, FileDeps, FixedCamera, LoopDeps, LoopPlayer, LoopState, NoStun, Phase,
    PhoneVideoStore, PlayError, PlayRequest, Played, RecordPhase, SessionDeps, StreamManager,
};
use futures_util::future::BoxFuture;
use serde_json::json;
use support::phone_sim::{h264_access_units, ivf_frames, PhoneSim};
use tokio::sync::mpsc;

fn tool_available(name: &str) -> bool {
    Command::new(name)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phone-stream").join(name)).expect("fixture")
}

/// An "ffmpeg" that records its stdin to `<dir>/capture-<pid>.es` and runs the real one.
const TEE_FFMPEG: &str = "#!/bin/sh\ntee \"$(dirname \"$0\")/capture-$$.es\" | ffmpeg \"$@\"\n";

/// Like [`TEE_FFMPEG`], but the first launch dies after reading ~40 KB, as a crashing ffmpeg would.
const CRASH_ONCE_FFMPEG: &str = "#!/bin/sh\nd=\"$(dirname \"$0\")\"\nif [ ! -e \"$d/crashed\" ]; then\n  touch \"$d/crashed\"\n  head -c 40000 > \"$d/crashed\"\n  exit 1\nfi\ntee \"$d/capture-$$.es\" | ffmpeg \"$@\"\n";

fn tee_ffmpeg(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("ffmpeg-tee.sh");
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn captures(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "es"))
        .collect();
    v.sort();
    v
}

/// Frames ffprobe can decode, and whether ffmpeg reported any decode error.
fn decode_count(path: &Path, demuxer: &str) -> (u64, String) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-f", demuxer, "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames,width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let frames = text.trim().split(',').next_back().and_then(|s| s.parse().ok()).unwrap_or(0);
    (frames, text.trim().to_string() + &String::from_utf8_lossy(&out.stderr))
}

async fn next_out(out: &mut mpsc::UnboundedReceiver<OutboundFrame>) -> OutboundFrame {
    tokio::time::timeout(Duration::from_secs(20), out.recv()).await.expect("frame for the phone in time").unwrap()
}

async fn run_loopback(codec: EncodedCodec, frames: Vec<Vec<u8>>, script: &str) {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let target = CameraTarget {
        sink: CameraSink::Null,
        geometry: Geometry { width: 1280, height: 720, fps: 30 },
        preview: false,
    };
    let mut deps = SessionDeps::new(tee_ffmpeg(dir.path(), script), Arc::new(FixedCamera(target)), Arc::new(NoStun));
    deps.rtc = RtcOptions { bind_ips: Some(vec![Ipv4Addr::LOCALHOST]), ..RtcOptions::default() };
    let (dtx, mut decisions) = mpsc::unbounded_channel();
    let (manager, mut out) = StreamManager::spawn(Arc::new(Arc::new(deps)), Arc::new(ChannelDecisions(dtx)));
    let mut status = manager.subscribe();

    // phone → stream_request; the "user" accepts.
    manager.on_relay_signal(json!({"kind":"stream_request","sessionId":"loop-1","deviceLabel":"Sim"}));
    let Some(DecisionEvent::Request(req)) = decisions.recv().await else { panic!("no decision request") };
    manager.accept(&req.session_id).await.unwrap();
    assert!(matches!(next_out(&mut out).await, OutboundFrame::Signal(StreamSignal::StreamAccept { .. })));
    assert!(matches!(next_out(&mut out).await, OutboundFrame::Signal(StreamSignal::Status { .. })));

    // offer → answer, then the phone trickles its candidate.
    let sim = PhoneSim::start(codec, frames, 8);
    manager.on_relay_signal(json!({"kind":"offer","sessionId":"loop-1","sdp": sim.offer}));
    let answer = match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::Answer { session_id, sdp }) => {
            assert_eq!(session_id, "loop-1");
            sdp
        }
        other => panic!("expected the answer, got {other:?}"),
    };
    assert!(answer.contains("a=candidate:"), "desktop candidates are embedded in the answer");
    sim.answer(answer.clone());
    for _ in 0..2 {
        let c = sim.candidates.recv_timeout(Duration::from_secs(10)).expect("phone candidate");
        manager.on_relay_signal(json!({"kind":"candidate","sessionId":"loop-1","candidate":c,"sdpMid":"0","sdpMLineIndex":0}));
    }

    // live: codec and size reach both the phone (status) and the UI (watch).
    let live = match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::Status { width, height, codec, .. }) => (width, height, codec),
        other => panic!("expected status live, got {other:?}"),
    };
    assert_eq!(live, (Some(640), Some(360), Some(codec.name().to_string())));
    tokio::time::timeout(Duration::from_secs(5), status.wait_for(|s| s.state == Phase::Live)).await.unwrap().unwrap();

    // Anything on the network can reach the receiver's UDP ports. Junk that looks like DTLS,
    // RTP, STUN or nothing at all, from a stranger's socket, must not disturb the stream.
    let junk = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let ports: Vec<u16> = answer
        .lines()
        .filter(|l| l.starts_with("a=candidate:") && l.contains(" typ host"))
        .filter_map(|l| l.split_whitespace().nth(5)?.parse().ok())
        .collect();
    assert!(!ports.is_empty(), "host candidate ports in the answer: {answer}");
    for port in &ports {
        let mut dtls = vec![22u8, 254, 253, 0, 0, 0, 0, 0, 0, 0, 1, 0, 40];
        dtls.extend_from_slice(&[0xab; 40]);
        let alert = [21u8, 254, 253, 0, 1, 0, 0, 0, 0, 0, 9, 0, 2, 2, 40];
        let rtp = [0x80u8, 102, 0, 1, 0, 0, 0, 1, 0xde, 0xad, 0xbe, 0xef, 1, 2, 3, 4];
        let stun = [0u8, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        for pkt in [&dtls[..], &alert[..], &rtp[..], &stun[..], &[0xff; 64][..], &[][..]] {
            let _ = junk.send_to(pkt, (Ipv4Addr::LOCALHOST, *port));
        }
    }

    tokio::time::sleep(Duration::from_millis(3500)).await;
    let st = manager.status();
    assert_eq!(st.state, Phase::Live, "{st:?}");
    assert!(st.frames >= 30, "frames written to ffmpeg: {st:?}");
    assert!(st.fps.unwrap_or(0.0) > 10.0, "{st:?}");

    manager.stop(StopReason::User).await;
    assert!(matches!(
        next_out(&mut out).await,
        OutboundFrame::StreamStop { reason: StopReason::User, .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state == Phase::Idle)).await.unwrap().unwrap();
    assert_eq!(manager.status().last_error, None);

    let report = tokio::task::spawn_blocking(move || sim.finish()).await.unwrap();
    assert!(report.connected);
    assert!(report.keyframe_requests >= 1, "the receiver asks for a keyframe at start: {report:?}");

    // One ffmpeg for the whole session (after the crash, if one was staged), and what it was
    // fed decodes — a restarted decoder starts on a fresh keyframe.
    let caps = captures(dir.path());
    assert_eq!(caps.len(), 1, "one surviving decoder: {caps:?}");
    let (decoded, info) = decode_count(&caps[0], codec.demuxer());
    eprintln!("{codec:?}: {decoded} frames decoded from {} bytes ({info})", std::fs::metadata(&caps[0]).unwrap().len());
    assert!(decoded >= 30, "only {decoded} decodable frames: {info}");
    assert!(!info.to_lowercase().contains("error"), "decode errors: {info}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h264_phone_stream_decodes_through_the_pipeline() {
    run_loopback(EncodedCodec::H264, h264_access_units(&fixture("h264-640x360.h264")), TEE_FFMPEG).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vp8_phone_stream_decodes_through_the_pipeline() {
    run_loopback(EncodedCodec::Vp8, ivf_frames(&fixture("vp8-640x360.ivf")), TEE_FFMPEG).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crashed_ffmpeg_is_restarted_and_resumes_on_a_keyframe() {
    run_loopback(EncodedCodec::H264, h264_access_units(&fixture("h264-640x360.h264")), CRASH_ONCE_FFMPEG).await;
}

/// The phone taps Stop: it sends `stream_stop{user}` over the relay and closes its
/// RTCPeerConnection, and the DTLS close reaches the desktop first (seen in a real run, where
/// the session used to end as `error: the connection was closed`). Either way round, and also
/// when the relay's frame never arrives, the session must end as a normal stop: idle, no
/// error, nothing echoed to the phone.
async fn run_phone_closes(relay_stop_follows: bool) {
    if !tool_available("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let target = CameraTarget {
        sink: CameraSink::Null,
        geometry: Geometry { width: 1280, height: 720, fps: 30 },
        preview: false,
    };
    let mut deps = SessionDeps::new(tee_ffmpeg(dir.path(), TEE_FFMPEG), Arc::new(FixedCamera(target)), Arc::new(NoStun));
    deps.rtc = RtcOptions { bind_ips: Some(vec![Ipv4Addr::LOCALHOST]), ..RtcOptions::default() };
    let (dtx, mut decisions) = mpsc::unbounded_channel();
    let (manager, mut out) = StreamManager::spawn(Arc::new(Arc::new(deps)), Arc::new(ChannelDecisions(dtx)));
    let mut status = manager.subscribe();

    manager.on_relay_signal(json!({"kind":"stream_request","sessionId":"close-1","deviceLabel":"Sim"}));
    let Some(DecisionEvent::Request(req)) = decisions.recv().await else { panic!("no decision request") };
    manager.accept(&req.session_id).await.unwrap();
    // The phone streams for two seconds, then closes its peer connection.
    let sim = PhoneSim::start_with(EncodedCodec::H264, h264_access_units(&fixture("h264-640x360.h264")), 2, true);
    manager.on_relay_signal(json!({"kind":"offer","sessionId":"close-1","sdp": sim.offer}));
    let answer = loop {
        if let OutboundFrame::Signal(StreamSignal::Answer { sdp, .. }) = next_out(&mut out).await {
            break sdp;
        }
    };
    sim.answer(answer);
    for _ in 0..2 {
        let c = sim.candidates.recv_timeout(Duration::from_secs(10)).expect("phone candidate");
        manager.on_relay_signal(json!({"kind":"candidate","sessionId":"close-1","candidate":c}));
    }
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state == Phase::Live)).await.unwrap().unwrap();

    let report = tokio::task::spawn_blocking(move || sim.finish()).await.unwrap();
    assert!(report.connected);
    let closed_at = std::time::Instant::now();
    if relay_stop_follows {
        tokio::time::sleep(Duration::from_millis(200)).await;
        manager.on_relay_stream_stop(Some(json!({"sessionId":"close-1","reason":"user"})));
    }
    // Well inside the 10 s media timeout: the close itself ended the session.
    let ended = tokio::time::timeout(Duration::from_secs(6), status.wait_for(|s| s.state == Phase::Idle || s.state == Phase::Error))
        .await
        .expect("the session ends soon after the phone closes")
        .unwrap()
        .clone();
    assert_eq!(ended.state, Phase::Idle, "a phone hanging up is not an error: {ended:?}");
    assert_eq!((ended.last_error.clone(), ended.last_error_code.clone()), (None, None), "{ended:?}");
    assert!(ended.ended_by_phone, "{ended:?}");
    if relay_stop_follows {
        assert!(closed_at.elapsed() < Duration::from_millis(1400), "the relay's stop ends the wait early");
    }
    // Nothing is sent back to a phone that hung up.
    while let Ok(f) = out.try_recv() {
        assert!(!matches!(f, OutboundFrame::StreamStop { .. }), "unexpected {f:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_phone_closing_the_connection_is_a_normal_stop() {
    run_phone_closes(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_phone_closing_before_its_relay_stop_arrives_is_a_normal_stop() {
    run_phone_closes(true).await;
}

// ---------------------------------------------------------------------------------------
// Record mode: the same phone, the same pipeline — and the stream teed into a take that
// is remuxed and handed to the loop engine when the phone stops.
// ---------------------------------------------------------------------------------------

/// A loop engine that takes any file it is given and "plays" it until told otherwise. It
/// needs no camera and no window; what it was handed is checked with ffprobe by the test.
#[derive(Default)]
struct TestLoop {
    state: std::sync::Mutex<LoopState>,
    plays: std::sync::Mutex<Vec<PlayRequest>>,
}

impl LoopPlayer for TestLoop {
    fn state(&self) -> BoxFuture<'_, LoopState> {
        Box::pin(async { self.state.lock().unwrap().clone() })
    }
    fn stop(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async {
            self.state.lock().unwrap().playing = false;
            Ok(())
        })
    }
    fn start(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async {
            self.state.lock().unwrap().playing = true;
            Ok(())
        })
    }
    fn play_file(&self, request: PlayRequest) -> BoxFuture<'_, Result<Played, PlayError>> {
        Box::pin(async move {
            let path = request.path.clone();
            *self.state.lock().unwrap() = LoopState { playing: true, name: Some(request.display_name.clone()), path: Some(path.clone()) };
            self.plays.lock().unwrap().push(request);
            Ok(Played::Looping { path })
        })
    }
}

/// Duration (s) and decodable frames of a container file, read as `demuxer` and nothing else.
fn probe_video(path: &Path, demuxer: &str) -> (f64, u64, String) {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-f", demuxer, "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=codec_name,width,height,nb_read_frames:format=duration", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .expect("ffprobe");
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{name}=")).map(str::to_string));
    let duration = field("duration").and_then(|d| d.parse().ok()).unwrap_or(0.0);
    let frames = field("nb_read_frames").and_then(|d| d.parse().ok()).unwrap_or(0);
    (duration, frames, text.replace('\n', " "))
}

fn store_files(store: &PhoneVideoStore) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(store.dir())
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// What ends the take.
#[derive(Clone, Copy, PartialEq)]
enum TakeEnd {
    /// The phone's Stop, a few seconds in.
    PhoneStops,
    /// The desktop's Stop / Cancel, a few seconds in.
    DesktopStops,
    /// The phone's Stop, well under two seconds into the take.
    PhoneStopsEarly,
}

async fn run_record(codec: EncodedCodec, frames: Vec<Vec<u8>>, end: TakeEnd) {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let app_data = tempfile::tempdir().unwrap();
    let store = PhoneVideoStore::in_app_data(app_data.path());
    let player = Arc::new(TestLoop::default());
    let target = CameraTarget { sink: CameraSink::Null, geometry: Geometry { width: 1280, height: 720, fps: 30 }, preview: false };
    let mut deps = SessionDeps::new(PathBuf::from("ffmpeg"), Arc::new(FixedCamera(target)), Arc::new(NoStun));
    deps.rtc = RtcOptions { bind_ips: Some(vec![Ipv4Addr::LOCALHOST]), ..RtcOptions::default() };
    deps.files = Some(FileDeps::new(store.clone(), player.clone()));
    let (dtx, mut decisions) = mpsc::unbounded_channel();
    let (manager, mut out) = StreamManager::spawn_with_loop(
        Arc::new(Arc::new(deps)),
        Arc::new(ChannelDecisions(dtx)),
        LoopDeps { player: player.clone(), store: Some(store.clone()) },
    );
    let mut status = manager.subscribe();

    // hello → hello_ok: this desktop records.
    manager.on_relay_signal(json!({"kind":"hello","sessionId":"hello-1","appProtocol":3}));
    match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::HelloOk { app_protocol, features, .. }) => {
            assert_eq!(app_protocol, 3);
            assert!(features.iter().any(|f| f == "record") && features.iter().any(|f| f == "live"), "{features:?}");
        }
        other => panic!("expected hello_ok, got {other:?}"),
    }

    // The request is asked about like a live one, and says what it is.
    manager.on_relay_signal(json!({"kind":"stream_request","sessionId":"rec-1","deviceLabel":"Sim","mode":"record"}));
    let Some(DecisionEvent::Request(req)) = decisions.recv().await else { panic!("no decision request") };
    assert_eq!(req.mode, StreamMode::Record);
    manager.accept(&req.session_id).await.unwrap();

    // The same media connection as live.
    let sim = PhoneSim::start(codec, frames, 9);
    manager.on_relay_signal(json!({"kind":"offer","sessionId":"rec-1","sdp": sim.offer}));
    let answer = loop {
        if let OutboundFrame::Signal(StreamSignal::Answer { sdp, .. }) = next_out(&mut out).await {
            break sdp;
        }
    };
    sim.answer(answer);
    for _ in 0..2 {
        let c = sim.candidates.recv_timeout(Duration::from_secs(10)).expect("phone candidate");
        manager.on_relay_signal(json!({"kind":"candidate","sessionId":"rec-1","candidate":c}));
    }
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state == Phase::Live)).await.unwrap().unwrap();
    assert_eq!(manager.status().mode, StreamMode::Record);
    assert!(player.plays.lock().unwrap().is_empty());

    // The phone is told when the take starts (after the pre-roll).
    let started = loop {
        if let OutboundFrame::Signal(StreamSignal::Status { record: Some(r), mode, .. }) = next_out(&mut out).await {
            assert_eq!((mode, r.phase, r.elapsed_ms, r.cap_ms), (Some(StreamMode::Record), RecordPhase::Recording, 0, 60_000));
            break std::time::Instant::now();
        }
    };
    assert_eq!(store_files(&store).len(), 1, "the take is teed into the store: {:?}", store_files(&store));

    if end == TakeEnd::PhoneStopsEarly {
        tokio::time::sleep(Duration::from_millis(700)).await;
        manager.on_relay_stream_stop(Some(json!({"sessionId":"rec-1","reason":"user"})));
        let ended = tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state.is_free())).await.unwrap().unwrap().clone();
        assert_eq!((ended.state, ended.last_error.clone(), ended.record.clone()), (Phase::Idle, None, None), "too short is not an error: {ended:?}");
        assert!(ended.ended_by_phone, "{ended:?}");
        let mut stops = vec![];
        while let Ok(Some(f)) = tokio::time::timeout(Duration::from_millis(500), out.recv()).await {
            if let OutboundFrame::StreamStop { reason, .. } = f {
                stops.push(reason);
            }
        }
        assert_eq!(stops, vec![StopReason::TooShort]);
        assert!(store_files(&store).is_empty(), "the partial take is deleted: {:?}", store_files(&store));
        assert!(player.plays.lock().unwrap().is_empty() && !player.state.lock().unwrap().playing, "nothing loops");
        let _ = tokio::task::spawn_blocking(move || sim.finish()).await;
        return;
    }

    // The live picture goes on while the take is recorded: frames keep reaching the decoder,
    // and the window's counter follows the take.
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.record.as_ref().is_some_and(|r| r.elapsed_ms >= 3000)))
        .await
        .expect("three seconds of take")
        .unwrap();
    let st = manager.status();
    assert_eq!(st.state, Phase::Live);
    assert!(st.frames >= 90 && st.fps.unwrap_or(0.0) > 10.0, "live passthrough during the take: {st:?}");
    match end {
        TakeEnd::PhoneStops => manager.on_relay_stream_stop(Some(json!({"sessionId":"rec-1","reason":"user"}))),
        TakeEnd::DesktopStops => manager.stop(StopReason::User).await,
        TakeEnd::PhoneStopsEarly => unreachable!(),
    }
    let take_ms = started.elapsed().as_millis() as i64;

    // Processing, then the success ending: `done`, never an error — and the loop's state.
    let mut phases = vec![];
    let done = loop {
        match next_out(&mut out).await {
            OutboundFrame::Signal(StreamSignal::Status { record: Some(r), .. }) => phases.push(r.phase),
            OutboundFrame::StreamStop { session_id, reason } => break (session_id, reason),
            _ => {}
        }
    };
    assert_eq!(done, ("rec-1".to_string(), StopReason::Done));
    assert!(phases.contains(&RecordPhase::Processing), "{phases:?}");
    match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::LoopStatus { playing, from_phone, error, .. }) => assert_eq!((playing, from_phone, error), (true, true, None)),
        other => panic!("expected loop_status, got {other:?}"),
    }
    let ended = tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.state.is_free())).await.unwrap().unwrap().clone();
    assert_eq!((ended.state, ended.mode, ended.last_error.clone()), (Phase::Idle, StreamMode::Record, None), "{ended:?}");
    let record = ended.record.clone().expect("record status");
    assert_eq!(record.phase, RecordPhase::Looping);
    assert!((record.elapsed_ms as i64 - take_ms).abs() < 700, "teed {} ms of a {take_ms} ms take", record.elapsed_ms);

    // What the loop engine was handed: one file in the store, the right container by
    // content, decodable, as long as the take.
    let plays = player.plays.lock().unwrap().clone();
    assert_eq!(plays.len(), 1);
    let (ext, demuxer) = match codec {
        EncodedCodec::H264 => ("mp4", "mov"),
        EncodedCodec::Vp8 => ("webm", "matroska"),
    };
    let video = plays[0].path.clone();
    assert_eq!(plays[0].display_name, format!("Phone recording.{ext}"));
    assert_eq!(plays[0].container.demuxer(), demuxer);
    assert!(store.contains(&video) && video.extension().is_some_and(|e| e == ext), "{video:?}");
    assert_eq!(store_files(&store), vec![video.file_name().unwrap().to_string_lossy().into_owned()], "the take file is gone");
    let head = std::fs::read(&video).unwrap();
    match codec {
        EncodedCodec::H264 => assert_eq!(&head[4..8], b"ftyp"),
        EncodedCodec::Vp8 => assert_eq!(&head[..4], &[0x1A, 0x45, 0xDF, 0xA3]),
    }
    let (duration, decoded, info) = probe_video(&video, demuxer);
    eprintln!("{codec:?} take: {duration:.2} s, {decoded} frames, {} bytes ({info})", head.len());
    assert!((duration * 1000.0 - record.elapsed_ms as f64).abs() < 250.0, "file {duration} s, teed {} ms", record.elapsed_ms);
    assert!(duration * 1000.0 >= MIN_TAKE_MS as f64);
    assert!(decoded as f64 >= duration * 20.0, "only {decoded} decodable frames in {duration} s: {info}");
    assert!(info.contains("width=640") && info.contains("height=360"), "{info}");
    assert!(!info.to_lowercase().contains("error"), "decode errors: {info}");

    // The phone goes away. The loop does not care.
    let report = tokio::task::spawn_blocking(move || sim.finish()).await.unwrap();
    assert!(report.connected);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(player.state.lock().unwrap().playing && video.exists(), "the loop outlives the phone");
    assert_eq!(manager.status().state, Phase::Idle);
    // Still paired, it can ask about (and steer) the loop.
    manager.on_relay_signal(json!({"kind":"loop_control","sessionId":"ctl-1","action":"status"}));
    match next_out(&mut out).await {
        OutboundFrame::Signal(StreamSignal::LoopStatus { playing, name, from_phone, .. }) => {
            assert_eq!((playing, name.as_deref(), from_phone), (true, Some(format!("Phone recording.{ext}").as_str()), true));
        }
        other => panic!("expected loop_status, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_h264_take_is_recorded_remuxed_and_looped_when_the_phone_stops() {
    run_record(EncodedCodec::H264, h264_access_units(&fixture("h264-640x360.h264")), TakeEnd::PhoneStops).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vp8_take_becomes_a_webm_when_the_desktop_stops() {
    run_record(EncodedCodec::Vp8, ivf_frames(&fixture("vp8-640x360.ivf")), TakeEnd::DesktopStops).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_take_stopped_too_early_is_deleted_and_nothing_loops() {
    run_record(EncodedCodec::H264, h264_access_units(&fixture("h264-640x360.h264")), TakeEnd::PhoneStopsEarly).await;
}
