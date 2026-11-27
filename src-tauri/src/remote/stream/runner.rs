//! One phone session from "accepted" to "ended": take over the virtual camera, gather
//! candidates, answer the offer, then feed every received frame to an ffmpeg that writes the
//! camera through the existing sinks (v4l2 / softcam / CMIO). Tauri-independent: the camera
//! handoff and the STUN lookup are injected, so the CLI and the integration test run the same
//! code with a fixed sink.
//!
//! A **file session** ([`SessionKind::File`]) shares the connection setup and nothing else:
//! no camera is claimed and no ffmpeg runs while the phone's video arrives over the data
//! channel. The bytes go to [`super::file_receive::PhoneVideoStore`]; once complete, the
//! file is handed to the desktop's own loop engine ([`super::file_receive::LoopPlayer`]) and
//! the session ends with [`StopReason::Done`].
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use super::file_receive::{self, reply, FileDeps, PartFile, TextStep, Transfer};
use super::media::{FrameOut, StreamWriter};
use super::rtc::{self, EncodedFrame, RtcClose, RtcCommand, RtcEvent, RtcMode, RtcOptions};
use super::signal::{FileOffer, StopReason, MAX_REMOTE_CANDIDATES};
use crate::pipeline::{self, CameraSink, EncodedCodec, Geometry, PipelineSpec, PreviewSpec, Source};

/// ffmpeg is restarted this many times after it dies; one more death ends the session. The
/// count is forgiven once the decoder has run [`RESTART_WINDOW`] without dying, so a hiccup
/// every hour never adds up to a dead stream.
pub const MAX_DECODER_RESTARTS: u32 = 3;
pub const RESTART_WINDOW: Duration = Duration::from_secs(30);
const DECODER_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// After the phone closes the connection, its `stream_stop` (which says why) is given this
/// long to arrive over the relay before the session ends as `peer_stopped`.
pub const PEER_STOP_GRACE: Duration = Duration::from_millis(1500);

/// Where the phone's frames go for this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraTarget {
    pub sink: CameraSink,
    pub geometry: Geometry,
    /// Also produce the UI preview (an MJPEG leg on a device sink; derived in Rust on a raw
    /// stdout sink).
    pub preview: bool,
}

/// Tells a running [`CameraControl::acquire`] that its session is over. The acquire is still
/// never dropped half way; it is asked to come back early (give up a system prompt it is
/// waiting in) so the camera can be handed back sooner.
#[derive(Clone)]
pub struct Cancel(watch::Receiver<bool>);

/// The session's end of a [`Cancel`].
pub struct CancelHandle(watch::Sender<bool>);

impl Cancel {
    pub fn new() -> (CancelHandle, Cancel) {
        let (tx, rx) = watch::channel(false);
        (CancelHandle(tx), Cancel(rx))
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the session has been cancelled (or its task is gone altogether).
    pub async fn cancelled(&self) {
        let mut rx = self.0.clone();
        let _ = rx.wait_for(|c| *c).await;
    }
}

impl CancelHandle {
    pub fn cancel(&self) {
        let _ = self.0.send(true);
    }
}

/// Hands the virtual camera to the phone stream and back.
///
/// `epoch` identifies the session (the state machine's counter; it never repeats within a
/// run). The camera belongs to the epoch that acquired it, and only that epoch's `release`
/// gives it back.
pub trait CameraControl: Send + Sync {
    /// Make the camera available and stop whatever else writes to it. Must remember what it
    /// changed as it goes, so `release` can undo a partially completed acquire. Runs to
    /// completion in its own task: it is never dropped half way. When `cancel` fires it
    /// should return (an error) as soon as it safely can.
    fn acquire(&self, epoch: u64, cancel: Cancel) -> BoxFuture<'_, Result<CameraTarget>>;
    /// Undo what `acquire(epoch)` did. Always called once per session, also after a failed
    /// or cancelled acquire; must be a no-op when there is nothing to undo **or when the
    /// camera is held by another epoch** (this session's acquire was refused because an
    /// earlier session had not released yet — that session's own release undoes it).
    fn release(&self, epoch: u64) -> BoxFuture<'_, ()>;
}

/// A fixed sink with nothing to hand back (CLI with `--device` / `--null`, tests).
pub struct FixedCamera(pub CameraTarget);

impl CameraControl for FixedCamera {
    fn acquire(&self, _epoch: u64, _cancel: Cancel) -> BoxFuture<'_, Result<CameraTarget>> {
        let t = self.0.clone();
        Box::pin(async move { Ok(t) })
    }
    fn release(&self, _epoch: u64) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The STUN server for the srflx candidate (from `/v1/ice-servers`), best effort.
pub trait IceSource: Send + Sync {
    fn stun_server(&self) -> BoxFuture<'_, Option<SocketAddr>>;
}

pub struct NoStun;

impl IceSource for NoStun {
    fn stun_server(&self) -> BoxFuture<'_, Option<SocketAddr>> {
        Box::pin(async { None })
    }
}

pub struct SessionDeps {
    pub ffmpeg: PathBuf,
    pub camera: Arc<dyn CameraControl>,
    pub ice: Arc<dyn IceSource>,
    pub rtc: RtcOptions,
    /// Accepted but no offer within this long → `timeout`.
    pub offer_timeout: Duration,
    /// The camera and sockets must be ready within this long → `timeout`. Separate from (and
    /// much longer than) the offer deadline: acquiring the camera can sit in a polkit or UAC
    /// prompt that the user has to read and answer.
    pub prepare_timeout: Duration,
    /// Where a phone's video file goes and what plays it. `None` (headless tools): file
    /// sessions end with an error.
    pub files: Option<FileDeps>,
}

impl SessionDeps {
    pub fn new(ffmpeg: PathBuf, camera: Arc<dyn CameraControl>, ice: Arc<dyn IceSource>) -> Self {
        Self {
            ffmpeg,
            camera,
            ice,
            rtc: RtcOptions::default(),
            offer_timeout: Duration::from_secs(20),
            prepare_timeout: Duration::from_secs(120),
            files: None,
        }
    }
}

/// What an accepted session is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionKind {
    /// The phone's camera becomes the virtual camera.
    Live,
    /// The phone sends this video file; the desktop loops it.
    File(FileOffer),
}

