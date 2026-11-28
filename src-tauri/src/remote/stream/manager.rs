//! The stream manager: runs the [`Machine`] in one task, executes its actions, and exposes a
//! small API for the relay client (incoming frames) and the UI (decisions, stop, status).
//! Tauri-independent; the Tauri wiring lives in `commands.rs` and `client.rs`.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use super::file_receive::{LoopPlayer, NoLoop, PhoneVideoStore, LOOP_RATE_LIMITED};
pub use super::file_receive::{LOOP_BUSY, LOOP_CAMERA_OFF};
use super::machine::{Action, DecisionError, Machine, StreamRequest, StreamStatus};
use super::runner::{SessionCommand, SessionEvent, SessionKind};
use super::signal::{
    clean_file_name, parse_signal, parse_stop, sanitize_label, LoopAction, OutboundFrame, RejectReason, SignalError, StopReason,
    StreamMode, StreamSignal, APP_PROTOCOL, MAX_DEVICE_LABEL, MAX_LOOP_ERROR,
};

/// Where "may this phone stream?" goes. The app's implementation (`commands::UiDecisions`)
/// shows a confirm dialog; the user's answer comes back through [`StreamManager::accept`] / [`StreamManager::reject`].
/// There is deliberately no auto-accept anywhere in the library: only the headless dev CLI
/// (`examples/phone_stream_cli.rs`) accepts on its own.
pub trait DecisionSink: Send + Sync {
    fn request(&self, req: StreamRequest);
    /// The pending request went away without a decision (timeout, phone cancelled/offline).
    fn dismissed(&self, session_id: &str);
}

/// Forwards requests to a channel (the CLI, tests, or an adapter for the UI).
pub struct ChannelDecisions(pub mpsc::UnboundedSender<DecisionEvent>);

#[derive(Debug, Clone, PartialEq)]
pub enum DecisionEvent {
    Request(StreamRequest),
    Dismissed(String),
}

impl DecisionSink for ChannelDecisions {
    fn request(&self, req: StreamRequest) {
        let _ = self.0.send(DecisionEvent::Request(req));
    }
    fn dismissed(&self, session_id: &str) {
        let _ = self.0.send(DecisionEvent::Dismissed(session_id.to_string()));
    }
}

/// Starts session tasks. The real one is [`super::runner::spawn_session`]; tests use a fake.
pub trait SessionBackend: Send + Sync {
    /// `epoch` is the machine's local id for this session task: unlike the phone-chosen
    /// `session_id` it never repeats, so it is what the camera handoff is keyed on.
    /// `kind` says whether this is the phone's camera or a video file to receive.
    fn start(
        &self,
        session_id: &str,
        epoch: u64,
        kind: SessionKind,
        events: mpsc::UnboundedSender<SessionEvent>,
    ) -> mpsc::UnboundedSender<SessionCommand>;

    /// Whether `mode: "meeting"` sessions can be started: this desktop has something that
    /// captures its sound (see [`super::meeting::MeetingSource`]).
    fn takes_meeting(&self) -> bool {
        false
    }
}

impl SessionBackend for Arc<super::runner::SessionDeps> {
    fn takes_meeting(&self) -> bool {
        self.meeting.is_some()
    }

    fn start(
        &self,
        _session_id: &str,
        epoch: u64,
        kind: SessionKind,
        events: mpsc::UnboundedSender<SessionEvent>,
    ) -> mpsc::UnboundedSender<SessionCommand> {
        super::runner::spawn_session(self.clone(), epoch, kind, events)
    }
}

/// The desktop's loop, for `loop_control` / `loop_status`.
#[derive(Clone)]
pub struct LoopDeps {
    pub player: Arc<dyn LoopPlayer>,
    /// Where phone videos are stored: a loaded video from there is `fromPhone`.
    pub store: Option<PhoneVideoStore>,
}

impl LoopDeps {
    /// Nothing to control (headless tools, tests): every status says "not playing".
    pub fn none() -> Self {
        Self { player: Arc::new(NoLoop), store: None }
    }
}

/// The phone may `stop` or `start` the loop this often; a faster one is answered with
/// [`LOOP_RATE_LIMITED`] and not acted on. (`status` only reads.)
pub const LOOP_ACTION_INTERVAL: Duration = Duration::from_secs(1);
/// `loop_control` requests waiting for the worker beyond this many are dropped.
const LOOP_QUEUE: usize = 16;
/// Refusals of requests this desktop cannot take (`too_large`, `unsupported`) are answered
/// at most this often; the rest are dropped unanswered. They cost nothing to provoke and
/// would otherwise be an unmetered way to make this machine send.
pub const MAX_REFUSALS: usize = 5;
pub const REFUSAL_WINDOW: Duration = Duration::from_secs(10);
/// `hello` is answered at most this often (a phone sends it once after pairing, and again
/// after a reconnect); the rest are dropped unanswered.
pub const MAX_HELLOS: usize = 5;
pub const HELLO_WINDOW: Duration = Duration::from_secs(10);

/// `hello_ok.features`: the request modes this desktop takes. `record` and `file` need a
/// place to put the video; `meeting` needs a capture source (Linux only, for now).
pub fn features(has_store: bool, meeting: bool) -> Vec<String> {
    let mut modes: Vec<&str> = if has_store { vec!["live", "record", "file"] } else { vec!["live"] };
    if meeting {
        modes.push("meeting");
    }
    modes.iter().map(|m| m.to_string()).collect()
}

/// At most `max` events per `window`.
#[derive(Debug)]
pub struct RateLimit {
    max: usize,
    window: Duration,
    recent: VecDeque<Instant>,
}

impl RateLimit {
    pub fn new(max: usize, window: Duration) -> Self {
        Self { max, window, recent: VecDeque::new() }
    }

