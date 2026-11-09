//! Wire types for protocol v1 of the phone remote. Field names are camelCase on the wire;
//! messages are tagged by `type`. Unknown message types deserialize to `Unknown` so a newer
//! server never breaks an older desktop.
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
            ServerMessage::Hello { protocol: 1, mobile_device_name: Some("Pixel 9".into()) }
        );
        let hello_null: ServerMessage =
            serde_json::from_str(r#"{"type":"hello","protocol":1,"mobileDeviceName":null}"#).unwrap();
        assert_eq!(hello_null, ServerMessage::Hello { protocol: 1, mobile_device_name: None });

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
