//! Wire types for the phone remote's relay protocol: v1 (pairing, `open_meeting`) plus the
//! v2 frames a desktop receives when it connects with `?protocol=2` (`peer`, `signal`,
//! `stream_stop`, `error`). Field names are camelCase on the wire; messages are tagged by
//! `type`. Unknown message types deserialize to `Unknown` so a newer server never breaks an
//! older desktop. The content of `signal.data` is the app-level phone-stream protocol, see
//! `stream::signal`.
use serde::{Deserialize, Serialize};

/// Server -> desktop WebSocket frames.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    Hello {
        #[serde(default)]
        protocol: u32,
        #[serde(default)]
        mobile_device_name: Option<String>,
        /// v2 only: whether the phone is connected to the relay right now.
        #[serde(default)]
        peer_online: Option<bool>,
    },
    OpenMeeting {
        command_id: String,
        url: String,
        #[serde(default)]
        created_at: Option<String>,
        #[serde(default)]
        sent_at: Option<String>,
    },
    Unpaired {
        #[serde(default)]
        reason: Option<String>,
    },
    Pong,
    /// v2: the other side connected or disconnected.
    Peer {
        role: String,
        online: bool,
    },
    /// v2: relayed verbatim from the other side; `data` is always a JSON object.
    Signal {
        #[serde(default)]
        from: Option<String>,
        data: serde_json::Value,
    },
    /// v2: the other side ended a stream; `data` is optional.
    StreamStop {
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        data: Option<serde_json::Value>,
    },
    /// v2: the relay refused one of our frames (`peer_offline`, `too_large`, …).
    Error {
        error: String,
    },
    #[serde(other)]
    Unknown,
}

/// Desktop -> server WebSocket frames. `Ping` must serialize to exactly `{"type":"ping"}`:
/// the server answers it with a hibernation auto-response without waking the Durable Object.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    Ack { command_id: String },
    Ping,
}

/// Query string that asks the relay for v2 frames on a header-authenticated desktop socket.
pub const PROTOCOL_V2_QUERY: &str = "protocol=2";

/// `GET /v1/ice-servers`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceServersResponse {
    #[serde(default)]
    pub ice_servers: Vec<IceServer>,
    #[serde(default)]
    pub turn: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IceServer {
    /// A string or an array of strings, as in `RTCIceServer`.
    #[serde(default, deserialize_with = "one_or_many")]
    pub urls: Vec<String>,
}

fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

