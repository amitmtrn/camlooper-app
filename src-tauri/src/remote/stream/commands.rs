//! Tauri glue for the phone stream: the app-wide [`StreamManager`], the decision sink that
//! asks the UI, and thin commands. The UI side (confirm dialog, live banner, stop button) is
//! `src/hooks/use-phone-stream.ts` and `src/components/PhoneStream*.tsx`; the tray's
//! "Stop phone camera" item calls [`remote_stream_stop`] too.
//!
//! Events (all payloads camelCase):
//! * `remote://stream-request` — [`StreamRequest`]: show "Pixel 9 wants to stream its camera".
//! * `remote://stream-request-dismissed` — `{sessionId}`: close that dialog (timed out after
//!   60 s, or the phone cancelled / went offline).
//! * `remote://stream-status` — [`StreamStatus`] on every change.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use once_cell::sync::OnceCell;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

use super::handoff::AppCamera;
use super::machine::{Phase, StreamRequest, StreamStatus};
use super::manager::{DecisionSink, StreamManager};
use super::runner::{IceSource, SessionDeps};
use super::signal::{OutboundFrame, StopReason};

static MANAGER: OnceCell<StreamManager> = OnceCell::new();

struct UiDecisions(AppHandle);

impl DecisionSink for UiDecisions {
    fn request(&self, req: StreamRequest) {
        crate::tray::show_main_window(&self.0);
        let _ = self.0.emit("remote://stream-request", req);
    }
    fn dismissed(&self, session_id: &str) {
        let _ = self.0.emit("remote://stream-request-dismissed", serde_json::json!({ "sessionId": session_id }));
    }
}

/// STUN from `/v1/ice-servers` for the current pairing; the first `stun:` URL, resolved.
struct RelayIce;

impl IceSource for RelayIce {
    fn stun_server(&self) -> BoxFuture<'_, Option<SocketAddr>> {
        Box::pin(async {
            let base = crate::remote::client::api_base()?;
            let pairing = crate::remote::client::current_pairing()?;
            let urls = match tokio::time::timeout(
                Duration::from_secs(3),
                crate::remote::client::fetch_ice_servers(&base, &pairing),
            )
            .await
            {
                Ok(Ok(urls)) => urls,
                Ok(Err(e)) => {
                    eprintln!("phone stream: ice-servers unavailable ({e}); host candidates only");
                    return None;
                }
                Err(_) => return None,
            };
            super::rtc::resolve_stun(&urls, Duration::from_millis(800)).await
        })
    }
}

/// Create the app's manager. Called once, from the relay task, which owns the outbound frames.
pub(crate) fn install(app: AppHandle) -> (StreamManager, mpsc::UnboundedReceiver<OutboundFrame>) {
    let ffmpeg = crate::camera_capture::resolve_ffmpeg_path(&app).unwrap_or_else(|e| {
        eprintln!("phone stream: {e}");
        std::path::PathBuf::from("ffmpeg")
    });
    let deps = Arc::new(SessionDeps::new(ffmpeg, Arc::new(AppCamera::default()), Arc::new(RelayIce)));
    let (manager, outbound) = StreamManager::spawn(Arc::new(deps), Arc::new(UiDecisions(app.clone())));
    let mut status = manager.subscribe();
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            let s = status.borrow_and_update().clone();
            crate::tray::set_phone_stream_active(&app, matches!(s.state, Phase::Connecting | Phase::Live));
            let _ = app.emit("remote://stream-status", s);
        }
    });
    let _ = MANAGER.set(manager.clone());
    (manager, outbound)
}

fn manager() -> Result<&'static StreamManager, String> {
    MANAGER.get().ok_or_else(|| "not_ready".to_string())
}

#[tauri::command]
pub async fn remote_stream_status() -> StreamStatus {
    MANAGER.get().map(|m| m.status()).unwrap_or_default()
}

/// `Err("no_such_request")` when the request already expired or was cancelled.
#[tauri::command]
pub async fn remote_stream_accept(session_id: String) -> Result<(), String> {
    manager()?.accept(&session_id).await.map_err(|_| "no_such_request".to_string())
}

#[tauri::command]
pub async fn remote_stream_reject(session_id: String) -> Result<(), String> {
    manager()?.reject(&session_id).await.map_err(|_| "no_such_request".to_string())
}

#[tauri::command]
pub async fn remote_stream_stop() -> Result<(), String> {
    manager()?.stop(StopReason::User).await;
    Ok(())
}
