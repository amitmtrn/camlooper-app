//! Headless dev tool (not shipped): receive a phone camera stream — or a video file the
//! phone wants looped — through the live relay and run the real pipeline, without the Tauri
//! app or its UI.
//!
//! ```text
//! cargo run --example phone_stream_cli -- --code 123456 [--device /dev/video10 | --null]
//!                                         [--api https://api.camlooper.com] [--seconds 60]
//! cargo run --example phone_stream_cli -- --pair-file /tmp/pair.json ...
//! ```
//!
//! * `--code` claims the 6-digit code shown on the phone page (the same claim the app makes),
//!   keeps the pairing in a private temp file — never the app's `remote.json` — and revokes it
//!   on exit. `--pair-file` reuses a pairing file (one written by `--code --keep-pair`).
//! * The first `stream_request` is **accepted automatically**. That is the only auto-accept
//!   anywhere: the app always asks the user (see `remote::stream::manager::DecisionSink`).
//! * Output: `--device` writes that v4l2 device; `--null` decodes and discards; neither means
//!   the app's own handoff (finds the CamLooper v4l2loopback device, loading the module if
//!   needed — one polkit prompt).
//! * A `mode: "file"` request (the phone page's "Loop a video") runs the app's own path:
//!   the file is received by `remote::stream::file_receive` into a private temp directory
//!   (`<tmp>/camlooper-cli-…/phone-videos/`, deleted on exit), then loaded and looped by the
//!   app's loop engine (`remote::stream::loop_engine::EngineLoop` → `VideoProcessor::
//!   load_video_with_original_name` + `start_streaming`, natural motion on, loop forever —
//!   what the main window sets by default) on the same output as above. `loop_control`
//!   (stop / start / status) from the phone acts on that loop and is answered with
//!   `loop_status`. A live stream on `--device` pauses the loop and resumes it afterwards.
//! * stdout: one JSON object per line (events and a status line per second) and nothing
//!   else. stderr: logs, including what the app itself prints to stdout.
//!   Tokens are never printed. `--debug` adds debug logs (SDP and candidate addresses).
//!   File mode adds `file_progress {received,size}`, `file_saved {path,sha256}`,
//!   `video_loaded {video}`, `looping {name}`, `loop_status {playing,…}`, a `loop` line per
//!   second while the loop plays, and `cleanup {deleted}` on exit.
//! * Exits after `--seconds` (default 300), on Ctrl-C / SIGTERM, or 10 s after a live stream
//!   ended (`live_ended`) — unless the phone asks for something else within those seconds
//!   (the page's "Stop, then Send & loop") or a video is loaded, whose loop then resumes.
//!   A file session never ends the run: the video keeps looping until one of the above.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camlooper_lib::pipeline::{CameraSink, Geometry};
use camlooper_lib::remote::client::{self, ConnectError};
use camlooper_lib::remote::protocol::ServerMessage;
use camlooper_lib::remote::store::{Pairing, Store};
use camlooper_lib::remote::stream::handoff::{ensure_virtual_camera, AppCamera};
use camlooper_lib::remote::stream::loop_engine::{self, EngineLoop, LoopHost, VideoInfo};
use camlooper_lib::remote::stream::rtc::resolve_stun;
use camlooper_lib::remote::stream::signal::{OutboundFrame, StopReason, StreamMode, StreamSignal};
use camlooper_lib::remote::stream::{
    route_server_message, CameraControl, CameraTarget, Cancel, ChannelDecisions, DecisionEvent, FileDeps, FilePhase, IceSource,
    LoopDeps, LoopPlayer, LoopState, Phase, PhoneVideoStore, SessionDeps, StreamManager, StreamStatus,
};
use camlooper_lib::virtual_camera::{self, VirtualCameraConfig};
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

struct Args {
    api: String,
    code: Option<String>,
    pair_file: Option<PathBuf>,
    keep_pair: bool,
    device: Option<String>,
    null: bool,
    seconds: u64,
    debug: bool,
}

const USAGE: &str = "usage: phone_stream_cli (--code <6 digits> [--keep-pair] | --pair-file <path>) \
[--device /dev/videoN | --null] [--api URL] [--seconds N] [--debug]";

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        api: "https://api.camlooper.com".into(),
        code: None,
        pair_file: None,
        keep_pair: false,
        device: None,
        null: false,
        seconds: 300,
        debug: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--api" => a.api = value("--api")?,
            "--code" => a.code = Some(value("--code")?),
            "--pair-file" => a.pair_file = Some(PathBuf::from(value("--pair-file")?)),
            "--keep-pair" => a.keep_pair = true,
            "--device" => a.device = Some(value("--device")?),
            "--null" => a.null = true,
            "--seconds" => a.seconds = value("--seconds")?.parse().map_err(|e| format!("--seconds: {e}"))?,
            "--debug" => a.debug = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if a.code.is_none() && a.pair_file.is_none() {
        return Err(USAGE.into());
    }
    if a.device.is_some() && a.null {
        return Err("--device and --null are exclusive".into());
    }
    Ok(a)
}

