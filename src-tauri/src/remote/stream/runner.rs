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
//!
//! A **record session** ([`SessionKind::Record`]) is a live session in every respect — same
//! handoff, same connection, same decoder, the phone's picture on the camera — plus a
//! [`Recorder`]: every frame written to the decoder is also teed into a take file
//! ([`super::record`]). When the take ends (the phone's or the desktop's Stop, the 60 s cap)
//! the decoder is stopped, the take is remuxed, and the same loop engine a file session
//! uses plays it; the session then ends with [`StopReason::Done`] and the camera stays with
//! the loop instead of being handed back.
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

use super::file_receive::{
    self, reply, FileDeps, PartFile, PhoneContainer, PlayError, PlayRequest, Played, Sniff, TextStep, Transfer,
};
use super::media::{FrameOut, StreamWriter};
use super::record::{self, PreRoll, PreRollStep, Take, TakeFault, TakeFrame, TakeStep};
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
#[derive(Clone, Debug)]
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
    /// A record session's take is about to be played by the loop engine on the camera
    /// `epoch` holds: stop refusing other writers. Nothing is undone yet — `release` still
    /// does that (and brings back what was there before) if the loop does not start.
    fn yield_to_loop(&self, _epoch: u64) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    /// The take loops on the camera now: forget what `acquire` changed, so that the
    /// `release` that follows leaves the camera on and does not resume what played before.
    fn keep_for_loop(&self, _epoch: u64) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
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
    /// What a meeting session sends to the phone: this computer's sound and a window.
    /// `None` (no capture on this platform yet): meeting requests are refused.
    pub meeting: Option<Arc<dyn super::meeting::MeetingSource>>,
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
            meeting: None,
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
    /// [`SessionKind::Live`], with the stream teed into a take that is looped once it ends
    /// ([`super::record`]).
    Record,
    /// The desktop sends this computer's sound and a window to the phone
    /// ([`super::meeting`]). The camera is not involved.
    Meeting,
}

/// Where a record session's take is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStage {
    /// Frames are being teed.
    Recording,
    /// The take ended; it is being remuxed and loaded.
    Processing,
    /// Looping on the virtual camera.
    Looping,
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
    /// Record sessions: end the take now. With enough of it the session goes on to loop it
    /// and ends as [`StopReason::Done`]; with too little it ends as
    /// [`StopReason::TooShort`]. Anywhere else this is [`SessionCommand::Stop`].
    Finish(StopReason),
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
    /// Record session: the take started (`Recording`, 0 ms), grew (once a second), ended
    /// and is being made into a loop (`Processing`), loops (`Looping`).
    Record { stage: RecordStage, elapsed_ms: u64 },
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
            spawn_guarded(camera, epoch, events, async move { run_session(deps, epoch, rx, &session_events, None).await });
        }
        // A live session with a tee. It needs what a file session needs: a place for the
        // video and something to loop it.
        SessionKind::Record => match deps.files.clone() {
            Some(files) => {
                let camera = deps.camera.clone();
                spawn_guarded(camera, epoch, events, async move {
                    let (outcome, loaded) = run_session_recording(deps, epoch, rx, &session_events, &files).await;
                    // As after a transfer: whatever is not the loaded video goes.
                    files.sweep_unused_keeping(loaded.as_deref()).await;
                    outcome
                });
            }
            None => spawn_reported(events, async { (StopReason::Error, Some("this build cannot record a loop from the phone".into())) }),
        },
        // Neither does a meeting session touch the camera: the loop keeps playing.
        SessionKind::Meeting => match deps.meeting.clone() {
            Some(source) => {
                spawn_reported_as("meeting share", events, async move {
                    super::meeting::run_meeting_session(deps, source, rx, &session_events).await
                })
            }
            None => spawn_reported_as("meeting share", events, async { (StopReason::Error, Some("this computer cannot share a meeting with the phone".into())) }),
        },
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
    spawn_reported_as("phone video", events, session)
}

