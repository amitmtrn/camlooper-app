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
/// The largest video a phone may send for looping (300 MiB).
pub const MAX_PHONE_VIDEO_BYTES: u64 = 300 * 1024 * 1024;
/// A phone video's name is shown, never used as a path; it is cut to this many characters.
pub const MAX_FILE_NAME: usize = 120;
const MAX_MIME: usize = 100;
/// `loop_status.error` is short text for the phone UI.
pub const MAX_LOOP_ERROR: usize = 120;
/// File extensions a phone video may have (also what the stored file is named with).
pub const PHONE_VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "3gp"];

/// The app-level protocol this desktop speaks (`hello` / `hello_ok`). Independent of the
/// relay's `?protocol=2`. 3 added `hello` itself and `mode: "record"`.
pub const APP_PROTOCOL: u32 = 3;
/// A recorded take is at most this long; the desktop finalizes it then, whatever the phone does.
pub const RECORD_CAP_MS: u64 = 60_000;
/// `hello_ok.features` entries are short lowercase words; this many at most are kept.
const MAX_FEATURES: usize = 16;
const MAX_FEATURE_LEN: usize = 24;

/// What a phone session carries: its live camera, one video file to loop, or its live camera
/// recorded on the desktop and then looped (`record`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    #[default]
    Live,
    File,
    /// The phone streams as for `live`; the desktop tees the stream to a file and, when the
    /// take ends, loops that file. Never treated as `live`.
    Record,
}

impl StreamMode {
    fn is_live(&self) -> bool {
        *self == StreamMode::Live
    }
}

/// The video a `mode: "file"` request announces. After [`parse_signal`]: `name` is cleaned
/// for display, `size` is within `1..=`[`MAX_PHONE_VIDEO_BYTES`] and the type is a video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffer {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub mime: String,
}

impl FileOffer {
    /// The name's extension, lower case, if it is one of [`PHONE_VIDEO_EXTENSIONS`].
    pub fn name_extension(&self) -> Option<&'static str> {
        let (stem, ext) = self.name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        let ext = ext.to_ascii_lowercase();
        PHONE_VIDEO_EXTENSIONS.iter().copied().find(|e| *e == ext)
    }

    /// The MIME type without parameters, lower case (`video/mp4; codecs=…` → `video/mp4`).
    pub fn mime_essence(&self) -> String {
        self.mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
    }
}

/// What the phone asks of the desktop's loop (`loop_control.action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopAction {
    Stop,
    Start,
    Status,
}

