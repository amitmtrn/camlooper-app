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
//!
//! Things that are deliberately *not* immediate:
//! * the phone leaving the relay while **live** does not end the stream — media is peer to
//!   peer and a WebSocket flap is not a reason to drop it. The stream ends only when the phone
//!   is still offline after [`PEER_OFFLINE_GRACE`] *and* no frame arrived in that time;
//! * a declined (or ignored) request silences further requests for [`DECLINE_COOLDOWN`], so a
//!   phone cannot keep raising the window;
//! * a session that does not report `Closed` within [`STOPPING_WATCHDOG`] of being asked to
//!   stop is given up on, so the machine can never stay busy forever.
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::runner::{MediaStats, SessionCommand, SessionEvent};
use super::signal::{LinkState, OutboundFrame, RejectReason, StopData, StopReason, StreamSignal, MAX_REMOTE_CANDIDATES};

/// The user has this long to accept or decline before the request is auto-rejected.
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(60);
/// While live, a changed resolution/codec is reported to the phone at most this often.
const STATUS_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// After the user declines (or ignores) a request, new requests are declined without a prompt
/// for this long.
pub const DECLINE_COOLDOWN: Duration = Duration::from_secs(15);
/// While live, the phone may be gone from the relay this long before the stream is ended —
/// and then only if no frame arrived meanwhile.
pub const PEER_OFFLINE_GRACE: Duration = Duration::from_secs(10);
/// A session asked to stop must report `Closed` within this long.
pub const STOPPING_WATCHDOG: Duration = Duration::from_secs(10);
/// A second Stop from the user this long into a stop that has not finished gives up on the
/// session at once instead of waiting for the watchdog.
pub const FORCE_STOP_AFTER: Duration = Duration::from_secs(5);

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

/// What the UI shows. Serialized camelCase for the Tauri commands/events (`src/hooks/use-phone-stream.ts`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    /// Increases with every published change (set by the manager; always 0 inside the
    /// machine). Events and command replies reach the UI on different channels, so it uses
    /// this to drop a snapshot older than one it already applied.
    pub seq: u64,
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
    /// While `requested`: Unix milliseconds at which the request auto-declines. With
    /// `session_id` and `device_name` this is all the UI needs to show the confirm dialog.
    pub expires_at: Option<i64>,
    pub last_error: Option<String>,
    /// The stop reason behind `last_error` (`ice_failed`, `timeout`, `error`, …) so the UI can
    /// show translated, actionable text; `last_error` stays the technical detail for logs.
    pub last_error_code: Option<String>,
    /// The last session ended normally because the phone stopped it (its Stop button, or it
    /// closed the connection). The UI shows a neutral note, never an error.
    pub ended_by_phone: bool,
}

