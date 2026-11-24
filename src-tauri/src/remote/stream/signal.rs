//! App-level signaling for the phone camera stream, carried inside the relay's
//! `{"type":"signal","data":{…}}` frames (and `stream_stop.data`). This module is the single
//! definition of that protocol on the desktop; `docs/remote-protocol.md` documents it.
//!
//! Every message carries the phone-generated `sessionId`. Messages are tagged by `kind`.
//! Parsing is strict about shape and size (the relay already caps a frame at 16 KB) and
//! lenient about extra fields, so a newer phone page never breaks an older desktop.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// `sessionId` is phone-generated and opaque, but bounded: ≤ 64 printable ASCII characters.
pub const MAX_SESSION_ID: usize = 64;
/// The relay drops frames over 16 KB of UTF-8. An SDP is the only large payload; keep the
/// whole frame comfortably below the limit.
pub const MAX_SDP_BYTES: usize = 14 * 1024;
pub const MAX_CANDIDATE_BYTES: usize = 1024;
pub const MAX_DEVICE_LABEL: usize = 40;
/// Remote ICE candidates taken per session, counting those inside the offer. A phone has a
/// handful; the cap bounds the pair table and the STUN checks a hostile peer (or relay) could
/// make this machine send.
pub const MAX_REMOTE_CANDIDATES: usize = 32;
/// Relay frame size limit (UTF-8 bytes), enforced again on the way out.
pub const MAX_RELAY_FRAME_BYTES: usize = 16 * 1024;

/// One signaling message (`signal.data`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum StreamSignal {
    /// phone → desktop: "may I stream my camera to you?"
    StreamRequest {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_label: Option<String>,
    },
    /// desktop → phone: the user accepted; send the offer.
    StreamAccept { session_id: String },
    /// desktop → phone.
    StreamReject { session_id: String, reason: RejectReason },
    /// phone → desktop.
    Offer { session_id: String, sdp: String },
    /// desktop → phone. The desktop's candidates are embedded in the SDP (no trickle).
    Answer { session_id: String, sdp: String },
    /// Either direction. An empty (or null) `candidate` means end-of-candidates.
    Candidate {
        session_id: String,
        #[serde(default, deserialize_with = "null_as_empty")]
        candidate: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_mid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sdp_m_line_index: Option<u32>,
    },
    /// desktop → phone: progress for the phone UI.
    Status {
        session_id: String,
        state: LinkState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codec: Option<String>,
    },
}

fn null_as_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    Declined,
    Busy,
    Timeout,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Connecting,
    Live,
}

/// Why a stream ended (`stream_stop.data.reason`). Unknown reasons from a newer phone are
/// kept verbatim rather than rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    User,
    IceFailed,
    Error,
    PeerOffline,
    /// Desktop-local: the phone closed the peer connection (DTLS close) and no `stream_stop`
    /// with a reason of its own followed. A normal ending, never sent to the phone.
    PeerStopped,
    Timeout,
    Other(String),
}

impl StopReason {
    pub fn as_str(&self) -> &str {
        match self {
            StopReason::User => "user",
            StopReason::IceFailed => "ice_failed",
            StopReason::Error => "error",
            StopReason::PeerOffline => "peer_offline",
            StopReason::PeerStopped => "peer_stopped",
            StopReason::Timeout => "timeout",
            StopReason::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> StopReason {
        match s {
            "user" => StopReason::User,
            "ice_failed" => StopReason::IceFailed,
            "error" => StopReason::Error,
            "peer_offline" => StopReason::PeerOffline,
            "peer_stopped" => StopReason::PeerStopped,
            "timeout" => StopReason::Timeout,
            other => StopReason::Other(sanitize_label(other, 32).unwrap_or_else(|| "unknown".into())),
        }
    }

    /// Whether this ending is a failure worth surfacing as `lastError`.
    pub fn is_failure(&self) -> bool {
        !matches!(self, StopReason::User | StopReason::PeerOffline | StopReason::PeerStopped)
    }
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    NotAnObject,
    UnknownKind(String),
    Malformed(String),
    BadSessionId,
    TooLarge(&'static str),
    BadSdp,
    BadCandidate,
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalError::NotAnObject => f.write_str("signal data is not an object"),
            SignalError::UnknownKind(k) => write!(f, "unknown signal kind {k:?}"),
            SignalError::Malformed(e) => write!(f, "malformed signal: {e}"),
            SignalError::BadSessionId => f.write_str("missing or invalid sessionId"),
            SignalError::TooLarge(what) => write!(f, "{what} too large"),
            SignalError::BadSdp => f.write_str("sdp is not a session description"),
            SignalError::BadCandidate => f.write_str("candidate is not an ICE candidate line"),
        }
    }
}

const KNOWN_KINDS: &[&str] = &["stream_request", "stream_accept", "stream_reject", "offer", "answer", "candidate", "status"];

/// Parse and validate `signal.data` from the phone. Values are normalised: the device label
/// is trimmed and stripped of control characters, an `a=` prefix on a candidate is removed.
pub fn parse_signal(data: &Value) -> Result<StreamSignal, SignalError> {
    let obj = data.as_object().ok_or(SignalError::NotAnObject)?;
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
    if !KNOWN_KINDS.contains(&kind) {
        return Err(SignalError::UnknownKind(kind.chars().take(32).collect()));
    }
    let sig: StreamSignal = serde_json::from_value(data.clone()).map_err(|e| SignalError::Malformed(e.to_string()))?;
    validate(sig)
}

pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_SESSION_ID && id.bytes().all(|b| b.is_ascii_graphic())
}