/// [`spawn_reported`] with the name the session goes by in the log.
fn spawn_reported_as(
    what: &'static str,
    events: mpsc::UnboundedSender<SessionEvent>,
    session: impl std::future::Future<Output = Outcome> + Send + 'static,
) {
    tokio::spawn(async move {
        let run = AssertUnwindSafe(session).catch_unwind().await;
        let (reason, error) = run.unwrap_or_else(|_| (StopReason::Error, Some(format!("internal error in the {what} session"))));
        match &error {
            Some(e) => eprintln!("{what}: ended ({reason}): {e}"),
            None => eprintln!("{what}: ended ({reason})"),
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

pub(super) type Outcome = (StopReason, Option<String>);
type Prepared = (CameraTarget, rtc::LocalTransport);

/// [`run_session`] for a record session. Besides the outcome: the path the loop engine says
/// it loaded, if the take loops.
async fn run_session_recording(
    deps: Arc<SessionDeps>,
    epoch: u64,
    cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
    files: &FileDeps,
) -> (Outcome, Option<PathBuf>) {
    let mut recorder = Recorder::new(files.clone());
    let outcome = run_session(deps, epoch, cmd_rx, events, Some(&mut recorder)).await;
    (outcome, recorder.loaded.take())
}

async fn run_session(
    deps: Arc<SessionDeps>,
    epoch: u64,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
    mut recorder: Option<&mut Recorder>,
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
    media.tee = recorder.is_some();
    // Commands that arrived while a frame was being written (see `MediaLoop::on_frame`).
    let mut pending: VecDeque<SessionCommand> = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let end = loop {
        tokio::select! {
            ev = rtc_rx.recv() => match ev {
                Some(RtcEvent::IceConnected) => {
                    eprintln!("phone stream: connected to the phone");
                    let _ = events.send(SessionEvent::IceConnected);
                }
                Some(RtcEvent::Frame(f)) => match media.on_frame(f, events, &mut cmd_rx, &mut pending).await {
                    Ok(None) => {
                        if let (Some(rec), Some(frame)) = (recorder.as_deref_mut(), media.written.take()) {
                            match rec.on_frame(frame, events).await {
                                TeeStep::Continue => {}
                                TeeStep::WantKeyframe => media.keyframe(),
                                // The cap: the take is finalized whatever the phone does.
                                TeeStep::Full => break End::Finish(StopReason::Done),
                            }
                        }
                    }
                    Ok(Some(interrupted)) => break interrupted,
                    Err(e) => break End::Now((StopReason::Error, Some(format!("{e:#}")))),
                },
                // The phone closed the connection: a normal ending. Its `stream_stop` usually
                // follows over the relay within a moment and carries the reason. With a
                // take that is long enough, that is the phone's Stop: no need to wait.
                Some(RtcEvent::Closed(RtcClose::PeerClosed)) => {
                    if recorder.as_deref().is_some_and(|r| r.take.has_minimum()) {
                        break End::Finish(StopReason::PeerStopped);
                    }
                    let reason = phone_stop_reason(&mut pending, &mut cmd_rx).await;
                    break if recorder.as_deref().is_some_and(|r| r.take.started()) { End::Finish(reason) } else { End::Now((reason, None)) };
                }
                Some(RtcEvent::Closed(RtcClose::Stopped)) | None => break End::Now((StopReason::Error, Some("receiver stopped".into()))),
                Some(RtcEvent::Closed(RtcClose::Failed { reason, detail })) => break End::Now((reason, Some(detail))),
                // A data channel next to the video: not part of a live session.
                Some(RtcEvent::ChannelOpen | RtcEvent::ChannelText(_) | RtcEvent::ChannelBinary(_) | RtcEvent::ChannelClosed) => {}
            },
            cmd = next_command(&mut pending, &mut cmd_rx) => match cmd {
                Some(SessionCommand::RemoteCandidate(c)) => {
                    let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                }
                Some(SessionCommand::Offer(_)) => log::debug!("phone stream: ignoring a second offer (no renegotiation)"),
                Some(SessionCommand::Stop(r)) => break End::Now((r, None)),
                Some(SessionCommand::Finish(r)) => break End::Finish(r),
                None => break End::Now((StopReason::Error, Some("session dropped".into()))),
            },
            _ = tick.tick() => {
                if let Err(e) = media.check_decoder().await {
                    break End::Now((StopReason::Error, Some(format!("{e:#}"))));
                }
                if media.live {
                    let _ = events.send(SessionEvent::Stats(media.stats()));
                }
                // The cap by the clock as well: a stream that stalled near the end never
                // delivers the frame that would show the take is full.
                if let Some(rec) = recorder.as_deref_mut() {
                    if rec.tick(events) {
                        break End::Finish(StopReason::Done);
                    }
                }
            }
        }
    };
    let outcome = match (end, recorder) {
        (End::Now(outcome), _) => outcome,
        // Not a record session: finishing is stopping.
        (End::Finish(reason), None) => (reason, None),
        (End::Finish(reason), Some(rec)) => {
            // The decoder lets go of the camera first: the loop is about to write to it.
            media.shutdown().await;
            let link = Link { rtc_tx: &rtc_tx, rtc_rx: &mut rtc_rx, cmd_rx: &mut cmd_rx, pending: &mut pending };
            rec.finish(reason, &deps, epoch, link, events).await
        }
    };
    let _ = rtc_tx.send(RtcCommand::Stop);
    media.shutdown().await;
    outcome
}

/// How the media loop of a session ended.
enum End {
    /// With this outcome.
    Now(Outcome),
    /// Record: the take is to be finished (the phone's or the desktop's Stop, the cap, the
    /// phone hanging up). What comes of it is the outcome.
    Finish(StopReason),
}

/// The connection and the command queue, for the part of a record session that comes after
/// the media loop.
struct Link<'a> {
    rtc_tx: &'a mpsc::UnboundedSender<RtcCommand>,
    rtc_rx: &'a mut mpsc::Receiver<RtcEvent>,
    cmd_rx: &'a mut mpsc::UnboundedReceiver<SessionCommand>,
    pending: &'a mut VecDeque<SessionCommand>,
}

// ---------------------------------------------------------------------------------------
// Record session
// ---------------------------------------------------------------------------------------

/// What the tee wants after a frame.
enum TeeStep {
    Continue,
    WantKeyframe,
    /// The take is at its cap.
    Full,
}

/// The recording side of a record session: the pre-roll, the tee file, and — once the take
/// ends — the remux and the hand-over to the loop engine.
struct Recorder {
    files: FileDeps,
    /// Set when the stream goes live; gone once the take has started.
    pre_roll: Option<PreRoll>,
    take: Take,
    part: Option<PartFile>,
    /// When the first frame was teed.
    started: Option<Instant>,
    /// The tee file could not be created or written: the take is lost, the live picture
    /// goes on until the session is stopped.
    broken: bool,
    /// The path the loop engine loaded, once the take loops.
    loaded: Option<PathBuf>,
}

impl Recorder {
    fn new(files: FileDeps) -> Self {
        Self { files, pre_roll: None, take: Take::new(), part: None, started: None, broken: false, loaded: None }
    }

    /// A frame reached the decoder.
    async fn on_frame(&mut self, frame: TakeFrame, events: &mpsc::UnboundedSender<SessionEvent>) -> TeeStep {
        if self.broken {
            return TeeStep::Continue;
        }
        if self.started.is_none() {
            let pre_roll = self.pre_roll.get_or_insert_with(|| PreRoll::new(frame.arrived));
            let first = match pre_roll.push(frame) {
                PreRollStep::Wait => return TeeStep::Continue,
                PreRollStep::WantKeyframe => return TeeStep::WantKeyframe,
                PreRollStep::Start(first) => first,
            };
            self.pre_roll = None;
            match record::create_take_file(&self.files.store).await {
                Ok(part) => self.part = Some(part),
                Err(e) => {
                    eprintln!("phone recording: could not create the take file: {e}");
                    self.broken = true;
                    return TeeStep::Continue;
                }
            }
            self.started = Some(Instant::now());
            eprintln!("phone recording: the take has started");
            let _ = events.send(SessionEvent::Record { stage: RecordStage::Recording, elapsed_ms: 0 });
            for f in &first {
                if let TeeStep::Full = self.tee(f, events).await {
                    return TeeStep::Full;
                }
            }
            return TeeStep::Continue;
        }
        self.tee(&frame, events).await
    }

    async fn tee(&mut self, frame: &TakeFrame, events: &mpsc::UnboundedSender<SessionEvent>) -> TeeStep {
        match self.take.push(frame) {
            TakeStep::Skip => TeeStep::Continue,
            TakeStep::Full => TeeStep::Full,
            // The picture grew just after the take started (the encoder's ramp outlasted
            // the pre-roll): the take is the larger picture. The prelude is deleted, not
            // muxed, and the take's clock — the cap, what the phone is shown — starts again.
            TakeStep::Restart(bytes) => {
                self.part = None; // deletes the prelude
                let (w, h) = self.take.dims().unwrap_or((0, 0));
                eprintln!("phone recording: the picture grew to {w}x{h}; the take starts again at that size");
                let created = match record::create_take_file(&self.files.store).await {
                    Ok(mut part) => part.write(&bytes).await.map(|()| part),
                    Err(e) => Err(e),
                };
                match created {
                    Ok(part) => self.part = Some(part),
                    Err(e) => {
                        eprintln!("phone recording: could not start the take file again: {e}");
                        self.broken = true;
                        return TeeStep::Continue;
                    }
                }
                self.started = Some(Instant::now());
                let _ = events.send(SessionEvent::Record { stage: RecordStage::Recording, elapsed_ms: 0 });
                TeeStep::Continue
            }
            TakeStep::Write(bytes) => {
                let written = match self.part.as_mut() {
                    Some(part) => part.write(&bytes).await,
                    None => Err(std::io::Error::other("no take file")),
                };
                if let Err(e) = written {
                    eprintln!("phone recording: could not write the take file: {e}");
                    self.broken = true;
                    self.part = None; // deletes it
                }
                TeeStep::Continue
            }
        }
    }

    /// Once a second: report the take's length. `true` when the cap has passed on the clock.
    fn tick(&mut self, events: &mpsc::UnboundedSender<SessionEvent>) -> bool {
        let Some(started) = self.started else { return false };
        if self.broken {
            return false;
        }
        let _ = events.send(SessionEvent::Record { stage: RecordStage::Recording, elapsed_ms: self.take.elapsed_ms() });
        started.elapsed() >= record::RECORD_CAP + Duration::from_secs(1)
    }

    /// The take is over: judge it, remux it, loop it. The connection is kept (and served)
    /// meanwhile, so that the phone does not see it fail before it hears how this ended.
    async fn finish(
        &mut self,
        reason: StopReason,
        deps: &Arc<SessionDeps>,
        epoch: u64,
        link: Link<'_>,
        events: &mpsc::UnboundedSender<SessionEvent>,
    ) -> Outcome {
        let Link { rtc_tx, rtc_rx, cmd_rx, pending } = link;
        let part = self.part.take();
        if !self.take.started() && !self.broken {
            // Stopped during the pre-roll: an ordinary stop of a live stream.
            return (if reason == StopReason::Done { StopReason::User } else { reason }, None);
        }
        let (summary, part) = match (self.take.verdict(), part) {
            (Ok(summary), Some(part)) => (summary, part),
            (Err(TakeFault::TooShort), _) if !self.broken => {
                eprintln!("phone recording: stopped after {} ms; too short to loop", self.take.elapsed_ms());
                return (StopReason::TooShort, Some(record::TOO_SHORT.into()));
            }
            (verdict, _) => {
                eprintln!("phone recording: the take cannot be looped ({verdict:?}, {} frames, {} ms)", self.take.frames(), self.take.elapsed_ms());
                return (StopReason::Incomplete, Some(record::INCOMPLETE.into()));
            }
        };
        eprintln!("phone recording: {} frames, {} ms; preparing the loop", summary.frames, summary.duration_ms);
        let _ = events.send(SessionEvent::Record { stage: RecordStage::Processing, elapsed_ms: summary.duration_ms });

        // --- remux, serving the connection; a stop (a cancel) drops it, which kills ffmpeg
        //     and deletes both files -----------------------------------------------------
        let recorded = {
            let remux = record::finalize(&deps.ffmpeg, &self.files.store, part, summary);
            tokio::pin!(remux);
            let mut connection_gone = false;
            loop {
                tokio::select! {
                    r = &mut remux => break r,
                    ev = rtc_rx.recv(), if !connection_gone => {
                        if matches!(ev, None | Some(RtcEvent::Closed(_))) {
                            connection_gone = true;
                        }
                    }
                    cmd = next_command(pending, cmd_rx) => match cmd {
                        Some(SessionCommand::Stop(r)) => return (r, None),
                        Some(SessionCommand::RemoteCandidate(c)) => {
                            let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                        }
                        // Already finishing.
                        Some(SessionCommand::Finish(_) | SessionCommand::Offer(_)) => {}
                        None => return (StopReason::Error, Some("session dropped".into())),
                    },
                }
            }
        };
        let recorded = match recorded {
            Ok(recorded) => recorded,
            Err(e) => {
                eprintln!("phone recording: could not remux the take: {e:#}");
                return (StopReason::Incomplete, Some(record::INCOMPLETE.into()));
            }
        };

        // --- the loop engine: the phone no longer owns the camera, the loop may start ----
        deps.camera.yield_to_loop(epoch).await;
        let request = (recorded.path.clone(), recorded.display_name.clone(), recorded.container);
        let played = play_received(&self.files, request, rtc_tx, rtc_rx, cmd_rx).await;
        // `recorded.hold` kept the file out of everybody's sweep until here; the session's
        // own sweep (the caller's) runs next and keeps it only if it is loaded.
        match played {
            PlayEnd::Played(Played::Looping { path }) => {
                // The camera stays as it is now: nothing of the handoff is undone.
                deps.camera.keep_for_loop(epoch).await;
                self.loaded = Some(path);
                let _ = events.send(SessionEvent::Record { stage: RecordStage::Looping, elapsed_ms: summary.duration_ms });
                (StopReason::Done, None)
            }
            PlayEnd::Played(Played::LoadedNotPlaying { path }) => {
                deps.camera.keep_for_loop(epoch).await;
                self.loaded = Some(path);
                (StopReason::Error, Some(NOT_STARTED.into()))
            }
            // Cancelled in time: nothing was loaded, the loop is as it was.
            PlayEnd::Stopped(reason) => (reason, None),
            PlayEnd::TimedOut => (StopReason::Timeout, Some(PLAY_TIMED_OUT.into())),
            PlayEnd::Failed(e) => {
                eprintln!("phone recording: could not play the take: {e:#}");
                (StopReason::Incomplete, Some(record::INCOMPLETE.into()))
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// File session
// ---------------------------------------------------------------------------------------

/// After the last message for the phone (`looping`, `error`) the connection is kept until
/// the phone has acknowledged it — a fixed wait would close on a message that was lost and
/// is still to be retransmitted (SCTP waits a second or more for that) — but no longer than
/// this, for a phone that is gone.
const FILE_DELIVERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Once the last message has arrived, the phone gets this long to close the connection
/// itself (it does, on `looping` and on `error`) before the desktop does.
const FILE_LINGER: Duration = Duration::from_millis(500);
const SAVE_FAILED: &str = "The video could not be saved on the computer.";
const PLAY_FAILED: &str = "The video could not be played on the computer.";
const PLAY_TIMED_OUT: &str = "Preparing the video on the computer took too long.";
const NOT_STARTED: &str = "The video was loaded on the computer, but playback could not be started.";

/// How the receiving part of a file session ended.
enum Received {
    /// Every byte is in the `.part` file and `end` was seen; it is a video, to be stored
    /// under this extension.
    Complete(PartFile, PhoneContainer, &'static str),
    /// A protocol violation, a stall or a disk error: tell the phone, then end as an error.
    Failed(String),
    /// Stopped, or the connection ended: nothing more to say on the data channel.
    Ended(Outcome),
}

async fn run_file_session(
    deps: Arc<SessionDeps>,
    offer: FileOffer,
    cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Outcome {
    let Some(files) = deps.files.clone() else {
        return (StopReason::Error, Some("this build cannot receive a video from the phone".into()));
    };
    let (outcome, loaded) = file_session(&deps, &files, offer, cmd_rx, events).await;
    // On every way out: whatever is not the loaded video goes — the previous phone video
    // after a success, the new one after a failure or a cancel, a `.part` whose delete did
    // not go through at the time.
    files.sweep_unused_keeping(loaded.as_deref()).await;
    outcome
}

/// The session itself. Besides the outcome: the path the loop engine says it loaded, if it
/// loaded the video.
async fn file_session(
    deps: &Arc<SessionDeps>,
    files: &FileDeps,
    offer: FileOffer,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> (Outcome, Option<PathBuf>) {
    // --- 1. sockets, while the phone sends its offer ------------------------------------
    let (transport, sdp, candidates) = match sockets_and_offer(deps, &mut cmd_rx, events).await {
        Ok(ready) => ready,
        Err(outcome) => return (outcome, None),
    };

    // --- 2. answer: a data channel and nothing else ---------------------------------------
    let (answer, rtc_tx, mut rtc_rx) = match rtc::accept_offer(transport, &sdp, deps.rtc.clone(), RtcMode::File) {
        Ok(x) => x,
        Err(e) => return ((StopReason::Error, Some(format!("{e:#}"))), None),
    };
    let _ = events.send(SessionEvent::Answer(answer));
    for c in candidates {
        let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
    }
    let say = |text: String| {
        let _ = rtc_tx.send(RtcCommand::ChannelText(text));
    };

    // --- 3. receive ------------------------------------------------------------------------
    let received = receive_file(files, &offer, &rtc_tx, &mut rtc_rx, &mut cmd_rx, events).await;
    let mut loaded: Option<PathBuf> = None;
    let outcome = match received {
        Received::Ended(outcome) => outcome,
        Received::Failed(message) => {
            say(reply::error(&message));
            linger(&rtc_tx, &mut rtc_rx).await;
            (StopReason::Error, Some(message))
        }
        Received::Complete(part, container, ext) => match part.commit(ext).await {
            Err(e) => {
                eprintln!("phone video: could not finish the file: {e}");
                say(reply::error(SAVE_FAILED));
                linger(&rtc_tx, &mut rtc_rx).await;
                (StopReason::Error, Some(SAVE_FAILED.into()))
            }
            // `_hold` keeps the committed file out of anybody's sweep until this session's
            // own, which runs once it is loaded (or not).
            Ok((path, _hold)) => {
                say(reply::progress(offer.size));
                say(reply::saved());
                let _ = events.send(SessionEvent::FileProgress { received: offer.size });
                // A stop that is already waiting (it came with the last bytes) is obeyed
                // before anything is loaded.
                if let Some(reason) = queued_stop(&rtc_tx, &mut cmd_rx) {
                    let _ = rtc_tx.send(RtcCommand::Stop);
                    return ((reason, None), None);
                }
                // --- 4. hand it to the loop engine ---------------------------------------
                let _ = events.send(SessionEvent::FileStage(FileStage::Processing));
                say(reply::processing());
                let request = (path, file_receive::load_name(&offer, ext), container);
                let played = play_received(files, request, &rtc_tx, &mut rtc_rx, &mut cmd_rx).await;
                match played {
                    // Looping — also when a stop came while it was loading and could no
                    // longer prevent it: what the phone, the UI and the camera show must
                    // be the same thing, so the session ends as the success it was.
                    PlayEnd::Played(Played::Looping { path }) => {
                        loaded = Some(path);
                        let _ = events.send(SessionEvent::FileStage(FileStage::Looping));
                        say(reply::looping(&offer.name));
                        linger(&rtc_tx, &mut rtc_rx).await;
                        (StopReason::Done, None)
                    }
                    // Loaded but not playing: the window was told (it shows the video,
                    // stopped), the phone hears why here and gets the loop's state after.
                    PlayEnd::Played(Played::LoadedNotPlaying { path }) => {
                        loaded = Some(path);
                        say(reply::error(NOT_STARTED));
                        linger(&rtc_tx, &mut rtc_rx).await;
                        (StopReason::Error, Some(NOT_STARTED.into()))
                    }
                    // Cancelled in time: nothing was loaded, the loop is as it was.
                    PlayEnd::Stopped(reason) => (reason, None),
                    PlayEnd::TimedOut => {
                        say(reply::error(PLAY_TIMED_OUT));
                        linger(&rtc_tx, &mut rtc_rx).await;
                        (StopReason::Timeout, Some(PLAY_TIMED_OUT.into()))
                    }
                    PlayEnd::Failed(e) => {
                        eprintln!("phone video: could not play the received video: {e:#}");
                        say(reply::error(PLAY_FAILED));
                        linger(&rtc_tx, &mut rtc_rx).await;
                        (StopReason::Error, Some(PLAY_FAILED.into()))
                    }
                }
            }
        },
    };
    let _ = rtc_tx.send(RtcCommand::Stop);
    (outcome, loaded)
}

/// What a session without a camera (file, meeting) waits for first: the sockets are bound
/// and the phone's offer has arrived. Nothing here needs undoing, so unlike the camera
/// handoff it is simply dropped on a stop. `Err` is the session's outcome.
pub(super) async fn sockets_and_offer(
    deps: &SessionDeps,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Result<(rtc::LocalTransport, String, Vec<String>), Outcome> {
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
            return Ok((transport.take().expect("checked"), sdp.take().expect("checked"), candidates));
        }
        tokio::select! {
            r = &mut prepare, if transport.is_none() => match r {
                Ok(t) => {
                    transport = Some(t);
                    let _ = events.send(SessionEvent::Prepared);
                }
                Err(e) => return Err((StopReason::Error, Some(format!("{e:#}")))),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCommand::Offer(o)) => sdp = Some(o),
                Some(SessionCommand::RemoteCandidate(c)) => {
                    if candidates.len() < MAX_REMOTE_CANDIDATES {
                        candidates.push(c);
                    }
                }
                Some(SessionCommand::Stop(r) | SessionCommand::Finish(r)) => return Err((r, None)),
                None => return Err((StopReason::Error, Some("session dropped".into()))),
            },
            _ = tokio::time::sleep_until(offer_deadline), if sdp.is_none() => {
                return Err((StopReason::Timeout, Some("no offer from the phone".into())));
            }
        }
    }
}

/// A `Stop` that is already in the queue, if any; candidates on the way are passed on.
fn queued_stop(
    rtc_tx: &mpsc::UnboundedSender<RtcCommand>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> Option<StopReason> {
    loop {
        match cmd_rx.try_recv() {
            Ok(SessionCommand::Stop(reason) | SessionCommand::Finish(reason)) => return Some(reason),
            Ok(SessionCommand::RemoteCandidate(c)) => {
                let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
            }
            Ok(SessionCommand::Offer(_)) => {}
            Err(mpsc::error::TryRecvError::Empty) => return None,
            Err(mpsc::error::TryRecvError::Disconnected) => return Some(StopReason::Error),
        }
    }
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
    // The first bytes have been seen to be a video.
    let mut sniffed = false;
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
                            Err(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                                eprintln!("phone video: not enough free disk space for {} bytes", offer.size);
                                break Received::Failed(file_receive::NO_SPACE.into());
                            }
                            Err(e) => {
                                eprintln!("phone video: could not create the file: {e}");
                                break Received::Failed(SAVE_FAILED.into());
                            }
                        }
                        eprintln!("phone video: receiving {} bytes", offer.size);
                        let _ = events.send(SessionEvent::FileProgress { received: 0 });
                    }
                    Ok(TextStep::Complete) => match part.take() {
                        // The content has the last word on what the file is, also for a
                        // file too short to have been judged while it arrived.
                        Some(p) => match accepted_video(offer, p.sniff(true)) {
                            Some((container, ext)) => return Received::Complete(p, container, ext),
                            None => {
                                part = Some(p);
                                break Received::Failed(file_receive::NOT_A_VIDEO.into());
                            }
                        },
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
                        // Not a video (or not the kind that was announced): stop at the
                        // first bytes instead of taking the other 300 MiB.
                        if !sniffed {
                            match part.as_ref().map(|p| p.sniff(false)) {
                                Some(Sniff::NeedMore) => {}
                                Some(sniff) if accepted_video(offer, sniff).is_some() => sniffed = true,
                                _ => break Received::Failed(file_receive::NOT_A_VIDEO.into()),
                            }
                        }
                        if let Some(received) = progress {
                            reported = received;
                            let _ = rtc_tx.send(RtcCommand::ChannelProgress(reply::progress(received)));
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
                Some(SessionCommand::Stop(r) | SessionCommand::Finish(r)) => break Received::Ended((r, None)),
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
                    let _ = rtc_tx.send(RtcCommand::ChannelProgress(reply::progress(reported)));
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

/// What the content must be for the file to be kept: a video ([`Sniff::Video`]) of the
/// kind the phone announced. Returns its container and the extension it is stored under.
fn accepted_video(offer: &FileOffer, sniff: Sniff) -> Option<(PhoneContainer, &'static str)> {
    match sniff {
        Sniff::Video { container, ext } => file_receive::stored_extension(offer, container, ext).map(|ext| (container, ext)),
        Sniff::NeedMore | Sniff::NotAVideo => None,
    }
}

/// How handing the video to the loop engine ended.
enum PlayEnd {
    /// It is loaded (looping, or loaded and not playing).
    Played(Played),
    /// A stop came in time: nothing was loaded.
    Stopped(StopReason),
    /// [`FileDeps::play_timeout`] passed and the engine gave up: nothing was loaded.
    TimedOut,
    Failed(anyhow::Error),
}

/// Load and start the received video, keeping the connection served meanwhile (loading can
/// take seconds; the phone is waiting for `looping` on it). The load runs in its own task
/// and is never dropped half way: a stop — the user's Cancel, the phone's, the deadline —
/// *asks* it to give up, and it answers whether it still could.
async fn play_received(
    files: &FileDeps,
    (path, display_name, container): (PathBuf, String, PhoneContainer),
    rtc_tx: &mpsc::UnboundedSender<RtcCommand>,
    rtc_rx: &mut mpsc::Receiver<RtcEvent>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> PlayEnd {
    let player = files.player.clone();
    let (cancel, cancelled) = Cancel::new();
    let request = PlayRequest { path, display_name, container, cancel: cancelled };
    let mut play = tokio::spawn(async move { player.play_file(request).await });
    let deadline = tokio::time::Instant::now() + files.play_timeout;
    let mut stopped: Option<StopReason> = None;
    let mut timed_out = false;
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
                Some(SessionCommand::Stop(r)) => {
                    stopped = Some(r);
                    cancel.cancel();
                }
                // A take that is already being finished.
                Some(SessionCommand::Finish(_)) => {}
                Some(SessionCommand::RemoteCandidate(c)) => {
                    let _ = rtc_tx.send(RtcCommand::RemoteCandidate(c));
                }
                Some(SessionCommand::Offer(_)) => {}
                None => {
                    stopped = Some(StopReason::Error);
                    cancel.cancel();
                }
            },
            _ = tokio::time::sleep_until(deadline), if !timed_out => {
                timed_out = true;
                cancel.cancel();
            }
        }
    };
    match result {
        Ok(Ok(played)) => PlayEnd::Played(played),
        // Cancelled by a stop; or by the deadline, when no stop asked for it first.
        Ok(Err(PlayError::Cancelled)) => match stopped {
            Some(reason) => PlayEnd::Stopped(reason),
            None if timed_out => PlayEnd::TimedOut,
            None => PlayEnd::Failed(anyhow!("loading the video was cancelled")),
        },
        Ok(Err(PlayError::Failed(e))) => PlayEnd::Failed(e),
        Err(_) => PlayEnd::Failed(anyhow!("loading the video failed unexpectedly")),
    }
}

/// Keep the connection up until the phone has everything that was said on the channel
/// ([`FILE_DELIVERY_TIMEOUT`]) and then closes it ([`FILE_LINGER`]), or is gone.
async fn linger(rtc_tx: &mpsc::UnboundedSender<RtcCommand>, rtc_rx: &mut mpsc::Receiver<RtcEvent>) {
    let (tx, delivered) = tokio::sync::oneshot::channel();
    let _ = rtc_tx.send(RtcCommand::Delivered(tx));
    tokio::pin!(delivered);
    let mut waiting_for_ack = true;
    let mut deadline = tokio::time::Instant::now() + FILE_DELIVERY_TIMEOUT;
    loop {
        tokio::select! {
            r = &mut delivered, if waiting_for_ack => {
                if r.is_err() {
                    return; // the receiver task is gone
                }
                waiting_for_ack = false;
                deadline = tokio::time::Instant::now() + FILE_LINGER;
            }
            ev = rtc_rx.recv() => {
                if matches!(ev, None | Some(RtcEvent::Closed(_))) {
                    return;
                }
            }
            _ = tokio::time::sleep_until(deadline) => return,
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
                Some(SessionCommand::Stop(r) | SessionCommand::Finish(r)) => return Err(((r, None), true)),
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
pub(super) async fn phone_stop_reason(
    pending: &mut VecDeque<SessionCommand>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> StopReason {
    let deadline = tokio::time::Instant::now() + PEER_STOP_GRACE;
    loop {
        match tokio::time::timeout_at(deadline, next_command(pending, cmd_rx)).await {
            Ok(Some(SessionCommand::Stop(reason) | SessionCommand::Finish(reason))) => return reason,
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
    /// A record session: keep what is written to the decoder for the tee.
    tee: bool,
    /// The frame the last [`Self::on_frame`] wrote to the decoder, when [`Self::tee`] is
    /// set. Taken (or overwritten) before the next one.
    written: Option<TakeFrame>,
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
            tee: false,
            written: None,
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
    ) -> Result<Option<End>> {
        self.written = None;
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
        let (bytes, keyframe) = match dec.writer.push(&f.data, f.rtp_ts) {
            FrameOut::NeedKeyframe => {
                self.keyframe();
                return Ok(None);
            }
            FrameOut::Write { bytes, keyframe } => (bytes, keyframe),
        };
        if let Some(d) = dec.writer.dimensions() {
            self.last_dims = Some(d);
        }
        enum Written {
            Done(std::io::Result<()>),
            Stalled,
            Interrupted(End),
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
                        Some(SessionCommand::Stop(r)) => break Written::Interrupted(End::Now((r, None))),
                        Some(SessionCommand::Finish(r)) => break Written::Interrupted(End::Finish(r)),
                        Some(other) => pending.push_back(other),
                        None => break Written::Interrupted(End::Now((StopReason::Error, Some("session dropped".into())))),
                    },
                    _ = &mut stalled => break Written::Stalled,
                }
            }
        };
        match written {
            Written::Done(Ok(())) => {}
            Written::Done(Err(e)) => return self.restart_after_crash(&format!("stopped reading ({e})")).await.map(|()| None),
            Written::Stalled => return self.restart_after_crash("stalled").await.map(|()| None),
            Written::Interrupted(end) => return Ok(Some(end)),
        }
        self.restarts.on_good_write(Instant::now());
        self.frames_written += 1;
        // What the decoder was given, for the tee: the Annex-B access unit as written, or
        // the VP8 frame (its IVF record header is the decoder stream's, not the take's).
        if self.tee {
            self.written = Some(TakeFrame {
                codec: f.codec,
                keyframe,
                rtp_ts: f.rtp_ts,
                arrived: Instant::now(),
                dims: self.last_dims,
                data: match f.codec {
                    EncodedCodec::H264 => bytes.into(),
                    EncodedCodec::Vp8 => f.data.clone(),
                },
            });
        }
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
        fn yield_to_loop(&self, epoch: u64) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.epochs.lock().unwrap().push(("yield", epoch)) })
        }
        fn keep_for_loop(&self, epoch: u64) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.epochs.lock().unwrap().push(("keep", epoch)) })
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
            // The network is read on every pass, also while there is more to write (then
            // without waiting): a browser's network thread does not wait for the page
            // either. Reading only when there was nothing left to write let the desktop's
            // SACKs and replies pile up in the socket buffer until the kernel dropped them,
            // and a dropped `looping` then had to wait for an SCTP retransmission.
            if sock.set_nonblocking(wrote).is_err() {
                break;
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
            Desk::build(player, idle_timeout, file_receive::PLAY_TIMEOUT, false)
        }

        /// `linked_data_dir`: the app knows its data directory through a symlink, so a
        /// canonicalised path spells it differently.
        fn build(player: FakeLoop, idle_timeout: Duration, play_timeout: Duration, linked_data_dir: bool) -> Desk {
            let app_data = tempfile::tempdir().unwrap();
            let data_dir = if linked_data_dir {
                let real = app_data.path().join("real");
                std::fs::create_dir(&real).unwrap();
                let link = app_data.path().join("link");
                #[cfg(unix)]
                std::os::unix::fs::symlink(&real, &link).unwrap();
                #[cfg(not(unix))]
                std::fs::create_dir(&link).unwrap();
                link
            } else {
                app_data.path().to_path_buf()
            };
            let store = PhoneVideoStore::in_app_data(&data_dir);
            let player = Arc::new(player);
            let camera = ProbeCamera::new(false, Duration::ZERO);
            // An ffmpeg that does not exist: a file session must never start one.
            let mut d = SessionDeps::new(PathBuf::from("/nonexistent/ffmpeg"), camera.clone(), Arc::new(NoStun));
            d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
            let mut files = FileDeps::new(store.clone(), player.clone());
            files.idle_timeout = idle_timeout;
            files.play_timeout = play_timeout;
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

        async fn wait_phase(&self, phase: FilePhase) {
            let mut status = self.manager.subscribe();
            tokio::time::timeout(Duration::from_secs(20), status.wait_for(|s| s.file.as_ref().is_some_and(|f| f.phase == phase)))
                .await
                .expect("the phase is reached in time")
                .unwrap();
        }

        /// The `loop_status` the phone gets once a file session is over.
        async fn loop_status(&mut self) -> (bool, Option<String>, bool) {
            loop {
                if let OutboundFrame::Signal(StreamSignal::LoopStatus { playing, name, from_phone, .. }) = self.next_out().await {
                    return (playing, name, from_phone);
                }
            }
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
    fn noise(len: usize) -> Vec<u8> {
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

    /// [`noise`] behind the first bytes of an MP4 (an `ftyp` box): what the receiver takes
    /// for a video. (The loop engine is a fake here; nothing parses the rest.)
    fn video_bytes(len: usize) -> Vec<u8> {
        let mut data = noise(len);
        let head = file_receive::tests::mp4_head(b"isom", 24);
        data[..head.len()].copy_from_slice(&head);
        data
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
        // The stall timeout has to outlast the transport's own recovery, as the real 20 s
        // does: a datagram dropped on the way to the first megabyte is retransmitted after
        // SCTP's RTO (1 s at best, 3 s before the first round-trip measurement). Anything
        // shorter mistakes that for the stall, before the stall this test stages.
        let idle = if how == "stall" { Duration::from_secs(5) } else { file_receive::IDLE_TIMEOUT };
        let mut desk = Desk::new(FakeLoop::default(), idle);
        let phone = FilePhone::start(script);
        desk.connect("abort-1", file, &phone).await;

        desk.wait_received(1024 * 1024).await;
        let files = desk.files();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].ends_with(".part"), "written under a .part name: {files:?}");
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

    /// A desktop video is looping when the phone's arrives.
    fn desk_video_playing(desk: &Desk) {
        *desk.player.state.lock().unwrap() =
            file_receive::LoopState { playing: true, name: Some("desk.mp4".into()), path: Some("/videos/desk.mp4".into()) };
    }

    /// H1: Cancel while the video is being loaded cancels the load. Nothing of the phone's
    /// video is left — not on the camera, not in the window's status, not on disk.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelling_while_the_video_is_loading_cancels_the_load() {
        let data = video_bytes(200_000);
        let (file, script) = script("clip.mp4", data.len(), data);
        let mut desk = Desk::new(FakeLoop { play_delay: Duration::from_secs(30), ..FakeLoop::default() }, file_receive::IDLE_TIMEOUT);
        desk_video_playing(&desk);
        // A phone video from earlier that is no longer loaded: every exit sweeps.
        std::fs::create_dir_all(desk.store.dir()).unwrap();
        let stale = desk.store.dir().join("phone-00000000000000aa.mp4");
        std::fs::write(&stale, b"older").unwrap();
        let phone = FilePhone::start(script);
        desk.connect("cancel-1", file, &phone).await;

        desk.wait_phase(FilePhase::Processing).await;
        let asked = Instant::now();
        desk.manager.stop(StopReason::User).await;
        let st = desk.ended().await;
        assert!(asked.elapsed() < Duration::from_secs(10), "the cancel did not wait for the load");
        assert_eq!((st.state, st.last_error, st.file), (Phase::Idle, None, None), "a cancel, not an error and not a loop");
        // The loop is what it was, and the phone is told so.
        assert_eq!(desk.loop_status().await, (true, Some("desk.mp4".into()), false));
        assert_eq!(desk.player.calls(), vec!["play_file clip.mp4"]);
        assert!(desk.player.played_bytes.lock().unwrap().is_none(), "the file was never loaded");
        assert!(desk.files().is_empty(), "the cancelled video and the stale one are gone: {:?}", desk.files());
        let _ = phone.ctl.send(PhoneCtl::Close);
        let report = phone.finish().await;
        assert!(!report.kinds().contains(&"looping"), "{:?}", report.kinds());
    }

    /// H1: when the load can no longer be prevented, the stop does not get to say "stopped":
    /// the video loops, and the window, the phone and the disk all say so.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cancel_that_comes_too_late_ends_as_looping_everywhere() {
        let data = video_bytes(200_000);
        let (file, script) = script("clip.mp4", data.len(), data);
        let player = FakeLoop { play_delay: Duration::from_millis(1500), ignore_cancel: true, ..FakeLoop::default() };
        let mut desk = Desk::new(player, file_receive::IDLE_TIMEOUT);
        desk_video_playing(&desk);
        let phone = FilePhone::start(script);
        desk.connect("late-1", file, &phone).await;

        desk.wait_phase(FilePhase::Processing).await;
        desk.manager.stop(StopReason::User).await;
        let st = desk.ended().await;
        assert_eq!((st.state, st.last_error.clone()), (Phase::Idle, None));
        assert_eq!(st.file.map(|f| f.phase), Some(FilePhase::Looping), "the window adopts the video on this");
        assert_eq!(desk.loop_status().await, (true, Some("clip.mp4".into()), true));
        assert_eq!(desk.files().len(), 1, "the looping video stays on disk: {:?}", desk.files());
        let report = phone.finish().await;
        assert_eq!(report.kinds().last(), Some(&"looping"), "{:?}", report.kinds());
    }

    #[tokio::test]
    async fn a_stop_that_is_already_queued_is_found_before_anything_is_loaded() {
        let (rtc_tx, mut rtc_rx) = mpsc::unbounded_channel();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        assert_eq!(queued_stop(&rtc_tx, &mut cmd_rx), None);
        // A late candidate is passed on; the stop behind it is still found.
        cmd_tx.send(SessionCommand::RemoteCandidate("candidate:1".into())).unwrap();
        cmd_tx.send(SessionCommand::Stop(StopReason::User)).unwrap();
        assert_eq!(queued_stop(&rtc_tx, &mut cmd_rx), Some(StopReason::User));
        assert!(matches!(rtc_rx.try_recv(), Ok(RtcCommand::RemoteCandidate(c)) if c == "candidate:1"));
        // The manager is gone: that ends the session too.
        drop(cmd_tx);
        assert_eq!(queued_stop(&rtc_tx, &mut cmd_rx), Some(StopReason::Error));
    }

    /// M4: a load that hangs (a system prompt nobody answers) does not keep the session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_load_that_takes_too_long_is_given_up() {
        let data = video_bytes(200_000);
        let (file, script) = script("clip.mp4", data.len(), data);
        let player = FakeLoop { play_delay: Duration::from_secs(120), ..FakeLoop::default() };
        let mut desk = Desk::build(player, file_receive::IDLE_TIMEOUT, Duration::from_millis(400), false);
        desk_video_playing(&desk);
        let phone = FilePhone::start(script);
        desk.connect("slow-1", file, &phone).await;

        let st = desk.ended().await;
        assert_eq!((st.state, st.last_error_code.as_deref()), (Phase::Error, Some("timeout")));
        assert_eq!(st.last_error.as_deref(), Some("Preparing the video on the computer took too long."));
        assert_eq!(st.file.map(|f| f.phase), Some(FilePhase::Failed));
        assert_eq!(desk.loop_status().await, (true, Some("desk.mp4".into()), false), "the earlier loop was not touched");
        assert!(desk.files().is_empty(), "{:?}", desk.files());
        let report = phone.finish().await;
        assert_eq!(report.replies.last(), Some(&json!({"t":"error","message":"Preparing the video on the computer took too long."})));
    }

    /// M3: loaded, but playback could not be started and there was nothing to go back to.
    /// The phone is told it failed *and* that the video is loaded, not playing; it stays.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_video_that_loads_but_does_not_start_is_reported_as_loaded_not_playing() {
        let data = video_bytes(200_000);
        let (file, script) = script("clip.mp4", data.len(), data);
        let mut desk = Desk::new(FakeLoop { not_startable: true, ..FakeLoop::default() }, file_receive::IDLE_TIMEOUT);
        let phone = FilePhone::start(script);
        desk.connect("idle-1", file, &phone).await;

        let st = desk.ended().await;
        let message = "The video was loaded on the computer, but playback could not be started.";
        assert_eq!((st.state, st.last_error.as_deref()), (Phase::Error, Some(message)));
        assert_eq!(st.file.map(|f| f.phase), Some(FilePhase::Failed));
        assert_eq!(desk.loop_status().await, (false, Some("clip.mp4".into()), true));
        assert_eq!(desk.files().len(), 1, "it is the loaded video, so it is kept: {:?}", desk.files());
        let report = phone.finish().await;
        assert_eq!(report.replies.last(), Some(&json!({"t":"error","message":message})));
    }

    /// M3: a video the engine cannot read costs the user nothing: what was looping still is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_video_that_cannot_be_played_leaves_the_earlier_loop_playing() {
        let data = video_bytes(200_000);
        let (file, script) = script("broken.mp4", data.len(), data);
        let mut desk = Desk::new(FakeLoop { fail_play: true, ..FakeLoop::default() }, file_receive::IDLE_TIMEOUT);
        desk_video_playing(&desk);
        let phone = FilePhone::start(script);
        desk.connect("bad-2", file, &phone).await;
        let st = desk.ended().await;
        assert_eq!(st.last_error.as_deref(), Some("The video could not be played on the computer."));
        assert_eq!(desk.loop_status().await, (true, Some("desk.mp4".into()), false));
        assert!(desk.files().is_empty());
        let _ = phone.finish().await;
    }

    /// H2: bytes that are not a video never reach the loader, whatever they are called.
    async fn not_a_video(name: &str, mime: &str, data: Vec<u8>) {
        let file = json!({"name":name,"size":data.len(),"mime":mime});
        let meta = json!({"t":"meta","name":name,"size":data.len(),"mime":mime});
        let script = PhoneScript { meta, data, chunk: 65536, pause_at: None };
        let mut desk = Desk::new(FakeLoop::default(), file_receive::IDLE_TIMEOUT);
        desk_video_playing(&desk);
        let phone = FilePhone::start(script);
        desk.connect("fake-1", file, &phone).await;

        let st = desk.ended().await;
        assert_eq!((st.state, st.last_error.as_deref()), (Phase::Error, Some(file_receive::NOT_A_VIDEO)), "{name}");
        assert_eq!(st.file.map(|f| f.phase), Some(FilePhase::Failed));
        assert!(desk.player.calls().is_empty(), "the loop engine never saw it");
        assert!(desk.files().is_empty(), "{:?}", desk.files());
        assert_eq!(desk.loop_status().await, (true, Some("desk.mp4".into()), false));
        let report = phone.finish().await;
        assert_eq!(report.replies.last(), Some(&json!({"t":"error","message":file_receive::NOT_A_VIDEO})));
        assert!(!report.kinds().contains(&"saved"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_playlist_named_mp4_is_refused_at_its_first_bytes() {
        // What ffmpeg would follow to other files if it were left to guess the format.
        let mut data = b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n".to_vec();
        data.resize(400_000, b'\n');
        not_a_video("holiday.mp4", "video/mp4", data).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn noise_and_a_file_too_short_to_be_a_video_are_refused() {
        not_a_video("clip.mp4", "video/mp4", noise(300_000)).await;
        // Ten bytes: judged when `end` arrives.
        not_a_video("clip.mp4", "video/mp4", noise(10)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_video_of_another_kind_than_announced_is_refused() {
        // An MP4, announced as WebM.
        not_a_video("clip.webm", "video/webm", video_bytes(200_000)).await;
    }

    /// H2 + L2: the stored extension and the load name follow the content, and the loader is
    /// told the container it was checked to be.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_file_is_stored_and_loaded_as_what_its_content_is() {
        // A QuickTime file that the phone calls "clip" with an unknown video type.
        let mut data = noise(150_000);
        let head = file_receive::tests::mp4_head(b"qt  ", 24);
        data[..head.len()].copy_from_slice(&head);
        let file = json!({"name":"videos/../clip","size":data.len(),"mime":"video/x-whatever"});
        let meta = json!({"t":"meta","name":"videos/../clip","size":data.len(),"mime":"video/x-whatever"});
        let mut desk = Desk::new(FakeLoop::default(), file_receive::IDLE_TIMEOUT);
        let phone = FilePhone::start(PhoneScript { meta, data, chunk: 65536, pause_at: None });
        desk.connect("qt-1", file, &phone).await;
        let st = desk.ended().await;
        assert_eq!(st.file.map(|f| (f.phase, f.name)), Some((FilePhase::Looping, "clip".into())), "the display name is the last component");
        let files = desk.files();
        assert!(files.len() == 1 && files[0].ends_with(".mov"), "{files:?}");
        assert_eq!(desk.player.calls(), vec!["play_file clip.mov"]);
        assert_eq!(*desk.player.played_container.lock().unwrap(), Some(PhoneContainer::IsoBmff));
        let _ = phone.finish().await;
    }

    /// M6: the engine reports the loaded path canonicalised (the data directory is behind a
    /// symlink). The sweep after the load must keep the looping video, and it is still
    /// `fromPhone`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_loader_that_canonicalises_the_path_does_not_lose_the_video() {
        let data = video_bytes(200_000);
        let (file, script) = script("clip.mp4", data.len(), data.clone());
        let player = FakeLoop { canonical_paths: true, ..FakeLoop::default() };
        let mut desk = Desk::build(player, file_receive::IDLE_TIMEOUT, file_receive::PLAY_TIMEOUT, true);
        std::fs::create_dir_all(desk.store.dir()).unwrap();
        let older = desk.store.dir().join("phone-00000000000000aa.mp4");
        std::fs::write(&older, b"older").unwrap();
        let phone = FilePhone::start(script);
        desk.connect("canon-1", file, &phone).await;

        let st = desk.ended().await;
        assert_eq!(st.file.map(|f| f.phase), Some(FilePhase::Looping));
        let loaded = desk.player.state.lock().unwrap().path.clone().expect("loaded");
        assert!(!loaded.starts_with(desk.store.dir()), "the engine spells the path differently: {loaded:?}");
        assert!(loaded.exists(), "the sweep deleted the video that is looping");
        assert_eq!(std::fs::read(&loaded).unwrap(), data);
        assert!(!older.exists(), "the previous phone video is still swept");
        assert_eq!(desk.files().len(), 1);
        assert_eq!(desk.loop_status().await, (true, Some("clip.mp4".into()), true), "fromPhone survives the other spelling");
        let _ = phone.finish().await;
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

    // --- record sessions: the recorder, with real ffmpeg and no network ---------------------

    use crate::remote::stream::record::testing::{ffmpeg_available, h264_frames};
    use crate::remote::stream::file_receive::{LoopPlayer, LoopState};

    /// A record session from the moment frames reach the decoder: the [`Recorder`], the
    /// store, a fake loop, and the two queues [`Recorder::finish`] serves.
    struct Booth {
        rec: Recorder,
        deps: Arc<SessionDeps>,
        camera: Arc<ProbeCamera>,
        player: Arc<FakeLoop>,
        store: PhoneVideoStore,
        events: mpsc::UnboundedSender<SessionEvent>,
        seen: mpsc::UnboundedReceiver<SessionEvent>,
        rtc_tx: mpsc::UnboundedSender<RtcCommand>,
        _rtc_cmds: mpsc::UnboundedReceiver<RtcCommand>,
        _rtc_events: mpsc::Sender<RtcEvent>,
        rtc_rx: mpsc::Receiver<RtcEvent>,
        cmd_tx: mpsc::UnboundedSender<SessionCommand>,
        cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
        _app_data: tempfile::TempDir,
    }

    const BOOTH_EPOCH: u64 = 7;

    impl Booth {
        fn new(player: FakeLoop) -> Booth {
            let app_data = tempfile::tempdir().unwrap();
            let store = PhoneVideoStore::in_app_data(app_data.path());
            let player = Arc::new(player);
            let camera = ProbeCamera::new(false, Duration::ZERO);
            let files = FileDeps::new(store.clone(), player.clone());
            let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera.clone(), Arc::new(NoStun));
            d.files = Some(files.clone());
            let (events, seen) = mpsc::unbounded_channel();
            let (rtc_tx, _rtc_cmds) = mpsc::unbounded_channel();
            let (_rtc_events, rtc_rx) = mpsc::channel(8);
            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            Booth {
                rec: Recorder::new(files),
                deps: Arc::new(d),
                camera,
                player,
                store,
                events,
                seen,
                rtc_tx,
                _rtc_cmds,
                _rtc_events,
                rtc_rx,
                cmd_tx,
                cmd_rx,
                _app_data: app_data,
            }
        }

        /// Frames reach the decoder. `true` once the take is full.
        async fn feed(&mut self, frames: Vec<TakeFrame>) -> bool {
            for f in frames {
                if let TeeStep::Full = self.rec.on_frame(f, &self.events).await {
                    return true;
                }
            }
            false
        }

        /// The take ends, then the session's own sweep runs — as in `spawn_session`.
        async fn finish(&mut self, reason: StopReason) -> Outcome {
            let mut pending = VecDeque::new();
            let link = Link { rtc_tx: &self.rtc_tx, rtc_rx: &mut self.rtc_rx, cmd_rx: &mut self.cmd_rx, pending: &mut pending };
            let outcome = self.rec.finish(reason, &self.deps, BOOTH_EPOCH, link, &self.events).await;
            self.rec.files.sweep_unused_keeping(self.rec.loaded.as_deref()).await;
            outcome
        }

        /// The take's stages as the state machine was told, without the repeats.
        fn stages(&mut self) -> Vec<(RecordStage, u64)> {
            let mut v: Vec<(RecordStage, u64)> = Vec::new();
            while let Ok(ev) = self.seen.try_recv() {
                if let SessionEvent::Record { stage, elapsed_ms } = ev {
                    v.push((stage, elapsed_ms));
                }
            }
            v
        }

        fn files(&self) -> Vec<String> {
            let Ok(dir) = std::fs::read_dir(self.store.dir()) else { return vec![] };
            let mut v: Vec<String> = dir.map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
            v.sort();
            v
        }

        fn camera_log(&self) -> Vec<(&'static str, u64)> {
            self.camera.epochs.lock().unwrap().clone()
        }
    }

    const THIRTY_FPS: Duration = Duration::from_micros(33_333);

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_take_is_remuxed_handed_to_the_loop_engine_and_keeps_looping() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let mut booth = Booth::new(FakeLoop::default());
        // Five seconds reach the decoder; the first second of them is the pre-roll.
        assert!(!booth.feed(h264_frames(150, THIRTY_FPS, Instant::now())).await);
        assert!(booth.rec.take.has_minimum());
        assert_eq!(booth.files().len(), 1, "the take is being teed into the store: {:?}", booth.files());
        assert!(booth.files()[0].starts_with("phone-") && booth.files()[0].ends_with(".part"), "{:?}", booth.files());
        assert!(booth.player.calls().is_empty(), "nothing is loaded while the take runs");

        // The phone taps Stop.
        assert_eq!(booth.finish(StopReason::User).await, (StopReason::Done, None));
        let stages = booth.stages();
        assert_eq!(stages.first(), Some(&(RecordStage::Recording, 0)));
        let teed_ms = booth.rec.take.elapsed_ms();
        assert!((3900..5000).contains(&teed_ms), "{teed_ms}");
        assert_eq!(&stages[stages.len() - 2..], &[(RecordStage::Processing, teed_ms), (RecordStage::Looping, teed_ms)]);

        // The loop engine got an MP4 by content, under a name of the desktop's choosing.
        assert_eq!(booth.player.calls(), vec!["play_file Phone recording.mp4"]);
        assert_eq!(*booth.player.played_container.lock().unwrap(), Some(PhoneContainer::IsoBmff));
        let played = booth.player.played_bytes.lock().unwrap().clone().expect("the file was there to be played");
        assert!(matches!(file_receive::sniff(&played[..64], false), Sniff::Video { container: PhoneContainer::IsoBmff, ext: "mp4" }));
        // The camera went from the phone to the loop: yielded before the load, kept after.
        assert_eq!(booth.camera_log(), vec![("yield", BOOTH_EPOCH), ("keep", BOOTH_EPOCH)]);

        // The session is over; the phone can go. What it left is the looping video, alone.
        let loaded = booth.rec.loaded.clone().expect("loaded");
        assert_eq!(booth.files().len(), 1, "{:?}", booth.files());
        assert!(booth.files()[0].ends_with(".mp4") && loaded.exists() && booth.store.contains(&loaded));
        let state = booth.player.state().await;
        assert!(state.playing && state.path.as_deref() == Some(loaded.as_path()));
        assert_eq!(state.name.as_deref(), Some("Phone recording.mp4"));
        // A later sweep (another session ending, the window loading nothing) keeps it too.
        assert_eq!(booth.rec.files.sweep_unused().await, 0);
        assert!(loaded.exists());
        // The duration the loader sees is the take's.
        let info = crate::video_processor::probe_phone_video(loaded, "x.mp4".into(), PhoneContainer::IsoBmff, Duration::from_secs(20))
            .await
            .unwrap();
        assert!(((info.duration * 1000.0) as i64 - teed_ms as i64).abs() < 150, "{info:?} vs {teed_ms} ms");
    }

    /// The encoder's ramp outlasts the pre-roll: the take starts at 320x180 and the stream
    /// becomes 640x360 two and a half seconds in. The 180p prelude is deleted, the take is
    /// the 360p picture alone, and its clock starts again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_take_that_grows_right_after_it_started_is_teed_again_at_the_larger_size() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let mut booth = Booth::new(FakeLoop::default());
        let t0 = Instant::now();
        // The fixture is 640x360 throughout; only what the stream says of itself differs.
        let mut small = h264_frames(75, THIRTY_FPS, t0);
        for f in &mut small {
            f.dims = Some((320, 180));
        }
        booth.feed(small).await;
        assert!(booth.rec.take.started(), "the pre-roll is over, the take runs at the small size");
        assert_eq!(booth.rec.take.dims(), Some((320, 180)));
        let prelude = booth.files();
        assert_eq!(prelude.len(), 1);
        let first_clock = booth.rec.started.expect("the take's clock runs");

        let later = t0 + THIRTY_FPS * 75;
        let mut large = h264_frames(120, THIRTY_FPS, later);
        for f in &mut large {
            f.rtp_ts = f.rtp_ts.wrapping_add(75 * 3000);
        }
        booth.feed(large).await;
        assert_eq!(booth.rec.take.dims(), Some((640, 360)));
        assert_eq!(booth.rec.take.frames(), 120, "the larger picture from its first keyframe, and nothing else");
        let now = booth.files();
        assert_eq!(now.len(), 1, "one take file: {now:?}");
        assert_ne!(now, prelude, "the prelude was deleted, not appended to");
        assert!(booth.rec.started.expect("still running") > first_clock, "the 60 s clock started again");
        // The phone and the window were told twice that a take began, both times at zero.
        let begun = booth.stages().iter().filter(|s| **s == (RecordStage::Recording, 0)).count();
        assert_eq!(begun, 2);

        assert_eq!(booth.finish(StopReason::User).await, (StopReason::Done, None));
        let teed_ms = booth.rec.take.elapsed_ms();
        assert!((3900..4000).contains(&teed_ms), "four seconds of the larger picture: {teed_ms}");
        let loaded = booth.rec.loaded.clone().expect("loaded");
        let info = crate::video_processor::probe_phone_video(loaded, "x.mp4".into(), PhoneContainer::IsoBmff, Duration::from_secs(20))
            .await
            .unwrap();
        assert_eq!((info.width, info.height), (640, 360));
        assert!(((info.duration * 1000.0) as i64 - teed_ms as i64).abs() < 150, "{info:?} vs {teed_ms} ms");
    }

    #[tokio::test]
    async fn a_take_under_two_seconds_is_deleted_and_nothing_is_loaded() {
        let mut booth = Booth::new(FakeLoop::default());
        // 2.5 s at the decoder: a second of it is the pre-roll.
        booth.feed(h264_frames(75, THIRTY_FPS, Instant::now())).await;
        assert!(booth.rec.take.started() && !booth.rec.take.has_minimum(), "{} ms", booth.rec.take.elapsed_ms());
        assert_eq!(booth.files().len(), 1);
        assert_eq!(booth.finish(StopReason::User).await, (StopReason::TooShort, Some(record::TOO_SHORT.into())));
        assert!(booth.files().is_empty(), "{:?}", booth.files());
        assert!(booth.player.calls().is_empty());
        assert!(booth.camera_log().is_empty(), "the camera handoff is left to the ordinary release");
        assert!(!booth.stages().iter().any(|(s, _)| *s != RecordStage::Recording), "never processed");
    }

    #[tokio::test]
    async fn a_stop_during_the_pre_roll_is_an_ordinary_stop() {
        let mut booth = Booth::new(FakeLoop::default());
        booth.feed(h264_frames(5, THIRTY_FPS, Instant::now())).await;
        assert!(!booth.rec.take.started());
        assert_eq!(booth.finish(StopReason::User).await, (StopReason::User, None));
        assert!(booth.files().is_empty() && booth.stages().is_empty());
    }

    #[tokio::test]
    async fn a_take_that_is_mostly_stalls_is_deleted_as_incomplete() {
        let mut booth = Booth::new(FakeLoop::default());
        let t0 = Instant::now();
        // The stream starts, then delivers a third of a second every four seconds.
        let mut frames = h264_frames(40, THIRTY_FPS, t0);
        for burst in 1..4u32 {
            let mut more = h264_frames(10, THIRTY_FPS, t0 + Duration::from_secs(4) * burst);
            for f in &mut more {
                f.rtp_ts = f.rtp_ts.wrapping_add(360_000 * burst);
            }
            frames.extend(more);
        }
        booth.feed(frames).await;
        assert!(booth.rec.take.has_minimum(), "long enough — but frozen most of the time");
        assert_eq!(booth.finish(StopReason::User).await, (StopReason::Incomplete, Some(record::INCOMPLETE.into())));
        assert_eq!(record::INCOMPLETE, "The recording was too incomplete to loop. Try again.");
        assert!(booth.files().is_empty(), "{:?}", booth.files());
        assert!(booth.player.calls().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_take_the_loader_refuses_is_deleted_and_the_camera_is_handed_back() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        // Something was looping before; the loader's probe fails for the take.
        let earlier = std::path::Path::new("/videos/earlier.mp4");
        let player = FakeLoop { fail_play: true, ..FakeLoop::default() };
        *player.state.lock().unwrap() = LoopState { playing: true, name: Some("earlier.mp4".into()), path: Some(earlier.into()) };
        let mut booth = Booth::new(player);
        booth.feed(h264_frames(150, THIRTY_FPS, Instant::now())).await;
        assert_eq!(booth.finish(StopReason::User).await, (StopReason::Incomplete, Some(record::INCOMPLETE.into())));
        assert_eq!(booth.player.calls(), vec!["play_file Phone recording.mp4"]);
        assert!(booth.files().is_empty(), "the remuxed file goes with the session's sweep: {:?}", booth.files());
        assert_eq!(booth.rec.loaded, None);
        // Yielded for the attempt, never kept: the release that follows undoes the handoff
        // (and resumes what played before).
        assert_eq!(booth.camera_log(), vec![("yield", BOOTH_EPOCH)]);
        assert_eq!(booth.player.state().await.name.as_deref(), Some("earlier.mp4"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_while_the_take_is_processed_deletes_it() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        // A cancel that is already waiting: found during the remux.
        let mut booth = Booth::new(FakeLoop::default());
        booth.feed(h264_frames(150, THIRTY_FPS, Instant::now())).await;
        booth.cmd_tx.send(SessionCommand::Stop(StopReason::User)).unwrap();
        assert_eq!(booth.finish(StopReason::User).await, (StopReason::User, None));
        assert!(booth.files().is_empty(), "{:?}", booth.files());
        assert!(booth.player.calls().is_empty());

        // A cancel while the loop engine is loading it (a slow probe, a system prompt).
        let mut booth = Booth::new(FakeLoop { play_delay: Duration::from_secs(30), ..FakeLoop::default() });
        booth.feed(h264_frames(150, THIRTY_FPS, Instant::now())).await;
        let (player, cmd_tx) = (booth.player.clone(), booth.cmd_tx.clone());
        let cancel = async move {
            while player.calls().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // A repeated finish is nothing; the stop is the cancel.
            cmd_tx.send(SessionCommand::Finish(StopReason::User)).unwrap();
            cmd_tx.send(SessionCommand::Stop(StopReason::User)).unwrap();
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(booth.finish(StopReason::User), cancel) })
            .await
            .expect("the cancel ends the load");
        assert_eq!(outcome, (StopReason::User, None));
        assert!(booth.files().is_empty(), "{:?}", booth.files());
        assert!(!booth.player.state().await.playing);
        assert_eq!(booth.camera_log(), vec![("yield", BOOTH_EPOCH)]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_take_is_finalized_at_sixty_seconds_whatever_the_phone_does() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let mut booth = Booth::new(FakeLoop::default());
        // 70 s of stream (10 fps keeps the test small): the tee says "full" at the cap.
        let frames = h264_frames(700, Duration::from_millis(100), Instant::now());
        assert!(booth.feed(frames).await, "the cap ends the take");
        let teed_ms = booth.rec.take.elapsed_ms();
        assert!((59_900..=60_000).contains(&teed_ms), "{teed_ms}");
        assert_eq!(booth.finish(StopReason::Done).await, (StopReason::Done, None));
        assert_eq!(booth.stages().last(), Some(&(RecordStage::Looping, teed_ms)));
        let loaded = booth.rec.loaded.clone().unwrap();
        let info = crate::video_processor::probe_phone_video(loaded, "x.mp4".into(), PhoneContainer::IsoBmff, Duration::from_secs(20))
            .await
            .unwrap();
        assert!((59.5..=60.5).contains(&info.duration), "{info:?}");

        // The cap by the clock, for a stream that stalled before its last frame came.
        let mut booth = Booth::new(FakeLoop::default());
        booth.feed(h264_frames(100, THIRTY_FPS, Instant::now())).await;
        assert!(!booth.rec.tick(&booth.events));
        booth.rec.started = Some(Instant::now() - record::RECORD_CAP - Duration::from_secs(2));
        assert!(booth.rec.tick(&booth.events));
    }

    #[tokio::test]
    async fn a_record_session_needs_a_store_and_claims_the_camera_like_a_live_one() {
        // No store (no app data directory): an error, and the camera is never touched.
        let camera = ProbeCamera::new(false, Duration::ZERO);
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let _tx = spawn_session(deps(camera.clone(), Arc::new(NoStun)), 3, SessionKind::Record, ev_tx);
        let (reason, error) = closed(&mut ev_rx).await;
        assert_eq!(reason, StopReason::Error);
        assert!(error.unwrap().contains("cannot record"));
        assert!(camera.epochs.lock().unwrap().is_empty());

        // With one: the live handoff, under the session's epoch; a stop before any media
        // ends it as a stop, and nothing is left in the store.
        let app_data = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(app_data.path());
        let camera = ProbeCamera::new(false, Duration::ZERO);
        let mut d = SessionDeps::new(PathBuf::from("ffmpeg"), camera.clone(), Arc::new(NoStun));
        d.rtc.bind_ips = Some(vec![std::net::Ipv4Addr::LOCALHOST]);
        d.files = Some(FileDeps::new(store.clone(), Arc::new(FakeLoop::default())));
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let tx = spawn_session(Arc::new(d), 9, SessionKind::Record, ev_tx);
        tx.send(SessionCommand::Finish(StopReason::User)).unwrap();
        assert_eq!(closed(&mut ev_rx).await, (StopReason::User, None));
        assert_eq!(*camera.epochs.lock().unwrap(), vec![("acquire", 9), ("release", 9)]);
        assert_eq!(store.sweep(None), 0);
    }
}