/// Prints `camlooper_lib` debug logs to stderr when `--debug` is given.
struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.target().starts_with("camlooper_lib")
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[{}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}
static LOGGER: StderrLogger = StderrLogger;

/// Where the JSON lines go once [`keep_stdout_for_json`] has run.
static JSON_OUT: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> = std::sync::OnceLock::new();

/// The loop engine and the virtual camera print progress with `println!` (the app's log).
/// Here stdout is the machine-readable channel, so keep a private handle to it for
/// [`emit`] and point fd 1 at stderr for everybody else.
#[cfg(unix)]
fn keep_stdout_for_json() {
    use std::os::fd::AsFd;
    let Ok(real) = std::io::stdout().as_fd().try_clone_to_owned() else { return };
    // SAFETY: dup2 on two descriptors this process owns; no memory is involved.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } >= 0 {
        let _ = JSON_OUT.set(std::sync::Mutex::new(std::fs::File::from(real)));
    }
}

#[cfg(not(unix))]
fn keep_stdout_for_json() {}

fn emit(v: serde_json::Value) {
    use std::io::Write;
    match JSON_OUT.get() {
        Some(out) => {
            let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(out, "{v}");
        }
        None => println!("{v}"),
    }
}

fn status_line(s: &StreamStatus) -> serde_json::Value {
    json!({
        "event": "status",
        "state": s.state,
        "codec": s.codec,
        "size": s.width.zip(s.height).map(|(w, h)| format!("{w}x{h}")),
        "fps": s.fps,
        "frames": s.frames,
        "lastError": s.last_error,
    })
}

struct RelayStun {
    base: Url,
    pairing: Pairing,
}

impl IceSource for RelayStun {
    fn stun_server(&self) -> BoxFuture<'_, Option<SocketAddr>> {
        Box::pin(async {
            match tokio::time::timeout(Duration::from_secs(3), client::fetch_ice_servers(&self.base, &self.pairing)).await {
                Ok(Ok(urls)) => resolve_stun(&urls, Duration::from_millis(800)).await,
                Ok(Err(e)) => {
                    eprintln!("ice-servers: {e}");
                    None
                }
                Err(_) => None,
            }
        })
    }
}

// ---------------------------------------------------------------------------------------
// The loop engine's surroundings (file mode)
// ---------------------------------------------------------------------------------------

/// The camera the CLI was pointed at, for live frames and for the loop alike.
#[derive(Clone)]
enum Output {
    /// `--device`: this v4l2 device, whatever its card label.
    Device(String),
    /// `--null`: decode and discard.
    Null,
    /// Neither: the app's own virtual camera.
    App,
}

/// How long the run goes on after a live stream ended with nothing loaded to loop: long
/// enough for the phone page's next request (it retries a `busy` answer for 8 s).
const LIVE_END_GRACE: Duration = Duration::from_secs(10);

const GEOMETRY: Geometry = Geometry { width: 1280, height: 720, fps: 30 };

/// What the app's `AppLoopHost` is to the main window: where the loop's camera comes from,
/// and who hears about the loop. Here the camera is the CLI's output and the listener is
/// stdout.
struct CliHost {
    output: Output,
    /// This run started the virtual camera (so it stops it on exit).
    started_camera: AtomicBool,
}

impl LoopHost for CliHost {
    fn ensure_camera(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async {
            if virtual_camera::get_virtual_camera_status().await.is_active {
                return Ok(());
            }
            let sink = match &self.output {
                Output::Device(device) => CameraSink::V4l2 { device: device.clone() },
                Output::Null => CameraSink::Null,
                Output::App => {
                    if ensure_virtual_camera().await? {
                        self.started_camera.store(true, Ordering::SeqCst);
                    }
                    return Ok(());
                }
            };
            let config = VirtualCameraConfig { width: GEOMETRY.width, height: GEOMETRY.height, fps: GEOMETRY.fps, ..VirtualCameraConfig::default() };
            virtual_camera::start_virtual_camera_on_sink(Some(config), sink).await?;
            self.started_camera.store(true, Ordering::SeqCst);
            Ok(())
        })
    }

    fn loop_changed(&self, playing: bool) {
        emit(json!({"event": "loop_changed", "playing": playing}));
    }

    fn video_loaded(&self, video: &VideoInfo) {
        emit(json!({"event": "video_loaded", "video": video}));
    }
}