/// Unicode format characters (general category Cf, plus the line/paragraph separators):
/// invisible, and the bidi
/// controls among them can reorder the text around a name ("…wants to stream" spoofing).
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Trim, drop control and invisible format characters (bidi overrides, zero-width, tags), cap
/// length; `None` if nothing printable is left.
pub fn sanitize_label(s: &str, max: usize) -> Option<String> {
    let cleaned: String = s.chars().filter(|c| !c.is_control() && !is_invisible_format(*c)).collect();
    let t: String = cleaned.trim().chars().take(max).collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn validate(sig: StreamSignal) -> Result<StreamSignal, SignalError> {
    if !valid_session_id(sig.session_id()) {
        return Err(SignalError::BadSessionId);
    }
    Ok(match sig {
        StreamSignal::StreamRequest { session_id, device_label } => StreamSignal::StreamRequest {
            session_id,
            device_label: device_label.and_then(|l| sanitize_label(&l, MAX_DEVICE_LABEL)),
        },
        StreamSignal::Offer { session_id, sdp } => {
            check_sdp(&sdp)?;
            StreamSignal::Offer { session_id, sdp }
        }
        StreamSignal::Answer { session_id, sdp } => {
            check_sdp(&sdp)?;
            StreamSignal::Answer { session_id, sdp }
        }
        StreamSignal::Candidate { session_id, candidate, sdp_mid, sdp_m_line_index } => {
            if candidate.len() > MAX_CANDIDATE_BYTES {
                return Err(SignalError::TooLarge("candidate"));
            }
            let c = candidate.trim();
            let c = c.strip_prefix("a=").unwrap_or(c).to_string();
            if !c.is_empty() && !c.starts_with("candidate:") {
                return Err(SignalError::BadCandidate);
            }
            StreamSignal::Candidate { session_id, candidate: c, sdp_mid, sdp_m_line_index }
        }
        other => other,
    })
}

fn check_sdp(sdp: &str) -> Result<(), SignalError> {
    if sdp.len() > MAX_SDP_BYTES {
        return Err(SignalError::TooLarge("sdp"));
    }
    if !sdp.trim_start().starts_with("v=0") {
        return Err(SignalError::BadSdp);
    }
    Ok(())
}

impl StreamSignal {
    pub fn session_id(&self) -> &str {
        match self {
            StreamSignal::StreamRequest { session_id, .. }
            | StreamSignal::StreamAccept { session_id }
            | StreamSignal::StreamReject { session_id, .. }
            | StreamSignal::Offer { session_id, .. }
            | StreamSignal::Answer { session_id, .. }
            | StreamSignal::Candidate { session_id, .. }
            | StreamSignal::Status { session_id, .. } => session_id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            StreamSignal::StreamRequest { .. } => "stream_request",
            StreamSignal::StreamAccept { .. } => "stream_accept",
            StreamSignal::StreamReject { .. } => "stream_reject",
            StreamSignal::Offer { .. } => "offer",
            StreamSignal::Answer { .. } => "answer",
            StreamSignal::Candidate { .. } => "candidate",
            StreamSignal::Status { .. } => "status",
        }
    }
}

/// `stream_stop.data` in either direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopData {
    pub session_id: Option<String>,
    pub reason: StopReason,
}

