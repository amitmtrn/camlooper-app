//! Remote v2, phase 1: the phone streams its camera to this computer over peer-to-peer
//! WebRTC and the stream becomes the CamLooper virtual camera.
//!
//! ```text
//!  relay (signal / stream_stop / peer)        ┌──────────── session task (runner) ────────────┐
//!  ──▶ client.rs ──▶ manager (machine) ──▶   │ camera handoff → rtc (str0m, UDP) → frames →    │
//!  ◀── outbound frames ◀──┘    ▲  ▲          │ Annex-B / IVF → ffmpeg stdin → existing sinks   │
//!                 UI decisions ┘  └───────── │ (v4l2 / softcam / CMIO)                         │
//!                                            └─────────────────────────────────────────────────┘
//! ```
//!
//! * [`signal`] — the app-level protocol inside relay `signal.data` (docs/remote-protocol.md).
//! * [`machine`] — pure state machine (idle → requested → connecting → live → stopping).
//! * [`manager`] — runs the machine, executes its actions; the API for client and UI.
//! * [`runner`] — one session: camera handoff, receiver, ffmpeg restarts.
//! * [`rtc`] — str0m receiver task, candidate gathering, STUN.
//! * [`media`] — Annex-B / IVF writers, SPS/VP8 size parsing, PLI limiter.
//! * [`record`] — a record session's take: pre-roll, the tee file, the remux to MP4/WebM.
//! * [`file_receive`] — phase 3: a video file from the phone over a data channel (protocol,
//!   storage, the loop engine's interface); the desktop loops it like a locally picked one.
//! * [`loop_engine`] — the app's video loop (`video_processor`) as a [`LoopPlayer`]; the
//!   same code plays a phone video in the app and in the headless CLI.
//! * [`handoff`] — the app's camera handoff (pause the video loop, start the camera).
//! * [`commands`] — Tauri glue: the global manager, events and commands for the UI.
//!
//! Everything except `commands` runs headless (`examples/phone_stream_cli.rs`,
//! `tests/phone_stream_loopback.rs`); `handoff` and `loop_engine` drive the app's global
//! camera and loop engine, which need no window either.
pub mod commands;
pub mod file_receive;
pub mod handoff;
pub mod loop_engine;
pub mod machine;
pub mod manager;
pub mod media;
pub mod record;
pub mod rtc;
pub mod runner;
pub mod signal;

pub use handoff::phone_owns_camera;
pub use file_receive::{FileDeps, LoopPlayer, LoopState, PhoneVideoStore, PlayError, PlayRequest, Played};
pub use machine::{FilePhase, FileStatus, Phase, StreamRequest, StreamStatus};
pub use manager::{ChannelDecisions, DecisionEvent, DecisionSink, LoopDeps, SessionBackend, StreamManager};
pub use runner::{CameraControl, CameraTarget, Cancel, FixedCamera, IceSource, NoStun, SessionDeps, SessionKind};
pub use signal::{RecordPhase, RecordStatus};

use super::protocol::ServerMessage;

/// The session a frame from the phone is about, if it names a valid one (see
/// [`signal::PairScope`]).
pub fn inbound_session_id(msg: &ServerMessage) -> Option<&str> {
    let (from, data) = match msg {
        ServerMessage::Signal { from, data } => (from, Some(data)),
        ServerMessage::StreamStop { from, data } => (from, data.as_ref()),
        _ => return None,
    };
    if from.as_deref().is_some_and(|f| f != "mobile") {
        return None;
    }
    data?.get("sessionId")?.as_str().filter(|id| signal::valid_session_id(id))
}