impl IceServersResponse {
    /// Every `stun:` URL, in order. TURN entries are ignored (the desktop has no TURN client).
    pub fn stun_urls(&self) -> Vec<String> {
        self.ice_servers
            .iter()
            .flat_map(|s| s.urls.iter())
            .filter(|u| u.starts_with("stun:"))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairClaimRequest<'a> {
    pub code: &'a str,
    pub platform: &'a str,
    pub app_version: &'a str,
    pub desktop_name: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairClaimResponse {
    pub pair_id: String,
    pub desktop_token: String,
    #[serde(default)]
    pub mobile_device_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_every_server_message() {
        let hello: ServerMessage =
            serde_json::from_str(r#"{"type":"hello","protocol":1,"mobileDeviceName":"Pixel 9"}"#).unwrap();
        assert_eq!(
            hello,
            ServerMessage::Hello { protocol: 1, mobile_device_name: Some("Pixel 9".into()), peer_online: None }
        );
        let hello_null: ServerMessage =
            serde_json::from_str(r#"{"type":"hello","protocol":1,"mobileDeviceName":null}"#).unwrap();
        assert_eq!(hello_null, ServerMessage::Hello { protocol: 1, mobile_device_name: None, peer_online: None });

        let open: ServerMessage = serde_json::from_str(
            r#"{"type":"open_meeting","commandId":"c1","url":"https://zoom.us/j/1","createdAt":"2026-11-08T10:00:00.000Z","sentAt":"2026-11-08T10:00:01.000Z"}"#,
        )
        .unwrap();
        assert_eq!(
            open,
            ServerMessage::OpenMeeting {
                command_id: "c1".into(),
                url: "https://zoom.us/j/1".into(),
                created_at: Some("2026-11-08T10:00:00.000Z".into()),
                sent_at: Some("2026-11-08T10:00:01.000Z".into()),
            }
        );

        let unpaired: ServerMessage =
            serde_json::from_str(r#"{"type":"unpaired","reason":"revoked"}"#).unwrap();
        assert_eq!(unpaired, ServerMessage::Unpaired { reason: Some("revoked".into()) });

        let pong: ServerMessage = serde_json::from_str(r#"{"type":"pong"}"#).unwrap();
        assert_eq!(pong, ServerMessage::Pong);

        let unknown: ServerMessage =
            serde_json::from_str(r#"{"type":"brand_new_thing","x":1}"#).unwrap();
        assert_eq!(unknown, ServerMessage::Unknown);
    }

    #[test]
    fn deserializes_v2_frames() {
        let hello: ServerMessage =
            serde_json::from_str(r#"{"type":"hello","protocol":2,"mobileDeviceName":"Pixel 9","peerOnline":true}"#)
                .unwrap();
        assert_eq!(
            hello,
            ServerMessage::Hello { protocol: 2, mobile_device_name: Some("Pixel 9".into()), peer_online: Some(true) }
        );
        let peer: ServerMessage = serde_json::from_str(r#"{"type":"peer","role":"mobile","online":false}"#).unwrap();
        assert_eq!(peer, ServerMessage::Peer { role: "mobile".into(), online: false });
        let signal: ServerMessage = serde_json::from_str(
            r#"{"type":"signal","from":"mobile","data":{"kind":"stream_request","sessionId":"s1"}}"#,
        )
        .unwrap();
        assert_eq!(
            signal,
            ServerMessage::Signal {
                from: Some("mobile".into()),
                data: serde_json::json!({"kind":"stream_request","sessionId":"s1"}),
            }
        );
        let stop: ServerMessage = serde_json::from_str(r#"{"type":"stream_stop","from":"mobile"}"#).unwrap();
        assert_eq!(stop, ServerMessage::StreamStop { from: Some("mobile".into()), data: None });
        let stop: ServerMessage =
            serde_json::from_str(r#"{"type":"stream_stop","from":"mobile","data":{"sessionId":"s1","reason":"user"}}"#)
                .unwrap();
        assert!(matches!(stop, ServerMessage::StreamStop { data: Some(_), .. }));
        let err: ServerMessage = serde_json::from_str(r#"{"type":"error","error":"peer_offline"}"#).unwrap();
        assert_eq!(err, ServerMessage::Error { error: "peer_offline".into() });
        // A malformed known type is an error (the caller logs and ignores it), not a panic.
        assert!(serde_json::from_str::<ServerMessage>(r#"{"type":"peer"}"#).is_err());
    }

    #[test]
    fn ice_servers_response() {
        let r: IceServersResponse = serde_json::from_str(
            r#"{"iceServers":[{"urls":["stun:stun.cloudflare.com:3478","stun:stun.l.google.com:19302"]},{"urls":"turn:t.example:3478","username":"u","credential":"c"}],"turn":"disabled","ttl":900}"#,
        )
        .unwrap();
        assert_eq!(r.stun_urls(), vec!["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"]);
        assert_eq!(r.turn.as_deref(), Some("disabled"));
    }

    #[test]
    fn serializes_client_messages_exactly() {
        assert_eq!(serde_json::to_string(&ClientMessage::Ping).unwrap(), r#"{"type":"ping"}"#);
        assert_eq!(
            serde_json::to_string(&ClientMessage::Ack { command_id: "c1".into() }).unwrap(),
            r#"{"type":"ack","commandId":"c1"}"#
        );
    }

    #[test]
    fn claim_request_is_camel_case() {
        let req = PairClaimRequest { code: "123456", platform: "linux", app_version: "0.2.4", desktop_name: "box" };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"code":"123456","platform":"linux","appVersion":"0.2.4","desktopName":"box"}"#
        );
        let resp: PairClaimResponse = serde_json::from_str(
            r#"{"pairId":"p","desktopToken":"t","mobileDeviceName":"Pixel"}"#,
        )
        .unwrap();
        assert_eq!(resp.mobile_device_name.as_deref(), Some("Pixel"));
    }
}
