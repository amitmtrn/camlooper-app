//! The stream manager: runs the [`Machine`] in one task, executes its actions, and exposes a
//! small API for the relay client (incoming frames) and the UI (decisions, stop, status).
//! Tauri-independent; the Tauri wiring lives in `commands.rs` and `client.rs`.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use super::machine::{Action, DecisionError, Machine, StreamRequest, StreamStatus};
use super::runner::{SessionCommand, SessionEvent};
use super::signal::{parse_signal, parse_stop, sanitize_label, OutboundFrame, StopReason, MAX_DEVICE_LABEL};

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
    fn start(&self, session_id: &str, events: mpsc::UnboundedSender<SessionEvent>) -> mpsc::UnboundedSender<SessionCommand>;
}

impl SessionBackend for Arc<super::runner::SessionDeps> {
    fn start(&self, _session_id: &str, events: mpsc::UnboundedSender<SessionEvent>) -> mpsc::UnboundedSender<SessionCommand> {
        super::runner::spawn_session(self.clone(), events)
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
    Session(String, SessionEvent),
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
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (status_tx, status_rx) = watch::channel(StreamStatus::default());
        tokio::spawn(run(rx, tx.downgrade(), backend, decisions, out_tx, status_tx));
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
) {
    let mut m = Machine::new();
    let mut session_tx: Option<(String, mpsc::UnboundedSender<SessionCommand>)> = None;
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
                            "peer_offline" => m.on_peer_offline(now),
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
                    Input::Session(id, ev) => {
                        let closed = matches!(ev, SessionEvent::Closed { .. });
                        let a = m.on_session_event(&id, ev, now);
                        if closed && session_tx.as_ref().is_some_and(|(s, _)| *s == id) {
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
                    eprintln!("phone stream: {} asks to stream its camera", req.device_name.as_deref().unwrap_or("the phone"));
                    decisions.request(req);
                }
                Action::DismissRequest(id) => decisions.dismissed(&id),
                Action::StartSession(id) => {
                    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
                    let cmd = backend.start(&id, ev_tx);
                    session_tx = Some((id.clone(), cmd));
                    let Some(me) = self_tx.upgrade() else { continue };
                    tokio::spawn(async move {
                        while let Some(ev) = ev_rx.recv().await {
                            if me.send(Input::Session(id.clone(), ev)).is_err() {
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
    use crate::remote::stream::signal::StreamSignal;
    use serde_json::json;
    use std::sync::Mutex;

    /// A backend whose sessions answer the offer and go live immediately.
    #[derive(Default)]
    struct FakeBackend {
        commands: Arc<Mutex<Vec<String>>>,
    }

    impl SessionBackend for FakeBackend {
        fn start(&self, _id: &str, events: mpsc::UnboundedSender<SessionEvent>) -> mpsc::UnboundedSender<SessionCommand> {
            let (tx, mut rx) = mpsc::unbounded_channel::<SessionCommand>();
            let log = self.commands.clone();
            tokio::spawn(async move {
                while let Some(cmd) = rx.recv().await {
                    log.lock().unwrap().push(format!("{cmd:?}"));
                    match cmd {
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

    #[test]
    fn seq_grows_with_every_published_change_and_only_then() {
        let mut published = StreamStatus::default();
        let mut m = Machine::new();
        let t = Instant::now();
        assert!(!publish(&mut published, m.status()), "nothing changed yet");
        assert_eq!(published.seq, 0);

        m.on_signal(StreamSignal::StreamRequest { session_id: "s".into(), device_label: None }, t);
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
}