/// Where a file session is once every byte has arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStage {
    /// Saved; being loaded by the loop engine.
    Processing,
    /// Looping on the virtual camera.
    Looping,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionCommand {
    Offer(String),
    RemoteCandidate(String),
    Stop(StopReason),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaStats {
    pub codec: Option<EncodedCodec>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Frames received from the phone over the last second.
    pub fps: f32,
    /// Frames written to ffmpeg since the session started.
    pub frames: u64,
    pub decoder_restarts: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// The camera handoff and the sockets are done (the session may still wait for the offer).
    Prepared,
    /// The session has ended by itself (deadline, failure) but cannot report `Closed` yet:
    /// the camera handoff is still under way and has to finish before it can be undone.
    /// Not sent when the end was a `Stop` command — the machine knows about that one.
    Releasing { reason: StopReason, error: Option<String> },
    Answer(String),
    IceConnected,
    /// The first frame reached ffmpeg.
    Live(MediaStats),
    /// Once a second while live.
    Stats(MediaStats),
    /// File session: the transfer has started (first, with 0) or moved on. At most every
    /// 250 ms, and once with the full size.
    FileProgress { received: u64 },
    /// File session: all bytes are saved.
    FileStage(FileStage),
    /// The session is over and the camera has been handed back.
    Closed { reason: StopReason, error: Option<String> },
}

/// Start a session task. It ends by sending exactly one `Closed`, after handing the camera
/// back — also when the session panics: otherwise the camera would stay claimed (video
/// playback refused until restart) and the state machine busy forever.
///
/// `epoch` is the state machine's id for this session; the camera is acquired and released
/// under it, so this session can never hand back a camera that another one holds.
pub fn spawn_session(
    deps: Arc<SessionDeps>,
    epoch: u64,
    kind: SessionKind,
    events: mpsc::UnboundedSender<SessionEvent>,
) -> mpsc::UnboundedSender<SessionCommand> {
    let (tx, rx) = mpsc::unbounded_channel();
    let session_events = events.clone();
    match kind {
        SessionKind::Live => {
            let camera = deps.camera.clone();
            spawn_guarded(camera, epoch, events, async move { run_session(deps, epoch, rx, &session_events).await });
        }
        // A file session never touches the camera: nothing to acquire, nothing to hand back.
        SessionKind::File(offer) => {
            spawn_reported(events, async move { run_file_session(deps, offer, rx, &session_events).await });
        }
    }
    tx
}

/// Run `session` to its outcome and send `Closed` — also when it panics, or the state
/// machine would stay busy forever.
fn spawn_reported(
    events: mpsc::UnboundedSender<SessionEvent>,
    session: impl std::future::Future<Output = Outcome> + Send + 'static,
) {
    tokio::spawn(async move {
        let run = AssertUnwindSafe(session).catch_unwind().await;
        let (reason, error) = run.unwrap_or_else(|_| (StopReason::Error, Some("internal error in the phone video transfer".into())));
        match &error {
            Some(e) => eprintln!("phone video: ended ({reason}): {e}"),
            None => eprintln!("phone video: ended ({reason})"),
        }
        let _ = events.send(SessionEvent::Closed { reason, error });
    });
}

/// Run `session` to its outcome, hand the camera back, send `Closed` — whatever happens in
/// between, a panic included.
fn spawn_guarded(
    camera: Arc<dyn CameraControl>,
    epoch: u64,
    events: mpsc::UnboundedSender<SessionEvent>,
    session: impl std::future::Future<Output = Outcome> + Send + 'static,
) {
    tokio::spawn(async move {
        let run = AssertUnwindSafe(session).catch_unwind().await;
        let (reason, error) = run.unwrap_or_else(|_| (StopReason::Error, Some("internal error in the phone stream".into())));
        if AssertUnwindSafe(camera.release(epoch)).catch_unwind().await.is_err() {
            eprintln!("phone stream: handing the camera back failed unexpectedly");
        }
        if let Some(e) = &error {
            eprintln!("phone stream: ended ({reason}): {e}");
        } else {
            eprintln!("phone stream: ended ({reason})");
        }
        let _ = events.send(SessionEvent::Closed { reason, error });
    });
}

type Outcome = (StopReason, Option<String>);
type Prepared = (CameraTarget, rtc::LocalTransport);

async fn run_session(
    deps: Arc<SessionDeps>,
    epoch: u64,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Outcome {
    // --- 1. prepare (camera, STUN, sockets) while the phone sends its offer -------------
    // In its own task, never cancelled: dropping `acquire` between "started the camera" and
    // "noted that it must be stopped again" would leave the camera on with nothing to undo.
    // It is told when the session is over, though, so it can come back early.
    let prep = deps.clone();
    let (cancel, cancelled) = Cancel::new();
    let mut prepare = tokio::spawn(async move {
        let target = prep.camera.acquire(epoch, cancelled.clone()).await?;
        // Nothing to undo in the rest: once the session is over it is simply dropped.
        tokio::select! {
            r = async {
                let stun = prep.ice.stun_server().await;
                let transport = rtc::gather(&prep.rtc, stun).await?;
                Ok::<Prepared, anyhow::Error>((target, transport))
            } => r,
            _ = cancelled.cancelled() => Err(anyhow!("the session ended while preparing")),
        }
    });
    let (target, transport, offer, candidates) = match offer_and_camera(&deps, &mut prepare, &mut cmd_rx, events).await {
        Ok(ready) => ready,
        Err((outcome, by_command)) => {
            // Stopped or timed out mid-acquire: let it finish so that `release` (the caller's
            // next step) undoes all of it rather than racing it. That can take as long as
            // the user leaves a system prompt open, so say what happened *before* waiting:
            // the phone and the UI must not sit in "connecting" until then.
            if !prepare.is_finished() {
                cancel.cancel();
                if !by_command {
                    let _ = events.send(SessionEvent::Releasing { reason: outcome.0.clone(), error: outcome.1.clone() });
                }
                // No deadline here: releasing under a running acquire would undo nothing
                // (or half). If it never returns, neither does this session, and the state
                // machine's watchdog reports that — while keeping the camera marked as held.
                let _ = (&mut prepare).await;
            }
            return outcome;
        }
    };

    // --- 2. answer -------------------------------------------------------------------------
    let (answer, rtc_tx, mut rtc_rx) = match rtc::accept_offer(transport, &offer, deps.rtc.clone(), RtcMode::Live) {
        Ok(x) => x,
        Err(e) => return (StopReason::Error, Some(format!("{e:#}"))),
    };
    let _ = events.send(SessionEvent::Answer(answer));
    for c in candidates {
        let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
    }

    // --- 3. media ------------------------------------------------------------------------
    let mut media = MediaLoop::new(deps.ffmpeg.clone(), target, rtc_tx.clone());
    // Commands that arrived while a frame was being written (see `MediaLoop::on_frame`).
    let mut pending: VecDeque<SessionCommand> = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            ev = rtc_rx.recv() => match ev {
                Some(RtcEvent::IceConnected) => {
                    eprintln!("phone stream: connected to the phone");
                    let _ = events.send(SessionEvent::IceConnected);
                }
                Some(RtcEvent::Frame(f)) => match media.on_frame(f, events, &mut cmd_rx, &mut pending).await {
                    Ok(None) => {}
                    Ok(Some(outcome)) => break outcome,
                    Err(e) => break (StopReason::Error, Some(format!("{e:#}"))),
                },
                // The phone closed the connection: a normal ending. Its `stream_stop` usually
                // follows over the relay within a moment and carries the reason.
                Some(RtcEvent::Closed(RtcClose::PeerClosed)) => {
                    break (phone_stop_reason(&mut pending, &mut cmd_rx).await, None);
                }
                Some(RtcEvent::Closed(RtcClose::Stopped)) | None => break (StopReason::Error, Some("receiver stopped".into())),
                Some(RtcEvent::Closed(RtcClose::Failed { reason, detail })) => break (reason, Some(detail)),
                // A data channel next to the video: not part of a live session.
                Some(RtcEvent::ChannelOpen | RtcEvent::ChannelText(_) | RtcEvent::ChannelBinary(_) | RtcEvent::ChannelClosed) => {}
            },
            cmd = next_command(&mut pending, &mut cmd_rx) => match cmd {
                Some(SessionCommand::RemoteCandidate(c)) => {
                    let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                }
                Some(SessionCommand::Offer(_)) => log::debug!("phone stream: ignoring a second offer (no renegotiation)"),
                Some(SessionCommand::Stop(r)) => break (r, None),
                None => break (StopReason::Error, Some("session dropped".into())),
            },
            _ = tick.tick() => {
                if let Err(e) = media.check_decoder().await {
                    break (StopReason::Error, Some(format!("{e:#}")));
                }
                if media.live {
                    let _ = events.send(SessionEvent::Stats(media.stats()));
                }
            }
        }
    };
    let _ = rtc_tx.send(RtcCommand::Stop);
    media.shutdown().await;
    outcome
}

// ---------------------------------------------------------------------------------------
// File session
// ---------------------------------------------------------------------------------------

/// After the last message for the phone (`looping`, `error`) the connection is kept this
/// long so the message is on the wire before the close; a phone that closes first ends it.
const FILE_LINGER: Duration = Duration::from_millis(500);
const SAVE_FAILED: &str = "The video could not be saved on the computer.";
const PLAY_FAILED: &str = "The video could not be played on the computer.";

/// How the receiving part of a file session ended.
enum Received {
    /// Every byte is in the `.part` file and `end` was seen.
    Complete(PartFile),
    /// A protocol violation, a stall or a disk error: tell the phone, then end as an error.
    Failed(String),
    /// Stopped, or the connection ended: nothing more to say on the data channel.
    Ended(Outcome),
}

