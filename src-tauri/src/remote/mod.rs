//! Phone remote: lets the companion mobile app ("CamLooper Remote") push a Zoom / Teams /
//! Meet link to this computer through the relay at api.camlooper.com.
//!
//! Privacy invariant: this module performs **no network activity at all** until the user
//! types a pairing code into the Remote card (`remote_link`). No HTTP client exists, no DNS
//! lookup happens and no socket is opened before that. Unlinking (from either side)
//! returns the app to that silent state. See docs/ARCHITECTURE.md, "Phone Remote".
pub mod client;
pub mod meeting_url;
pub mod protocol;
pub mod store;

pub use client::{OpenMeetingCommand, RemoteState};

/// Current remote state for the UI (no I/O).
#[tauri::command]
pub async fn remote_get_state() -> RemoteState {
    client::snapshot()
}

/// Claim the 6-digit code shown by the phone. This is the first network call the app ever
/// makes. `Err` carries a stable code the UI maps to a message:
/// `code_invalid` | `rate_limited` | `network` | `server` | `disabled`.
#[tauri::command]
pub async fn remote_link(app: tauri::AppHandle, code: String) -> Result<RemoteState, String> {
    client::link(app, code).await.map_err(|e| e.code().to_string())
}

/// Revoke the pairing on the server (best effort), forget it locally and go silent.
#[tauri::command]
pub async fn remote_unlink(app: tauri::AppHandle) -> Result<RemoteState, String> {
    client::unlink(app).await.map_err(|e| e.to_string())
}

/// Startup catch-up: a command the server flushed before React registered its listeners.
/// Returns without clearing; `remote_ack_command` clears it.
#[tauri::command]
pub async fn remote_take_pending_command() -> Option<OpenMeetingCommand> {
    client::pending_command()
}

#[tauri::command]
pub async fn remote_ack_command(command_id: String) {
    client::ack(command_id)
}

#[tauri::command]
pub async fn remote_set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    client::set_autostart(&app, enabled).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remote_get_autostart(app: tauri::AppHandle) -> bool {
    client::autostart_enabled(&app)
}

pub fn init(app: tauri::AppHandle) {
    client::init(app)
}

pub fn is_linked() -> bool {
    client::is_linked()
}

pub fn toggle_autostart_from_tray(app: &tauri::AppHandle) {
    client::toggle_autostart_from_tray(app)
}