/// The real loop player, with the two moments the E2E wants to see: the file as it was
/// handed over (path and SHA-256), and the loop running.
struct ReportingLoop(EngineLoop);

fn sha256_hex(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl LoopPlayer for ReportingLoop {
    fn state(&self) -> BoxFuture<'_, LoopState> {
        self.0.state()
    }
    fn stop(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        self.0.stop()
    }
    fn start(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        self.0.start()
    }
    fn play_file<'a>(&'a self, path: &'a Path, display_name: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let hashed = path.to_path_buf();
            let sha256 = tokio::task::spawn_blocking(move || sha256_hex(&hashed)).await?;
            let size = std::fs::metadata(path).map(|m| m.len()).ok();
            emit(json!({"event": "file_saved", "path": path, "sha256": sha256.ok(), "size": size}));
            self.0.play_file(path, display_name).await?;
            emit(json!({"event": "looping", "name": display_name}));
            Ok(())
        })
    }
}

/// `--device` / `--null` for a live stream: a fixed sink, as before — but the loop this run
/// may have put on the same device steps aside first and comes back afterwards, as the
/// app's handoff does for its camera.
struct CliCamera {
    target: CameraTarget,
    resume_loop: AtomicBool,
}

impl CameraControl for CliCamera {
    fn acquire(&self, _epoch: u64, _cancel: Cancel) -> BoxFuture<'_, anyhow::Result<CameraTarget>> {
        Box::pin(async {
            let was_playing = loop_engine::pause_for_live(Duration::from_secs(5)).await?;
            self.resume_loop.store(was_playing, Ordering::SeqCst);
            Ok(self.target.clone())
        })
    }
    fn release(&self, _epoch: u64) -> BoxFuture<'_, ()> {
        Box::pin(async {
            if self.resume_loop.swap(false, Ordering::SeqCst) {
                if let Err(e) = loop_engine::resume_after_live().await {
                    eprintln!("could not resume the video loop: {e}");
                }
            }
        })
    }
}

/// Everything file mode set up, and how to take it down again.
struct FileMode {
    /// Stands in for the app data directory; holds `phone-videos/`.
    tmp: tempfile::TempDir,
    store: PhoneVideoStore,
    host: Arc<CliHost>,
    player: Arc<dyn LoopPlayer>,
}

impl FileMode {
    fn new(output: Output) -> anyhow::Result<FileMode> {
        let tmp = tempfile::Builder::new().prefix("camlooper-cli-").tempdir()?;
        let store = PhoneVideoStore::in_app_data(tmp.path());
        let host = Arc::new(CliHost { output, started_camera: AtomicBool::new(false) });
        let player: Arc<dyn LoopPlayer> = Arc::new(ReportingLoop(EngineLoop::new(host.clone())));
        Ok(FileMode { tmp, store, host, player })
    }

    /// Stop the loop (its ffmpeg holds the device and the file), stop the camera this run
    /// started, delete the received videos and their directory.
    async fn cleanup(self) {
        let stopped = loop_engine::stop_and_wait(Duration::from_secs(5)).await;
        if self.host.started_camera.load(Ordering::SeqCst) {
            let _ = virtual_camera::stop_virtual_camera().await;
        }
        let deleted = self.store.sweep(None);
        let dir = self.tmp.path().to_path_buf();
        let removed = self.tmp.close().is_ok();
        emit(json!({"event": "cleanup", "deleted": deleted, "dir": dir, "dirRemoved": removed, "loopStopped": stopped}));
    }
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn terminated() {
    std::future::pending().await
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    keep_stdout_for_json();
    if args.debug {
        let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Debug));
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let code = match run(args).await {
        Ok(()) => 0,
        Err(e) => {
            emit(json!({"event": "error", "error": format!("{e:#}")}));
            1
        }
    };
    std::process::exit(code);
}

