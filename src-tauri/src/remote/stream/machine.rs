//! The phone-stream state machine. Pure: inputs in, actions out, no I/O and no clock of its
//! own (`now` is passed in), so every transition — busy, timeouts, stale session ids — is
//! unit-tested without a network, a phone or ffmpeg. [`super::manager`] runs it in a task and
//! executes the actions.
//!
//! ```text
//!   idle/error ──stream_request──▶ requested ──accept──▶ connecting ──first frame──▶ live
//!        ▲            (user decides, 60 s)  │                 │                         │
//!        │                       reject/timeout               └──── stop / failure ─────┤
//!        └──────────────────────────────────┴──────────────── stopping ◀───────────────┘
//! ```
//! One session at a time: a second `stream_request` while busy is answered `busy`. Every
//! message whose `sessionId` is not the current session's is ignored.
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::runner::{MediaStats, SessionCommand, SessionEvent};
use super::signal::{LinkState, OutboundFrame, RejectReason, StopData, StopReason, StreamSignal};

/// The user has this long to accept or decline before the request is auto-rejected.
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
/// While live, a changed resolution/codec is reported to the phone at most this often.
const STATUS_MIN_INTERVAL: Duration = Duration::from_secs(2);
const MAX_EARLY_CANDIDATES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Requested,
    Connecting,
    Live,
    Stopping,
    Error,
}

impl Phase {
    /// Whether a new request can be considered.
    pub fn is_free(self) -> bool {
        matches!(self, Phase::Idle | Phase::Error)
    }
}

/// What the UI shows. Serialized camelCase for Task B's Tauri commands/events.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    pub state: Phase,
    pub session_id: Option<String>,
    /// The phone's label from `stream_request` (falls back to the paired phone's name).
    pub device_name: Option<String>,
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f32>,
    pub frames: u64,
    /// Unix milliseconds when `state` was entered.
    pub since: Option<i64>,
    pub last_error: Option<String>,
}

impl Default for StreamStatus {
    fn default() -> Self {
        Self {
            state: Phase::Idle,
            session_id: None,
            device_name: None,
            codec: None,
            width: None,
            height: None,
            fps: None,
            frames: 0,
            since: None,
            last_error: None,
        }
    }
}

/// A request awaiting the user's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamRequest {
    pub session_id: String,
    pub device_name: Option<String>,
    /// Unix milliseconds; the request auto-rejects 60 s after this.
    pub received_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(OutboundFrame),
    /// Show the confirm dialog.
    AskUser(StreamRequest),
    /// Close the confirm dialog (timed out, phone cancelled, phone went offline).
    DismissRequest(String),
    /// Start the session task for this id (camera handoff + receiver).
    StartSession(String),
    ToSession(SessionCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionError {
    /// No pending request with that session id (it expired, was cancelled, or is stale).
    NoSuchRequest,
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no pending stream request with that id")
    }
}