async fn run_file_session(
    deps: Arc<SessionDeps>,
    offer: FileOffer,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Outcome {
    let Some(files) = deps.files.clone() else {
        return (StopReason::Error, Some("this build cannot receive a video from the phone".into()));
    };

    // --- 1. sockets, while the phone sends its offer ------------------------------------
    // Nothing here needs undoing, so unlike the camera handoff it is simply dropped on a stop.
    let (transport, sdp, candidates) = {
        let prepare = async {
            let stun = deps.ice.stun_server().await;
            rtc::gather(&deps.rtc, stun).await
        };
        tokio::pin!(prepare);
        let offer_deadline = tokio::time::Instant::now() + deps.offer_timeout;
        let mut transport: Option<rtc::LocalTransport> = None;
        let mut sdp: Option<String> = None;
        let mut candidates: Vec<String> = Vec::new();
        loop {
            if transport.is_some() && sdp.is_some() {
                break (transport.take().expect("checked"), sdp.take().expect("checked"), candidates);
            }
            tokio::select! {
                r = &mut prepare, if transport.is_none() => match r {
                    Ok(t) => {
                        transport = Some(t);
                        let _ = events.send(SessionEvent::Prepared);
                    }
                    Err(e) => return (StopReason::Error, Some(format!("{e:#}"))),
                },
                cmd = cmd_rx.recv() => match cmd {
                    Some(SessionCommand::Offer(o)) => sdp = Some(o),
                    Some(SessionCommand::RemoteCandidate(c)) => {
                        if candidates.len() < MAX_REMOTE_CANDIDATES {
                            candidates.push(c);
                        }
                    }
                    Some(SessionCommand::Stop(r)) => return (r, None),
                    None => return (StopReason::Error, Some("session dropped".into())),
                },
                _ = tokio::time::sleep_until(offer_deadline), if sdp.is_none() => {
                    return (StopReason::Timeout, Some("no offer from the phone".into()));
                }
            }
        }
    };

    // --- 2. answer: a data channel and nothing else ---------------------------------------
    let (answer, rtc_tx, mut rtc_rx) = match rtc::accept_offer(transport, &sdp, deps.rtc.clone(), RtcMode::File) {
        Ok(x) => x,
        Err(e) => return (StopReason::Error, Some(format!("{e:#}"))),
    };
    let _ = events.send(SessionEvent::Answer(answer));
    for c in candidates {
        let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
    }
    let say = |text: String| {
        let _ = rtc_tx.send(RtcCommand::ChannelText(text));
    };

    // --- 3. receive ------------------------------------------------------------------------
    let received = receive_file(&files, &offer, &rtc_tx, &mut rtc_rx, &mut cmd_rx, events).await;
    let outcome = match received {
        Received::Ended(outcome) => outcome,
        Received::Failed(message) => {
            say(reply::error(&message));
            linger(&mut rtc_rx).await;
            (StopReason::Error, Some(message))
        }
        Received::Complete(part) => match part.commit().await {
            Err(e) => {
                eprintln!("phone video: could not finish the file: {e}");
                say(reply::error(SAVE_FAILED));
                linger(&mut rtc_rx).await;
                (StopReason::Error, Some(SAVE_FAILED.into()))
            }
            Ok(path) => {
                say(reply::progress(offer.size));
                say(reply::saved());
                let _ = events.send(SessionEvent::FileProgress { received: offer.size });
                // --- 4. hand it to the loop engine ---------------------------------------
                let _ = events.send(SessionEvent::FileStage(FileStage::Processing));
                say(reply::processing());
                let (played, stopped) = play_received(&files, &offer, path, &rtc_tx, &mut rtc_rx, &mut cmd_rx).await;
                // Whatever is not loaded now goes: the previous phone video after a
                // success, the new one after a failure.
                files.sweep_unused().await;
                match played {
                    Ok(()) => {
                        let _ = events.send(SessionEvent::FileStage(FileStage::Looping));
                        say(reply::looping(&offer.name));
                        linger(&mut rtc_rx).await;
                        // A stop that came while the video was loading could no longer
                        // prevent it; the session still ends as that stop.
                        (stopped.unwrap_or(StopReason::Done), None)
                    }
                    Err(e) => {
                        eprintln!("phone video: could not play the received video: {e:#}");
                        say(reply::error(PLAY_FAILED));
                        linger(&mut rtc_rx).await;
                        (StopReason::Error, Some(PLAY_FAILED.into()))
                    }
                }
            }
        },
    };
    let _ = rtc_tx.send(RtcCommand::Stop);
    outcome
}

/// The transfer itself: `meta`, the chunks, `end`. The partial file is deleted on every way
/// out but [`Received::Complete`].
async fn receive_file(
    files: &FileDeps,
    offer: &FileOffer,
    rtc_tx: &mpsc::UnboundedSender<RtcCommand>,
    rtc_rx: &mut mpsc::Receiver<RtcEvent>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Received {
    let mut transfer = Transfer::new(offer.clone(), Instant::now(), files.idle_timeout);
    let mut part: Option<PartFile> = None;
    // The byte count last told to the phone and the UI.
    let mut reported: u64 = 0;
    let mut no_commands = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let ended = loop {
        tokio::select! {
            ev = rtc_rx.recv() => match ev {
                Some(RtcEvent::IceConnected) => {
                    eprintln!("phone video: connected to the phone");
                    let _ = events.send(SessionEvent::IceConnected);
                }
                Some(RtcEvent::ChannelOpen | RtcEvent::Frame(_)) => {}
                Some(RtcEvent::ChannelText(text)) => match transfer.on_text(&text, Instant::now()) {
                    Ok(TextStep::Started) => {
                        match files.store.create(offer).await {
                            Ok(p) => part = Some(p),
                            Err(e) => {
                                eprintln!("phone video: could not create the file: {e}");
                                break Received::Failed(SAVE_FAILED.into());
                            }
                        }
                        eprintln!("phone video: receiving {} bytes", offer.size);
                        let _ = events.send(SessionEvent::FileProgress { received: 0 });
                    }
                    Ok(TextStep::Complete) => match part.take() {
                        Some(p) => return Received::Complete(p),
                        None => break Received::Failed(SAVE_FAILED.into()),
                    },
                    Ok(TextStep::Ignored) => {}
                    Err(e) => break Received::Failed(e.message().into()),
                },
                Some(RtcEvent::ChannelBinary(data)) => match transfer.on_binary(data.len(), Instant::now()) {
                    Ok(progress) => {
                        let written = match part.as_mut() {
                            Some(p) => p.write(&data).await,
                            None => Err(std::io::Error::other("no file")),
                        };
                        if let Err(e) = written {
                            eprintln!("phone video: could not write the file: {e}");
                            break Received::Failed(SAVE_FAILED.into());
                        }
                        if let Some(received) = progress {
                            reported = received;
                            let _ = rtc_tx.send(RtcCommand::ChannelText(reply::progress(received)));
                            let _ = events.send(SessionEvent::FileProgress { received });
                        }
                    }
                    Err(e) => break Received::Failed(e.message().into()),
                },
                // The phone gave up (closed the channel or the whole connection). Its
                // `stream_stop` usually follows over the relay and carries the reason.
                Some(RtcEvent::ChannelClosed | RtcEvent::Closed(RtcClose::PeerClosed)) => {
                    break Received::Ended((phone_stop_reason(&mut no_commands, cmd_rx).await, None));
                }
                Some(RtcEvent::Closed(RtcClose::Stopped)) | None => break Received::Ended((StopReason::Error, Some("receiver stopped".into()))),
                Some(RtcEvent::Closed(RtcClose::Failed { reason, detail })) => break Received::Ended((reason, Some(detail))),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCommand::RemoteCandidate(c)) => {
                    let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                }
                Some(SessionCommand::Offer(_)) => log::debug!("phone video: ignoring a second offer (no renegotiation)"),
                // The phone's or the user's cancel.
                Some(SessionCommand::Stop(r)) => break Received::Ended((r, None)),
                None => break Received::Ended((StopReason::Error, Some("session dropped".into()))),
            },
            _ = tick.tick() => {
                if let Err(e) = transfer.check_idle(Instant::now()) {
                    break Received::Failed(e.message().into());
                }
                // The throttle only reports when a chunk arrives; when the phone pauses,
                // the last chunks would otherwise never show.
                if transfer.received() > reported {
                    reported = transfer.received();
                    let _ = rtc_tx.send(RtcCommand::ChannelText(reply::progress(reported)));
                    let _ = events.send(SessionEvent::FileProgress { received: reported });
                }
            }
        }
    };
    if let Some(p) = part.take() {
        p.discard().await;
    }
    ended
}

/// Load and start the received video, keeping the connection served meanwhile (loading can
/// take seconds; the phone is waiting for `looping` on it). The load runs in its own task
/// and is never dropped half way. Returns its result, and a stop that arrived meanwhile.
async fn play_received(
    files: &FileDeps,
    offer: &FileOffer,
    path: PathBuf,
    rtc_tx: &mpsc::UnboundedSender<RtcCommand>,
    rtc_rx: &mut mpsc::Receiver<RtcEvent>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> (Result<()>, Option<StopReason>) {
    let player = files.player.clone();
    let name = file_receive::load_name(offer);
    let mut play = tokio::spawn(async move { player.play_file(&path, &name).await });
    let mut stopped: Option<StopReason> = None;
    let mut connection_gone = false;
    let result = loop {
        tokio::select! {
            r = &mut play => break r,
            ev = rtc_rx.recv(), if !connection_gone => {
                if matches!(ev, None | Some(RtcEvent::Closed(_))) {
                    connection_gone = true;
                }
            }
            cmd = cmd_rx.recv(), if stopped.is_none() => match cmd {
                Some(SessionCommand::Stop(r)) => stopped = Some(r),
                Some(SessionCommand::RemoteCandidate(c)) => {
                    let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                }
                Some(SessionCommand::Offer(_)) => {}
                None => stopped = Some(StopReason::Error),
            },
        }
    };
    let result = result.unwrap_or_else(|_| Err(anyhow!("loading the video failed unexpectedly")));
    (result, stopped)
}

/// Keep the connection up for [`FILE_LINGER`], or until the phone closes it.
async fn linger(rtc_rx: &mut mpsc::Receiver<RtcEvent>) {
    let deadline = tokio::time::Instant::now() + FILE_LINGER;
    loop {
        match tokio::time::timeout_at(deadline, rtc_rx.recv()).await {
            Ok(Some(RtcEvent::Closed(_))) | Ok(None) | Err(_) => return,
            Ok(Some(_)) => {}
        }
    }
}

/// Phase 1: wait until the camera is ready *and* the offer has arrived. The two have separate
/// deadlines; a stop or a failure ends the wait with the session's outcome, and whether it
/// was a `Stop` command (which the machine already knows about).
async fn offer_and_camera(
    deps: &SessionDeps,
    prepare: &mut tokio::task::JoinHandle<Result<Prepared>>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Result<(CameraTarget, rtc::LocalTransport, String, Vec<String>), (Outcome, bool)> {
    let started = tokio::time::Instant::now();
    let offer_deadline = started + deps.offer_timeout;
    let prepare_deadline = started + deps.prepare_timeout;
    let mut prepared: Option<Prepared> = None;
    let mut offer: Option<String> = None;
    let mut candidates: Vec<String> = Vec::new();
    loop {
        if let (true, Some(sdp)) = (prepared.is_some(), offer.as_ref()) {
            let (target, transport) = prepared.take().expect("checked");
            return Ok((target, transport, sdp.clone(), candidates));
        }
        tokio::select! {
            r = &mut *prepare, if prepared.is_none() => match r {
                Ok(Ok(p)) => {
                    prepared = Some(p);
                    let _ = events.send(SessionEvent::Prepared);
                }
                Ok(Err(e)) => return Err(((StopReason::Error, Some(format!("{e:#}"))), false)),
                Err(_) => return Err(((StopReason::Error, Some("preparing the camera failed unexpectedly".into())), false)),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCommand::Offer(sdp)) => offer = Some(sdp),
                Some(SessionCommand::RemoteCandidate(c)) => {
                    if candidates.len() < MAX_REMOTE_CANDIDATES {
                        candidates.push(c);
                    }
                }
                Some(SessionCommand::Stop(r)) => return Err(((r, None), true)),
                None => return Err(((StopReason::Error, Some("session dropped".into())), false)),
            },
            _ = tokio::time::sleep_until(offer_deadline), if offer.is_none() => {
                return Err(((StopReason::Timeout, Some("no offer from the phone".into())), false));
            }
            _ = tokio::time::sleep_until(prepare_deadline), if prepared.is_none() => {
                return Err(((StopReason::Timeout, Some("preparing the camera took too long".into())), false));
            }
        }
    }
}

async fn next_command(
    pending: &mut VecDeque<SessionCommand>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> Option<SessionCommand> {
    match pending.pop_front() {
        Some(c) => Some(c),
        None => cmd_rx.recv().await,
    }
}

/// The phone closed the connection. Wait briefly for the `stream_stop` it sent over the relay
/// (the DTLS close usually wins the race) and use its reason; without one it is `peer_stopped`.
async fn phone_stop_reason(
    pending: &mut VecDeque<SessionCommand>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> StopReason {
    let deadline = tokio::time::Instant::now() + PEER_STOP_GRACE;
    loop {
        match tokio::time::timeout_at(deadline, next_command(pending, cmd_rx)).await {
            Ok(Some(SessionCommand::Stop(reason))) => return reason,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return StopReason::PeerStopped,
        }
    }
}

/// How many times ffmpeg may die before the session gives up: [`MAX_DECODER_RESTARTS`] within
/// a window, forgiven after [`RESTART_WINDOW`] of the decoder taking frames again.
#[derive(Debug, Default)]
struct RestartBudget {
    recent: u32,
    total: u32,
    last_crash: Option<Instant>,
}

impl RestartBudget {
    /// ffmpeg died. `true` if it may be restarted.
    fn on_crash(&mut self, now: Instant) -> bool {
        self.recent += 1;
        self.total += 1;
        self.last_crash = Some(now);
        self.recent <= MAX_DECODER_RESTARTS
    }

    /// A frame was written to the decoder.
    fn on_good_write(&mut self, now: Instant) {
        if self.recent > 0 && self.last_crash.is_some_and(|t| now.duration_since(t) >= RESTART_WINDOW) {
            self.recent = 0;
        }
    }
}

/// ffmpeg plus the writer that feeds it.
struct Decoder {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    reader: Option<tokio::task::JoinHandle<()>>,
    running: Arc<AtomicBool>,
    writer: StreamWriter,
}

impl Decoder {
    fn spawn(ffmpeg: &std::path::Path, codec: EncodedCodec, target: &CameraTarget) -> Result<Decoder> {
        let spec = PipelineSpec {
            source: Source::EncodedStdin { codec },
            sink: target.sink.clone(),
            geometry: target.geometry,
            preview: target.preview.then(PreviewSpec::default),
            realtime: false,
        };
        let stdout_used = target.sink.frame_bytes(&target.geometry).is_some() || target.preview;
        let mut cmd = tokio::process::Command::new(ffmpeg);
        cmd.args(pipeline::build(&spec))
            .stdin(Stdio::piped())
            .stdout(if stdout_used { Stdio::piped() } else { Stdio::null() })
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("could not start ffmpeg ({}): {e}", ffmpeg.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("ffmpeg has no stdin"))?;
        let running = Arc::new(AtomicBool::new(true));
        let reader = child.stdout.take().map(|out| {
            crate::video_processor::spawn_sink_reader(out, target.sink.clone(), target.geometry, running.clone())
        });
        eprintln!("phone stream: decoder started ({} → {:?})", codec.name(), target.sink);
        Ok(Decoder { child, stdin: Some(stdin), reader, running, writer: StreamWriter::new(codec) })
    }

    async fn stop(mut self) {
        self.running.store(false, Ordering::Relaxed);
        drop(self.stdin.take()); // EOF lets ffmpeg flush and exit
        if tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        if let Some(mut r) = self.reader.take() {
            if tokio::time::timeout(Duration::from_secs(1), &mut r).await.is_err() {
                // Stuck pushing a frame into the camera. Left alone it would overlap the next
                // decoder's reader on the same device.
                r.abort();
                let _ = tokio::time::timeout(Duration::from_millis(250), r).await;
            }
        }
    }
}

/// The orderly way out is [`Decoder::stop`]. This is for every other way (a panic unwinding
/// through the media loop, an early return): the reader must not keep pushing frames into
/// the camera after the session has handed it back. ffmpeg itself dies with `kill_on_drop`.
/// `abort` only takes effect where the reader awaits, so it also checks `running` before
/// every frame; a push that blocks inside the driver ends when that call returns.
impl Drop for Decoder {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(r) = self.reader.take() {
            r.abort();
        }
    }
}

struct MediaLoop {
    ffmpeg: PathBuf,
    target: CameraTarget,
    rtc: mpsc::UnboundedSender<RtcCommand>,
    decoder: Option<Decoder>,
    restarts: RestartBudget,
    live: bool,
    frames_written: u64,
    window_start: Instant,
    window_frames: u32,
    fps: f32,
    last_codec: Option<EncodedCodec>,
    last_dims: Option<(u32, u32)>,
}

impl MediaLoop {
    fn new(ffmpeg: PathBuf, target: CameraTarget, rtc: mpsc::UnboundedSender<RtcCommand>) -> Self {
        Self {
            ffmpeg,
            target,
            rtc,
            decoder: None,
            restarts: RestartBudget::default(),
            live: false,
            frames_written: 0,
            window_start: Instant::now(),
            window_frames: 0,
            fps: 0.0,
            last_codec: None,
            last_dims: None,
        }
    }

    fn stats(&self) -> MediaStats {
        MediaStats {
            codec: self.last_codec,
            width: self.last_dims.map(|d| d.0),
            height: self.last_dims.map(|d| d.1),
            fps: self.fps,
            frames: self.frames_written,
            decoder_restarts: self.restarts.total,
        }
    }

    fn keyframe(&self) {
        let _ = self.rtc.send(RtcCommand::RequestKeyframe);
    }

    async fn start_decoder(&mut self, codec: EncodedCodec) -> Result<()> {
        if let Some(old) = self.decoder.take() {
            old.stop().await;
        }
        self.decoder = Some(Decoder::spawn(&self.ffmpeg, codec, &self.target)?);
        self.keyframe(); // a fresh decoder can only start on a keyframe
        Ok(())
    }

    /// ffmpeg died: restart it (bounded) or give up.
    async fn restart_after_crash(&mut self, why: &str) -> Result<()> {
        let codec = self.decoder.as_ref().map(|d| d.writer.codec()).or(self.last_codec);
        if !self.restarts.on_crash(Instant::now()) {
            return Err(anyhow!("ffmpeg stopped {} times in a row ({why})", self.restarts.recent));
        }
        eprintln!("phone stream: decoder {why}; restarting ({}/{MAX_DECODER_RESTARTS})", self.restarts.recent);
        match codec {
            Some(c) => self.start_decoder(c).await,
            None => Ok(()),
        }
    }

    /// Write one frame to the decoder. `Ok(Some(outcome))` when a stop arrived while the
    /// write was blocked (ffmpeg not reading): the write is abandoned rather than letting it
    /// run out its timeout first. Other commands that arrive meanwhile are parked in `pending`.
    async fn on_frame(
        &mut self,
        f: EncodedFrame,
        events: &mpsc::UnboundedSender<SessionEvent>,
        cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
        pending: &mut VecDeque<SessionCommand>,
    ) -> Result<Option<Outcome>> {
        self.count_frame();
        let codec_changed = self.decoder.as_ref().map(|d| d.writer.codec()) != Some(f.codec);
        if codec_changed {
            if self.last_codec.is_some() {
                eprintln!("phone stream: codec changed to {}; restarting the decoder", f.codec.name());
            }
            self.start_decoder(f.codec).await?;
            self.last_codec = Some(f.codec);
        }
        let Some(dec) = self.decoder.as_mut() else { return Ok(None) };
        if !f.contiguous {
            dec.writer.require_keyframe();
        }
        let bytes = match dec.writer.push(&f.data, f.rtp_ts) {
            FrameOut::NeedKeyframe => {
                self.keyframe();
                return Ok(None);
            }
            FrameOut::Write { bytes, .. } => bytes,
        };
        if let Some(d) = dec.writer.dimensions() {
            self.last_dims = Some(d);
        }
        enum Written {
            Done(std::io::Result<()>),
            Stalled,
            Interrupted(Outcome),
        }
        let written = {
            let write = async {
                let stdin = dec.stdin.as_mut().ok_or_else(|| std::io::Error::other("stdin closed"))?;
                stdin.write_all(&bytes).await?;
                stdin.flush().await
            };
            tokio::pin!(write);
            let stalled = tokio::time::sleep(DECODER_WRITE_TIMEOUT);
            tokio::pin!(stalled);
            loop {
                tokio::select! {
                    biased;
                    r = &mut write => break Written::Done(r),
                    cmd = cmd_rx.recv() => match cmd {
                        Some(SessionCommand::Stop(r)) => break Written::Interrupted((r, None)),
                        Some(other) => pending.push_back(other),
                        None => break Written::Interrupted((StopReason::Error, Some("session dropped".into()))),
                    },
                    _ = &mut stalled => break Written::Stalled,
                }
            }
        };
        match written {
            Written::Done(Ok(())) => {}
            Written::Done(Err(e)) => return self.restart_after_crash(&format!("stopped reading ({e})")).await.map(|()| None),
            Written::Stalled => return self.restart_after_crash("stalled").await.map(|()| None),
            Written::Interrupted(outcome) => return Ok(Some(outcome)),
        }
        self.restarts.on_good_write(Instant::now());
        self.frames_written += 1;
        if !self.live {
            self.live = true;
            eprintln!(
                "phone stream: live ({} {})",
                f.codec.name(),
                self.last_dims.map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "?x?".into())
            );
            let _ = events.send(SessionEvent::Live(self.stats()));
        }
        Ok(None)
    }

    fn count_frame(&mut self) {
        self.window_frames += 1;
        self.roll_window();
    }

    fn roll_window(&mut self) {
        let elapsed = self.window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.window_frames as f32 / elapsed.as_secs_f32();
            self.window_frames = 0;
            self.window_start = Instant::now();
        }
    }

    async fn check_decoder(&mut self) -> Result<()> {
        self.roll_window();
        if self.window_start.elapsed() >= Duration::from_secs(2) {
            self.fps = 0.0; // nothing arrived for a while
        }
        let exited = match self.decoder.as_mut() {
            Some(d) => d.child.try_wait().ok().flatten(),
            None => None,
        };
        if let Some(status) = exited {
            return self.restart_after_crash(&format!("exited ({status})")).await;
        }
        Ok(())
    }

    async fn shutdown(&mut self) {
        if let Some(d) = self.decoder.take() {
            d.stop().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn restart_budget_is_per_window_not_per_session() {
        let t = Instant::now();
        let mut b = RestartBudget::default();
        // Three quick deaths are tolerated, the fourth is not.
        for i in 0..MAX_DECODER_RESTARTS {
            assert!(b.on_crash(t + Duration::from_secs(i as u64)));
        }
        assert!(!b.on_crash(t + Duration::from_secs(5)));

        // Hiccups spread over a long stream never add up.
        let mut b = RestartBudget::default();
        let mut now = t;
        for _ in 0..20 {
            assert!(b.on_crash(now));
            b.on_good_write(now + Duration::from_secs(1)); // too soon to forgive
            assert_eq!(b.recent, 1);
            now += RESTART_WINDOW + Duration::from_secs(1);
            b.on_good_write(now);
            assert_eq!(b.recent, 0);
        }
        assert_eq!(b.total, 20, "the stats still count every restart");
    }

    /// Counts acquire/release; optionally panics or dawdles in `acquire`.
    struct ProbeCamera {
        acquired: AtomicUsize,
        released: AtomicUsize,
        panic_in_acquire: bool,
        acquire_delay: Duration,
        /// Come back early (with an error) when the session is cancelled, like a system
        /// prompt that is closed.
        honour_cancel: bool,
        cancelled: AtomicUsize,
        epochs: std::sync::Mutex<Vec<(&'static str, u64)>>,
    }

    impl ProbeCamera {
        fn new(panic_in_acquire: bool, acquire_delay: Duration) -> Arc<Self> {
            Self::with(panic_in_acquire, acquire_delay, false)
        }
        fn with(panic_in_acquire: bool, acquire_delay: Duration, honour_cancel: bool) -> Arc<Self> {
            Arc::new(Self {
                acquired: AtomicUsize::new(0),
                released: AtomicUsize::new(0),
                panic_in_acquire,
                acquire_delay,
                honour_cancel,
                cancelled: AtomicUsize::new(0),
                epochs: Default::default(),
            })
        }
    }

    impl CameraControl for ProbeCamera {
        fn acquire(&self, epoch: u64, cancel: Cancel) -> BoxFuture<'_, Result<CameraTarget>> {
            Box::pin(async move {
                self.epochs.lock().unwrap().push(("acquire", epoch));
                tokio::select! {
                    _ = tokio::time::sleep(self.acquire_delay) => {}
                    _ = cancel.cancelled(), if self.honour_cancel => {
                        self.cancelled.fetch_add(1, Ordering::SeqCst);
                        return Err(anyhow!("cancelled"));
                    }
                }
                if self.panic_in_acquire {
                    panic!("boom (expected in this test)");
                }
                self.acquired.fetch_add(1, Ordering::SeqCst);
                Ok(CameraTarget {
                    sink: CameraSink::Null,
                    geometry: Geometry { width: 640, height: 360, fps: 30 },
                    preview: false,
                })
            })
        }
        fn release(&self, epoch: u64) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.epochs.lock().unwrap().push(("release", epoch));
                // A release that ran before the acquire finished would undo nothing.
                assert!(
                    self.panic_in_acquire || self.honour_cancel || self.acquired.load(Ordering::SeqCst) == 1,
                    "release raced the acquire"
                );
                self.released.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    struct PanickingIce;
    impl IceSource for PanickingIce {
        fn stun_server(&self) -> BoxFuture<'_, Option<SocketAddr>> {
            Box::pin(async { panic!("boom (expected in this test)") })
        }
    }

    fn deps(camera: Arc<ProbeCamera>, ice: Arc<dyn IceSource>) -> Arc<SessionDeps> {
        let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera, ice);
        d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
        Arc::new(d)
    }

    async fn closed(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> (StopReason, Option<String>) {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), events.recv()).await.expect("Closed in time") {
                Some(SessionEvent::Closed { reason, error }) => return (reason, error),
                Some(_) => {}
                None => panic!("the session ended without Closed"),
            }
        }
    }

    #[tokio::test]
    async fn a_panic_while_preparing_still_releases_the_camera_and_reports_closed() {
        for (camera, ice) in [
            (ProbeCamera::new(true, Duration::ZERO), Arc::new(NoStun) as Arc<dyn IceSource>),
            (ProbeCamera::new(false, Duration::ZERO), Arc::new(PanickingIce) as Arc<dyn IceSource>),
        ] {
            let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
            let _tx = spawn_session(deps(camera.clone(), ice), 1, SessionKind::Live, ev_tx);
            let (reason, error) = closed(&mut ev_rx).await;
            assert_eq!(reason, StopReason::Error);
            assert!(error.is_some());
            assert_eq!(camera.released.load(Ordering::SeqCst), 1, "the camera is handed back exactly once");
        }
    }

    #[tokio::test]
    async fn a_panic_in_the_session_itself_still_releases_and_reports_closed() {
        let camera = ProbeCamera::new(true, Duration::ZERO);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        spawn_guarded(camera.clone(), 1, ev_tx, async { panic!("boom (expected in this test)") });
        let (reason, error) = closed(&mut ev_rx).await;
        assert_eq!((reason, error.as_deref()), (StopReason::Error, Some("internal error in the phone stream")));
        assert_eq!(camera.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_stop_during_a_slow_acquire_waits_for_it_before_releasing() {
        let camera = ProbeCamera::new(false, Duration::from_millis(400));
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let tx = spawn_session(deps(camera.clone(), Arc::new(NoStun)), 1, SessionKind::Live, ev_tx);
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(SessionCommand::Stop(StopReason::User)).unwrap();
        assert_eq!(closed(&mut ev_rx).await, (StopReason::User, None));
        // ProbeCamera::release asserts that the acquire had completed.
        assert_eq!((camera.acquired.load(Ordering::SeqCst), camera.released.load(Ordering::SeqCst)), (1, 1));
    }

    #[tokio::test]
    async fn the_offer_deadline_does_not_cut_a_slow_camera_short() {
        // The offer is there at once; the camera takes longer than the offer deadline.
        let camera = ProbeCamera::new(false, Duration::from_millis(600));
        let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera.clone(), Arc::new(NoStun));
        d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
        d.offer_timeout = Duration::from_millis(200);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let tx = spawn_session(Arc::new(d), 1, SessionKind::Live, ev_tx);
        tx.send(SessionCommand::Offer("v=0\r\nnot really an offer\r\n".into())).unwrap();
        // It gets as far as reading the offer (which is garbage) instead of timing out.
        let (reason, error) = closed(&mut ev_rx).await;
        assert_eq!(reason, StopReason::Error);
        assert_eq!(error.as_deref(), Some("the phone's offer could not be read"), "fixed text, no SDP fragments");
        assert_eq!(camera.released.load(Ordering::SeqCst), 1);

        // No offer at all still times out on the short deadline.
        let camera = ProbeCamera::new(false, Duration::ZERO);
        let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera, Arc::new(NoStun));
        d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
        d.offer_timeout = Duration::from_millis(200);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let _tx = spawn_session(Arc::new(d), 1, SessionKind::Live, ev_tx);
        assert_eq!(closed(&mut ev_rx).await, (StopReason::Timeout, Some("no offer from the phone".into())));
    }

    #[tokio::test]
    async fn the_camera_is_acquired_and_released_under_the_sessions_epoch() {
        let camera = ProbeCamera::new(false, Duration::ZERO);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let tx = spawn_session(deps(camera.clone(), Arc::new(NoStun)), 7, SessionKind::Live, ev_tx);
        tx.send(SessionCommand::Stop(StopReason::User)).unwrap();
        closed(&mut ev_rx).await;
        assert_eq!(*camera.epochs.lock().unwrap(), vec![("acquire", 7), ("release", 7)]);
    }

    #[tokio::test]
    async fn the_prepare_deadline_is_reported_before_waiting_for_the_acquire() {
        // The acquire sits in a "prompt" far longer than the prepare deadline and ignores
        // the cancel (a UAC prompt cannot be closed from outside).
        let camera = ProbeCamera::new(false, Duration::from_millis(700));
        let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera.clone(), Arc::new(NoStun));
        d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
        d.prepare_timeout = Duration::from_millis(100);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let started = Instant::now();
        let _tx = spawn_session(Arc::new(d), 1, SessionKind::Live, ev_tx);
        let error = Some("preparing the camera took too long".to_string());
        let first = tokio::time::timeout(Duration::from_secs(5), ev_rx.recv()).await.unwrap().unwrap();
        assert_eq!(first, SessionEvent::Releasing { reason: StopReason::Timeout, error: error.clone() });
        assert!(started.elapsed() < Duration::from_millis(500), "reported at the deadline, not after the acquire");
        assert_eq!(camera.released.load(Ordering::SeqCst), 0, "not released under a running acquire");
        assert_eq!(closed(&mut ev_rx).await, (StopReason::Timeout, error));
        assert_eq!(camera.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_stop_cancels_an_acquire_that_can_be_cancelled() {
        // An acquire that would take a minute (a polkit prompt left open) but gives up when
        // the session is over.
        let camera = ProbeCamera::with(false, Duration::from_secs(60), true);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let tx = spawn_session(deps(camera.clone(), Arc::new(NoStun)), 1, SessionKind::Live, ev_tx);
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(SessionCommand::Stop(StopReason::User)).unwrap();
        // `closed` allows 5 s: the stop did not wait out the acquire. No `Releasing` either —
        // the machine asked for this stop.
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ev_rx.recv()).await.unwrap().unwrap() {
                SessionEvent::Closed { reason, error } => {
                    assert_eq!((reason, error), (StopReason::User, None));
                    break;
                }
                other => assert!(!matches!(other, SessionEvent::Releasing { .. }), "{other:?}"),
            }
        }
        assert_eq!(camera.cancelled.load(Ordering::SeqCst), 1);
        assert_eq!((camera.acquired.load(Ordering::SeqCst), camera.released.load(Ordering::SeqCst)), (0, 1));
    }

    // --- file sessions: a second str0m instance is the phone -----------------------------
    //
    // Real UDP on loopback, real ICE/DTLS/SCTP, the real manager, state machine, session
    // task, receiver and store. Only the loop engine is a fake (no ffmpeg, no camera).

    use crate::remote::stream::file_receive::testing::FakeLoop;
    use crate::remote::stream::file_receive::PhoneVideoStore;
    use crate::remote::stream::machine::{FilePhase, Phase};
    use crate::remote::stream::manager::{ChannelDecisions, DecisionEvent, LoopDeps, StreamManager};
    use crate::remote::stream::signal::{OutboundFrame, StreamMode, StreamSignal};
    use serde_json::{json, Value};

    /// What the phone does once its data channel is open.
    #[derive(Clone)]
    struct PhoneScript {
        /// The `meta` message (normally equal to the request's `file`).
        meta: Value,
        data: Vec<u8>,
        chunk: usize,
        /// Stop sending after this many bytes and wait for [`PhoneCtl`] (or the end).
        pause_at: Option<usize>,
    }

    enum PhoneCtl {
        /// Like `RTCPeerConnection.close()`.
        Close,
    }

    #[derive(Debug, Default)]
    struct PhoneReport {
        connected: bool,
        channel_opened: bool,
        bytes_sent: usize,
        /// Every text message the desktop sent on the data channel, in order.
        replies: Vec<Value>,
    }

    impl PhoneReport {
        fn kinds(&self) -> Vec<&str> {
            self.replies.iter().filter_map(|r| r["t"].as_str()).collect()
        }
    }

    struct FilePhone {
        offer: String,
        answer_tx: std::sync::mpsc::Sender<String>,
        candidates: std::sync::mpsc::Receiver<String>,
        ctl: std::sync::mpsc::Sender<PhoneCtl>,
        join: std::thread::JoinHandle<PhoneReport>,
    }

    impl FilePhone {
        /// `pc.createDataChannel("camlooper-file", {ordered: true})` and nothing else.
        fn start(script: PhoneScript) -> FilePhone {
            use str0m::change::SdpAnswer;
            use str0m::{Candidate, Rtc, RtcConfig};
            let mut rtc: Rtc = RtcConfig::new()
                .set_crypto_provider(Arc::new(str0m_purecrypto::default_provider()))
                .clear_codecs()
                .set_sctp_max_buffered_amount(1024 * 1024)
                .build(Instant::now());
            let mut change = rtc.sdp_api();
            let cid = change.add_channel(file_receive::DATA_CHANNEL_LABEL.to_string());
            let (offer, pending) = change.apply().expect("offer");
            let offer = offer.to_sdp_string();
            assert!(offer.contains("m=application") && !offer.contains("m=video"), "{offer}");
            let (answer_tx, answer_rx) = std::sync::mpsc::channel::<String>();
            let (cand_tx, cand_rx) = std::sync::mpsc::channel::<String>();
            let (ctl_tx, ctl_rx) = std::sync::mpsc::channel::<PhoneCtl>();
            let join = std::thread::spawn(move || {
                let Ok(answer) = answer_rx.recv_timeout(Duration::from_secs(20)) else { return PhoneReport::default() };
                let answer = SdpAnswer::from_sdp_string(&answer).expect("parse answer");
                rtc.sdp_api().accept_answer(pending, answer).expect("accept answer");
                let sock = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
                let local = sock.local_addr().unwrap();
                let c = Candidate::host(local, "udp").unwrap();
                let _ = cand_tx.send(c.to_sdp_string());
                let _ = cand_tx.send(String::new());
                rtc.add_local_candidate(c);
                phone_loop(rtc, cid, sock, local, script, ctl_rx)
            });
            FilePhone { offer, answer_tx, candidates: cand_rx, ctl: ctl_tx, join }
        }

        async fn finish(self) -> PhoneReport {
            tokio::task::spawn_blocking(move || self.join.join().expect("phone thread")).await.unwrap()
        }
    }

    fn phone_loop(
        mut rtc: str0m::Rtc,
        cid: str0m::channel::ChannelId,
        sock: std::net::UdpSocket,
        local: SocketAddr,
        script: PhoneScript,
        ctl: std::sync::mpsc::Receiver<PhoneCtl>,
    ) -> PhoneReport {
        use str0m::net::{Protocol, Receive};
        use str0m::{Event, IceConnectionState, Input, Output};
        #[derive(PartialEq)]
        enum Stage {
            Meta,
            Data,
            End,
            Wait,
        }
        sock.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        let mut report = PhoneReport::default();
        let mut stage = Stage::Meta;
        let deadline = Instant::now() + Duration::from_secs(40);
        // Once the desktop said its last word (or went away), hang on briefly and leave.
        let mut leave_at: Option<Instant> = None;
        let mut buf = vec![0u8; 2048];
        loop {
            let now = Instant::now();
            if now >= deadline || leave_at.is_some_and(|t| now >= t) || !rtc.is_alive() {
                break;
            }
            loop {
                match rtc.poll_output() {
                    Ok(Output::Timeout(_)) => break,
                    Ok(Output::Transmit(t)) => {
                        let _ = sock.send_to(&t.contents, t.destination);
                    }
                    Ok(Output::Event(e)) => match e {
                        Event::IceConnectionStateChange(IceConnectionState::Connected | IceConnectionState::Completed) => {
                            report.connected = true;
                        }
                        Event::ChannelOpen(id, _) if id == cid => report.channel_opened = true,
                        Event::ChannelData(d) if d.id == cid && !d.binary => {
                            let v: Value = serde_json::from_slice(&d.data).expect("the desktop sends JSON text");
                            if matches!(v["t"].as_str(), Some("looping" | "error")) {
                                leave_at = Some(Instant::now() + Duration::from_millis(200));
                            }
                            report.replies.push(v);
                        }
                        Event::ChannelClose(_) | Event::Closed => {
                            leave_at.get_or_insert(Instant::now() + Duration::from_millis(100));
                        }
                        _ => {}
                    },
                    Err(_) => return report,
                }
            }
            if let Ok(PhoneCtl::Close) = ctl.try_recv() {
                if rtc.close().is_ok() {
                    // Drain the close (DTLS close_notify) onto the wire.
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
                break;
            }
            // Send as much as the channel takes (its buffer is the flow control).
            let mut wrote = false;
            if report.channel_opened && stage != Stage::Wait {
                let Some(mut ch) = rtc.channel(cid) else { break };
                if stage == Stage::Meta && ch.write(false, script.meta.to_string().as_bytes()).unwrap_or(false) {
                    stage = Stage::Data;
                    wrote = true;
                }
                while stage == Stage::Data {
                    let limit = script.pause_at.unwrap_or(script.data.len()).min(script.data.len());
                    if report.bytes_sent >= limit {
                        stage = if report.bytes_sent >= script.data.len() { Stage::End } else { Stage::Wait };
                        break;
                    }
                    let end = (report.bytes_sent + script.chunk).min(limit);
                    if !ch.write(true, &script.data[report.bytes_sent..end]).unwrap_or(false) {
                        break;
                    }
                    report.bytes_sent = end;
                    wrote = true;
                }
                if stage == Stage::End && ch.write(false, br#"{"t":"end"}"#).unwrap_or(false) {
                    stage = Stage::Wait;
                    wrote = true;
                }
            }
            if wrote {
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
        rtc.disconnect();
        report
    }

    /// The desktop side of a file session test.
    struct Desk {
        manager: StreamManager,
        out: mpsc::UnboundedReceiver<OutboundFrame>,
        decisions: mpsc::UnboundedReceiver<DecisionEvent>,
        store: PhoneVideoStore,
        player: Arc<FakeLoop>,
        camera: Arc<ProbeCamera>,
        _app_data: tempfile::TempDir,
    }

    impl Desk {
        fn new(player: FakeLoop, idle_timeout: Duration) -> Desk {
            let app_data = tempfile::tempdir().unwrap();
            let store = PhoneVideoStore::in_app_data(app_data.path());
            let player = Arc::new(player);
            let camera = ProbeCamera::new(false, Duration::ZERO);
            // An ffmpeg that does not exist: a file session must never start one.
            let mut d = SessionDeps::new(PathBuf::from("/nonexistent/ffmpeg"), camera.clone(), Arc::new(NoStun));
            d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
            let mut files = FileDeps::new(store.clone(), player.clone());
            files.idle_timeout = idle_timeout;
            d.files = Some(files);
            let (dtx, decisions) = mpsc::unbounded_channel();
            let (manager, out) = StreamManager::spawn_with_loop(
                Arc::new(Arc::new(d)),
                Arc::new(ChannelDecisions(dtx)),
                LoopDeps { player: player.clone(), store: Some(store.clone()) },
            );
            Desk { manager, out, decisions, store, player, camera, _app_data: app_data }
        }

        /// Request (as `file`), accept, offer/answer, candidates.
        async fn connect(&mut self, id: &str, file: Value, phone: &FilePhone) {
            self.manager.on_relay_signal(json!({"kind":"stream_request","sessionId":id,"deviceLabel":"Sim","mode":"file","file":file}));
            let Some(DecisionEvent::Request(req)) = self.decisions.recv().await else { panic!("no decision request") };
            assert_eq!(req.mode, StreamMode::File);
            self.manager.accept(&req.session_id).await.unwrap();
            self.manager.on_relay_signal(json!({"kind":"offer","sessionId":id,"sdp":phone.offer}));
            let answer = loop {
                if let OutboundFrame::Signal(StreamSignal::Answer { sdp, .. }) = self.next_out().await {
                    break sdp;
                }
            };
            assert!(answer.contains("m=application") && !answer.contains("m=video"), "{answer}");
            phone.answer_tx.send(answer).unwrap();
            for _ in 0..2 {
                let c = phone.candidates.recv_timeout(Duration::from_secs(10)).expect("phone candidate");
                self.manager.on_relay_signal(json!({"kind":"candidate","sessionId":id,"candidate":c,"sdpMid":"0","sdpMLineIndex":0}));
            }
        }

        async fn next_out(&mut self) -> OutboundFrame {
            tokio::time::timeout(Duration::from_secs(20), self.out.recv()).await.expect("a frame for the phone in time").unwrap()
        }

        /// Wait until the session is over (idle or error).
        async fn ended(&self) -> crate::remote::stream::machine::StreamStatus {
            let mut status = self.manager.subscribe();
            let st = tokio::time::timeout(Duration::from_secs(30), status.wait_for(|s| s.session_id.is_none() && s.state.is_free()))
                .await
                .expect("the session ends in time")
                .unwrap()
                .clone();
            st
        }

        async fn wait_received(&self, at_least: u64) {
            let mut status = self.manager.subscribe();
            tokio::time::timeout(
                Duration::from_secs(20),
                status.wait_for(|s| s.file.as_ref().is_some_and(|f| f.phase == FilePhase::Receiving && f.received >= at_least)),
            )
            .await
            .expect("bytes arrive in time")
            .unwrap();
        }

        fn files(&self) -> Vec<String> {
            let Ok(dir) = std::fs::read_dir(self.store.dir()) else { return vec![] };
            let mut v: Vec<String> = dir.map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            v.sort();
            v
        }

        fn no_camera_was_claimed(&self) {
            assert!(self.camera.epochs.lock().unwrap().is_empty(), "a file session never touches the camera handoff");
        }
    }

    /// Bytes that do not repeat with the chunk size, so a lost, doubled or reordered chunk
    /// changes the file.
    fn video_bytes(len: usize) -> Vec<u8> {
        let mut x: u32 = 0x1234_5678;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn script(name: &str, announced: usize, data: Vec<u8>) -> (Value, PhoneScript) {
        let file = json!({"name":name,"size":announced,"mime":"video/mp4"});
        let meta = json!({"t":"meta","name":name,"size":announced,"mime":"video/mp4"});
        (file, PhoneScript { meta, data, chunk: 65536, pause_at: None })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_phone_video_arrives_byte_identical_and_is_looped() {
        let data = video_bytes(3 * 1024 * 1024 + 12_345);
        let (file, script) = script("My Holiday.mp4", data.len(), data.clone());
        let mut desk = Desk::new(FakeLoop::default(), file_receive::IDLE_TIMEOUT);
        // A phone video from earlier, and a desktop video that is playing.
        std::fs::create_dir_all(desk.store.dir()).unwrap();
        let older = desk.store.dir().join("phone-00000000000000aa.mp4");
        std::fs::write(&older, b"older").unwrap();
        *desk.player.state.lock().unwrap() =
            file_receive::LoopState { playing: true, name: Some("desk.mp4".into()), path: Some("/videos/desk.mp4".into()) };

        let phone = FilePhone::start(script);
        desk.connect("file-1", file, &phone).await;

        // The good ending for the phone, over the relay: `done`, then the loop's state.
        let (mut stop, mut loop_status) = (None, None);
        while loop_status.is_none() {
            match desk.next_out().await {
                OutboundFrame::StreamStop { reason, .. } => stop = Some(reason),
                OutboundFrame::Signal(sig @ StreamSignal::LoopStatus { .. }) => loop_status = Some(sig),
                _ => {}
            }
        }
        assert_eq!(stop, Some(StopReason::Done));
        assert_eq!(
            loop_status,
            Some(StreamSignal::LoopStatus {
                session_id: "file-1".into(),
                playing: true,
                name: Some("My Holiday.mp4".into()),
                from_phone: true,
                error: None,
            })
        );

        // The UI: idle, no error, and the outcome still readable.
        let st = desk.ended().await;
        assert_eq!((st.state, st.last_error.clone(), st.last_error_code.clone(), st.ended_by_phone), (Phase::Idle, None, None, false));
        let shown = st.file.clone().expect("file status");
        assert_eq!((st.mode, shown.phase, shown.received, shown.size), (StreamMode::File, FilePhase::Looping, data.len() as u64, data.len() as u64));
        assert_eq!(shown.name, "My Holiday.mp4");

        // On disk: exactly one file, ours, with a name the phone had no say in — and the
        // bytes the phone sent.
        let files = desk.files();
        assert_eq!(files.len(), 1, "{files:?}");
        let name = &files[0];
        assert!(name.starts_with("phone-") && name.ends_with(".mp4") && name.len() == "phone-.mp4".len() + 16, "{name}");
        assert!(!older.exists(), "the previous phone video is gone once the new one is loaded");
        let saved = std::fs::read(desk.store.dir().join(name)).unwrap();
        assert_eq!(saved.len(), data.len());
        assert!(saved == data, "the saved file differs from what the phone sent");
        // The loop engine got that file, under the display name, with everything in it.
        assert_eq!(desk.player.calls(), vec!["play_file My Holiday.mp4"]);
        assert!(desk.player.played_bytes.lock().unwrap().as_deref() == Some(&data[..]), "handed over before it was complete");
        let loaded = desk.player.state.lock().unwrap().clone();
        assert_eq!(loaded.path.as_deref(), Some(desk.store.dir().join(name).as_path()));
        desk.no_camera_was_claimed();

        // What the phone saw on the data channel.
        let report = phone.finish().await;
        assert!(report.connected && report.channel_opened);
        assert_eq!(report.bytes_sent, data.len());
        let kinds = report.kinds();
        let tail: Vec<&str> = kinds.iter().copied().skip_while(|k| *k == "progress").collect();
        assert_eq!(tail, vec!["saved", "processing", "looping"], "{kinds:?}");
        let progress: Vec<u64> = report.replies.iter().filter(|r| r["t"] == "progress").map(|r| r["received"].as_u64().unwrap()).collect();
        assert!(!progress.is_empty() && progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
        assert_eq!(progress.last(), Some(&(data.len() as u64)), "a final progress with everything");
        assert_eq!(report.replies.last().unwrap(), &json!({"t":"looping","name":"My Holiday.mp4"}));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn more_bytes_than_announced_fail_the_transfer_and_leave_nothing_behind() {
        // Announces 100 000 bytes, sends 300 000.
        let (file, script) = script("clip.mp4", 100_000, video_bytes(300_000));
        let mut desk = Desk::new(FakeLoop::default(), file_receive::IDLE_TIMEOUT);
        let phone = FilePhone::start(script);
        desk.connect("big-1", file, &phone).await;

        let st = desk.ended().await;
        assert_eq!((st.state, st.last_error_code.as_deref()), (Phase::Error, Some("error")));
        assert_eq!(st.last_error.as_deref(), Some("The video is larger than announced."));
        let shown = st.file.expect("file status");
        assert_eq!((shown.phase, shown.error.as_deref()), (FilePhase::Failed, Some("The video is larger than announced.")));
        assert!(shown.received <= 100_000, "{shown:?}");
        assert!(desk.files().is_empty(), "the partial file is deleted: {:?}", desk.files());
        assert!(desk.player.calls().is_empty(), "nothing was loaded");
        // The phone is told on both paths.
        let mut stop = None;
        while stop.is_none() {
            if let OutboundFrame::StreamStop { reason, .. } = desk.next_out().await {
                stop = Some(reason);
            }
        }
        assert_eq!(stop, Some(StopReason::Error));
        let report = phone.finish().await;
        assert_eq!(report.replies.last(), Some(&json!({"t":"error","message":"The video is larger than announced."})), "{:?}", report.kinds());
        assert!(!report.kinds().contains(&"saved"));
        desk.no_camera_was_claimed();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_meta_that_differs_from_the_accepted_file_is_refused_before_anything_is_written() {
        // The user accepted "clip.mp4" of 50 000 bytes; the channel announces another file.
        let (file, mut script) = script("clip.mp4", 50_000, video_bytes(50_000));
        script.meta = json!({"t":"meta","name":"other.mp4","size":50_000,"mime":"video/mp4"});
        let mut desk = Desk::new(FakeLoop::default(), file_receive::IDLE_TIMEOUT);
        let phone = FilePhone::start(script);
        desk.connect("meta-1", file, &phone).await;

        let st = desk.ended().await;
        assert_eq!(st.state, Phase::Error);
        let shown = st.file.expect("file status");
        assert_eq!((shown.phase, shown.received), (FilePhase::Failed, 0));
        assert_eq!(shown.error.as_deref(), Some("The video is not the one that was accepted."));
        assert!(desk.files().is_empty(), "no file was ever created: {:?}", desk.files());
        assert!(desk.player.calls().is_empty());
        let report = phone.finish().await;
        assert_eq!(report.kinds(), vec!["error"], "{:?}", report.replies);
    }

    /// Half the file is there, then the transfer is abandoned — by the desktop's Cancel, by
    /// the phone hanging up, or by the phone going silent.
    async fn abort_midway(how: &str) {
        let data = video_bytes(2 * 1024 * 1024);
        let (file, mut script) = script("clip.mp4", data.len(), data);
        script.pause_at = Some(1024 * 1024);
        let idle = if how == "stall" { Duration::from_millis(1500) } else { file_receive::IDLE_TIMEOUT };
        let mut desk = Desk::new(FakeLoop::default(), idle);
        let phone = FilePhone::start(script);
        desk.connect("abort-1", file, &phone).await;

        desk.wait_received(1024 * 1024).await;
        let files = desk.files();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].ends_with(".mp4.part"), "written under a .part name: {files:?}");
        assert_eq!(std::fs::metadata(desk.store.dir().join(&files[0])).unwrap().len(), 1024 * 1024);

        match how {
            "desktop" => desk.manager.stop(StopReason::User).await,
            "phone" => phone.ctl.send(PhoneCtl::Close).unwrap(),
            _ => {}
        }
        let st = desk.ended().await;
        assert!(desk.files().is_empty(), "the partial file is deleted: {:?}", desk.files());
        assert!(desk.player.calls().is_empty(), "nothing was loaded");
        match how {
            // A cancel is not an error on either side.
            "desktop" => assert_eq!((st.state, st.last_error, st.ended_by_phone, st.file), (Phase::Idle, None, false, None)),
            "phone" => assert_eq!((st.state, st.last_error, st.ended_by_phone, st.file), (Phase::Idle, None, true, None)),
            _ => {
                assert_eq!((st.state, st.last_error.as_deref()), (Phase::Error, Some("The transfer stalled.")));
                assert_eq!(st.file.map(|f| (f.phase, f.received)), Some((FilePhase::Failed, 1024 * 1024)));
            }
        }
        if how != "phone" {
            let _ = phone.ctl.send(PhoneCtl::Close);
        }
        let report = phone.finish().await;
        assert_eq!(report.bytes_sent, 1024 * 1024);
        if how == "stall" {
            assert_eq!(report.replies.last(), Some(&json!({"t":"error","message":"The transfer stalled."})));
        }
        desk.no_camera_was_claimed();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelling_on_the_desktop_midway_deletes_the_partial_file() {
        abort_midway("desktop").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_phone_hanging_up_midway_deletes_the_partial_file() {
        abort_midway("phone").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_transfer_that_stalls_fails_and_deletes_the_partial_file() {
        abort_midway("stall").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_video_that_cannot_be_played_is_deleted_and_reported() {
        let data = video_bytes(200_000);
        let (file, script) = script("broken.mp4", data.len(), data);
        let mut desk = Desk::new(FakeLoop { fail_play: true, ..FakeLoop::default() }, file_receive::IDLE_TIMEOUT);
        let phone = FilePhone::start(script);
        desk.connect("bad-1", file, &phone).await;

        let st = desk.ended().await;
        assert_eq!((st.state, st.last_error.as_deref()), (Phase::Error, Some("The video could not be played on the computer.")));
        assert_eq!(st.file.map(|f| f.phase), Some(FilePhase::Failed));
        assert!(desk.files().is_empty(), "a video that is not loaded is not kept: {:?}", desk.files());
        let report = phone.finish().await;
        let kinds = report.kinds();
        assert_eq!(kinds[kinds.len() - 3..], ["saved", "processing", "error"], "{kinds:?}");
        // The loop's state reaches the phone after the failure too.
        let mut loop_status = None;
        while loop_status.is_none() {
            if let OutboundFrame::Signal(StreamSignal::LoopStatus { playing, from_phone, .. }) = desk.next_out().await {
                loop_status = Some((playing, from_phone));
            }
        }
        assert_eq!(loop_status, Some((false, false)));
    }

    #[tokio::test]
    async fn a_file_session_without_a_store_ends_with_an_error() {
        // The headless CLI has no loop engine.
        let camera = ProbeCamera::new(false, Duration::ZERO);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let offer = FileOffer { name: "clip.mp4".into(), size: 10, mime: "video/mp4".into() };
        let _tx = spawn_session(deps(camera.clone(), Arc::new(NoStun)), 1, SessionKind::File(offer), ev_tx);
        let (reason, error) = closed(&mut ev_rx).await;
        assert_eq!(reason, StopReason::Error);
        assert!(error.is_some());
        assert!(camera.epochs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_file_session_refuses_an_offer_that_brings_media() {
        let tmp = tempfile::tempdir().unwrap();
        let camera = ProbeCamera::new(false, Duration::ZERO);
        let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera, Arc::new(NoStun));
        d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
        d.files = Some(FileDeps::new(PhoneVideoStore::in_app_data(tmp.path()), Arc::new(FakeLoop::default())));
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let offer = FileOffer { name: "clip.mp4".into(), size: 10, mime: "video/mp4".into() };
        let tx = spawn_session(Arc::new(d), 1, SessionKind::File(offer), ev_tx);
        // A camera offer in a session that was accepted as a file transfer.
        let video_offer = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\nc=IN IP4 0.0.0.0\r\na=ice-ufrag:abcd\r\na=ice-pwd:abcdefghijklmnopqrstuvwx\r\na=fingerprint:sha-256 7B:8B:F0:65:5F:78:E2:51:3B:AC:6F:F3:3F:46:1B:35:DC:B8:5F:64:1A:24:C2:43:F0:A1:58:D0:A1:2C:19:08\r\na=setup:actpass\r\na=mid:0\r\na=sendonly\r\na=rtcp-mux\r\na=rtpmap:96 VP8/90000\r\n";
        tx.send(SessionCommand::Offer(video_offer.into())).unwrap();
        let (reason, error) = closed(&mut ev_rx).await;
        assert_eq!((reason, error.as_deref()), (StopReason::Error, Some("the phone's offer does not match the request")));
        assert!(!tmp.path().join("phone-videos").exists(), "nothing was written");
    }
}
