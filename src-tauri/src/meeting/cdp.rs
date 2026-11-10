//! The few bits of the Chrome DevTools Protocol auto-join needs: list the browser's targets over
//! its localhost HTTP endpoint and `Runtime.evaluate` on one page over its WebSocket.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub web_socket_debugger_url: Option<String>,
}

/// Loopback-only client. `no_proxy` so a system proxy never sees 127.0.0.1 requests.
pub fn local_http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?)
}

pub async fn is_alive(http: &reqwest::Client, port: u16) -> bool {
    http.get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

pub async fn list_targets(http: &reqwest::Client, port: u16) -> Result<Vec<Target>> {
    Ok(http
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// The meeting tab: a debuggable page whose host matches, preferring one that did not exist
/// before we launched (so an older meeting tab in the same window is not picked by mistake).
pub fn pick_target<'a>(
    targets: &'a [Target],
    host_matches: &dyn Fn(&str) -> bool,
    before: &HashSet<String>,
) -> Option<&'a Target> {
    let matching: Vec<&Target> = targets
        .iter()
        .filter(|t| t.kind == "page" && t.web_socket_debugger_url.is_some())
        .filter(|t| {
            url::Url::parse(&t.url)
                .ok()
                .and_then(|u| u.host_str().map(|h| host_matches(h)))
                .unwrap_or(false)
        })
        .collect();
    matching
        .iter()
        .copied()
        .find(|t| !before.contains(&t.id))
        .or_else(|| matching.first().copied())
}

pub struct PageSession {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl PageSession {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _) = tokio::time::timeout(Duration::from_secs(5), connect_async(ws_url))
            .await
            .context("devtools connect timed out")??;
        Ok(Self { ws, next_id: 0 })
    }

    /// Evaluate `expression` in the page and return its value (`returnByValue`).
    pub async fn evaluate(&mut self, expression: &str) -> Result<serde_json::Value> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = serde_json::json!({
            "id": id,
            "method": "Runtime.evaluate",
            "params": { "expression": expression, "returnByValue": true, "awaitPromise": true }
        });
        self.ws.send(Message::Text(msg.to_string().into())).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let next = tokio::time::timeout_at(deadline, self.ws.next())
                .await
                .context("devtools evaluate timed out")?;
            let frame = match next {
                Some(frame) => frame?,
                None => return Err(anyhow!("devtools socket closed")),
            };
            let text = match frame {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => return Err(anyhow!("devtools socket closed")),
                _ => continue,
            };
            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                continue; // an event or an older reply
            }
            if let Some(err) = v.get("error") {
                return Err(anyhow!("devtools error: {err}"));
            }
            if let Some(ex) = v.pointer("/result/exceptionDetails") {
                let desc = ex
                    .pointer("/exception/description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("unknown");
                return Err(anyhow!("join script threw: {desc}"));
            }
            return Ok(v.pointer("/result/result/value").cloned().unwrap_or(serde_json::Value::Null));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str, kind: &str, url: &str) -> Target {
        Target { id: id.into(), kind: kind.into(), url: url.into(), web_socket_debugger_url: Some(format!("ws://127.0.0.1:1/devtools/page/{id}")) }
    }

    #[test]
    fn picks_new_matching_page_over_old_one() {
        let targets = vec![
            t("old", "page", "https://meet.google.com/old-meet-ing"),
            t("sw", "service_worker", "https://meet.google.com/sw.js"),
            t("new", "page", "https://meet.google.com/new-meet-ing"),
            t("other", "page", "https://example.com/"),
        ];
        let before: HashSet<String> = ["old".to_string()].into_iter().collect();
        let picked = pick_target(&targets, &|h| h == "meet.google.com", &before).unwrap();
        assert_eq!(picked.id, "new");
        // Nothing new: fall back to the existing matching tab rather than giving up.
        let before: HashSet<String> = ["old".to_string(), "new".to_string()].into_iter().collect();
        assert_eq!(pick_target(&targets, &|h| h == "meet.google.com", &before).unwrap().id, "old");
        assert!(pick_target(&targets, &|h| h == "zoom.us", &HashSet::new()).is_none());
    }

    #[test]
    fn parses_json_list_shape() {
        let json = r#"[{"id":"7C4C","type":"page","url":"https://meet.google.com/x","webSocketDebuggerUrl":"ws://127.0.0.1:40227/devtools/page/7C4C","title":"Meet"}]"#;
        let targets: Vec<Target> = serde_json::from_str(json).unwrap();
        assert_eq!(targets[0].kind, "page");
        assert!(targets[0].web_socket_debugger_url.as_deref().unwrap().ends_with("/7C4C"));
    }
}