#[derive(Debug)]
struct Session {
    id: String,
    requested_at: Instant,
    accepted: bool,
    early_offer: Option<String>,
    early_candidates: Vec<String>,
    /// A stop has been requested; waiting for the session task to report `Closed`.
    stop_reason: Option<StopReason>,
    last_status: Option<(Option<String>, Option<u32>, Option<u32>)>,
    last_status_at: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Machine {
    status: StreamStatus,
    session: Option<Session>,
    /// The paired phone's name, used when a request carries no label.
    pub default_device_name: Option<String>,
}

fn wall_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn signal(sig: StreamSignal) -> Action {
    Action::Send(OutboundFrame::Signal(sig))
}

impl Machine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self) -> &StreamStatus {
        &self.status
    }

    pub fn current_session(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.id.as_str())
    }

    fn enter(&mut self, phase: Phase) {
        if self.status.state != phase {
            self.status.state = phase;
            self.status.since = Some(wall_ms());
        }
    }

    /// Back to idle (or error), forgetting the session.
    fn finish(&mut self, error: Option<String>) {
        self.session = None;
        let state = if error.is_some() { Phase::Error } else { Phase::Idle };
        self.status = StreamStatus { state, last_error: error, since: Some(wall_ms()), ..StreamStatus::default() };
    }

    fn is_current(&self, id: &str) -> bool {
        self.session.as_ref().is_some_and(|s| s.id == id)
    }

    // --- inputs from the phone ---------------------------------------------------------

    pub fn on_signal(&mut self, sig: StreamSignal, now: Instant) -> Vec<Action> {
        match sig {
            StreamSignal::StreamRequest { session_id, device_label } => self.on_request(session_id, device_label, now),
            StreamSignal::Offer { session_id, sdp } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session_id) else {
                    return vec![];
                };
                if s.stop_reason.is_some() {
                    return vec![];
                }
                if s.accepted {
                    vec![Action::ToSession(SessionCommand::Offer(sdp))]
                } else {
                    // Sent before the user decided; hold it until they accept.
                    s.early_offer = Some(sdp);
                    vec![]
                }
            }
            StreamSignal::Candidate { session_id, candidate, .. } => {
                let Some(s) = self.session.as_mut().filter(|s| s.id == session_id) else {
                    return vec![];
                };
                if s.stop_reason.is_some() {
                    return vec![];
                }
                if s.accepted {
                    vec![Action::ToSession(SessionCommand::RemoteCandidate(candidate))]
                } else {
                    if s.early_candidates.len() < MAX_EARLY_CANDIDATES {
                        s.early_candidates.push(candidate);
                    }
                    vec![]
                }
            }
            // Desktop → phone kinds coming from the phone: ignore.
            StreamSignal::StreamAccept { .. }
            | StreamSignal::StreamReject { .. }
            | StreamSignal::Answer { .. }
            | StreamSignal::Status { .. } => vec![],
        }
    }

    fn on_request(&mut self, session_id: String, device_label: Option<String>, now: Instant) -> Vec<Action> {
        if self.is_current(&session_id) {
            return vec![]; // a retransmitted request
        }
        if !self.status.state.is_free() {
            return vec![signal(StreamSignal::StreamReject { session_id, reason: RejectReason::Busy })];
        }
        let received_at = wall_ms();
        let device_name = device_label.or_else(|| self.default_device_name.clone());
        self.session = Some(Session {
            id: session_id.clone(),
            requested_at: now,
            accepted: false,
            early_offer: None,
            early_candidates: Vec::new(),
            stop_reason: None,
            last_status: None,
            last_status_at: None,
        });
        self.status = StreamStatus {
            session_id: Some(session_id.clone()),
            device_name: device_name.clone(),
            ..StreamStatus::default()
        };
        self.enter(Phase::Requested);
        vec![Action::AskUser(StreamRequest {
            session_id,
            device_name,
            received_at,
            expires_at: received_at + DECISION_TIMEOUT.as_millis() as i64,
        })]
    }

    /// `stream_stop` from the phone. A stop without a (valid) sessionId applies to the
    /// current session: there is only ever one.
    pub fn on_remote_stop(&mut self, stop: StopData, _now: Instant) -> Vec<Action> {
        let Some(current) = self.current_session().map(str::to_string) else {
            return vec![];
        };
        if stop.session_id.as_deref().is_some_and(|id| id != current) {
            return vec![];
        }
        self.end_without_telling_phone(stop.reason)
    }

    /// The phone left the relay (`peer{online:false}`, `hello{peerOnline:false}`, or a
    /// `peer_offline` error for something we sent).
    pub fn on_peer_offline(&mut self, _now: Instant) -> Vec<Action> {
        if self.session.is_none() {
            return vec![];
        }
        self.end_without_telling_phone(StopReason::PeerOffline)
    }

    fn end_without_telling_phone(&mut self, reason: StopReason) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else { return vec![] };
        if !s.accepted {
            let id = s.id.clone();
            self.finish(None);
            return vec![Action::DismissRequest(id)];
        }
        if s.stop_reason.is_some() {
            return vec![];
        }
        s.stop_reason = Some(reason.clone());
        self.enter(Phase::Stopping);
        vec![Action::ToSession(SessionCommand::Stop(reason))]
    }

    // --- inputs from the user ----------------------------------------------------------

    pub fn accept(&mut self, session_id: &str, _now: Instant) -> Result<Vec<Action>, DecisionError> {
        let Some(s) = self.session.as_mut().filter(|s| s.id == session_id && !s.accepted) else {
            return Err(DecisionError::NoSuchRequest);
        };
        if self.status.state != Phase::Requested {
            return Err(DecisionError::NoSuchRequest);
        }
        s.accepted = true;
        let id = s.id.clone();
        let mut actions = vec![
            signal(StreamSignal::StreamAccept { session_id: id.clone() }),
            signal(StreamSignal::Status {
                session_id: id.clone(),
                state: LinkState::Connecting,
                width: None,
                height: None,
                fps: None,
                codec: None,
            }),
            Action::StartSession(id),
        ];
        if let Some(offer) = s.early_offer.take() {
            actions.push(Action::ToSession(SessionCommand::Offer(offer)));
        }
        for c in s.early_candidates.drain(..) {
            actions.push(Action::ToSession(SessionCommand::RemoteCandidate(c)));
        }
        self.enter(Phase::Connecting);
        Ok(actions)
    }

    pub fn reject(&mut self, session_id: &str, _now: Instant) -> Result<Vec<Action>, DecisionError> {
        if !self.session.as_ref().is_some_and(|s| s.id == session_id && !s.accepted) {
            return Err(DecisionError::NoSuchRequest);
        }
        self.finish(None);
        Ok(vec![signal(StreamSignal::StreamReject { session_id: session_id.to_string(), reason: RejectReason::Declined })])
    }

    /// Stop from the desktop side (the user's stop button, or the app shutting down).
    pub fn stop(&mut self, reason: StopReason, _now: Instant) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else { return vec![] };
        let id = s.id.clone();
        if !s.accepted {
            self.finish(None);
            return vec![
                Action::DismissRequest(id.clone()),
                signal(StreamSignal::StreamReject { session_id: id, reason: RejectReason::Declined }),
            ];
        }
        if s.stop_reason.is_some() {
            return vec![];
        }
        s.stop_reason = Some(reason.clone());
        self.enter(Phase::Stopping);
        vec![
            Action::Send(OutboundFrame::StreamStop { session_id: id, reason: reason.clone() }),
            Action::ToSession(SessionCommand::Stop(reason)),
        ]
    }

    // --- time --------------------------------------------------------------------------

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let Some(s) = self.session.as_ref() else { return vec![] };
        if !s.accepted && now.duration_since(s.requested_at) >= DECISION_TIMEOUT {
            let id = s.id.clone();
            self.finish(None);
            return vec![
                Action::DismissRequest(id.clone()),
                signal(StreamSignal::StreamReject { session_id: id, reason: RejectReason::Timeout }),
            ];
        }
        vec![]
    }

    // --- inputs from the session task --------------------------------------------------

    pub fn on_session_event(&mut self, session_id: &str, ev: SessionEvent, now: Instant) -> Vec<Action> {
        if !self.is_current(session_id) {
            return vec![];
        }
        match ev {
            SessionEvent::Answer(sdp) => {
                if self.session.as_ref().is_some_and(|s| s.stop_reason.is_some()) {
                    return vec![];
                }
                vec![signal(StreamSignal::Answer { session_id: session_id.to_string(), sdp })]
            }
            SessionEvent::IceConnected => vec![],
            SessionEvent::Live(stats) => {
                if self.session.as_ref().is_some_and(|s| s.stop_reason.is_some()) {
                    return vec![];
                }
                self.apply_stats(&stats);
                self.enter(Phase::Live);
                self.status_update(now, true)
            }
            SessionEvent::Stats(stats) => {
                self.apply_stats(&stats);
                if self.status.state == Phase::Live {
                    self.status_update(now, false)
                } else {
                    vec![]
                }
            }
            SessionEvent::Closed { reason, error } => {
                let s = self.session.as_ref().expect("is_current checked");
                let requested = s.stop_reason.clone();
                let id = s.id.clone();
                match requested {
                    // We (or the phone) asked for this stop; the phone already knows.
                    Some(_) => {
                        self.finish(error.filter(|_| reason.is_failure()));
                        vec![]
                    }
                    // The session failed on its own (ICE, timeout, ffmpeg): tell the phone.
                    None => {
                        let message = error.unwrap_or_else(|| reason.to_string());
                        self.finish(Some(message));
                        vec![Action::Send(OutboundFrame::StreamStop { session_id: id, reason })]
                    }
                }
            }
        }
    }

    fn apply_stats(&mut self, stats: &MediaStats) {
        self.status.codec = stats.codec.map(|c| c.name().to_string());
        self.status.width = stats.width;
        self.status.height = stats.height;
        self.status.fps = Some((stats.fps * 10.0).round() / 10.0);
        self.status.frames = stats.frames;
    }

    /// A `status: live` for the phone when it is the first, or when codec/size changed (rate
    /// limited so a flapping resolution cannot eat the relay's 300 frames/minute).
    fn status_update(&mut self, now: Instant, force: bool) -> Vec<Action> {
        let st = &self.status;
        let key = (st.codec.clone(), st.width, st.height);
        let Some(s) = self.session.as_mut() else { return vec![] };
        let changed = s.last_status.as_ref() != Some(&key);
        let allowed = s.last_status_at.is_none_or(|t| now.duration_since(t) >= STATUS_MIN_INTERVAL);
        if !(force || (changed && allowed)) {
            return vec![];
        }
        s.last_status = Some(key);
        s.last_status_at = Some(now);
        vec![signal(StreamSignal::Status {
            session_id: s.id.clone(),
            state: LinkState::Live,
            width: st.width,
            height: st.height,
            fps: st.fps.map(|f| f.round() as u32),
            codec: st.codec.clone(),
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::EncodedCodec;

    fn req(id: &str) -> StreamSignal {
        StreamSignal::StreamRequest { session_id: id.into(), device_label: Some("Pixel".into()) }
    }
    fn offer(id: &str) -> StreamSignal {
        StreamSignal::Offer { session_id: id.into(), sdp: "v=0".into() }
    }
    fn cand(id: &str, c: &str) -> StreamSignal {
        StreamSignal::Candidate { session_id: id.into(), candidate: c.into(), sdp_mid: None, sdp_m_line_index: None }
    }
    fn stats(w: u32, h: u32) -> MediaStats {
        MediaStats { codec: Some(EncodedCodec::H264), width: Some(w), height: Some(h), fps: 29.7, frames: 10, decoder_restarts: 0 }
    }
    fn sent(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(OutboundFrame::Signal(s)) => Some(match s {
                    StreamSignal::StreamReject { reason, .. } => format!("reject:{reason:?}"),
                    StreamSignal::Status { state, .. } => format!("status:{state:?}"),
                    other => other.kind().to_string(),
                }),
                Action::Send(OutboundFrame::StreamStop { reason, .. }) => Some(format!("stop:{reason}")),
                _ => None,
            })
            .collect()
    }
    fn live(m: &mut Machine, id: &str, t: Instant) {
        m.on_signal(req(id), t);
        m.accept(id, t).unwrap();
        m.on_signal(offer(id), t);
        m.on_session_event(id, SessionEvent::Live(stats(1280, 720)), t);
        assert_eq!(m.status().state, Phase::Live);
    }

    #[test]
    fn happy_path() {
        let t = Instant::now();
        let mut m = Machine::new();
        let a = m.on_signal(req("s1"), t);
        assert!(matches!(&a[..], [Action::AskUser(r)] if r.session_id == "s1" && r.device_name.as_deref() == Some("Pixel")));
        assert_eq!(m.status().state, Phase::Requested);

        let a = m.accept("s1", t).unwrap();
        assert_eq!(sent(&a), vec!["stream_accept", "status:Connecting"]);
        assert!(a.contains(&Action::StartSession("s1".into())));
        assert_eq!(m.status().state, Phase::Connecting);

        assert_eq!(m.on_signal(offer("s1"), t), vec![Action::ToSession(SessionCommand::Offer("v=0".into()))]);
        assert_eq!(
            m.on_signal(cand("s1", "candidate:1"), t),
            vec![Action::ToSession(SessionCommand::RemoteCandidate("candidate:1".into()))]
        );
        assert_eq!(sent(&m.on_session_event("s1", SessionEvent::Answer("v=0 a".into()), t)), vec!["answer"]);

        let a = m.on_session_event("s1", SessionEvent::Live(stats(1280, 720)), t);
        assert_eq!(sent(&a), vec!["status:Live"]);
        let st = m.status();
        assert_eq!(st.state, Phase::Live);
        assert_eq!((st.width, st.height, st.codec.as_deref()), (Some(1280), Some(720), Some("H264")));
        assert_eq!(st.device_name.as_deref(), Some("Pixel"));

        let a = m.stop(StopReason::User, t);
        assert_eq!(sent(&a), vec!["stop:user"]);
        assert!(a.contains(&Action::ToSession(SessionCommand::Stop(StopReason::User))));
        assert_eq!(m.status().state, Phase::Stopping);
        assert!(m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t).is_empty());
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(m.current_session(), None);
    }

    #[test]
    fn second_request_while_busy_is_rejected_busy() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        assert_eq!(sent(&m.on_signal(req("s2"), t)), vec!["reject:Busy"]);
        // A retransmission of the current request is not "busy".
        assert!(m.on_signal(req("s1"), t).is_empty());
        m.accept("s1", t).unwrap();
        assert_eq!(sent(&m.on_signal(req("s2"), t)), vec!["reject:Busy"]);
        m.stop(StopReason::User, t);
        assert_eq!(sent(&m.on_signal(req("s3"), t)), vec!["reject:Busy"], "still stopping");
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t);
        assert!(matches!(&m.on_signal(req("s3"), t)[..], [Action::AskUser(_)]));
    }

    #[test]
    fn undecided_request_times_out_after_60s() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        assert!(m.tick(t + Duration::from_secs(59)).is_empty());
        let a = m.tick(t + DECISION_TIMEOUT);
        assert_eq!(sent(&a), vec!["reject:Timeout"]);
        assert!(a.contains(&Action::DismissRequest("s1".into())));
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(m.accept("s1", t + DECISION_TIMEOUT), Err(DecisionError::NoSuchRequest));
    }

    #[test]
    fn decline() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        assert_eq!(m.reject("nope", t), Err(DecisionError::NoSuchRequest));
        assert_eq!(sent(&m.reject("s1", t).unwrap()), vec!["reject:Declined"]);
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(m.reject("s1", t), Err(DecisionError::NoSuchRequest));
    }

    #[test]
    fn stale_session_ids_are_ignored() {
        let t = Instant::now();
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        assert!(m.on_signal(offer("old"), t).is_empty());
        assert!(m.on_signal(cand("old", "candidate:1"), t).is_empty());
        assert!(m.on_remote_stop(StopData { session_id: Some("old".into()), reason: StopReason::User }, t).is_empty());
        assert!(m.on_session_event("old", SessionEvent::Closed { reason: StopReason::Error, error: None }, t).is_empty());
        assert_eq!(m.accept("old", t), Err(DecisionError::NoSuchRequest));
        assert_eq!(m.status().state, Phase::Live);
        // Accepting twice is not possible either.
        assert_eq!(m.accept("s1", t), Err(DecisionError::NoSuchRequest));
    }

    #[test]
    fn offer_and_candidates_before_accept_are_held() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        assert!(m.on_signal(offer("s1"), t).is_empty());
        assert!(m.on_signal(cand("s1", "candidate:a"), t).is_empty());
        let a = m.accept("s1", t).unwrap();
        let start = a.iter().position(|x| *x == Action::StartSession("s1".into())).unwrap();
        assert_eq!(a[start + 1], Action::ToSession(SessionCommand::Offer("v=0".into())));
        assert_eq!(a[start + 2], Action::ToSession(SessionCommand::RemoteCandidate("candidate:a".into())));
    }

    #[test]
    fn phone_stop_and_peer_offline() {
        let t = Instant::now();
        // Phone cancels before the user decided: the dialog closes, nothing is sent.
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        let a = m.on_remote_stop(StopData { session_id: Some("s1".into()), reason: StopReason::User }, t);
        assert_eq!(a, vec![Action::DismissRequest("s1".into())]);
        assert_eq!(m.status().state, Phase::Idle);

        // Phone stops a live stream: no stream_stop echoed back.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        let a = m.on_remote_stop(StopData { session_id: None, reason: StopReason::User }, t);
        assert_eq!(a, vec![Action::ToSession(SessionCommand::Stop(StopReason::User))]);
        assert!(sent(&a).is_empty());
        assert_eq!(m.status().state, Phase::Stopping);
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t);
        assert_eq!(m.status().state, Phase::Idle);

        // Phone goes offline mid-stream.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        let a = m.on_peer_offline(t);
        assert_eq!(a, vec![Action::ToSession(SessionCommand::Stop(StopReason::PeerOffline))]);
        assert!(m.on_peer_offline(t).is_empty(), "only once");
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::PeerOffline, error: None }, t);
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(m.status().last_error, None);
    }

    #[test]
    fn session_failure_tells_the_phone_and_records_the_error() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        m.accept("s1", t).unwrap();
        m.on_signal(offer("s1"), t);
        let a = m.on_session_event(
            "s1",
            SessionEvent::Closed { reason: StopReason::IceFailed, error: Some("no ICE connection".into()) },
            t,
        );
        assert_eq!(sent(&a), vec!["stop:ice_failed"]);
        assert_eq!(m.status().state, Phase::Error);
        assert_eq!(m.status().last_error.as_deref(), Some("no ICE connection"));
        // An error state does not block the next request.
        assert!(matches!(&m.on_signal(req("s2"), t)[..], [Action::AskUser(_)]));
        assert_eq!(m.status().last_error, None);
    }

    #[test]
    fn status_updates_on_resolution_change_are_rate_limited() {
        let t = Instant::now();
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        // Same size: nothing.
        assert!(m.on_session_event("s1", SessionEvent::Stats(stats(1280, 720)), t + Duration::from_secs(5)).is_empty());
        // New size right away after the live status: held back…
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        assert!(m.on_session_event("s1", SessionEvent::Stats(stats(640, 360)), t + Duration::from_millis(500)).is_empty());
        assert_eq!(m.status().width, Some(640), "local status still updates");
        // …and sent once the interval has passed.
        let a = m.on_session_event("s1", SessionEvent::Stats(stats(640, 360)), t + Duration::from_secs(3));
        assert_eq!(sent(&a), vec!["status:Live"]);
    }

    #[test]
    fn user_stop_before_decision_declines() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        let a = m.stop(StopReason::User, t);
        assert_eq!(sent(&a), vec!["reject:Declined"]);
        assert_eq!(m.status().state, Phase::Idle);
        assert!(m.stop(StopReason::User, t).is_empty());
    }

    #[test]
    fn default_device_name_is_used_without_a_label() {
        let t = Instant::now();
        let mut m = Machine { default_device_name: Some("Amit's Pixel".into()), ..Machine::default() };
        m.on_signal(StreamSignal::StreamRequest { session_id: "s".into(), device_label: None }, t);
        assert_eq!(m.status().device_name.as_deref(), Some("Amit's Pixel"));
    }
}