impl Default for StreamStatus {
    fn default() -> Self {
        Self {
            seq: 0,
            state: Phase::Idle,
            session_id: None,
            device_name: None,
            codec: None,
            width: None,
            height: None,
            fps: None,
            frames: 0,
            since: None,
            expires_at: None,
            last_error: None,
            last_error_code: None,
            ended_by_phone: false,
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
    /// Non-empty candidates taken from the phone so far (held or forwarded).
    candidates: usize,
    /// A stop has been requested; waiting for the session task to report `Closed`.
    stop_reason: Option<StopReason>,
    stop_requested_at: Option<Instant>,
    /// The pending stop came from the phone's `stream_stop`.
    stop_from_phone: bool,
    /// While live: when the phone was last seen leaving the relay, and the frame count then.
    peer_offline: Option<(Instant, u64)>,
    last_status: Option<(Option<String>, Option<u32>, Option<u32>)>,
    last_status_at: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Machine {
    status: StreamStatus,
    session: Option<Session>,
    /// The paired phone's name, used when a request carries no label.
    pub default_device_name: Option<String>,
    /// Requests are declined without asking until then (see [`DECLINE_COOLDOWN`]).
    quiet_until: Option<Instant>,
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
        if phase != Phase::Requested {
            self.status.expires_at = None;
        }
    }

    /// Back to idle (or error), forgetting the session.
    fn finish(&mut self, error: Option<(StopReason, String)>) {
        self.session = None;
        let state = if error.is_some() { Phase::Error } else { Phase::Idle };
        let (code, error) = error.map(|(r, e)| (r.as_str().to_string(), e)).unzip();
        self.status =
            StreamStatus { state, last_error: error, last_error_code: code, since: Some(wall_ms()), ..StreamStatus::default() };
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
                // The cap is per session, not per phase: the relay lets a phone send 300
                // frames a minute for as long as the stream lasts.
                if !candidate.is_empty() {
                    if s.candidates >= MAX_REMOTE_CANDIDATES {
                        return vec![];
                    }
                    s.candidates += 1;
                }
                if s.accepted {
                    vec![Action::ToSession(SessionCommand::RemoteCandidate(candidate))]
                } else {
                    s.early_candidates.push(candidate);
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
        if self.quiet_until.is_some_and(|t| now < t) {
            // The user just said no (or ignored the dialog). Do not raise the window again.
            return vec![signal(StreamSignal::StreamReject { session_id, reason: RejectReason::Declined })];
        }
        let received_at = wall_ms();
        let device_name = device_label.or_else(|| self.default_device_name.clone());
        self.session = Some(Session {
            id: session_id.clone(),
            requested_at: now,
            accepted: false,
            early_offer: None,
            early_candidates: Vec::new(),
            candidates: 0,
            stop_reason: None,
            stop_requested_at: None,
            stop_from_phone: false,
            peer_offline: None,
            last_status: None,
            last_status_at: None,
        });
        self.status = StreamStatus {
            session_id: Some(session_id.clone()),
            device_name: device_name.clone(),
            ..StreamStatus::default()
        };
        self.enter(Phase::Requested);
        let expires_at = received_at + DECISION_TIMEOUT.as_millis() as i64;
        self.status.expires_at = Some(expires_at);
        vec![Action::AskUser(StreamRequest { session_id, device_name, received_at, expires_at })]
    }

    /// `stream_stop` from the phone. Once the session is accepted the stop must name it: a
    /// bare stop, or a late one from an earlier session, must not end a stream that is
    /// running. Before the user decided, a stop without a (valid) `sessionId` still cancels
    /// the pending request — there is nothing to protect yet.
    pub fn on_remote_stop(&mut self, stop: StopData, now: Instant) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else { return vec![] };
        match stop.session_id.as_deref() {
            Some(id) if id != s.id => return vec![],
            None if s.accepted => return vec![],
            _ => {}
        }
        if s.stop_reason.is_none() {
            s.stop_from_phone = true;
        }
        self.end_without_telling_phone(stop.reason, now)
    }

    /// The phone left the relay (`peer{online:false}`, `hello{peerOnline:false}`, or a
    /// `peer_offline` error for something we sent). Before the stream is live that ends the
    /// attempt at once (signaling cannot complete). A live stream does not need the relay:
    /// it gets [`PEER_OFFLINE_GRACE`], see [`Self::tick`].
    pub fn on_peer_offline(&mut self, now: Instant) -> Vec<Action> {
        let frames = self.status.frames;
        let live = self.status.state == Phase::Live;
        let Some(s) = self.session.as_mut() else { return vec![] };
        if live && s.stop_reason.is_none() {
            s.peer_offline.get_or_insert((now, frames));
            return vec![];
        }
        self.end_without_telling_phone(StopReason::PeerOffline, now)
    }

    /// The phone is (back) on the relay.
    pub fn on_peer_online(&mut self, _now: Instant) -> Vec<Action> {
        if let Some(s) = self.session.as_mut() {
            s.peer_offline = None;
        }
        vec![]
    }

    fn end_without_telling_phone(&mut self, reason: StopReason, now: Instant) -> Vec<Action> {
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
        s.stop_requested_at = Some(now);
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

    pub fn reject(&mut self, session_id: &str, now: Instant) -> Result<Vec<Action>, DecisionError> {
        if !self.session.as_ref().is_some_and(|s| s.id == session_id && !s.accepted) {
            return Err(DecisionError::NoSuchRequest);
        }
        self.quiet_until = Some(now + DECLINE_COOLDOWN);
        self.finish(None);
        Ok(vec![signal(StreamSignal::StreamReject { session_id: session_id.to_string(), reason: RejectReason::Declined })])
    }

    /// Stop from the desktop side (the user's stop button, or the app shutting down).
    pub fn stop(&mut self, reason: StopReason, now: Instant) -> Vec<Action> {
        let Some(s) = self.session.as_mut() else { return vec![] };
        let id = s.id.clone();
        if !s.accepted {
            self.quiet_until = Some(now + DECLINE_COOLDOWN);
            self.finish(None);
            return vec![
                Action::DismissRequest(id.clone()),
                signal(StreamSignal::StreamReject { session_id: id, reason: RejectReason::Declined }),
            ];
        }
        if s.stop_reason.is_some() {
            // Already stopping. A repeated Stop after [`FORCE_STOP_AFTER`] is the user's way
            // out of a teardown that hangs: give up on the session now, as the watchdog
            // would a little later.
            if s.stop_requested_at.is_some_and(|t| now.duration_since(t) >= FORCE_STOP_AFTER) {
                eprintln!("phone stream: forced stop; giving up on a session that is still shutting down");
                self.finish(None);
            }
            return vec![];
        }
        s.stop_reason = Some(reason.clone());
        s.stop_requested_at = Some(now);
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
            self.quiet_until = Some(now + DECLINE_COOLDOWN);
            self.finish(None);
            return vec![
                Action::DismissRequest(id.clone()),
                signal(StreamSignal::StreamReject { session_id: id, reason: RejectReason::Timeout }),
            ];
        }
        // The session task was asked to stop and never reported back (stuck in a system
        // prompt, a hung driver call). Its `Closed` — if it ever comes — is for a session
        // that is no longer current and is ignored; the camera handoff refuses a new acquire
        // until the old one has been released.
        if s.stop_requested_at.is_some_and(|t| now.duration_since(t) >= STOPPING_WATCHDOG) {
            eprintln!("phone stream: the session did not shut down within {}s; giving up on it", STOPPING_WATCHDOG.as_secs());
            self.finish(None);
            return vec![];
        }
        // Live, and the phone has been off the relay for the whole grace period.
        if let Some((since, frames_then)) = s.peer_offline {
            if s.stop_reason.is_none() && now.duration_since(since) >= PEER_OFFLINE_GRACE {
                let frames = self.status.frames;
                if frames > frames_then {
                    // Video is still arriving peer to peer: the stream is healthy without
                    // the relay. Look again after another grace period.
                    if let Some(s) = self.session.as_mut() {
                        s.peer_offline = Some((now, frames));
                    }
                } else {
                    return self.end_without_telling_phone(StopReason::PeerOffline, now);
                }
            }
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
                let from_phone = s.stop_from_phone;
                let phone_offline = s.peer_offline.is_some();
                let id = s.id.clone();
                match requested {
                    // We (or the phone) asked for this stop; the phone already knows.
                    Some(_) => {
                        let failure = error.filter(|_| reason.is_failure()).map(|e| (reason, e));
                        let clean = failure.is_none();
                        self.finish(failure);
                        self.status.ended_by_phone = clean && from_phone;
                        vec![]
                    }
                    // The phone closed the connection and its `stream_stop` never came (or
                    // was not waited for). A normal ending; the phone knows.
                    None if reason == StopReason::PeerStopped => {
                        self.finish(None);
                        self.status.ended_by_phone = true;
                        vec![]
                    }
                    // The video dried up while the phone was off the relay: it went away
                    // (tab closed, phone asleep). Not an error, and nobody to tell.
                    None if phone_offline && reason == StopReason::Timeout => {
                        self.finish(None);
                        vec![]
                    }
                    // The session failed on its own (ICE, timeout, ffmpeg): tell the phone.
                    None => {
                        let message = error.unwrap_or_else(|| reason.to_string());
                        self.finish(Some((reason.clone(), message)));
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

        // Phone stops a live stream: no stream_stop echoed back, no error, and the status
        // says it was the phone's doing so the UI can show a neutral note.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        let a = m.on_remote_stop(StopData { session_id: Some("s1".into()), reason: StopReason::User }, t);
        assert_eq!(a, vec![Action::ToSession(SessionCommand::Stop(StopReason::User))]);
        assert!(sent(&a).is_empty());
        assert_eq!(m.status().state, Phase::Stopping);
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t);
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(m.status().last_error, None);
        assert!(m.status().ended_by_phone);

        // A stop from the desktop's own Stop button is not "the phone stopped".
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        m.stop(StopReason::User, t);
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t);
        assert!(!m.status().ended_by_phone);

        // Phone goes offline while connecting: signaling cannot finish, the attempt ends now.
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        m.accept("s1", t).unwrap();
        let a = m.on_peer_offline(t);
        assert_eq!(a, vec![Action::ToSession(SessionCommand::Stop(StopReason::PeerOffline))]);
        assert!(m.on_peer_offline(t).is_empty(), "only once");
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::PeerOffline, error: None }, t);
        assert_eq!(m.status().state, Phase::Idle);
        assert_eq!(m.status().last_error, None);
        assert!(!m.status().ended_by_phone);
    }

    #[test]
    fn the_phone_closing_the_connection_is_a_normal_end_not_an_error() {
        let t = Instant::now();
        // The DTLS close beat the relay's stream_stop and none followed within the grace
        // period: the session reports `peer_stopped` on its own.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        let a = m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::PeerStopped, error: None }, t);
        assert!(a.is_empty(), "nothing is sent to a phone that hung up: {a:?}");
        let st = m.status();
        assert_eq!((st.state, st.last_error.clone(), st.last_error_code.clone()), (Phase::Idle, None, None));
        assert!(st.ended_by_phone);
        // The relay's stop arriving late finds no session and does nothing.
        assert!(m.on_remote_stop(StopData { session_id: Some("s1".into()), reason: StopReason::User }, t).is_empty());
        assert_eq!(m.status().state, Phase::Idle);
        assert!(!StopReason::PeerStopped.is_failure());
        assert_eq!(StopReason::PeerStopped.as_str(), "peer_stopped");
    }

    #[test]
    fn a_stop_must_name_the_session_once_it_is_accepted() {
        let t = Instant::now();
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        // Bare, and from an earlier session: both ignored.
        assert!(m.on_remote_stop(StopData { session_id: None, reason: StopReason::User }, t).is_empty());
        assert!(m.on_remote_stop(StopData { session_id: Some("s0".into()), reason: StopReason::User }, t).is_empty());
        assert_eq!(m.status().state, Phase::Live);
        // Before the user decided, a bare stop still cancels the request.
        let mut m = Machine::new();
        m.on_signal(req("s2"), t);
        let a = m.on_remote_stop(StopData { session_id: None, reason: StopReason::User }, t);
        assert_eq!(a, vec![Action::DismissRequest("s2".into())]);
    }

    #[test]
    fn a_relay_flap_does_not_end_a_live_stream() {
        let t = Instant::now();
        let secs = Duration::from_secs;
        // Offline and back within the grace period: nothing happens.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        assert!(m.on_peer_offline(t).is_empty());
        assert_eq!(m.status().state, Phase::Live);
        assert!(m.tick(t + secs(9)).is_empty());
        m.on_peer_online(t + secs(9));
        assert!(m.tick(t + secs(60)).is_empty());
        assert_eq!(m.status().state, Phase::Live);

        // Still offline after the grace period, but video keeps arriving peer to peer: live.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        m.on_peer_offline(t);
        let mut later = stats(1280, 720);
        later.frames = 300;
        m.on_session_event("s1", SessionEvent::Stats(later.clone()), t + secs(9));
        assert!(m.tick(t + PEER_OFFLINE_GRACE).is_empty());
        assert_eq!(m.status().state, Phase::Live);
        // …and once the video stops as well, the next grace period ends it, without an error.
        let a = m.tick(t + PEER_OFFLINE_GRACE * 2);
        assert_eq!(a, vec![Action::ToSession(SessionCommand::Stop(StopReason::PeerOffline))]);
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::PeerOffline, error: None }, t + secs(21));
        assert_eq!((m.status().state, m.status().last_error.clone()), (Phase::Idle, None));

