//! The stream manager: runs the [`Machine`] in one task, executes its actions, and exposes a
//! small API for the relay client (incoming frames) and the UI (decisions, stop, status).
//! Tauri-independent; the Tauri wiring lives in `commands.rs` and `client.rs`.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use super::file_receive::{LoopPlayer, NoLoop, PhoneVideoStore};
use super::machine::{Action, DecisionError, Machine, StreamRequest, StreamStatus};
use super::runner::{SessionCommand, SessionEvent, SessionKind};
use super::signal::{
    clean_file_name, parse_signal, parse_stop, sanitize_label, LoopAction, OutboundFrame, SignalError, StopReason, StreamSignal,
    MAX_DEVICE_LABEL, MAX_LOOP_ERROR,
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
}

impl SessionBackend for Arc<super::runner::SessionDeps> {
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

/// `loop_status.error` when a `loop_control` came while a phone session was active.
pub const LOOP_BUSY: &str = "busy";

/// One `loop_status` to produce: after doing `action` (unless refused), or just reporting.
struct LoopJob {
    session_id: String,
    action: Option<LoopAction>,
    refused: bool,
}

/// Executes loop jobs one at a time, in order (a `stop` followed by a `start` must not
/// overtake each other), off the manager task: the loop engine's lock can be held for
/// seconds while a video loads.
async fn loop_worker(mut jobs: mpsc::UnboundedReceiver<LoopJob>, deps: LoopDeps, out: mpsc::UnboundedSender<OutboundFrame>) {
    while let Some(job) = jobs.recv().await {
        let result = match (job.refused, job.action) {
            (true, _) => Err(LOOP_BUSY.to_string()),
            (false, Some(LoopAction::Stop)) => deps.player.stop().await.map_err(|e| e.to_string()),
            (false, Some(LoopAction::Start)) => deps.player.start().await.map_err(|e| e.to_string()),
            (false, Some(LoopAction::Status) | None) => Ok(()),
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
        let (loop_tx, loop_rx) = mpsc::unbounded_channel();
        tokio::spawn(loop_worker(loop_rx, loops, out_tx.clone()));
        tokio::spawn(run(rx, tx.downgrade(), backend, decisions, out_tx, status_tx, loop_tx));
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

async fn run(
    mut rx: mpsc::UnboundedReceiver<Input>,
    self_tx: mpsc::WeakUnboundedSender<Input>,
    backend: Arc<dyn SessionBackend>,
    decisions: Arc<dyn DecisionSink>,
    out: mpsc::UnboundedSender<OutboundFrame>,
    status: watch::Sender<StreamStatus>,
    loops: mpsc::UnboundedSender<LoopJob>,
) {
    let mut m = Machine::new();
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
                        Ok(sig) => m.on_signal(sig, now),
                        // A request this desktop cannot take (too large, not a video):
                        // answered at once, whatever else is going on.
                        Err(SignalError::Refused { session_id, reason }) => {
                            eprintln!("phone stream: refusing a request from the phone ({reason:?})");
                            vec![Action::Send(OutboundFrame::Signal(StreamSignal::StreamReject { session_id, reason }))]
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

        for action in actions {
            match action {
                Action::Send(frame) => {
                    log::debug!("phone stream: → phone {}", frame.describe());
                    let _ = out.send(frame);
                }
                Action::AskUser(req) => {
                    let what = if req.file.is_some() { "loop a video" } else { "stream its camera" };
                    eprintln!("phone stream: {} asks to {what}", req.device_name.as_deref().unwrap_or("the phone"));
                    decisions.request(req);
                }
                Action::DismissRequest(id) => decisions.dismissed(&id),
                Action::Loop { session_id, action, refused } => {
                    let _ = loops.send(LoopJob { session_id, action: Some(action), refused });
                }
                Action::ReportLoop { session_id } => {
                    let _ = loops.send(LoopJob { session_id, action: None, refused: false });
                }
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
    use crate::remote::stream::runner::FileStage;
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
    }

    impl SessionBackend for FakeBackend {
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
                        }
                        SessionCommand::Stop(_) if hang_on_stop => {}
                        SessionCommand::Stop(reason) => {
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
}