/// Lenient about shape: a missing `data`, `sessionId` or `reason` still parses (the relay
/// allows a bare `{"type":"stream_stop"}`), and an invalid `sessionId` is treated as absent.
/// Whether a session-less stop applies is the machine's decision: only to a request the user
/// has not answered yet, never to an accepted session.
pub fn parse_stop(data: Option<&Value>) -> StopData {
    let obj = data.and_then(Value::as_object);
    let session_id = obj
        .and_then(|o| o.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|s| valid_session_id(s))
        .map(str::to_string);
    let reason = obj
        .and_then(|o| o.get("reason"))
        .and_then(Value::as_str)
        .map(StopReason::parse)
        .unwrap_or(StopReason::User);
    StopData { session_id, reason }
}

/// A frame queued while the relay socket was down is still sent after the reconnect if it is
/// younger than this…
pub const OUTBOUND_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

/// …and it is about the session that is current now, or it ends a session (the phone must
/// learn about a stop or a rejection even if the desktop has moved on). Everything else
/// belongs to a moment that has passed.
pub fn worth_sending_after_reconnect(frame: &OutboundFrame, age: std::time::Duration, current_session: Option<&str>) -> bool {
    age <= OUTBOUND_MAX_AGE && (frame.is_terminal() || current_session == Some(frame.session_id()))
}

/// A frame for the relay, already in its wire shape.
#[derive(Debug, Clone, PartialEq)]
pub enum OutboundFrame {
    Signal(StreamSignal),
    StreamStop { session_id: String, reason: StopReason },
}

impl OutboundFrame {
    pub fn session_id(&self) -> &str {
        match self {
            OutboundFrame::Signal(sig) => sig.session_id(),
            OutboundFrame::StreamStop { session_id, .. } => session_id,
        }
    }

    /// Frames that end a session. They stay meaningful to the phone after the session is gone
    /// here, so they survive a relay reconnect when the rest of a dead session's frames do not.
    pub fn is_terminal(&self) -> bool {
        matches!(self, OutboundFrame::StreamStop { .. } | OutboundFrame::Signal(StreamSignal::StreamReject { .. }))
    }

    /// The complete relay frame. `None` if it would exceed the relay's frame limit (the relay
    /// would answer `too_large` and count a protocol violation).
    pub fn to_json(&self) -> Option<String> {
        let v = match self {
            OutboundFrame::Signal(sig) => serde_json::json!({ "type": "signal", "data": sig }),
            OutboundFrame::StreamStop { session_id, reason } => serde_json::json!({
                "type": "stream_stop",
                "data": { "sessionId": session_id, "reason": reason.as_str() },
            }),
        };
        let s = v.to_string();
        (s.len() <= MAX_RELAY_FRAME_BYTES).then_some(s)
    }

