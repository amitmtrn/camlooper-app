//! One phone session from "accepted" to "ended": take over the virtual camera, gather
//! candidates, answer the offer, then feed every received frame to an ffmpeg that writes the
//! camera through the existing sinks (v4l2 / softcam / CMIO). Tauri-independent: the camera
//! handoff and the STUN lookup are injected, so the CLI and the integration test run the same
//! code with a fixed sink.
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
use tokio::sync::mpsc;

use super::media::{FrameOut, StreamWriter};
use super::rtc::{self, EncodedFrame, RtcClose, RtcCommand, RtcEvent, RtcOptions};
use super::signal::{StopReason, MAX_REMOTE_CANDIDATES};
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

/// Hands the virtual camera to the phone stream and back.
pub trait CameraControl: Send + Sync {
    /// Make the camera available and stop whatever else writes to it. Must remember what it
    /// changed as it goes, so `release` can undo a partially completed acquire. Runs to
    /// completion in its own task: it is never dropped half way.
    fn acquire(&self) -> BoxFuture<'_, Result<CameraTarget>>;
    /// Undo `acquire`. Always called once per session, also after a failed or cancelled
    /// acquire; must be a no-op when there is nothing to undo.
    fn release(&self) -> BoxFuture<'_, ()>;
}

/// A fixed sink with nothing to hand back (CLI with `--device` / `--null`, tests).
pub struct FixedCamera(pub CameraTarget);

impl CameraControl for FixedCamera {
    fn acquire(&self) -> BoxFuture<'_, Result<CameraTarget>> {
        let t = self.0.clone();
        Box::pin(async move { Ok(t) })
    }
    fn release(&self) -> BoxFuture<'_, ()> {
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
        }
    }
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
    Answer(String),
    IceConnected,
    /// The first frame reached ffmpeg.
    Live(MediaStats),
    /// Once a second while live.
    Stats(MediaStats),
    /// The session is over and the camera has been handed back.
    Closed { reason: StopReason, error: Option<String> },
}

/// Start a session task. It ends by sending exactly one `Closed`, after handing the camera
/// back — also when the session panics: otherwise the camera would stay claimed (video
/// playback refused until restart) and the state machine busy forever.
pub fn spawn_session(deps: Arc<SessionDeps>, events: mpsc::UnboundedSender<SessionEvent>) -> mpsc::UnboundedSender<SessionCommand> {
    let (tx, rx) = mpsc::unbounded_channel();
    let camera = deps.camera.clone();
    let session_events = events.clone();
    spawn_guarded(camera, events, async move { run_session(deps, rx, &session_events).await });
    tx
}

/// Run `session` to its outcome, hand the camera back, send `Closed` — whatever happens in
/// between, a panic included.
fn spawn_guarded(
    camera: Arc<dyn CameraControl>,
    events: mpsc::UnboundedSender<SessionEvent>,
    session: impl std::future::Future<Output = Outcome> + Send + 'static,
) {
    tokio::spawn(async move {
        let run = AssertUnwindSafe(session).catch_unwind().await;
        let (reason, error) = run.unwrap_or_else(|_| (StopReason::Error, Some("internal error in the phone stream".into())));
        if AssertUnwindSafe(camera.release()).catch_unwind().await.is_err() {
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
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Outcome {
    // --- 1. prepare (camera, STUN, sockets) while the phone sends its offer -------------
    // In its own task, never cancelled: dropping `acquire` between "started the camera" and
    // "noted that it must be stopped again" would leave the camera on with nothing to undo.
    let prep = deps.clone();
    let mut prepare = tokio::spawn(async move {
        let target = prep.camera.acquire().await?;
        let stun = prep.ice.stun_server().await;
        let transport = rtc::gather(&prep.rtc, stun).await?;
        Ok::<Prepared, anyhow::Error>((target, transport))
    });
    let (target, transport, offer, candidates) = match offer_and_camera(&deps, &mut prepare, &mut cmd_rx).await {
        Ok(ready) => ready,
        Err(outcome) => {
            // Stopped or timed out mid-acquire: let it finish so that `release` (the caller's
            // next step) undoes all of it rather than racing it.
            if !prepare.is_finished() {
                let _ = tokio::time::timeout(deps.prepare_timeout, &mut prepare).await;
            }
            return outcome;
        }
    };

    // --- 2. answer -------------------------------------------------------------------------
    let (answer, rtc_tx, mut rtc_rx) = match rtc::accept_offer(transport, &offer, deps.rtc.clone()) {
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

/// Phase 1: wait until the camera is ready *and* the offer has arrived. The two have separate
/// deadlines; a stop or a failure ends the wait with the session's outcome.
async fn offer_and_camera(
    deps: &SessionDeps,
    prepare: &mut tokio::task::JoinHandle<Result<Prepared>>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
) -> Result<(CameraTarget, rtc::LocalTransport, String, Vec<String>), Outcome> {
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
                Ok(Ok(p)) => prepared = Some(p),
                Ok(Err(e)) => return Err((StopReason::Error, Some(format!("{e:#}")))),
                Err(_) => return Err((StopReason::Error, Some("preparing the camera failed unexpectedly".into()))),
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SessionCommand::Offer(sdp)) => offer = Some(sdp),
                Some(SessionCommand::RemoteCandidate(c)) => {
                    if candidates.len() < MAX_REMOTE_CANDIDATES {
                        candidates.push(c);
                    }
                }
                Some(SessionCommand::Stop(r)) => return Err((r, None)),
                None => return Err((StopReason::Error, Some("session dropped".into()))),
            },
            _ = tokio::time::sleep_until(offer_deadline), if offer.is_none() => {
                return Err((StopReason::Timeout, Some("no offer from the phone".into())));
            }
            _ = tokio::time::sleep_until(prepare_deadline), if prepared.is_none() => {
                return Err((StopReason::Timeout, Some("preparing the camera took too long".into())));
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
    }

    impl ProbeCamera {
        fn new(panic_in_acquire: bool, acquire_delay: Duration) -> Arc<Self> {
            Arc::new(Self { acquired: AtomicUsize::new(0), released: AtomicUsize::new(0), panic_in_acquire, acquire_delay })
        }
    }

    impl CameraControl for ProbeCamera {
        fn acquire(&self) -> BoxFuture<'_, Result<CameraTarget>> {
            Box::pin(async move {
                tokio::time::sleep(self.acquire_delay).await;
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
        fn release(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                // A release that ran before the acquire finished would undo nothing.
                assert!(self.panic_in_acquire || self.acquired.load(Ordering::SeqCst) == 1, "release raced the acquire");
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
            let _tx = spawn_session(deps(camera.clone(), ice), ev_tx);
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
        spawn_guarded(camera.clone(), ev_tx, async { panic!("boom (expected in this test)") });
        let (reason, error) = closed(&mut ev_rx).await;
        assert_eq!((reason, error.as_deref()), (StopReason::Error, Some("internal error in the phone stream")));
        assert_eq!(camera.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_stop_during_a_slow_acquire_waits_for_it_before_releasing() {
        let camera = ProbeCamera::new(false, Duration::from_millis(400));
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let tx = spawn_session(deps(camera.clone(), Arc::new(NoStun)), ev_tx);
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
        let tx = spawn_session(Arc::new(d), ev_tx);
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
        let _tx = spawn_session(Arc::new(d), ev_tx);
        assert_eq!(closed(&mut ev_rx).await, (StopReason::Timeout, Some("no offer from the phone".into())));
    }
}