async fn run(args: Args) -> anyhow::Result<()> {
    let mut base = Url::parse(&args.api)?;
    if !base.path().ends_with('/') {
        let p = format!("{}/", base.path());
        base.set_path(&p);
    }

    // --- pairing -------------------------------------------------------------------------
    let (pairing, pair_path, claimed) = match (&args.code, &args.pair_file) {
        (Some(code), file) => {
            let pairing = client::claim_pairing(&base, code.trim(), "CamLooper CLI".into())
                .await
                .map_err(|e| anyhow::anyhow!("claim failed: {}", e.code()))?;
            let path = file
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join(format!("camlooper-cli-pair-{}.json", std::process::id())));
            Store::at(path.clone()).save(&pairing)?; // 0600, atomic
            emit(json!({"event": "paired", "mobileDeviceName": pairing.mobile_device_name, "pairFile": path}));
            (pairing, path, true)
        }
        (None, Some(file)) => {
            let pairing = Store::at(file.clone())
                .load()
                .ok_or_else(|| anyhow::anyhow!("no usable pairing in {}", file.display()))?;
            (pairing, file.clone(), false)
        }
        (None, None) => unreachable!("parse_args requires one"),
    };

    let output = match (&args.device, args.null) {
        (Some(device), _) => Output::Device(device.clone()),
        (None, true) => Output::Null,
        (None, false) => Output::App,
    };
    let result = match FileMode::new(output) {
        Ok(files) => {
            let result = stream(&args, &base, &pairing, &files).await;
            files.cleanup().await;
            result
        }
        Err(e) => Err(e),
    };

    if claimed && !args.keep_pair {
        let revoked = client::delete_pair(&base, &pairing).await.is_ok();
        let _ = Store::at(pair_path).delete();
        emit(json!({"event": "unpaired", "revoked": revoked}));
    }
    result
}

