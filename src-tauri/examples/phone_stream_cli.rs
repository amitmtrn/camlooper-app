//! Headless dev tool (not shipped): receive a phone camera stream through the live relay
//! and run the real pipeline, without the Tauri app or its UI.
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
//! * stdout: one JSON object per line (events and a status line per second). stderr: logs.
//!   Tokens are never printed. `--debug` adds debug logs (SDP and candidate addresses).
//! * Exits after `--seconds` (default 300), when a stream that went live ends, or on Ctrl-C.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use camlooper_lib::pipeline::{CameraSink, Geometry};
use camlooper_lib::remote::client::{self, ConnectError};
use camlooper_lib::remote::protocol::ServerMessage;
use camlooper_lib::remote::store::{Pairing, Store};
use camlooper_lib::remote::stream::handoff::AppCamera;
use camlooper_lib::remote::stream::rtc::resolve_stun;
use camlooper_lib::remote::stream::signal::StopReason;
use camlooper_lib::remote::stream::{
    route_server_message, CameraControl, CameraTarget, ChannelDecisions, DecisionEvent, FixedCamera, IceSource, Phase,
    SessionDeps, StreamManager, StreamStatus,
};
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

fn emit(v: serde_json::Value) {
    println!("{v}");
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

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
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

    let result = stream(&args, &base, &pairing).await;

    if claimed && !args.keep_pair {
        let revoked = client::delete_pair(&base, &pairing).await.is_ok();
        let _ = Store::at(pair_path).delete();
        emit(json!({"event": "unpaired", "revoked": revoked}));
    }
    result
}

async fn stream(args: &Args, base: &Url, pairing: &Pairing) -> anyhow::Result<()> {
    let geometry = Geometry { width: 1280, height: 720, fps: 30 };
    let camera: Arc<dyn CameraControl> = match (&args.device, args.null) {
        (Some(device), _) => Arc::new(FixedCamera(CameraTarget {
            sink: CameraSink::V4l2 { device: device.clone() },
            geometry,
            preview: false,
        })),
        (None, true) => Arc::new(FixedCamera(CameraTarget { sink: CameraSink::Null, geometry, preview: false })),
        (None, false) => Arc::new(AppCamera::default()),
    };
    let ice = Arc::new(RelayStun { base: base.clone(), pairing: pairing.clone() });
    #[cfg(not(target_os = "windows"))]
    let ffmpeg = camlooper_lib::system_ffmpeg();
    #[cfg(target_os = "windows")]
    let ffmpeg = PathBuf::from("ffmpeg");
    let deps = Arc::new(SessionDeps::new(ffmpeg, camera, ice));
    let (dtx, mut decisions) = tokio::sync::mpsc::unbounded_channel();
    let (manager, mut outbound) = StreamManager::spawn(Arc::new(deps), Arc::new(ChannelDecisions(dtx)));
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
                if let Some(text) = frame.to_json() {
                    ws.send(Message::text(text)).await?;
                }
            }
            d = decisions.recv() => match d {
                Some(DecisionEvent::Request(req)) => {
                    emit(json!({"event": "stream_request", "deviceName": req.device_name, "autoAccept": true}));
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
                emit(json!({"event": "state", "state": s.state, "lastError": s.last_error}));
                if matches!(s.state, Phase::Connecting | Phase::Live | Phase::Stopping) {
                    was_streaming = true;
                } else if s.state.is_free() && (was_streaming || stopping) {
                    break; // the stream ended
                }
            }
            _ = tick.tick() => {
                let s = manager.status();
                if s.state == Phase::Live {
                    emit(status_line(&s));
                }
            }
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