/// Hand the relay's v2 frames to the stream manager. Returns the message when the caller
/// (the v1 handler) still needs to see it: everything except `peer`, `signal`, `stream_stop`
/// and `error`. `hello` is both: its `peerOnline` goes to the manager, the rest to the caller.
pub fn route_server_message(stream: &StreamManager, msg: ServerMessage) -> Option<ServerMessage> {
    match msg {
        ServerMessage::Signal { from, data } => {
            if from.as_deref().is_none_or(|f| f == "mobile") {
                stream.on_relay_signal(data);
            }
            None
        }
        ServerMessage::StreamStop { from, data } => {
            if from.as_deref().is_none_or(|f| f == "mobile") {
                stream.on_relay_stream_stop(data);
            }
            None
        }
        ServerMessage::Peer { role, online } => {
            if role == "mobile" {
                stream.on_peer_online(online);
            }
            None
        }
        ServerMessage::Error { error } => {
            stream.on_relay_error(error);
            None
        }
        ServerMessage::Hello { protocol, mobile_device_name, peer_online } => {
            if let Some(online) = peer_online {
                stream.on_peer_online(online);
            }
            stream.set_device_name(mobile_device_name.clone());
            Some(ServerMessage::Hello { protocol, mobile_device_name, peer_online })
        }
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::stream::runner::{SessionCommand, SessionEvent};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::mpsc;

    struct NeverStarts;
    impl SessionBackend for NeverStarts {
        fn start(&self, _: &str, _: u64, _: SessionKind, _: mpsc::UnboundedSender<SessionEvent>) -> mpsc::UnboundedSender<SessionCommand> {
            mpsc::unbounded_channel().0
        }
    }

    fn parse(s: &str) -> ServerMessage {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn only_the_phones_frames_introduce_a_session() {
        let id = |s: &str| inbound_session_id(&parse(s)).map(str::to_string);
        assert_eq!(id(r#"{"type":"signal","from":"mobile","data":{"kind":"stream_request","sessionId":"s1"}}"#).as_deref(), Some("s1"));
        assert_eq!(id(r#"{"type":"stream_stop","from":"mobile","data":{"sessionId":"s2","reason":"user"}}"#).as_deref(), Some("s2"));
        assert_eq!(id(r#"{"type":"signal","from":"desktop","data":{"kind":"stream_request","sessionId":"s1"}}"#), None);
        assert_eq!(id(r#"{"type":"stream_stop","from":"mobile"}"#), None);
        assert_eq!(id(r#"{"type":"signal","from":"mobile","data":{"kind":"stream_request","sessionId":""}}"#), None);
        assert_eq!(id(r#"{"type":"pong"}"#), None);
    }

    #[tokio::test]
    async fn routes_v2_frames_and_passes_v1_through() {
        let (dtx, mut drx) = mpsc::unbounded_channel();
        let (m, mut out) = StreamManager::spawn(Arc::new(NeverStarts), Arc::new(ChannelDecisions(dtx)));

        // v1 frames are untouched.
        for v1 in [
            r#"{"type":"open_meeting","commandId":"c","url":"https://zoom.us/j/1"}"#,
            r#"{"type":"unpaired","reason":"revoked"}"#,
            r#"{"type":"pong"}"#,
        ] {
            assert_eq!(route_server_message(&m, parse(v1)), Some(parse(v1)));
        }
        // hello reaches both.
        let hello = parse(r#"{"type":"hello","protocol":2,"mobileDeviceName":"Pixel","peerOnline":true}"#);
        assert_eq!(route_server_message(&m, hello.clone()), Some(hello));

        // A desktop-originated echo is ignored; the phone's request reaches the decision sink.
        assert_eq!(
            route_server_message(&m, parse(r#"{"type":"signal","from":"desktop","data":{"kind":"stream_request","sessionId":"x"}}"#)),
            None
        );
        assert_eq!(
            route_server_message(&m, parse(r#"{"type":"signal","from":"mobile","data":{"kind":"stream_request","sessionId":"s1"}}"#)),
            None
        );
        let ev = tokio::time::timeout(Duration::from_secs(2), drx.recv()).await.unwrap().unwrap();
        match ev {
            DecisionEvent::Request(r) => {
                assert_eq!(r.session_id, "s1");
                assert_eq!(r.device_name.as_deref(), Some("Pixel"), "falls back to the paired name from hello");
            }
            other => panic!("{other:?}"),
        }
        // The phone goes offline: the pending request is dismissed.
        assert_eq!(route_server_message(&m, parse(r#"{"type":"peer","role":"mobile","online":false}"#)), None);
        let ev = tokio::time::timeout(Duration::from_secs(2), drx.recv()).await.unwrap().unwrap();
        assert_eq!(ev, DecisionEvent::Dismissed("s1".into()));
        assert_eq!(route_server_message(&m, parse(r#"{"type":"error","error":"rate_limited"}"#)), None);
        assert!(out.try_recv().is_err(), "nothing was sent to the phone");
    }
}
