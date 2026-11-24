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
use camlooper_lib::remote::stream::signal::{OutboundFrame, StopReason, StreamSignal};
use camlooper_lib::remote::stream::{
    CameraTarget, ChannelDecisions, DecisionEvent, FixedCamera, NoStun, Phase, SessionDeps, StreamManager,
};
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
    sim.answer(answer);
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