    /// Whether one more event is allowed at `now`; counts it if so.
    pub fn allow(&mut self, now: Instant) -> bool {
        while self.recent.front().is_some_and(|t| now.saturating_duration_since(*t) >= self.window) {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.max {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

/// One `loop_status` to produce: after doing `action` (unless refused), or just reporting.
struct LoopJob {
    session_id: String,
    action: Option<LoopAction>,
    /// Decided when the request arrived: not to be acted on, answered with this error code.
    refusal: Option<&'static str>,
}

impl LoopJob {
    /// Nothing to do and nothing to say but the loop's state: a later answer says the same.
    fn only_reports(&self) -> bool {
        self.refusal.is_none() && matches!(self.action, None | Some(LoopAction::Status))
    }
}

/// Executes loop jobs one at a time, in order (a `stop` followed by a `start` must not
/// overtake each other), off the manager task: the loop engine's lock can be held for
/// seconds while a video loads.
///
/// `session_active` is the manager's current answer to "is a phone session active or still
/// shutting down?". It was already asked when the request arrived; the engine asks again
/// right before it acts, because the request may have waited here meanwhile.
async fn loop_worker(
    mut jobs: mpsc::Receiver<LoopJob>,
    deps: LoopDeps,
    out: mpsc::UnboundedSender<OutboundFrame>,
    session_active: Arc<AtomicBool>,
) {
    let mut batch: VecDeque<LoopJob> = VecDeque::new();
    loop {
        if batch.is_empty() {
            match jobs.recv().await {
                Some(job) => batch.push_back(job),
                None => break,
            }
        }
        while let Ok(job) = jobs.try_recv() {
            batch.push_back(job);
        }
        let Some(job) = batch.pop_front() else { continue };
        // Status requests that piled up are answered once: whatever is answered next
        // carries the loop's state too.
        if job.only_reports() && !batch.is_empty() {
            continue;
        }
        let refused = || session_active.load(Ordering::SeqCst);
        let result = match (job.refusal, job.action) {
            (Some(code), _) => Err(code.to_string()),
            (None, Some(action @ (LoopAction::Stop | LoopAction::Start))) => deps.player.control(action, &refused).await,
            (None, Some(LoopAction::Status) | None) => Ok(()),
        };
        let state = deps.player.state().await;
        let from_phone = match (&deps.store, &state.path) {
            (Some(store), Some(path)) => store.contains(path),
            _ => false,
        };
        let _ = out.send(OutboundFrame::Signal(StreamSignal::LoopStatus {
            session_id: job.session_id,
            playing: state.playing,
            name: state.name.as_deref().and_then(clean_file_name),
            from_phone,
            error: result.err().and_then(|e| sanitize_label(&e, MAX_LOOP_ERROR)),
        }));
    }
}

enum Input {
    Signal(Value),
    RemoteStop(Option<Value>),
    PeerOnline(bool),
    RelayError(String),
    Accept(String, oneshot::Sender<Result<(), DecisionError>>),
    Reject(String, oneshot::Sender<Result<(), DecisionError>>),
    Stop(StopReason, oneshot::Sender<()>),
    SetDeviceName(Option<String>),
    /// From the session task started with this epoch.
    Session(u64, SessionEvent),
}

#[derive(Clone)]
pub struct StreamManager {
    tx: mpsc::UnboundedSender<Input>,
    status: watch::Receiver<StreamStatus>,
}

impl StreamManager {
    /// Spawn the manager task (needs a tokio runtime). Frames for the relay come out of the
    /// returned receiver; the relay client sends them on its socket.
    pub fn spawn(
        backend: Arc<dyn SessionBackend>,
        decisions: Arc<dyn DecisionSink>,
    ) -> (StreamManager, mpsc::UnboundedReceiver<OutboundFrame>) {
        Self::spawn_with_loop(backend, decisions, LoopDeps::none())
    }

    /// [`Self::spawn`] with the desktop's loop engine, which `loop_control` acts on.
    pub fn spawn_with_loop(
        backend: Arc<dyn SessionBackend>,
        decisions: Arc<dyn DecisionSink>,
        loops: LoopDeps,
    ) -> (StreamManager, mpsc::UnboundedReceiver<OutboundFrame>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = watch::channel(StreamStatus::default());
        let (loop_tx, loop_rx) = mpsc::channel(LOOP_QUEUE);
        let session_active = Arc::new(AtomicBool::new(false));
        // Without a place to put them, videos are not accepted at all.
        let file_sessions = loops.store.is_some();
        let meeting_sessions = backend.takes_meeting();
        tokio::spawn(loop_worker(loop_rx, loops, out_tx.clone(), session_active.clone()));
        let wiring = Wiring { self_tx: tx.downgrade(), backend, decisions, out: out_tx, status: status_tx, loops: loop_tx, session_active, file_sessions, meeting_sessions };
        tokio::spawn(run(rx, wiring));
        (StreamManager { tx, status: status_rx }, out_rx)
    }

    /// `{"type":"signal","from":"mobile","data":…}` from the relay.
    pub fn on_relay_signal(&self, data: Value) {
        let _ = self.tx.send(Input::Signal(data));
    }

    /// `{"type":"stream_stop","from":"mobile","data"?:…}` from the relay.
    pub fn on_relay_stream_stop(&self, data: Option<Value>) {
        let _ = self.tx.send(Input::RemoteStop(data));
    }

    /// `peer{role:"mobile"}` and `hello.peerOnline`.
    pub fn on_peer_online(&self, online: bool) {
        let _ = self.tx.send(Input::PeerOnline(online));
    }

    /// `{"type":"error","error":…}` from the relay.
    pub fn on_relay_error(&self, code: String) {
        let _ = self.tx.send(Input::RelayError(code));
    }

    /// The paired phone's name, shown when a request carries no `deviceLabel`.
    pub fn set_device_name(&self, name: Option<String>) {
        let _ = self.tx.send(Input::SetDeviceName(name));
    }

    pub async fn accept(&self, session_id: &str) -> Result<(), DecisionError> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Input::Accept(session_id.to_string(), tx));
        rx.await.unwrap_or(Err(DecisionError::NoSuchRequest))
    }

    pub async fn reject(&self, session_id: &str) -> Result<(), DecisionError> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Input::Reject(session_id.to_string(), tx));
        rx.await.unwrap_or(Err(DecisionError::NoSuchRequest))
    }

    /// Stop whatever is happening (declines a pending request). Returns once the stop has
    /// been initiated; watch [`Self::subscribe`] for `idle`.
    pub async fn stop(&self, reason: StopReason) {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Input::Stop(reason, tx));
        let _ = rx.await;
    }

    pub fn status(&self) -> StreamStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<StreamStatus> {
        self.status.clone()
    }
}

/// What the manager task works with.
struct Wiring {
    self_tx: mpsc::WeakUnboundedSender<Input>,
    backend: Arc<dyn SessionBackend>,
    decisions: Arc<dyn DecisionSink>,
    out: mpsc::UnboundedSender<OutboundFrame>,
    status: watch::Sender<StreamStatus>,
    loops: mpsc::Sender<LoopJob>,
    /// For the loop worker: a session is accepted, or a given-up one is still shutting down.
    session_active: Arc<AtomicBool>,
    /// Whether `mode: "file"` requests are taken (there is a store for the videos).
    file_sessions: bool,
    /// Whether `mode: "meeting"` requests are taken (the backend can capture).
    meeting_sessions: bool,
}