    pub fn describe(&self) -> String {
        match self {
            OutboundFrame::Signal(sig) => sig.kind().to_string(),
            OutboundFrame::StreamStop { reason, .. } => format!("stream_stop({reason})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_device_label_is_cut_to_40_characters_and_cannot_reorder_the_dialog() {
        // Long enough to push the dialog's buttons off screen, with a newline and a
        // right-to-left override that would let it rewrite the sentence it is shown in.
        let label = format!("\u{202e}Update required\n{}", "W".repeat(100));
        let StreamSignal::StreamRequest { device_label: Some(shown), .. } =
            parse_signal(&json!({"kind":"stream_request","sessionId":"s1","deviceLabel":label})).unwrap()
        else {
            panic!("not a request")
        };
        assert_eq!(shown.chars().count(), MAX_DEVICE_LABEL);
        assert_eq!(MAX_DEVICE_LABEL, 40);
        assert!(shown.starts_with("Update requiredWWW"), "{shown:?}");
        assert!(!shown.chars().any(|c| c.is_control() || c == '\u{202e}'));
    }

    #[test]
    fn parses_every_phone_message() {
        assert_eq!(
            parse_signal(&json!({"kind":"stream_request","sessionId":"s1","deviceLabel":"  Pixel\u{7}  9 "})).unwrap(),
            StreamSignal::StreamRequest { session_id: "s1".into(), device_label: Some("Pixel  9".into()) }
        );
        assert_eq!(
            parse_signal(&json!({"kind":"stream_request","sessionId":"s1"})).unwrap(),
            StreamSignal::StreamRequest { session_id: "s1".into(), device_label: None }
        );
        assert_eq!(
            parse_signal(&json!({"kind":"offer","sessionId":"s1","sdp":"v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n"})).unwrap(),
            StreamSignal::Offer { session_id: "s1".into(), sdp: "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n".into() }
        );
        assert_eq!(
            parse_signal(&json!({"kind":"candidate","sessionId":"s1","candidate":"a=candidate:1 1 udp 2122260223 192.168.1.5 50000 typ host","sdpMid":"0","sdpMLineIndex":0})).unwrap(),
            StreamSignal::Candidate {
                session_id: "s1".into(),
                candidate: "candidate:1 1 udp 2122260223 192.168.1.5 50000 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_m_line_index: Some(0),
            }
        );
    }

    #[test]
    fn labels_lose_invisible_and_bidi_characters() {
        // RLO + zero-width + BOM + soft hyphen around a name: only the visible text survives.
        assert_eq!(sanitize_label("\u{202e}Pix\u{200b}el\u{feff}\u{ad} 9\u{2066}", 64).as_deref(), Some("Pixel 9"));
        assert_eq!(sanitize_label("\u{202e}\u{200f}\u{2028}", 64), None);
        // Ordinary non-ASCII names are untouched.
        assert_eq!(sanitize_label("טלפון של עמית 📱", 64).as_deref(), Some("טלפון של עמית 📱"));
    }

    #[test]
    fn frames_queued_while_the_relay_was_down() {
        use std::time::Duration;
        let answer = OutboundFrame::Signal(StreamSignal::Answer { session_id: "cur".into(), sdp: "v=0".into() });
        let stale_answer = OutboundFrame::Signal(StreamSignal::Answer { session_id: "old".into(), sdp: "v=0".into() });
        let stop_old = OutboundFrame::StreamStop { session_id: "old".into(), reason: StopReason::Error };
        let reject_old = OutboundFrame::Signal(StreamSignal::StreamReject { session_id: "old".into(), reason: RejectReason::Busy });
        let s = Duration::from_secs;
        // The current session's frames survive a blip; another session's do not…
        assert!(worth_sending_after_reconnect(&answer, s(3), Some("cur")));
        assert!(!worth_sending_after_reconnect(&stale_answer, s(3), Some("cur")));
        assert!(!worth_sending_after_reconnect(&answer, s(3), None));
        // …except the ones that tell the phone a session is over.
        assert!(worth_sending_after_reconnect(&stop_old, s(3), Some("cur")));
        assert!(worth_sending_after_reconnect(&reject_old, s(3), None));
        // Nothing survives a long outage.
        assert!(!worth_sending_after_reconnect(&answer, OUTBOUND_MAX_AGE + s(1), Some("cur")));
        assert!(!worth_sending_after_reconnect(&stop_old, OUTBOUND_MAX_AGE + s(1), None));
    }

    #[test]
    fn end_of_candidates_forms() {
        for v in [
            json!({"kind":"candidate","sessionId":"s1","candidate":""}),
            json!({"kind":"candidate","sessionId":"s1","candidate":null}),
            json!({"kind":"candidate","sessionId":"s1"}),
        ] {
            match parse_signal(&v).unwrap() {
                StreamSignal::Candidate { candidate, .. } => assert_eq!(candidate, ""),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn rejects_invalid_input() {
        assert_eq!(parse_signal(&json!("x")), Err(SignalError::NotAnObject));
        assert_eq!(parse_signal(&json!([1])), Err(SignalError::NotAnObject));
        assert!(matches!(parse_signal(&json!({"kind":"teleport","sessionId":"s"})), Err(SignalError::UnknownKind(_))));
        assert!(matches!(parse_signal(&json!({"sessionId":"s"})), Err(SignalError::UnknownKind(_))));
        assert!(matches!(parse_signal(&json!({"kind":"stream_request"})), Err(SignalError::Malformed(e)) if e.contains("sessionId")));
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":""})), Err(SignalError::BadSessionId));
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"has space"})), Err(SignalError::BadSessionId));
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"x".repeat(65)})), Err(SignalError::BadSessionId));
        assert!(parse_signal(&json!({"kind":"stream_request","sessionId":"x".repeat(64)})).is_ok());
        assert_eq!(parse_signal(&json!({"kind":"offer","sessionId":"s","sdp":"hello"})), Err(SignalError::BadSdp));
        let big = format!("v=0\r\n{}", "a=x\r\n".repeat(4000));
        assert_eq!(parse_signal(&json!({"kind":"offer","sessionId":"s","sdp":big})), Err(SignalError::TooLarge("sdp")));
        assert_eq!(
            parse_signal(&json!({"kind":"candidate","sessionId":"s","candidate":"rm -rf"})),
            Err(SignalError::BadCandidate)
        );
        assert!(matches!(parse_signal(&json!({"kind":"offer","sessionId":"s"})), Err(SignalError::Malformed(_))));
        assert!(matches!(parse_signal(&json!({"kind":"offer","sessionId":7,"sdp":"v=0"})), Err(SignalError::Malformed(_))));
    }

    #[test]
    fn serializes_desktop_messages_exactly() {
        let s = |sig: StreamSignal| serde_json::to_value(sig).unwrap();
        assert_eq!(s(StreamSignal::StreamAccept { session_id: "s".into() }), json!({"kind":"stream_accept","sessionId":"s"}));
        assert_eq!(
            s(StreamSignal::StreamReject { session_id: "s".into(), reason: RejectReason::Busy }),
            json!({"kind":"stream_reject","sessionId":"s","reason":"busy"})
        );
        assert_eq!(
            s(StreamSignal::Answer { session_id: "s".into(), sdp: "v=0".into() }),
            json!({"kind":"answer","sessionId":"s","sdp":"v=0"})
        );
        assert_eq!(
            s(StreamSignal::Status { session_id: "s".into(), state: LinkState::Connecting, width: None, height: None, fps: None, codec: None }),
            json!({"kind":"status","sessionId":"s","state":"connecting"})
        );
        assert_eq!(
            s(StreamSignal::Status {
                session_id: "s".into(),
                state: LinkState::Live,
                width: Some(1280),
                height: Some(720),
                fps: Some(30),
                codec: Some("H264".into())
            }),
            json!({"kind":"status","sessionId":"s","state":"live","width":1280,"height":720,"fps":30,"codec":"H264"})
        );
        assert_eq!(
            s(StreamSignal::Candidate { session_id: "s".into(), candidate: "".into(), sdp_mid: None, sdp_m_line_index: None }),
            json!({"kind":"candidate","sessionId":"s","candidate":""})
        );
    }

    #[test]
    fn relay_frames_have_the_wire_shape() {
        let f = OutboundFrame::Signal(StreamSignal::StreamAccept { session_id: "s".into() });
        assert_eq!(
            serde_json::from_str::<Value>(&f.to_json().unwrap()).unwrap(),
            json!({"type":"signal","data":{"kind":"stream_accept","sessionId":"s"}})
        );
        let stop = OutboundFrame::StreamStop { session_id: "s".into(), reason: StopReason::IceFailed };
        assert_eq!(
            serde_json::from_str::<Value>(&stop.to_json().unwrap()).unwrap(),
            json!({"type":"stream_stop","data":{"sessionId":"s","reason":"ice_failed"}})
        );
        let huge = OutboundFrame::Signal(StreamSignal::Answer { session_id: "s".into(), sdp: "v".repeat(17 * 1024) });
        assert_eq!(huge.to_json(), None);
    }

    #[test]
    fn stop_data_is_lenient() {
        assert_eq!(parse_stop(None), StopData { session_id: None, reason: StopReason::User });
        assert_eq!(
            parse_stop(Some(&json!({"sessionId":"s","reason":"timeout"}))),
            StopData { session_id: Some("s".into()), reason: StopReason::Timeout }
        );
        assert_eq!(
            parse_stop(Some(&json!({"sessionId":"bad id","reason":"camera_lost\n"}))),
            StopData { session_id: None, reason: StopReason::Other("camera_lost".into()) }
        );
        for r in ["user", "ice_failed", "error", "peer_offline", "timeout"] {
            assert_eq!(StopReason::parse(r).as_str(), r);
        }
    }
}