async fn stream(args: &Args, base: &Url, pairing: &Pairing, files: &FileMode) -> anyhow::Result<()> {
    let fixed = |sink| -> Arc<dyn CameraControl> {
        Arc::new(CliCamera { target: CameraTarget { sink, geometry: GEOMETRY, preview: false }, resume_loop: AtomicBool::new(false) })
    };
    let camera: Arc<dyn CameraControl> = match &files.host.output {
        Output::Device(device) => fixed(CameraSink::V4l2 { device: device.clone() }),
        Output::Null => fixed(CameraSink::Null),
        Output::App => Arc::new(AppCamera::default()),
    };
    // What the main window pushes for a freshly loaded video: loop forever, natural motion.
    loop_engine::apply_window_default_settings().await?;
    let ice = Arc::new(RelayStun { base: base.clone(), pairing: pairing.clone() });
    #[cfg(not(target_os = "windows"))]
    let ffmpeg = camlooper_lib::system_ffmpeg();
    #[cfg(target_os = "windows")]
    let ffmpeg = PathBuf::from("ffmpeg");
    let mut deps = SessionDeps::new(ffmpeg, camera, ice);
    deps.files = Some(FileDeps::new(files.store.clone(), files.player.clone()));
    let (dtx, mut decisions) = tokio::sync::mpsc::unbounded_channel();
    let (manager, mut outbound) = StreamManager::spawn_with_loop(
        Arc::new(Arc::new(deps)),
        Arc::new(ChannelDecisions(dtx)),
        LoopDeps { player: files.player.clone(), store: Some(files.store.clone()) },
    );
    manager.set_device_name(pairing.mobile_device_name.clone());
    let mut status = manager.subscribe();

    let mut ws = match client::connect(base, pairing).await {
        Ok(ws) => ws,
        Err(ConnectError::Revoked) => anyhow::bail!("the pairing was revoked"),
        Err(ConnectError::Other(e)) => anyhow::bail!("relay connection failed: {e}"),
    };
    emit(json!({"event": "connected", "protocol": 2}));

    let deadline = tokio::time::sleep(Duration::from_secs(args.seconds));
    tokio::pin!(deadline);
    let mut ping = tokio::time::interval(Duration::from_secs(30));
    ping.tick().await;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut was_streaming = false;
    let mut stopping = false;
    // The session that is (or was last) up is a file transfer; and the progress last printed.
    let mut file_session = false;
    let mut progress_shown: Option<(String, u64)> = None;
    // Set when a live stream ended: the run ends then, unless the phone comes back first.
    let mut exit_at: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMessage>(text.as_str()) {
                    Ok(m) => match route_server_message(&manager, m) {
                        Some(ServerMessage::Hello { peer_online, mobile_device_name, .. }) => {
                            emit(json!({"event": "hello", "phoneOnline": peer_online, "mobileDeviceName": mobile_device_name}));
                        }
                        Some(ServerMessage::Unpaired { reason }) => anyhow::bail!("unpaired by the server ({reason:?})"),
                        // open_meeting is deliberately not acked: it belongs to the app.
                        _ => {}
                    },
                    Err(e) => eprintln!("unreadable relay frame: {e}"),
                },
                Some(Ok(Message::Ping(p))) => { let _ = ws.send(Message::Pong(p)).await; }
                Some(Ok(Message::Close(f))) => anyhow::bail!("relay closed the socket ({:?})", f.map(|f| u16::from(f.code))),
                Some(Ok(_)) => {}
                Some(Err(e)) => anyhow::bail!("relay socket error: {e}"),
                None => anyhow::bail!("relay socket closed"),
            },
            frame = outbound.recv() => {
                let Some(frame) = frame else { break };
                emit(json!({"event": "send", "frame": frame.describe()}));
                if let OutboundFrame::Signal(StreamSignal::LoopStatus { playing, name, from_phone, error, .. }) = &frame {
                    emit(json!({"event": "loop_status", "playing": playing, "name": name, "fromPhone": from_phone, "error": error}));
                }
                if let Some(text) = frame.to_json() {
                    ws.send(Message::text(text)).await?;
                }
            }
            d = decisions.recv() => match d {
                Some(DecisionEvent::Request(req)) => {
                    exit_at = None;
                    emit(json!({"event": "stream_request", "deviceName": req.device_name, "mode": req.mode, "file": req.file, "autoAccept": true}));
                    if let Err(e) = manager.accept(&req.session_id).await {
                        eprintln!("accept failed: {e}");
                    }
                }
                Some(DecisionEvent::Dismissed(_)) => emit(json!({"event": "request_dismissed"})),
                None => break,
            },
            changed = status.changed() => {
                if changed.is_err() { break }
                let s = status.borrow_and_update().clone();
                emit(json!({"event": "state", "state": s.state, "mode": s.mode, "filePhase": s.file.as_ref().map(|f| f.phase), "lastError": s.last_error}));
                if let (Some(id), Some(f)) = (&s.session_id, &s.file) {
                    let shown = progress_shown.as_ref().filter(|(shown_id, _)| shown_id == id).map(|(_, n)| *n);
                    let moving = matches!(f.phase, FilePhase::Receiving | FilePhase::Processing | FilePhase::Looping);
                    if moving && shown != Some(f.received) {
                        emit(json!({"event": "file_progress", "received": f.received, "size": f.size}));
                        progress_shown = Some((id.clone(), f.received));
                    }
                }
                if matches!(s.state, Phase::Connecting | Phase::Live | Phase::Stopping) {
                    was_streaming = true;
                    file_session = s.mode == StreamMode::File;
                } else if s.state.is_free() && stopping {
                    break;
                } else if s.state.is_free() && was_streaming {
                    // A file session is over (looping, failed or cancelled): keep serving —
                    // the loop plays on and the phone may send `loop_control` or more. So
                    // too after a live stream that interrupted a loop. A live stream with
                    // nothing to go back to ends the run, after a moment for a follow-up.
                    was_streaming = false;
                    if !file_session && files.player.state().await.path.is_none() {
                        emit(json!({"event": "live_ended", "exitInSeconds": LIVE_END_GRACE.as_secs()}));
                        exit_at = Some(tokio::time::Instant::now() + LIVE_END_GRACE);
                    }
                }
            }
            _ = tick.tick() => {
                let s = manager.status();
                if s.state == Phase::Live && s.mode == StreamMode::Live {
                    emit(status_line(&s));
                }
                let p = loop_engine::playback().await;
                if p.playing {
                    emit(json!({"event": "loop", "playing": true, "preparing": p.preparing, "fps": p.fps, "seconds": p.seconds}));
                }
            }
            _ = async { tokio::time::sleep_until(exit_at.expect("guarded")).await }, if exit_at.is_some() && !stopping => break,
            _ = ping.tick() => { ws.send(Message::text(r#"{"type":"ping"}"#)).await?; }
            _ = &mut deadline, if !stopping => {
                emit(json!({"event": "deadline", "seconds": args.seconds}));
                stopping = true;
                manager.stop(StopReason::User).await;
                if manager.status().state.is_free() { break }
            }
            _ = tokio::signal::ctrl_c(), if !stopping => {
                stopping = true;
                manager.stop(StopReason::User).await;
                if manager.status().state.is_free() { break }
            }
            _ = terminated(), if !stopping => {
                stopping = true;
                manager.stop(StopReason::User).await;
                if manager.status().state.is_free() { break }
            }
        }
    }
    // Deliver a final stream_stop if one is queued, then close politely.
    while let Ok(frame) = outbound.try_recv() {
        if let Some(text) = frame.to_json() {
            let _ = ws.send(Message::text(text)).await;
        }
    }
    let _ = ws.close(None).await;
    emit(status_line(&manager.status()));
    Ok(())
}