        // The phone vanished (tab closed): the media timeout fires while it is off the relay.
        // That is the phone going away, not a failure to report.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        m.on_peer_offline(t);
        let a = m.on_session_event(
            "s1",
            SessionEvent::Closed { reason: StopReason::Timeout, error: Some("no video from the phone".into()) },
            t + secs(10),
        );
        assert!(a.is_empty());
        assert_eq!((m.status().state, m.status().last_error.clone()), (Phase::Idle, None));
        // The same timeout with the phone on the relay is an error, as before.
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        let a = m.on_session_event(
            "s1",
            SessionEvent::Closed { reason: StopReason::Timeout, error: Some("no video from the phone".into()) },
            t,
        );
        assert_eq!(sent(&a), vec!["stop:timeout"]);
        assert_eq!(m.status().state, Phase::Error);
    }

    #[test]
    fn a_session_that_never_reports_closed_does_not_keep_the_machine_busy() {
        let t = Instant::now();
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        m.stop(StopReason::User, t);
        assert!(m.tick(t + STOPPING_WATCHDOG - Duration::from_secs(1)).is_empty());
        assert_eq!(m.status().state, Phase::Stopping);
        assert!(m.tick(t + STOPPING_WATCHDOG).is_empty());
        assert_eq!(m.status().state, Phase::Idle);
        // The late Closed belongs to no current session.
        assert!(m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t).is_empty());
        assert!(matches!(&m.on_signal(req("s2"), t)[..], [Action::AskUser(_)]));
    }

    #[test]
    fn declining_silences_new_requests_for_a_while() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        m.reject("s1", t).unwrap();
        // A new sessionId right away: declined without raising the dialog again.
        let a = m.on_signal(req("s2"), t + Duration::from_secs(1));
        assert_eq!(sent(&a), vec!["reject:Declined"]);
        assert!(!a.iter().any(|x| matches!(x, Action::AskUser(_))));
        assert_eq!(m.status().state, Phase::Idle);
        // After the cooldown the user is asked again.
        assert!(matches!(&m.on_signal(req("s3"), t + DECLINE_COOLDOWN)[..], [Action::AskUser(_)]));
        // An ignored dialog (60 s timeout) counts as a no, too.
        let t2 = t + DECLINE_COOLDOWN + DECISION_TIMEOUT;
        m.tick(t2);
        assert_eq!(sent(&m.on_signal(req("s4"), t2 + Duration::from_secs(1))), vec!["reject:Declined"]);
    }

    #[test]
    fn candidates_are_capped_per_session() {
        let t = Instant::now();
        let mut m = Machine::new();
        m.on_signal(req("s1"), t);
        // Some early, the rest after the accept: one budget for both.
        for i in 0..10 {
            m.on_signal(cand("s1", &format!("candidate:{i}")), t);
        }
        let a = m.accept("s1", t).unwrap();
        let held = a.iter().filter(|x| matches!(x, Action::ToSession(SessionCommand::RemoteCandidate(_)))).count();
        assert_eq!(held, 10);
        let mut forwarded = 0;
        for i in 0..200 {
            forwarded += m.on_signal(cand("s1", &format!("candidate:late{i}")), t).len();
        }
        assert_eq!(held + forwarded, MAX_REMOTE_CANDIDATES);
        // End-of-candidates is not a candidate and still passes.
        assert_eq!(m.on_signal(cand("s1", ""), t).len(), 1);
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
        assert_eq!(m.status().last_error_code.as_deref(), Some("ice_failed"));
        // An error state does not block the next request.
        assert!(matches!(&m.on_signal(req("s2"), t)[..], [Action::AskUser(_)]));
        assert_eq!(m.status().last_error, None);
        assert_eq!(m.status().last_error_code, None);
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
    fn the_status_alone_describes_a_pending_request() {
        let t = Instant::now();
        let mut m = Machine::new();
        let a = m.on_signal(req("s1"), t);
        let [Action::AskUser(r)] = &a[..] else { panic!("{a:?}") };
        let st = m.status().clone();
        assert_eq!((st.state, st.session_id.as_deref(), st.device_name.as_deref()), (Phase::Requested, Some("s1"), Some("Pixel")));
        assert_eq!(st.expires_at, Some(r.expires_at));
        assert_eq!(r.expires_at - r.received_at, DECISION_TIMEOUT.as_millis() as i64);
        // Leaving `requested` — accepted, declined, timed out — clears the deadline.
        m.accept("s1", t).unwrap();
        assert_eq!((m.status().state, m.status().expires_at), (Phase::Connecting, None));
        let mut m = Machine::new();
        m.on_signal(req("s2"), t);
        m.reject("s2", t).unwrap();
        assert_eq!((m.status().state, m.status().expires_at), (Phase::Idle, None));
    }

    #[test]
    fn a_second_stop_after_five_seconds_forces_a_hung_teardown() {
        let t = Instant::now();
        let mut m = Machine::new();
        live(&mut m, "s1", t);
        assert_eq!(sent(&m.stop(StopReason::User, t)), vec!["stop:user"]);
        assert_eq!(m.status().state, Phase::Stopping);
        // Too early: an impatient double click changes nothing.
        assert!(m.stop(StopReason::User, t + FORCE_STOP_AFTER - Duration::from_millis(1)).is_empty());
        assert_eq!(m.status().state, Phase::Stopping);
        // From 5 s on, Stop gives up on the session without waiting for the watchdog.
        assert!(m.stop(StopReason::User, t + FORCE_STOP_AFTER).is_empty());
        assert_eq!((m.status().state, m.status().last_error.clone()), (Phase::Idle, None));
        // The abandoned session's late `Closed` is ignored, and a new request is taken.
        m.on_session_event("s1", SessionEvent::Closed { reason: StopReason::User, error: None }, t + FORCE_STOP_AFTER);
        assert!(matches!(&m.on_signal(req("s2"), t + FORCE_STOP_AFTER)[..], [Action::AskUser(_)]));
    }

    #[test]
    fn default_device_name_is_used_without_a_label() {
        let t = Instant::now();
        let mut m = Machine { default_device_name: Some("Amit's Pixel".into()), ..Machine::default() };
        m.on_signal(StreamSignal::StreamRequest { session_id: "s".into(), device_label: None }, t);
        assert_eq!(m.status().device_name.as_deref(), Some("Amit's Pixel"));
    }
}