async fn run(mut rx: mpsc::UnboundedReceiver<Input>, wiring: Wiring) {
    let Wiring { self_tx, backend, decisions, out, status, loops, session_active, file_sessions, meeting_sessions } = wiring;
    let mut m = Machine::new();
    let mut refusals = RateLimit::new(MAX_REFUSALS, REFUSAL_WINDOW);
    let mut hellos = RateLimit::new(MAX_HELLOS, HELLO_WINDOW);
    let mut last_loop_action: Option<Instant> = None;
    let queue = |job: LoopJob| {
        if loops.try_send(job).is_err() {
            log::debug!("phone stream: dropping a loop_control (too many are waiting)");
        }
    };
    let mut session_tx: Option<(u64, mpsc::UnboundedSender<SessionCommand>)> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let now = Instant::now();
        let actions = tokio::select! {
            input = rx.recv() => {
                let Some(input) = input else { break };
                match input {
                    Input::Signal(data) => match parse_signal(&data) {
                        // This desktop has nowhere to put a video (no app data directory).
                        Ok(StreamSignal::StreamRequest { session_id, mode: StreamMode::File | StreamMode::Record, .. }) if !file_sessions => {
                            eprintln!("phone stream: refusing a video from the phone (no place to store it)");
                            refusal(&mut refusals, now, session_id, RejectReason::Unsupported)
                        }
                        // This desktop cannot capture a meeting (not Linux, or a headless tool).
                        Ok(StreamSignal::StreamRequest { session_id, mode: StreamMode::Meeting, .. }) if !meeting_sessions => {
                            eprintln!("phone stream: refusing a meeting request (this computer cannot share a meeting)");
                            refusal(&mut refusals, now, session_id, RejectReason::Unsupported)
                        }
                        // "Which app protocol do you speak?" — answered whatever else is
                        // going on, with what this desktop takes.
                        Ok(StreamSignal::Hello { session_id, app_protocol }) => {
                            if hellos.allow(now) {
                                log::debug!("phone stream: hello from the phone (app protocol {app_protocol})");
                                vec![Action::Send(OutboundFrame::Signal(StreamSignal::HelloOk {
                                    session_id,
                                    app_protocol: APP_PROTOCOL,
                                    features: features(file_sessions, meeting_sessions),
                                }))]
                            } else {
                                log::debug!("phone stream: not answering a hello (too many lately)");
                                vec![]
                            }
                        }
                        Ok(sig) => m.on_signal(sig, now),
                        // A request this desktop cannot take (too large, not a video):
                        // answered at once, whatever else is going on — but not without end.
                        Err(SignalError::Refused { session_id, reason }) => {
                            eprintln!("phone stream: refusing a request from the phone ({reason:?})");
                            refusal(&mut refusals, now, session_id, reason)
                        }
                        Err(e) => {
                            eprintln!("phone stream: ignoring a signal from the phone ({e})");
                            vec![]
                        }
                    },
                    Input::RemoteStop(data) => m.on_remote_stop(parse_stop(data.as_ref()), now),
                    Input::PeerOnline(true) => m.on_peer_online(now),
                    Input::PeerOnline(false) => m.on_peer_offline(now),
                    Input::RelayError(code) => {
                        match code.as_str() {
                            "peer_offline" => m.on_peer_unreachable(now),
                            _ => {
                                eprintln!("phone stream: relay refused a frame ({code})");
                                vec![]
                            }
                        }
                    }
                    Input::Accept(id, reply) => {
                        let r = m.accept(&id, now);
                        let (res, actions) = split(r);
                        let _ = reply.send(res);
                        actions
                    }
                    Input::Reject(id, reply) => {
                        let r = m.reject(&id, now);
                        let (res, actions) = split(r);
                        let _ = reply.send(res);
                        actions
                    }
                    Input::Stop(reason, reply) => {
                        let a = m.stop(reason, now);
                        let _ = reply.send(());
                        a
                    }
                    Input::SetDeviceName(name) => {
                        // From the relay's `hello`: as untrusted as a request's label.
                        m.default_device_name = name.and_then(|n| sanitize_label(&n, MAX_DEVICE_LABEL));
                        vec![]
                    }
                    Input::Session(epoch, ev) => {
                        let closed = matches!(ev, SessionEvent::Closed { .. });
                        let a = m.on_session_event(epoch, ev, now);
                        if closed && session_tx.as_ref().is_some_and(|(e, _)| *e == epoch) {
                            session_tx = None;
                        }
                        a
                    }
                }
            }
            _ = tick.tick() => m.tick(now),
        };

        // Before any loop job of this round is queued: what the worker sees when it asks
        // again must be at least as new as what the machine just decided.
        session_active.store(m.loop_refused(), Ordering::SeqCst);
        for action in actions {
            match action {
                Action::Send(frame) => {
                    log::debug!("phone stream: → phone {}", frame.describe());
                    let _ = out.send(frame);
                }
                Action::AskUser(req) => {
                    let what = match req.mode {
                        StreamMode::File => "loop a video",
                        StreamMode::Record => "record a loop",
                        StreamMode::Live => "stream its camera",
                        StreamMode::Meeting => "hear and see the meeting",
                    };
                    eprintln!("phone stream: {} asks to {what}", req.device_name.as_deref().unwrap_or("the phone"));
                    decisions.request(req);
                }
                Action::DismissRequest(id) => decisions.dismissed(&id),
                Action::Loop { session_id, action, refused } => {
                    let acts = matches!(action, LoopAction::Stop | LoopAction::Start);
                    // `now` is taken before `select!` waits, so it can be up to a tick old; the
                    // rate limit must measure from when this request actually arrived.
                    let now = Instant::now();
                    let refusal = if refused {
                        Some(LOOP_BUSY)
                    } else if acts && last_loop_action.is_some_and(|t| now.saturating_duration_since(t) < LOOP_ACTION_INTERVAL) {
                        Some(LOOP_RATE_LIMITED)
                    } else {
                        None
                    };
                    if acts && refusal.is_none() {
                        last_loop_action = Some(now);
                    }
                    queue(LoopJob { session_id, action: Some(action), refusal });
                }
                Action::ReportLoop { session_id } => queue(LoopJob { session_id, action: None, refusal: None }),
                Action::StartSession { session_id, epoch, kind } => {
                    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
                    let cmd = backend.start(&session_id, epoch, kind, ev_tx);
                    session_tx = Some((epoch, cmd));
                    let Some(me) = self_tx.upgrade() else { continue };
                    tokio::spawn(async move {
                        while let Some(ev) = ev_rx.recv().await {
                            if me.send(Input::Session(epoch, ev)).is_err() {
                                break;
                            }
                        }
                    });
                }
                Action::ToSession(cmd) => match &session_tx {
                    Some((_, tx)) => {
                        let _ = tx.send(cmd);
                    }
                    None => log::debug!("phone stream: no session task for {cmd:?}"),
                },
            }
        }
        status.send_if_modified(|s| publish(s, m.status()));
    }
}

/// The answer to a request this desktop cannot take, or nothing once [`MAX_REFUSALS`] were
/// sent within [`REFUSAL_WINDOW`].
fn refusal(limit: &mut RateLimit, now: Instant, session_id: String, reason: RejectReason) -> Vec<Action> {
    if !limit.allow(now) {
        log::debug!("phone stream: not answering a refused request (too many lately)");
        return vec![];
    }
    vec![Action::Send(OutboundFrame::Signal(StreamSignal::StreamReject { session_id, reason }))]
}

/// Copy the machine's status into the published one if it differs, stamping the next `seq`.
/// The machine never touches `seq`; it only ever grows here, once per published change.
fn publish(published: &mut StreamStatus, current: &StreamStatus) -> bool {
    let next = StreamStatus { seq: published.seq, ..current.clone() };
    if *published == next {
        return false;
    }
    *published = StreamStatus { seq: published.seq + 1, ..next };
    true
}