/// One signaling message (`signal.data`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum StreamSignal {
    /// phone → desktop: "may I stream my camera to you?" — or, with `mode: "file"`, "may I
    /// loop this video on your camera?". No `mode` means live (older phone pages).
    StreamRequest {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_label: Option<String>,
        #[serde(default, skip_serializing_if = "StreamMode::is_live")]
        mode: StreamMode,
        /// Present exactly when `mode` is `file`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<FileOffer>,
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
        /// `record` for a record session; absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<StreamMode>,
        /// Record sessions: where the take is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<RecordStatus>,
    },
    /// phone → desktop, once after pairing: "which app protocol do you speak?". `sessionId`
    /// is any valid id; no session needs to be open. Answered with `hello_ok`.
    Hello { session_id: String, app_protocol: u32 },
    /// desktop → phone: the answer to `hello`. `features` are the request modes this
    /// desktop takes (`live`, `record`, `file`).
    HelloOk { session_id: String, app_protocol: u32, features: Vec<String> },
    /// phone → desktop: act on the video loaded on the desktop. `sessionId` is any valid id;
    /// no session needs to be open. Always answered with `loop_status`.
    LoopControl { session_id: String, action: LoopAction },
    /// desktop → phone: the desktop's loop, as an answer to `loop_control` and after a phone
    /// video was loaded.
    LoopStatus {
        session_id: String,
        playing: bool,
        /// Display name of the loaded video.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The loaded video came from the phone.
        from_phone: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Where a recorded take is (`record.phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordPhase {
    /// From the request on: connecting, then teeing the stream into the take.
    Recording,
    /// The take ended; it is being remuxed and loaded.
    Processing,
    /// Looping on the virtual camera. The session is over by now.
    Looping,
    Failed,
}

/// `record` in the UI status and in the phone's `status` signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordStatus {
    /// Media in the take so far (after the pre-roll).
    pub elapsed_ms: u64,
    /// [`RECORD_CAP_MS`].
    pub cap_ms: u64,
    pub phase: RecordPhase,
    /// With `failed`: short text saying why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RecordStatus {
    pub fn new(phase: RecordPhase, elapsed_ms: u64) -> Self {
        Self { elapsed_ms: elapsed_ms.min(RECORD_CAP_MS), cap_ms: RECORD_CAP_MS, phase, error: None }
    }
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
    /// Not something this desktop can take: not a video, an unknown `mode`, a `file` request
    /// without a usable `file`.
    Unsupported,
    /// The announced video is over [`MAX_PHONE_VIDEO_BYTES`].
    TooLarge,
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
    /// A file or record session finished: the video is looping. A success on both sides,
    /// never an error.
    Done,
    /// A record session was stopped before the take held [`super::record::MIN_TAKE_MS`] of
    /// media. Nothing loops; not an error.
    TooShort,
    /// A record session's take could not be made into a loop (no keyframe, mostly stalls,
    /// the remux or the loader refused it). Nothing loops.
    Incomplete,
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
            StopReason::Done => "done",
            StopReason::TooShort => "too_short",
            StopReason::Incomplete => "incomplete",
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
            "done" | "finished" => StopReason::Done,
            "too_short" => StopReason::TooShort,
            "incomplete" => StopReason::Incomplete,
            other => StopReason::Other(sanitize_label(other, 32).unwrap_or_else(|| "unknown".into())),
        }
    }

    /// Whether this ending is a failure worth surfacing as `lastError`.
    pub fn is_failure(&self) -> bool {
        !matches!(
            self,
            StopReason::User | StopReason::PeerOffline | StopReason::PeerStopped | StopReason::Done | StopReason::TooShort
        )
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
    /// A `stream_request` that is well-formed enough to answer, and the answer is no
    /// (`stream_reject` with this reason): the video is too large, not a video, …
    Refused { session_id: String, reason: RejectReason },
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
            SignalError::Refused { reason, .. } => write!(f, "request refused ({reason:?})"),
        }
    }
}

const KNOWN_KINDS: &[&str] = &[
    "stream_request",
    "stream_accept",
    "stream_reject",
    "offer",
    "answer",
    "candidate",
    "status",
    "hello",
    "hello_ok",
    "loop_control",
    "loop_status",
];

/// Parse and validate `signal.data` from the phone. Values are normalised: the device label
/// is trimmed and stripped of control characters, an `a=` prefix on a candidate is removed.
pub fn parse_signal(data: &Value) -> Result<StreamSignal, SignalError> {
    let obj = data.as_object().ok_or(SignalError::NotAnObject)?;
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
    if !KNOWN_KINDS.contains(&kind) {
        return Err(SignalError::UnknownKind(kind.chars().take(32).collect()));
    }
    if kind == "stream_request" {
        return parse_request(obj);
    }
    let sig: StreamSignal = serde_json::from_value(data.clone()).map_err(|e| SignalError::Malformed(e.to_string()))?;
    validate(sig)
}

/// `stream_request`, read by hand: a request that names a session but asks for something
/// this desktop cannot take is *refused* (the phone gets a `stream_reject` with the reason)
/// rather than dropped as malformed, which would leave the phone waiting.
fn parse_request(obj: &serde_json::Map<String, Value>) -> Result<StreamSignal, SignalError> {
    let session_id = match obj.get("sessionId") {
        Some(Value::String(id)) => id.clone(),
        Some(_) => return Err(SignalError::Malformed("sessionId is not a string".into())),
        None => return Err(SignalError::Malformed("missing field `sessionId`".into())),
    };
    if !valid_session_id(&session_id) {
        return Err(SignalError::BadSessionId);
    }
    let refuse = |reason| SignalError::Refused { session_id: session_id.clone(), reason };
    let device_label = obj.get("deviceLabel").and_then(Value::as_str).and_then(|l| sanitize_label(l, MAX_DEVICE_LABEL));
    let mode = match obj.get("mode") {
        None | Some(Value::Null) => StreamMode::Live,
        Some(Value::String(m)) if m == "live" => StreamMode::Live,
        Some(Value::String(m)) if m == "file" => StreamMode::File,
        Some(Value::String(m)) if m == "record" => StreamMode::Record,
        // A mode from a newer phone page: answered `unsupported`, never taken for live.
        Some(_) => return Err(refuse(RejectReason::Unsupported)),
    };
    // Live and record carry no file; a stray `file` on them is dropped.
    if mode != StreamMode::File {
        return Ok(StreamSignal::StreamRequest { session_id, device_label, mode, file: None });
    }
    let file = obj.get("file").and_then(Value::as_object).ok_or_else(|| refuse(RejectReason::Unsupported))?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .and_then(clean_file_name)
        .ok_or_else(|| refuse(RejectReason::Unsupported))?;
    let mime = clean_mime(file.get("mime").and_then(Value::as_str).unwrap_or_default());
    let size = match file.get("size") {
        Some(Value::Number(n)) => match n.as_u64() {
            Some(0) => return Err(refuse(RejectReason::Unsupported)),
            Some(size) if size > MAX_PHONE_VIDEO_BYTES => return Err(refuse(RejectReason::TooLarge)),
            Some(size) => size,
            // Not a non-negative integer: a whole float beyond u64 is still "too large",
            // anything else (negative, fractional) is not a size.
            None if n.as_f64().is_some_and(|f| f.is_finite() && f > MAX_PHONE_VIDEO_BYTES as f64) => {
                return Err(refuse(RejectReason::TooLarge))
            }
            None => return Err(refuse(RejectReason::Unsupported)),
        },
        _ => return Err(refuse(RejectReason::Unsupported)),
    };
    let offer = FileOffer { name, size, mime };
    if !(offer.mime_essence().starts_with("video/") || offer.name_extension().is_some()) {
        return Err(refuse(RejectReason::Unsupported));
    }
    Ok(StreamSignal::StreamRequest { session_id, device_label, mode, file: Some(offer) })
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

/// A phone video's name as it is shown: its last path component (a name is not a path:
/// nothing before a `/` or `\\` survives, and neither does `.` or `..`), then
/// [`sanitize_label`]; at most [`MAX_FILE_NAME`] characters. Display only — the stored
/// file's name is never derived from it — but it is also the name the loader is given, and
/// nothing down there should ever see a separator.
pub fn clean_file_name(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    sanitize_label(last, MAX_FILE_NAME).filter(|n| n != "." && n != "..")
}

/// A MIME type reduced to printable ASCII and a sane length.
pub fn clean_mime(mime: &str) -> String {
    mime.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(MAX_MIME).collect::<String>().trim().to_string()
}

fn validate(sig: StreamSignal) -> Result<StreamSignal, SignalError> {
    if !valid_session_id(sig.session_id()) {
        return Err(SignalError::BadSessionId);
    }
    Ok(match sig {
        StreamSignal::LoopStatus { session_id, playing, name, from_phone, error } => StreamSignal::LoopStatus {
            session_id,
            playing,
            name: name.and_then(|n| sanitize_label(&n, MAX_FILE_NAME)),
            from_phone,
            error: error.and_then(|e| sanitize_label(&e, MAX_LOOP_ERROR)),
        },
        StreamSignal::Hello { session_id, app_protocol } => {
            if app_protocol == 0 {
                return Err(SignalError::Malformed("appProtocol must be at least 1".into()));
            }
            StreamSignal::Hello { session_id, app_protocol }
        }
        StreamSignal::HelloOk { session_id, app_protocol, features } => StreamSignal::HelloOk {
            session_id,
            app_protocol,
            features: features
                .into_iter()
                .filter(|f| !f.is_empty() && f.len() <= MAX_FEATURE_LEN && f.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
                .take(MAX_FEATURES)
                .collect(),
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
            | StreamSignal::Status { session_id, .. }
            | StreamSignal::Hello { session_id, .. }
            | StreamSignal::HelloOk { session_id, .. }
            | StreamSignal::LoopControl { session_id, .. }
            | StreamSignal::LoopStatus { session_id, .. } => session_id,
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
            StreamSignal::Hello { .. } => "hello",
            StreamSignal::HelloOk { .. } => "hello_ok",
            StreamSignal::LoopControl { .. } => "loop_control",
            StreamSignal::LoopStatus { .. } => "loop_status",
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

/// How many session ids a [`PairScope`] remembers (most recently heard first to stay).
pub const MAX_KNOWN_SESSIONS: usize = 64;

/// Which sessions outbound frames may be about on the relay socket's current pairing.
///
/// Every frame the desktop sends names a `sessionId` the phone introduced. The frames reach
/// the socket through two queues, so one produced for phone A (the `stream_stop` of the
/// stream that an unlink ended, an `answer` with this computer's addresses) can arrive after
/// the socket has moved on to phone B. Timing cannot rule that out; this does: a frame is
/// sent only if its session id was heard *from the phone of the current pairing*.
#[derive(Debug, Default)]
pub struct PairScope {
    pair_id: Option<String>,
    sessions: std::collections::VecDeque<String>,
}

impl PairScope {
    /// A socket for `pair_id` is up. `true` if that is another pairing than before (or the
    /// first): no session is known any more, and whatever was queued must be dropped.
    pub fn enter(&mut self, pair_id: &str) -> bool {
        if self.pair_id.as_deref() == Some(pair_id) {
            return false;
        }
        self.pair_id = Some(pair_id.to_string());
        self.sessions.clear();
        true
    }

    /// The phone sent a frame naming this session.
    pub fn note_inbound(&mut self, session_id: &str) {
        if let Some(i) = self.sessions.iter().position(|s| s == session_id) {
            self.sessions.remove(i);
        } else if self.sessions.len() >= MAX_KNOWN_SESSIONS {
            self.sessions.pop_front();
        }
        self.sessions.push_back(session_id.to_string());
    }

    pub fn allows(&self, frame: &OutboundFrame) -> bool {
        self.sessions.iter().any(|s| s == frame.session_id())
    }
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

    fn file_req(file: Value) -> Result<StreamSignal, SignalError> {
        parse_signal(&json!({"kind":"stream_request","sessionId":"f1","mode":"file","file":file}))
    }
    fn refused(reason: RejectReason) -> Result<StreamSignal, SignalError> {
        Err(SignalError::Refused { session_id: "f1".into(), reason })
    }

    #[test]
    fn a_request_without_a_mode_is_a_live_request() {
        // What the phone page sent before file mode existed, and what it serializes back to.
        let old = json!({"kind":"stream_request","sessionId":"s1","deviceLabel":"Pixel"});
        let sig = parse_signal(&old).unwrap();
        assert_eq!(
            sig,
            StreamSignal::StreamRequest { session_id: "s1".into(), device_label: Some("Pixel".into()), mode: StreamMode::Live, file: None }
        );
        assert_eq!(serde_json::to_value(&sig).unwrap(), old);
        // An explicit live mode is the same; a stray `file` on it is dropped.
        let explicit = json!({"kind":"stream_request","sessionId":"s1","deviceLabel":"Pixel","mode":"live","file":{"name":"x.mp4","size":1,"mime":"video/mp4"}});
        assert_eq!(parse_signal(&explicit).unwrap(), sig);
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"s1","deviceLabel":"Pixel","mode":null})).unwrap(), sig);
    }

    #[test]
    fn hello_is_answered_with_the_app_protocol_and_the_features() {
        assert_eq!(APP_PROTOCOL, 3);
        assert_eq!(
            parse_signal(&json!({"kind":"hello","sessionId":"h1","appProtocol":3})).unwrap(),
            StreamSignal::Hello { session_id: "h1".into(), app_protocol: 3 }
        );
        // A newer phone (extra fields, a higher protocol) is still answered.
        assert_eq!(
            parse_signal(&json!({"kind":"hello","sessionId":"h1","appProtocol":7,"wants":["x"]})).unwrap(),
            StreamSignal::Hello { session_id: "h1".into(), app_protocol: 7 }
        );
        // No protocol, not a number, zero, negative, or nobody to answer.
        for bad in [
            json!({"kind":"hello","sessionId":"h1"}),
            json!({"kind":"hello","sessionId":"h1","appProtocol":"3"}),
            json!({"kind":"hello","sessionId":"h1","appProtocol":0}),
            json!({"kind":"hello","sessionId":"h1","appProtocol":-1}),
            json!({"kind":"hello","sessionId":"h1","appProtocol":3.5}),
            json!({"kind":"hello","appProtocol":3}),
        ] {
            assert!(matches!(parse_signal(&bad), Err(SignalError::Malformed(_))), "{bad}");
        }
        assert_eq!(parse_signal(&json!({"kind":"hello","sessionId":"bad id","appProtocol":3})), Err(SignalError::BadSessionId));

        let ok = StreamSignal::HelloOk { session_id: "h1".into(), app_protocol: APP_PROTOCOL, features: vec!["live".into(), "record".into()] };
        assert_eq!(ok.kind(), "hello_ok");
        let wire = json!({"kind":"hello_ok","sessionId":"h1","appProtocol":3,"features":["live","record"]});
        assert_eq!(serde_json::to_value(&ok).unwrap(), wire);
        assert_eq!(parse_signal(&wire).unwrap(), ok);
        // Features are short lowercase words; anything else is dropped, and the list is bounded.
        let StreamSignal::HelloOk { features, .. } =
            parse_signal(&json!({"kind":"hello_ok","sessionId":"h1","appProtocol":3,"features":["live","Re cord","","x".repeat(40),"file"]})).unwrap()
        else {
            panic!("not a hello_ok")
        };
        assert_eq!(features, vec!["live", "file"]);
        assert!(matches!(parse_signal(&json!({"kind":"hello_ok","sessionId":"h1","appProtocol":3})), Err(SignalError::Malformed(_))));
        assert!(matches!(parse_signal(&json!({"kind":"hello_ok","sessionId":"h1","appProtocol":3,"features":"live"})), Err(SignalError::Malformed(_))));
    }

    #[test]
    fn a_record_request_is_its_own_mode_and_never_live() {
        let wire = json!({"kind":"stream_request","sessionId":"r1","deviceLabel":"Pixel","mode":"record"});
        let sig = parse_signal(&wire).unwrap();
        assert_eq!(
            sig,
            StreamSignal::StreamRequest { session_id: "r1".into(), device_label: Some("Pixel".into()), mode: StreamMode::Record, file: None }
        );
        assert_eq!(serde_json::to_value(&sig).unwrap(), wire);
        // A stray `file` on it is dropped, not required.
        let with_file = json!({"kind":"stream_request","sessionId":"r1","deviceLabel":"Pixel","mode":"record","file":{"name":"x.mp4","size":1}});
        assert_eq!(parse_signal(&with_file).unwrap(), sig);
        // Modes are exact: nothing close to a known one is taken for it, or for live.
        for mode in ["Record", "RECORD", "record ", "rec", "live_record", ""] {
            assert_eq!(
                parse_signal(&json!({"kind":"stream_request","sessionId":"f1","mode":mode})),
                refused(RejectReason::Unsupported),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn the_phone_status_carries_the_take() {
        let status = StreamSignal::Status {
            session_id: "r1".into(),
            state: LinkState::Live,
            width: Some(1280),
            height: Some(720),
            fps: Some(30),
            codec: Some("H264".into()),
            mode: Some(StreamMode::Record),
            record: Some(RecordStatus::new(RecordPhase::Recording, 12_345)),
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({"kind":"status","sessionId":"r1","state":"live","width":1280,"height":720,"fps":30,"codec":"H264",
                   "mode":"record","record":{"elapsedMs":12345,"capMs":60000,"phase":"recording"}})
        );
        let failed = RecordStatus { error: Some("too short".into()), ..RecordStatus::new(RecordPhase::Failed, 99_000) };
        assert_eq!(serde_json::to_value(&failed).unwrap(), json!({"elapsedMs":60000,"capMs":60000,"phase":"failed","error":"too short"}));
        for (phase, text) in [(RecordPhase::Processing, "processing"), (RecordPhase::Looping, "looping")] {
            assert_eq!(serde_json::to_value(phase).unwrap(), json!(text));
        }
    }

    #[test]
    fn the_record_endings_have_their_own_reasons() {
        for (reason, text, failure) in [(StopReason::TooShort, "too_short", false), (StopReason::Incomplete, "incomplete", true)] {
            assert_eq!(StopReason::parse(text), reason);
            assert_eq!(reason.as_str(), text);
            assert_eq!(reason.is_failure(), failure, "{text}");
        }
    }

    #[test]
    fn a_file_request_carries_the_validated_file() {
        let sig = file_req(json!({"name":"holiday.MP4","size":1048576,"mime":"video/mp4"})).unwrap();
        let StreamSignal::StreamRequest { mode, file: Some(file), .. } = &sig else { panic!("{sig:?}") };
        assert_eq!(*mode, StreamMode::File);
        assert_eq!(*file, FileOffer { name: "holiday.MP4".into(), size: 1048576, mime: "video/mp4".into() });
        assert_eq!(file.name_extension(), Some("mp4"));
        assert_eq!(
            serde_json::to_value(&sig).unwrap(),
            json!({"kind":"stream_request","sessionId":"f1","mode":"file","file":{"name":"holiday.MP4","size":1048576,"mime":"video/mp4"}})
        );
        // The limits are inclusive.
        assert!(file_req(json!({"name":"a.mp4","size":1,"mime":"video/mp4"})).is_ok());
        assert!(file_req(json!({"name":"a.mp4","size":MAX_PHONE_VIDEO_BYTES,"mime":"video/mp4"})).is_ok());
        assert_eq!(MAX_PHONE_VIDEO_BYTES, 314_572_800);
    }

    #[test]
    fn bad_sizes_are_refused() {
        let with_size = |size: Value| file_req(json!({"name":"a.mp4","size":size,"mime":"video/mp4"}));
        assert_eq!(with_size(json!(MAX_PHONE_VIDEO_BYTES + 1)), refused(RejectReason::TooLarge));
        assert_eq!(with_size(json!(u64::MAX)), refused(RejectReason::TooLarge));
        assert_eq!(with_size(json!(1e30)), refused(RejectReason::TooLarge));
        assert_eq!(with_size(json!(0)), refused(RejectReason::Unsupported));
        assert_eq!(with_size(json!(-5)), refused(RejectReason::Unsupported));
        assert_eq!(with_size(json!(1000.5)), refused(RejectReason::Unsupported));
        assert_eq!(with_size(json!("1000")), refused(RejectReason::Unsupported));
        assert_eq!(with_size(Value::Null), refused(RejectReason::Unsupported));
        assert_eq!(file_req(json!({"name":"a.mp4","mime":"video/mp4"})), refused(RejectReason::Unsupported));
    }

    #[test]
    fn a_file_request_needs_a_file_that_is_a_video() {
        // Missing or misshapen `file`.
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"f1","mode":"file"})), refused(RejectReason::Unsupported));
        assert_eq!(file_req(json!("a.mp4")), refused(RejectReason::Unsupported));
        assert_eq!(file_req(json!({"size":10,"mime":"video/mp4"})), refused(RejectReason::Unsupported));
        assert_eq!(file_req(json!({"name":" \u{7} ","size":10,"mime":"video/mp4"})), refused(RejectReason::Unsupported));
        // Neither a video type nor a video extension.
        assert_eq!(file_req(json!({"name":"notes.pdf","size":10,"mime":"application/pdf"})), refused(RejectReason::Unsupported));
        assert_eq!(file_req(json!({"name":"clip.avi","size":10,"mime":""})), refused(RejectReason::Unsupported));
        assert_eq!(file_req(json!({"name":".mp4","size":10})), refused(RejectReason::Unsupported));
        // Either one is enough.
        assert!(file_req(json!({"name":"clip","size":10,"mime":"video/quicktime"})).is_ok());
        assert!(file_req(json!({"name":"clip.unknown","size":10,"mime":"Video/MP4; codecs=avc1"})).is_ok());
        for ext in PHONE_VIDEO_EXTENSIONS {
            assert!(file_req(json!({"name":format!("clip.{}", ext.to_uppercase()),"size":10,"mime":"application/octet-stream"})).is_ok(), "{ext}");
        }
        // A mode this desktop does not know is answered, not ignored.
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"f1","mode":"screen"})), refused(RejectReason::Unsupported));
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"f1","mode":7})), refused(RejectReason::Unsupported));
        // Without a usable session id there is nobody to answer.
        assert_eq!(parse_signal(&json!({"kind":"stream_request","sessionId":"bad id","mode":"file"})), Err(SignalError::BadSessionId));
    }

    #[test]
    fn a_file_name_is_reduced_to_its_last_path_component() {
        for (sent, shown) in [
            ("holiday.mp4", Some("holiday.mp4")),
            ("/sdcard/DCIM/Camera/VID_1.mp4", Some("VID_1.mp4")),
            ("C:\\Users\\me\\Videos\\clip.mov", Some("clip.mov")),
            ("../../../etc/passwd", Some("passwd")),
            ("..\\..\\x.3gp", Some("x.3gp")),
            ("a/b\\c/../d.webm", Some("d.webm")),
            ("  spaced name .mkv ", Some("spaced name .mkv")),
            // Nothing left that could be shown: not a name.
            ("clips/", None),
            ("..", None),
            ("a/..", None),
            (".", None),
            ("/", None),
            ("", None),
        ] {
            assert_eq!(clean_file_name(sent).as_deref(), shown, "{sent:?}");
        }
        // Such a request is refused rather than shown with an empty name.
        assert!(matches!(
            file_req(json!({"name":"videos/","size":10,"mime":"video/mp4"})),
            Err(SignalError::Refused { reason: RejectReason::Unsupported, .. })
        ));
    }

    #[test]
    fn a_file_name_is_cleaned_for_display_and_is_never_a_path() {
        let name = format!("\u{202e}../../etc/pass\nwd\u{200b}{}.mp4", "x".repeat(300));
        let StreamSignal::StreamRequest { file: Some(file), .. } = file_req(json!({"name":name,"size":10,"mime":"video/mp4"})).unwrap()
        else {
            panic!("not a file request")
        };
        assert_eq!(file.name.chars().count(), MAX_FILE_NAME);
        assert_eq!(MAX_FILE_NAME, 120);
        // Only the last path component is a name: nothing before a separator survives.
        assert!(file.name.starts_with("passwdxxx"), "{:?}", file.name);
        assert!(!file.name.contains(['/', '\\']));
        assert!(!file.name.chars().any(|c| c.is_control() || c == '\u{202e}' || c == '\u{200b}'));
        // Cut off before its extension: the type still decides.
        assert_eq!(file.name_extension(), None);
        let StreamSignal::StreamRequest { file: Some(file), .. } =
            file_req(json!({"name":"  my clip.mov  ","size":10,"mime":"video/quicktime\r\nx: y"})).unwrap()
        else {
            panic!("not a file request")
        };
        assert_eq!((file.name.as_str(), file.mime.as_str()), ("my clip.mov", "video/quicktimex: y"));
    }

    #[test]
    fn loop_control_and_loop_status() {
        for (text, action) in [("stop", LoopAction::Stop), ("start", LoopAction::Start), ("status", LoopAction::Status)] {
            assert_eq!(
                parse_signal(&json!({"kind":"loop_control","sessionId":"c1","action":text})).unwrap(),
                StreamSignal::LoopControl { session_id: "c1".into(), action }
            );
        }
        // An unknown or missing action, or no session id to answer on.
        assert!(matches!(parse_signal(&json!({"kind":"loop_control","sessionId":"c1","action":"rewind"})), Err(SignalError::Malformed(_))));
        assert!(matches!(parse_signal(&json!({"kind":"loop_control","sessionId":"c1"})), Err(SignalError::Malformed(_))));
        assert!(matches!(parse_signal(&json!({"kind":"loop_control","sessionId":"c1","action":1})), Err(SignalError::Malformed(_))));
        assert_eq!(parse_signal(&json!({"kind":"loop_control","sessionId":"","action":"stop"})), Err(SignalError::BadSessionId));

        let s = |sig: StreamSignal| serde_json::to_value(sig).unwrap();
        assert_eq!(
            s(StreamSignal::LoopStatus { session_id: "c1".into(), playing: true, name: Some("clip.mp4".into()), from_phone: true, error: None }),
            json!({"kind":"loop_status","sessionId":"c1","playing":true,"name":"clip.mp4","fromPhone":true})
        );
        assert_eq!(
            s(StreamSignal::LoopStatus { session_id: "c1".into(), playing: false, name: None, from_phone: false, error: Some("busy".into()) }),
            json!({"kind":"loop_status","sessionId":"c1","playing":false,"fromPhone":false,"error":"busy"})
        );
        assert_eq!(
            s(StreamSignal::StreamReject { session_id: "s".into(), reason: RejectReason::TooLarge }),
            json!({"kind":"stream_reject","sessionId":"s","reason":"too_large"})
        );
        assert_eq!(
            s(StreamSignal::StreamReject { session_id: "s".into(), reason: RejectReason::Unsupported }),
            json!({"kind":"stream_reject","sessionId":"s","reason":"unsupported"})
        );
    }

    #[test]
    fn a_finished_file_session_is_not_a_failure() {
        assert_eq!(StopReason::parse("done"), StopReason::Done);
        assert_eq!(StopReason::parse("finished"), StopReason::Done);
        assert_eq!(StopReason::Done.as_str(), "done");
        assert!(!StopReason::Done.is_failure());
        let stop = OutboundFrame::StreamStop { session_id: "s".into(), reason: StopReason::Done };
        assert_eq!(
            serde_json::from_str::<Value>(&stop.to_json().unwrap()).unwrap(),
            json!({"type":"stream_stop","data":{"sessionId":"s","reason":"done"}})
        );
    }

    #[test]
    fn parses_every_phone_message() {
        assert_eq!(
            parse_signal(&json!({"kind":"stream_request","sessionId":"s1","deviceLabel":"  Pixel\u{7}  9 "})).unwrap(),
            StreamSignal::StreamRequest { session_id: "s1".into(), device_label: Some("Pixel  9".into()), mode: StreamMode::Live, file: None }
        );
        assert_eq!(
            parse_signal(&json!({"kind":"stream_request","sessionId":"s1"})).unwrap(),
            StreamSignal::StreamRequest { session_id: "s1".into(), device_label: None, mode: StreamMode::Live, file: None }
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
    fn frames_for_another_pairing_are_never_sent() {
        let stop = |id: &str| OutboundFrame::StreamStop { session_id: id.into(), reason: StopReason::User };
        let answer = |id: &str| OutboundFrame::Signal(StreamSignal::Answer { session_id: id.into(), sdp: "v=0".into() });
        let mut scope = PairScope::default();
        assert!(!scope.allows(&stop("a1")), "nothing is known before a phone has spoken");

        // Paired with phone A, which starts session a1.
        assert!(scope.enter("pair-A"));
        scope.note_inbound("a1");
        assert!(scope.allows(&answer("a1")) && scope.allows(&stop("a1")));
        assert!(!scope.allows(&answer("zz")));
        // The socket reconnects for the same pairing: a1 is still A's.
        assert!(!scope.enter("pair-A"));
        assert!(scope.allows(&stop("a1")));

        // Relinked to phone B mid-stream. The stop for A's session — terminal and fresh, so
        // the age rule alone would send it — and A's answer must not go to B.
        assert!(scope.enter("pair-B"));
        assert!(!scope.allows(&stop("a1")));
        assert!(!scope.allows(&answer("a1")));
        scope.note_inbound("b1");
        assert!(scope.allows(&answer("b1")));
        // Back to A (revoke, quick re-pair): a new pairing in every respect.
        assert!(scope.enter("pair-A"));
        assert!(!scope.allows(&stop("a1")) && !scope.allows(&stop("b1")));
    }

    #[test]
    fn the_known_sessions_are_bounded_and_the_active_one_stays() {
        let mut scope = PairScope::default();
        scope.enter("p");
        scope.note_inbound("live");
        for i in 0..(MAX_KNOWN_SESSIONS * 3) {
            scope.note_inbound(&format!("other{i}"));
            // The live session keeps talking (candidates, its eventual stop).
            if i % 10 == 0 {
                scope.note_inbound("live");
            }
        }
        assert_eq!(scope.sessions.len(), MAX_KNOWN_SESSIONS);
        let stop = |id: &str| OutboundFrame::StreamStop { session_id: id.into(), reason: StopReason::User };
        assert!(scope.allows(&stop("live")));
        assert!(!scope.allows(&stop("other0")));
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
        assert!(matches!(parse_signal(&json!({"kind":"stream_request","sessionId":7})), Err(SignalError::Malformed(_))));
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
            s(StreamSignal::Status { session_id: "s".into(), state: LinkState::Connecting, width: None, height: None, fps: None, codec: None, mode: None, record: None }),
            json!({"kind":"status","sessionId":"s","state":"connecting"})
        );
        assert_eq!(
            s(StreamSignal::Status {
                session_id: "s".into(),
                state: LinkState::Live,
                width: Some(1280),
                height: Some(720),
                fps: Some(30),
                codec: Some("H264".into()),
                mode: None,
                record: None,
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