fn split(r: Result<Vec<Action>, DecisionError>) -> (Result<(), DecisionError>, Vec<Action>) {
    match r {
        Ok(a) => (Ok(()), a),
        Err(e) => (Err(e), vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::stream::machine::Phase;
    use crate::remote::stream::runner::MediaStats;
    use crate::remote::stream::file_receive::testing::FakeLoop;
    use crate::remote::stream::machine::FilePhase;
    use crate::remote::stream::runner::{FileStage, RecordStage};
    use crate::remote::stream::signal::{StreamMode, StreamSignal};
    use serde_json::json;
    use std::sync::Mutex;

    /// A started session's epoch and its event channel.
    type Started = (u64, mpsc::UnboundedSender<SessionEvent>);

    /// A backend whose sessions answer the offer and go live immediately.
    #[derive(Default)]
    struct FakeBackend {
        commands: Arc<Mutex<Vec<String>>>,
        /// Sessions swallow `Stop` and never report `Closed` (a hung teardown)…
        hang_on_stop: bool,
        /// …until the test sends it for them: every started session's (epoch, events).
        started: Arc<Mutex<Vec<Started>>>,
        /// This desktop has a capture source for meeting sessions.
        meeting: bool,
    }

    impl SessionBackend for FakeBackend {
        fn takes_meeting(&self) -> bool {
            self.meeting
        }

        fn start(
            &self,
            _id: &str,
            epoch: u64,
            kind: SessionKind,
            events: mpsc::UnboundedSender<SessionEvent>,
        ) -> mpsc::UnboundedSender<SessionCommand> {
            let (tx, mut rx) = mpsc::unbounded_channel::<SessionCommand>();
            let log = self.commands.clone();
            log.lock().unwrap().push(format!("start {kind:?}"));
            let hang_on_stop = self.hang_on_stop;
            self.started.lock().unwrap().push((epoch, events.clone()));
            tokio::spawn(async move {
                while let Some(cmd) = rx.recv().await {
                    log.lock().unwrap().push(format!("{cmd:?}"));
                    match cmd {
                        // A file session: the whole transfer happens at once.
                        SessionCommand::Offer(_) if matches!(kind, SessionKind::File(_)) => {
                            let SessionKind::File(file) = &kind else { unreachable!() };
                            let _ = events.send(SessionEvent::Answer("v=0 answer".into()));
                            let _ = events.send(SessionEvent::FileProgress { received: 0 });
                            let _ = events.send(SessionEvent::FileProgress { received: file.size });
                            let _ = events.send(SessionEvent::FileStage(FileStage::Processing));
                            let _ = events.send(SessionEvent::FileStage(FileStage::Looping));
                            let _ = events.send(SessionEvent::Closed { reason: StopReason::Done, error: None });
                            return;
                        }
                        SessionCommand::Offer(_) => {
                            let _ = events.send(SessionEvent::Answer("v=0 answer".into()));
                            let _ = events.send(SessionEvent::Live(MediaStats {
                                codec: Some(crate::pipeline::EncodedCodec::Vp8),
                                width: Some(640),
                                height: Some(480),
                                fps: 30.0,
                                frames: 1,
                                decoder_restarts: 0,
                            }));
                            // A record session: the take starts right after the pre-roll.
                            if kind == SessionKind::Record {
                                let _ = events.send(SessionEvent::Record { stage: RecordStage::Recording, elapsed_ms: 0 });
                            }
                        }
                        SessionCommand::Stop(_) if hang_on_stop => {}
                        // A take of 5 s that is finished: processed, looping, done.
                        SessionCommand::Finish(_) if kind == SessionKind::Record => {
                            let _ = events.send(SessionEvent::Record { stage: RecordStage::Processing, elapsed_ms: 5000 });
                            let _ = events.send(SessionEvent::Record { stage: RecordStage::Looping, elapsed_ms: 5000 });
                            let _ = events.send(SessionEvent::Closed { reason: StopReason::Done, error: None });
                            return;
                        }
                        SessionCommand::Stop(reason) | SessionCommand::Finish(reason) => {
                            let _ = events.send(SessionEvent::Closed { reason, error: None });
                            return;
                        }
                        SessionCommand::RemoteCandidate(_) => {}
                    }
                }
            });
            tx
        }
    }

    async fn next_frame(out: &mut mpsc::UnboundedReceiver<OutboundFrame>) -> Value {
        let f = tokio::time::timeout(Duration::from_secs(2), out.recv()).await.unwrap().unwrap();
        serde_json::from_str(&f.to_json().unwrap()).unwrap()
    }

    async fn wait_state(m: &StreamManager, phase: Phase) {
        let mut rx = m.subscribe();
        tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|s| s.state == phase)).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn end_to_end_through_the_manager() {
        let backend = Arc::new(FakeBackend::default());
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(backend.clone(), Arc::new(ChannelDecisions(dtx)));

        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"abc","deviceLabel":"Pixel"}));
        let DecisionEvent::Request(req) = drx.recv().await.unwrap() else { panic!() };
        assert_eq!(req.session_id, "abc");
        assert_eq!(m.reject("other").await, Err(DecisionError::NoSuchRequest));
        m.accept("abc").await.unwrap();
        assert_eq!(next_frame(&mut out).await, json!({"type":"signal","data":{"kind":"stream_accept","sessionId":"abc"}}));
        assert_eq!(next_frame(&mut out).await["data"]["state"], "connecting");

        m.on_relay_signal(json!({"kind":"offer","sessionId":"abc","sdp":"v=0 offer"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"answer","sessionId":"abc","sdp":"v=0 answer"}));
        let live = next_frame(&mut out).await;
        assert_eq!(live["data"]["state"], "live");
        assert_eq!(live["data"]["codec"], "VP8");
        wait_state(&m, Phase::Live).await;

        // Garbage and a second phone are handled without disturbing the stream.
        m.on_relay_signal(json!("nonsense"));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"intruder"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"intruder","reason":"busy"}));

        // The phone's relay socket flaps: a live peer-to-peer stream is not touched.
        m.on_peer_online(false);
        m.on_peer_online(true);
        // A bare stop (no sessionId) cannot end an accepted session either.
        m.on_relay_stream_stop(None);
        m.on_relay_stream_stop(Some(json!({"sessionId":"older","reason":"user"})));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"intruder2"}));
        assert_eq!(next_frame(&mut out).await["data"]["reason"], "busy");
        assert_eq!(m.status().state, Phase::Live);

        // The phone's own stop ends it, quietly: idle, no error, flagged as the phone's doing.
        m.on_relay_stream_stop(Some(json!({"sessionId":"abc","reason":"user"})));
        wait_state(&m, Phase::Idle).await;
        let st = m.status();
        assert_eq!((st.last_error, st.ended_by_phone), (None, true));
        let log = backend.commands.lock().unwrap().clone();
        assert!(log.iter().any(|c| c.contains("Stop(User)")), "{log:?}");
        assert!(out.try_recv().is_err(), "no stream_stop is echoed to the phone");
    }

    /// The UI's forced stop (a second Stop 5 s into a hung one) must not let a second session
    /// start next to the first, which still holds the camera; and with the phone reusing its
    /// `sessionId`, the first session's late events must not be taken for the second's.
    #[tokio::test]
    async fn a_forced_stop_keeps_the_machine_busy_until_the_old_session_has_closed() {
        let backend = Arc::new(FakeBackend { hang_on_stop: true, ..FakeBackend::default() });
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(backend.clone(), Arc::new(ChannelDecisions(dtx)));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"same"}));
        drx.recv().await.unwrap();
        m.accept("same").await.unwrap();
        m.on_relay_signal(json!({"kind":"offer","sessionId":"same","sdp":"v=0"}));
        wait_state(&m, Phase::Live).await;

        m.stop(StopReason::User).await;
        wait_state(&m, Phase::Stopping).await;
        // The manager runs on the real clock: wait out the force-stop threshold.
        tokio::time::sleep(crate::remote::stream::machine::FORCE_STOP_AFTER + Duration::from_millis(100)).await;
        m.stop(StopReason::User).await;
        wait_state(&m, Phase::Error).await;
        assert_eq!(m.status().last_error.as_deref(), Some(crate::remote::stream::machine::STILL_RELEASING));

        while out.try_recv().is_ok() {}
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"same"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"same","reason":"busy"}));
        assert_eq!(backend.started.lock().unwrap().len(), 1, "no second session next to the first");

        // The old session finally reports in: free again, and the same id starts epoch 2.
        let (epoch, old_events) = backend.started.lock().unwrap()[0].clone();
        assert_eq!(epoch, 1);
        old_events.send(SessionEvent::Closed { reason: StopReason::User, error: None }).unwrap();
        wait_state(&m, Phase::Idle).await;
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"same"}));
        drx.recv().await.unwrap();
        m.accept("same").await.unwrap();
        wait_state(&m, Phase::Connecting).await;
        assert_eq!(backend.started.lock().unwrap().iter().map(|(e, _)| *e).collect::<Vec<_>>(), vec![1, 2]);
        // A stray event from the first session task does not touch the second session.
        let _ = old_events.send(SessionEvent::Closed { reason: StopReason::Error, error: Some("late".into()) });
        m.on_relay_signal(json!({"kind":"offer","sessionId":"same","sdp":"v=0"}));
        wait_state(&m, Phase::Live).await;
        assert_eq!(m.status().last_error, None);
    }

    #[test]
    fn seq_grows_with_every_published_change_and_only_then() {
        let mut published = StreamStatus::default();
        let mut m = Machine::new();
        let t = Instant::now();
        assert!(!publish(&mut published, m.status()), "nothing changed yet");
        assert_eq!(published.seq, 0);

        m.on_signal(StreamSignal::StreamRequest { session_id: "s".into(), device_label: None, mode: StreamMode::Live, file: None }, t);
        assert!(publish(&mut published, m.status()));
        assert_eq!((published.seq, published.state), (1, Phase::Requested));
        assert!(!publish(&mut published, m.status()), "same status, same seq");
        assert_eq!(published.seq, 1);

        // Back to an idle status that is field-for-field the default: still a newer seq, so
        // the UI can tell it from the idle it saw before the request.
        m.reject("s", t).unwrap();
        assert!(publish(&mut published, m.status()));
        assert_eq!((published.seq, published.state, published.expires_at), (2, Phase::Idle, None));
    }

    #[tokio::test]
    async fn the_paired_name_from_hello_is_sanitized() {
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, _out) = StreamManager::spawn(Arc::new(FakeBackend::default()), Arc::new(ChannelDecisions(dtx)));
        m.set_device_name(Some("Pixel\u{202e}\u{7}evil".into()));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"n"}));
        let DecisionEvent::Request(req) = drx.recv().await.unwrap() else { panic!() };
        assert_eq!(req.device_name.as_deref(), Some("Pixelevil"));
    }

    #[tokio::test]
    async fn user_stop_sends_stream_stop() {
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(Arc::new(FakeBackend::default()), Arc::new(ChannelDecisions(dtx)));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"s"}));
        drx.recv().await.unwrap();
        m.accept("s").await.unwrap();
        m.on_relay_signal(json!({"kind":"offer","sessionId":"s","sdp":"v=0"}));
        wait_state(&m, Phase::Live).await;
        m.stop(StopReason::User).await;
        wait_state(&m, Phase::Idle).await;
        let mut frames = vec![];
        while let Ok(f) = out.try_recv() {
            frames.push(f);
        }
        assert!(frames.contains(&OutboundFrame::StreamStop { session_id: "s".into(), reason: StopReason::User }), "{frames:?}");
        assert!(!frames.iter().any(|f| matches!(f, OutboundFrame::Signal(StreamSignal::StreamReject { .. }))));
    }

    fn with_loop(
        backend: Arc<FakeBackend>,
        player: Arc<FakeLoop>,
        store: &PhoneVideoStore,
    ) -> (StreamManager, mpsc::UnboundedReceiver<OutboundFrame>, mpsc::UnboundedReceiver<DecisionEvent>) {
        let (dtx, drx) = mpsc::unbounded_channel();
        let loops = LoopDeps { player, store: Some(store.clone()) };
        let (m, out) = StreamManager::spawn_with_loop(backend, Arc::new(ChannelDecisions(dtx)), loops);
        (m, out, drx)
    }

    #[tokio::test]
    async fn loop_control_stops_starts_and_reports_the_desktops_loop() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let from_phone = store.dir().join("phone-0123456789abcdef.mp4");
        let player = FakeLoop::loaded("holiday\u{202e}.mp4", &from_phone, true);
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), player.clone(), &store);

        // No session is opened for any of this.
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"status"}));
        assert_eq!(
            next_frame(&mut out).await,
            json!({"type":"signal","data":{"kind":"loop_status","sessionId":"c1","playing":true,"name":"holiday.mp4","fromPhone":true}})
        );
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c2","action":"stop"}));
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"loop_status","sessionId":"c2","playing":false,"name":"holiday.mp4","fromPhone":true})
        );
        // (The phone may act on the loop once a second.)
        tokio::time::sleep(LOOP_ACTION_INTERVAL + Duration::from_millis(50)).await;
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c3","action":"start"}));
        assert_eq!(next_frame(&mut out).await["data"]["playing"], true);
        assert_eq!(player.calls(), vec!["stop", "start"]);
        assert_eq!(m.status().state, Phase::Idle);
        // An action this desktop does not know is not acted on (and not answered).
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c4","action":"rewind"}));
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c5","action":"status"}));
        assert_eq!(next_frame(&mut out).await["data"]["sessionId"], "c5");

        // A video the user picked on the desktop is not `fromPhone`.
        let player = FakeLoop::loaded("desk.mp4", std::path::Path::new("/tmp/desk.mp4"), false);
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), player, &store);
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"status"}));
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"loop_status","sessionId":"c1","playing":false,"name":"desk.mp4","fromPhone":false})
        );

        // Nothing loaded: `start` is answered with an error, and still with the state.
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), Arc::new(FakeLoop::default()), &store);
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"start"}));
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"loop_status","sessionId":"c1","playing":false,"fromPhone":false,"error":"No video is loaded."})
        );
    }

    #[tokio::test]
    async fn loop_control_during_a_session_is_refused_with_an_error_status() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let player = FakeLoop::loaded("desk.mp4", std::path::Path::new("/tmp/desk.mp4"), true);
        let (m, mut out, mut drx) = with_loop(Arc::new(FakeBackend::default()), player.clone(), &store);
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"s"}));
        drx.recv().await.unwrap();
        m.accept("s").await.unwrap();
        m.on_relay_signal(json!({"kind":"offer","sessionId":"s","sdp":"v=0"}));
        wait_state(&m, Phase::Live).await;
        while out.try_recv().is_ok() {}

        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"stop"}));
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"loop_status","sessionId":"c1","playing":true,"name":"desk.mp4","fromPhone":false,"error":"busy"})
        );
        assert!(player.calls().is_empty(), "nothing was done to the loop");
        assert_eq!(m.status().state, Phase::Live);
    }

    #[tokio::test]
    async fn a_file_session_through_the_manager() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let backend = Arc::new(FakeBackend::default());
        let player = Arc::new(FakeLoop::default());
        let (m, mut out, mut drx) = with_loop(backend.clone(), player.clone(), &store);

        // Requests this desktop cannot take are answered without asking the user.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"big","mode":"file","file":{"name":"a.mp4","size":314572801u64,"mime":"video/mp4"}}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"big","reason":"too_large"}));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"pdf","mode":"file","file":{"name":"a.pdf","size":10,"mime":"application/pdf"}}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"pdf","reason":"unsupported"}));
        assert!(drx.try_recv().is_err());
        assert_eq!(m.status().state, Phase::Idle);

        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"f1","deviceLabel":"Pixel","mode":"file","file":{"name":"clip.mp4","size":2048,"mime":"video/mp4"}}));
        let DecisionEvent::Request(req) = drx.recv().await.unwrap() else { panic!() };
        assert_eq!((req.mode, req.file.as_ref().map(|f| (f.name.as_str(), f.size))), (StreamMode::File, Some(("clip.mp4", 2048))));
        let st = m.status();
        assert_eq!((st.state, st.mode, st.file.as_ref().map(|f| f.phase)), (Phase::Requested, StreamMode::File, Some(FilePhase::Pending)));
        // While it waits for an answer, a second request is busy.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"other"}));
        assert_eq!(next_frame(&mut out).await["data"]["reason"], "busy");

        m.accept("f1").await.unwrap();
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "stream_accept");
        assert_eq!(next_frame(&mut out).await["data"]["state"], "connecting");
        assert!(backend.commands.lock().unwrap()[0].starts_with("start File(FileOffer"), "the backend is told it is a file session");
        m.on_relay_signal(json!({"kind":"offer","sessionId":"f1","sdp":"v=0 offer"}));
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "answer");
        // The good ending: `done`, then the loop's state. Never an error.
        assert_eq!(next_frame(&mut out).await, json!({"type":"stream_stop","data":{"sessionId":"f1","reason":"done"}}));
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "loop_status");
        wait_state(&m, Phase::Idle).await;
        let st = m.status();
        assert_eq!((st.last_error, st.last_error_code, st.ended_by_phone), (None, None, false));
        let file = st.file.expect("the outcome stays in the status");
        assert_eq!((file.phase, file.received, file.size), (FilePhase::Looping, 2048, 2048));
        // And the machine is free for the next request.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"next"}));
        assert!(matches!(drx.recv().await.unwrap(), DecisionEvent::Request(_)));
    }

    #[test]
    fn a_rate_limit_allows_so_many_per_window() {
        let t = Instant::now();
        let ms = Duration::from_millis;
        let mut limit = RateLimit::new(2, Duration::from_secs(10));
        assert!(limit.allow(t) && limit.allow(t + ms(1)));
        assert!(!limit.allow(t + ms(2)) && !limit.allow(t + ms(9_999)));
        // The first one leaves the window: one more, and only one.
        assert!(limit.allow(t + ms(10_000)));
        assert!(!limit.allow(t + ms(10_000)));
        assert!(limit.allow(t + ms(10_001)));
    }

    /// M2: `start` and `stop` at most once a second; `status` is not an action.
    #[tokio::test]
    async fn loop_actions_from_the_phone_are_rate_limited() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let player = FakeLoop::loaded("desk.mp4", std::path::Path::new("/tmp/desk.mp4"), true);
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), player.clone(), &store);
        assert_eq!(LOOP_ACTION_INTERVAL, Duration::from_secs(1));

        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"stop"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"loop_status","sessionId":"c1","playing":false,"name":"desk.mp4","fromPhone":false}));
        // Hammering start/stop: answered with the state and an error, not acted on.
        for (id, action) in [("c2", "start"), ("c3", "stop"), ("c4", "start")] {
            m.on_relay_signal(json!({"kind":"loop_control","sessionId":id,"action":action}));
            assert_eq!(
                next_frame(&mut out).await["data"],
                json!({"kind":"loop_status","sessionId":id,"playing":false,"name":"desk.mp4","fromPhone":false,"error":"rate_limited"})
            );
        }
        assert_eq!(player.calls(), vec!["stop"]);
        // Asking is always fine.
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c5","action":"status"}));
        assert_eq!(next_frame(&mut out).await["data"].get("error"), None);
        // A second later the next action goes through.
        tokio::time::sleep(LOOP_ACTION_INTERVAL + Duration::from_millis(50)).await;
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c6","action":"start"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"loop_status","sessionId":"c6","playing":true,"name":"desk.mp4","fromPhone":false}));
        assert_eq!(player.calls(), vec!["stop", "start"]);
    }

    /// M2: the phone can start the loop on a camera that is running, but cannot make the
    /// computer set one up (that can be a polkit or UAC prompt).
    #[tokio::test]
    async fn a_start_from_the_phone_never_turns_the_camera_on() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let player = FakeLoop { camera_off: true, ..FakeLoop::default() };
        *player.state.lock().unwrap() =
            crate::remote::stream::LoopState { playing: false, name: Some("desk.mp4".into()), path: Some("/tmp/desk.mp4".into()) };
        let player = Arc::new(player);
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), player.clone(), &store);
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"start"}));
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"loop_status","sessionId":"c1","playing":false,"name":"desk.mp4","fromPhone":false,"error":"camera_off"})
        );
        assert!(player.calls().is_empty(), "start was never called: {:?}", player.calls());
    }

    fn job(id: &str, action: Option<LoopAction>) -> LoopJob {
        LoopJob { session_id: id.into(), action, refusal: None }
    }

    /// M1: the refusal is decided again when the job runs. A session that was accepted
    /// while the request waited in the queue is seen, and nothing is done to the loop.
    #[tokio::test]
    async fn a_queued_loop_action_is_refused_if_a_session_became_active_meanwhile() {
        let player = FakeLoop::loaded("desk.mp4", std::path::Path::new("/tmp/desk.mp4"), true);
        let (jobs, rx) = mpsc::channel(LOOP_QUEUE);
        let (out_tx, mut out) = mpsc::unbounded_channel();
        let active = Arc::new(AtomicBool::new(false));
        // Queued while idle (so not refused then)…
        jobs.send(job("c1", Some(LoopAction::Stop))).await.unwrap();
        jobs.send(job("c2", Some(LoopAction::Start))).await.unwrap();
        // …and a session is accepted before the worker gets to them.
        active.store(true, Ordering::SeqCst);
        tokio::spawn(loop_worker(rx, LoopDeps { player: player.clone(), store: None }, out_tx, active.clone()));
        for id in ["c1", "c2"] {
            assert_eq!(
                next_frame(&mut out).await["data"],
                json!({"kind":"loop_status","sessionId":id,"playing":true,"name":"desk.mp4","fromPhone":false,"error":"busy"})
            );
        }
        assert!(player.calls().is_empty(), "nothing was done to the loop");
        // The session is over: the next one acts.
        active.store(false, Ordering::SeqCst);
        jobs.send(job("c3", Some(LoopAction::Stop))).await.unwrap();
        assert_eq!(next_frame(&mut out).await["data"].get("error"), None);
        assert_eq!(player.calls(), vec!["stop"]);
    }

    /// M2: status requests that pile up are answered once; actions and refusals never drop.
    #[tokio::test]
    async fn piled_up_status_requests_are_answered_once() {
        let player = FakeLoop::loaded("desk.mp4", std::path::Path::new("/tmp/desk.mp4"), true);
        let (jobs, rx) = mpsc::channel(LOOP_QUEUE);
        let (out_tx, mut out) = mpsc::unbounded_channel();
        for id in ["s1", "s2", "s3"] {
            jobs.try_send(job(id, Some(LoopAction::Status))).ok().unwrap();
        }
        jobs.try_send(job("report", None)).ok().unwrap();
        jobs.try_send(job("stop", Some(LoopAction::Stop))).ok().unwrap();
        jobs.try_send(LoopJob { session_id: "late".into(), action: Some(LoopAction::Start), refusal: Some(LOOP_RATE_LIMITED) }).ok().unwrap();
        jobs.try_send(job("s4", Some(LoopAction::Status))).ok().unwrap();
        jobs.try_send(job("s5", Some(LoopAction::Status))).ok().unwrap();
        // The queue is bounded: one more than it holds is not taken.
        for n in 0..LOOP_QUEUE {
            let _ = jobs.try_send(job(&format!("x{n}"), Some(LoopAction::Status)));
        }
        assert!(jobs.try_send(job("over", Some(LoopAction::Status))).is_err());
        tokio::spawn(loop_worker(rx, LoopDeps { player: player.clone(), store: None }, out_tx, Arc::new(AtomicBool::new(false))));
        let mut answered = vec![];
        loop {
            let frame = next_frame(&mut out).await;
            let id = frame["data"]["sessionId"].as_str().unwrap().to_string();
            let last = id == format!("x{}", LOOP_QUEUE - 8 - 1);
            answered.push((id, frame["data"]["playing"].as_bool().unwrap(), frame["data"]["error"].as_str().map(str::to_string)));
            if last {
                break;
            }
        }
        assert_eq!(
            answered,
            vec![
                ("stop".to_string(), false, None),
                ("late".to_string(), false, Some("rate_limited".to_string())),
                (format!("x{}", LOOP_QUEUE - 8 - 1), false, None),
            ]
        );
        assert_eq!(player.calls(), vec!["stop"]);
    }

    /// L1: without a place to store them, videos are refused when asked, not after Accept.
    #[tokio::test]
    async fn file_requests_are_refused_when_there_is_no_store() {
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(Arc::new(FakeBackend::default()), Arc::new(ChannelDecisions(dtx)));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"f1","mode":"file","file":{"name":"a.mp4","size":10,"mime":"video/mp4"}}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"f1","reason":"unsupported"}));
        assert!(drx.try_recv().is_err(), "the user is not asked");
        assert_eq!(m.status().state, Phase::Idle);
        // The camera stream does not need the store.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"live"}));
        assert!(matches!(drx.recv().await.unwrap(), DecisionEvent::Request(_)));
    }

    /// L6: refusals are cheap to provoke; only so many are answered.
    #[tokio::test]
    async fn refusals_of_bad_requests_are_throttled() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), Arc::new(FakeLoop::default()), &store);
        for n in 0..(MAX_REFUSALS + 20) {
            m.on_relay_signal(json!({"kind":"stream_request","sessionId":format!("big{n}"),"mode":"file","file":{"name":"a.mp4","size":314572801u64,"mime":"video/mp4"}}));
        }
        // Something that is always answered, to know the manager is past all of them.
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"marker","action":"status"}));
        let mut rejects = 0;
        loop {
            let frame = next_frame(&mut out).await;
            match frame["data"]["kind"].as_str() {
                Some("stream_reject") => rejects += 1,
                Some("loop_status") => break,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(rejects, MAX_REFUSALS);
    }

    #[tokio::test]
    async fn hello_is_answered_with_what_this_desktop_takes() {
        // With a place for videos: live, record and file.
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let (m, mut out, _drx) = with_loop(Arc::new(FakeBackend::default()), Arc::new(FakeLoop::default()), &store);
        m.on_relay_signal(json!({"kind":"hello","sessionId":"h1","appProtocol":3}));
        assert_eq!(
            next_frame(&mut out).await,
            json!({"type":"signal","data":{"kind":"hello_ok","sessionId":"h1","appProtocol":3,"features":["live","record","file"]}})
        );
        assert_eq!(m.status().state, Phase::Idle, "a hello is not a session");
        // Malformed ones are dropped; the rest are answered, but not without end.
        m.on_relay_signal(json!({"kind":"hello","sessionId":"h1"}));
        m.on_relay_signal(json!({"kind":"hello","sessionId":"h1","appProtocol":"3"}));
        for i in 0..(MAX_HELLOS * 2) {
            m.on_relay_signal(json!({"kind":"hello","sessionId":format!("h{i}"),"appProtocol":4}));
        }
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"after","action":"status"}));
        let mut answered = 1;
        loop {
            let f = next_frame(&mut out).await;
            if f["data"]["kind"] == "loop_status" {
                break;
            }
            assert_eq!((f["data"]["kind"].as_str(), f["data"]["appProtocol"].as_u64()), (Some("hello_ok"), Some(3)));
            answered += 1;
        }
        assert_eq!(answered, MAX_HELLOS);

        // Without one: live only — the phone must not offer Record.
        let (dtx, _drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(Arc::new(FakeBackend::default()), Arc::new(ChannelDecisions(dtx)));
        m.on_relay_signal(json!({"kind":"hello","sessionId":"h1","appProtocol":3}));
        assert_eq!(next_frame(&mut out).await["data"]["features"], json!(["live"]));
        // …and a record request that comes anyway is refused, not taken for live.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"r1","mode":"record"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"r1","reason":"unsupported"}));
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(features(true, false), vec!["live", "record", "file"]);
        assert_eq!(features(true, true), vec!["live", "record", "file", "meeting"]);
        assert_eq!(features(false, true), vec!["live", "meeting"]);
    }

    #[tokio::test]
    async fn meeting_is_offered_and_taken_only_where_the_desktop_can_capture() {
        // No capture source (Windows, macOS, a headless tool): not in hello_ok, and a
        // request that comes anyway is refused — never taken for a live camera.
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(Arc::new(FakeBackend::default()), Arc::new(ChannelDecisions(dtx)));
        m.on_relay_signal(json!({"kind":"hello","sessionId":"h1","appProtocol":3}));
        assert_eq!(next_frame(&mut out).await["data"]["features"], json!(["live"]));
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"m1","mode":"meeting"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"m1","reason":"unsupported"}));
        assert_eq!(m.status().state, Phase::Idle);
        assert!(drx.try_recv().is_err(), "nobody was asked");

        // With one: offered under the same app protocol, asked for, started as a meeting.
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let backend = Arc::new(FakeBackend { meeting: true, ..FakeBackend::default() });
        let player = FakeLoop::loaded("holiday.mp4", &store.dir().join("phone-00000000000000aa.mp4"), true);
        let (m, mut out, mut drx) = with_loop(backend.clone(), player.clone(), &store);
        m.on_relay_signal(json!({"kind":"hello","sessionId":"h2","appProtocol":3}));
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"hello_ok","sessionId":"h2","appProtocol":3,"features":["live","record","file","meeting"]})
        );
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"m2","deviceLabel":"Pixel","mode":"meeting"}));
        let DecisionEvent::Request(req) = drx.recv().await.unwrap() else { panic!() };
        assert_eq!((req.mode, req.file.clone()), (StreamMode::Meeting, None));
        assert_eq!(serde_json::to_value(&req).unwrap()["mode"], "meeting");
        m.accept("m2").await.unwrap();
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "stream_accept");
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"status","sessionId":"m2","state":"connecting","mode":"meeting"}));
        m.on_relay_signal(json!({"kind":"offer","sessionId":"m2","sdp":"v=0\r\n"}));
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "answer");
        assert_eq!(next_frame(&mut out).await["data"]["mode"], "meeting");
        wait_state(&m, Phase::Live).await;
        assert_eq!(m.status().mode, StreamMode::Meeting);
        assert_eq!(serde_json::to_value(m.status()).unwrap()["mode"], "meeting");
        assert!(backend.commands.lock().unwrap().contains(&"start Meeting".to_string()));

        // The loop is still the phone's to stop while it listens to the meeting.
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"stop"}));
        let status = next_frame(&mut out).await;
        assert_eq!(status["data"]["kind"], "loop_status");
        assert_eq!(status["data"]["playing"], false);
        assert!(status["data"].get("error").is_none(), "{status}");

        m.stop(StopReason::User).await;
        assert_eq!(next_frame(&mut out).await, json!({"type":"stream_stop","data":{"sessionId":"m2","reason":"user"}}));
        wait_state(&m, Phase::Idle).await;
    }

    #[tokio::test]
    async fn a_record_session_through_the_manager() {
        let store = PhoneVideoStore::in_app_data(std::path::Path::new("/app-data"));
        let backend = Arc::new(FakeBackend::default());
        let player = FakeLoop::loaded("Phone recording.mp4", &store.dir().join("phone-00000000000000aa.mp4"), true);
        let (m, mut out, mut drx) = with_loop(backend.clone(), player, &store);

        // A mode nobody knows is answered `unsupported`; it never becomes a live session.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"x1","mode":"timelapse"}));
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"stream_reject","sessionId":"x1","reason":"unsupported"}));

        // No hello first: the request itself is unambiguous.
        m.on_relay_signal(json!({"kind":"stream_request","sessionId":"r1","deviceLabel":"Pixel","mode":"record"}));
        let DecisionEvent::Request(req) = drx.recv().await.unwrap() else { panic!() };
        assert_eq!((req.mode, req.file.clone(), req.device_name.as_deref()), (StreamMode::Record, None, Some("Pixel")));
        m.accept("r1").await.unwrap();
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "stream_accept");
        assert_eq!(next_frame(&mut out).await["data"], json!({"kind":"status","sessionId":"r1","state":"connecting","mode":"record"}));
        m.on_relay_signal(json!({"kind":"offer","sessionId":"r1","sdp":"v=0 offer"}));
        assert_eq!(next_frame(&mut out).await["data"]["kind"], "answer");
        // Live (the pre-roll), then the take starts.
        let live = next_frame(&mut out).await;
        assert_eq!((live["data"]["state"].as_str(), live["data"]["mode"].as_str(), live["data"].get("record")), (Some("live"), Some("record"), None));
        assert_eq!(next_frame(&mut out).await["data"]["record"], json!({"elapsedMs":0,"capMs":60000,"phase":"recording"}));
        wait_state(&m, Phase::Live).await;
        let log = backend.commands.lock().unwrap().clone();
        assert_eq!(log[0], "start Record", "a record session, not a live one: {log:?}");

        // What the window gets while the take runs.
        let v = serde_json::to_value(m.status()).unwrap();
        assert_eq!((v["state"].as_str(), v["mode"].as_str()), (Some("live"), Some("record")));
        assert_eq!(v["record"], json!({"elapsedMs":0,"capMs":60000,"phase":"recording"}));
        assert_eq!(v["deviceName"], "Pixel");

        // The phone stops: the take is finished, processed, loops; the phone hears `done`
        // and what the loop is doing.
        m.on_relay_stream_stop(Some(json!({"sessionId":"r1","reason":"user"})));
        let mut kinds = vec![];
        loop {
            let f = next_frame(&mut out).await;
            if f["type"] == "stream_stop" {
                assert_eq!(f["data"], json!({"sessionId":"r1","reason":"done"}));
                break;
            }
            kinds.push(f["data"]["record"]["phase"].as_str().unwrap_or("?").to_string());
        }
        assert!(kinds.contains(&"processing".to_string()), "{kinds:?}");
        assert_eq!(
            next_frame(&mut out).await["data"],
            json!({"kind":"loop_status","sessionId":"r1","playing":true,"name":"Phone recording.mp4","fromPhone":true})
        );
        wait_state(&m, Phase::Idle).await;
        let st = m.status();
        assert_eq!((st.mode, st.last_error.clone(), st.ended_by_phone), (StreamMode::Record, None, false));
        assert_eq!(serde_json::to_value(&st).unwrap()["record"], json!({"elapsedMs":5000,"capMs":60000,"phase":"looping"}));
        let log = backend.commands.lock().unwrap().clone();
        assert!(log.iter().any(|c| c == "Finish(User)") && !log.iter().any(|c| c.starts_with("Stop(")), "{log:?}");

        // With the session over, the phone steers the loop as after a file.
        m.on_relay_signal(json!({"kind":"loop_control","sessionId":"c1","action":"stop"}));
        assert_eq!(next_frame(&mut out).await["data"]["playing"], false);
    }
}
