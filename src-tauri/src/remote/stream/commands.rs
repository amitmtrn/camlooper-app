//! Tauri glue for the phone stream: the app-wide [`StreamManager`], the decision sink that
//! asks the UI, and thin commands. The UI side (confirm dialog, live banner, stop button) is
//! `src/hooks/use-phone-stream.ts` and `src/components/PhoneStream*.tsx`; the tray's
//! "Stop phone camera" item calls [`remote_stream_stop`] too.
//!
//! Events (all payloads camelCase):
//! * `remote://stream-status` — [`StreamStatus`] on every change, with a growing `seq`. This
//!   is the UI's single source of truth: the confirm dialog is shown while the status says
//!   `requested` (it carries `sessionId`, `deviceName` and `expiresAt`).
//! * `remote://stream-request` — [`StreamRequest`] and `remote://stream-request-dismissed` —
//!   `{sessionId}`: informational (logging); the UI must not depend on their order relative
//!   to the status events, which travel through a separate task.
//! * `remote://stream-stop-failed` — error code: the tray's "Stop phone camera" failed.
//! * `remote://phone-video-loaded` — `{video: VideoInfo, playing}`: a video received from the
//!   phone was loaded. `playing: true`: it is looping. `playing: false`: it is loaded but
//!   playback could not be started (and there was no earlier video to go back to). `video`
//!   is exactly what `load_video_file` / `stream_upload_and_load_video` return for a
//!   desktop-picked file, so the UI can put it where it puts those. The virtual camera is
//!   running by then (started here if it was off).
//! * `remote://loop-changed` — `{playing}`: the loop was stopped or started from here and
//!   not by the window: the phone's `loop_control`, or a phone video that could not be
//!   started after the earlier one had been stopped for it.
//!
//! [`StreamStatus`] also carries `mode` (`"live"` | `"file"` | `"record"`) and, for a file
//! session, `file: {name, size, received, phase, error?}`; for a record session,
//! `record: {elapsedMs, capMs, phase, error?}` (`phase`: `recording` | `processing` |
//! `looping` | `failed`) — see `machine.rs`. A recorded take that loops is announced with
//! the same `remote://phone-video-loaded` as a received file.
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use once_cell::sync::OnceCell;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use super::file_receive::{FileDeps, LoopPlayer, PhoneVideoStore};
use super::handoff::AppCamera;
use super::loop_engine::{EngineLoop, LoopHost, VideoInfo};
use super::machine::{Phase, StreamRequest, StreamStatus};
use super::manager::{DecisionSink, LoopDeps, StreamManager};
use super::runner::{IceSource, SessionDeps};
use super::signal::{OutboundFrame, StopReason, StreamMode};

static MANAGER: OnceCell<StreamManager> = OnceCell::new();
/// `<app data dir>/phone-videos/`, for the sweep on exit.
static PHONE_VIDEOS: OnceCell<PhoneVideoStore> = OnceCell::new();

/// The app's side of the loop engine ([`EngineLoop`]): the platform virtual camera, and
/// the events that keep the main window in step with what the phone did.
struct AppLoopHost(AppHandle);

impl LoopHost for AppLoopHost {
    fn ensure_camera(&self) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { super::handoff::ensure_virtual_camera().await.map(|_| ()) })
    }

    fn loop_changed(&self, playing: bool) {
        let _ = self.0.emit("remote://loop-changed", serde_json::json!({ "playing": playing }));
    }

    fn video_loaded(&self, video: &VideoInfo, playing: bool) {
        let _ = self.0.emit("remote://phone-video-loaded", serde_json::json!({ "video": video, "playing": playing }));
    }
}

/// A video was loaded from the computer itself (the window's file picker, a recording): a
/// phone video that was loaded until now is no longer needed on disk. Files a transfer is
/// working on are left alone by the store.
pub async fn sweep_unused_phone_videos() {
    let Some(store) = PHONE_VIDEOS.get() else { return };
    let loaded = crate::video_processor::loaded_video().await.map(|(path, _)| path);
    let removed = store.sweep(loaded.as_deref());
    if removed > 0 {
        log::debug!("phone stream: removed {removed} phone video file(s) that are no longer loaded");
    }
}

/// The app is exiting: no phone video is restored on the next start (the app does not
/// remember the last loaded video), so none is kept. Best effort — a file still held open
/// by a playing ffmpeg (Windows) is removed by the sweep at the next start.
pub fn sweep_phone_videos_for_exit() {
    if let Some(store) = PHONE_VIDEOS.get() {
        store.sweep(None);
    }
}

/// An accept is refused until the request has been in front of the user this long. The dialog
/// keeps its Accept button disabled for longer than this; the check here is the backstop for
/// a keystroke or click that was already on its way when the window appeared.
pub const MIN_DECISION_DELAY: Duration = Duration::from_millis(800);

/// The request last handed to the UI, and when.
static LAST_REQUEST: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// Whether an accept for `session_id` at `now` comes too soon after the request was shown.
/// A session the UI was never asked about is not this check's business (the machine answers
/// `no_such_request`).
fn accept_too_soon(shown: Option<&(String, Instant)>, session_id: &str, now: Instant) -> bool {
    shown.is_some_and(|(id, at)| id == session_id && now.saturating_duration_since(*at) < MIN_DECISION_DELAY)
}

struct UiDecisions(AppHandle);

impl DecisionSink for UiDecisions {
    fn request(&self, req: StreamRequest) {
        *LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner()) = Some((req.session_id.clone(), Instant::now()));
        // On screen and asking for attention, but without taking keyboard focus.
        crate::tray::raise_main_window_for_request(&self.0);
        let _ = self.0.emit("remote://stream-request", req);
    }
    fn dismissed(&self, session_id: &str) {
        crate::tray::clear_attention(&self.0);
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
    // Phone videos go under the app's own data directory and nowhere else. Without one
    // there is no fallback (a shared temp directory is a place where another user can put
    // a directory or a symlink first): videos from the phone are then refused outright.
    let store = match app.path().app_data_dir() {
        Ok(app_data) => Some(PhoneVideoStore::in_app_data(&app_data)),
        Err(e) => {
            eprintln!("phone stream: no app data directory ({e}); videos from the phone are disabled");
            None
        }
    };
    if let Some(store) = &store {
        // Left over from the last run (a crash, a file that was still open at exit).
        // Nothing is loaded yet and nothing will be restored, so everything goes. (A second
        // instance cannot be looping one of them: `tauri_plugin_single_instance` is the
        // first plugin registered in lib.rs, long before the relay task calls this.)
        let swept = store.sweep(None);
        if swept > 0 {
            eprintln!("phone stream: removed {swept} leftover phone video file(s)");
        }
        let _ = PHONE_VIDEOS.set(store.clone());
    }
    let player: Arc<dyn LoopPlayer> = Arc::new(EngineLoop::new(Arc::new(AppLoopHost(app.clone()))));
    let mut deps = SessionDeps::new(ffmpeg, Arc::new(AppCamera::default()), Arc::new(RelayIce));
    deps.files = store.clone().map(|store| FileDeps::new(store, player.clone()));
    let (manager, outbound) = StreamManager::spawn_with_loop(
        Arc::new(Arc::new(deps)),
        Arc::new(UiDecisions(app.clone())),
        LoopDeps { player, store },
    );
    let mut status = manager.subscribe();
    tokio::spawn(async move {
        while status.changed().await.is_ok() {
            let s = status.borrow_and_update().clone();
            crate::tray::set_phone_stream(&app, tray_state(s.state, s.mode));
            let _ = app.emit("remote://stream-status", s);
        }
    });
    let _ = MANAGER.set(manager.clone());
    (manager, outbound)
}

fn tray_state(phase: Phase, mode: StreamMode) -> crate::tray::PhoneTray {
    use crate::tray::PhoneTray;
    match phase {
        Phase::Connecting => PhoneTray::Connecting,
        // A video being received is a session that Stop can cancel, but no camera is
        // "live": the tray must not say so.
        Phase::Live if mode == StreamMode::File => PhoneTray::Connecting,
        Phase::Live => PhoneTray::Live,
        Phase::Stopping => PhoneTray::Stopping,
        Phase::Idle | Phase::Requested | Phase::Error => PhoneTray::Idle,
    }
}

fn manager() -> Result<&'static StreamManager, String> {
    MANAGER.get().ok_or_else(|| "not_ready".to_string())
}

/// A request is pending or a session is up (quitting now would leave the phone guessing),
/// or a given-up session still holds the camera.
pub fn stream_busy() -> bool {
    MANAGER.get().is_some_and(|m| m.status().is_busy())
}

/// The window is being closed with the confirm dialog open: that is a "no". Without this the
/// phone would wait out the full 60 s for an answer nobody can give any more.
pub fn decline_pending_request() {
    let Some(m) = MANAGER.get() else { return };
    let st = m.status();
    let (Phase::Requested, Some(id)) = (st.state, st.session_id) else { return };
    tauri::async_runtime::spawn(async move {
        let _ = m.reject(&id).await;
    });
}

/// The app is quitting: stop the stream (or decline the pending request), then give the
/// session a moment to release the camera and the relay socket a moment to carry the
/// `stream_stop` to the phone. Bounded, so a hung teardown cannot keep the app from quitting.
pub async fn stop_for_exit() {
    let Some(m) = MANAGER.get() else { return };
    if !m.status().is_busy() {
        return;
    }
    m.stop(StopReason::User).await;
    let mut status = m.subscribe();
    // Also the last chance for a given-up session to hand the camera back.
    let _ = tokio::time::timeout(Duration::from_secs(3), status.wait_for(|s| !s.is_busy())).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tauri::command]
pub async fn remote_stream_status() -> StreamStatus {
    MANAGER.get().map(|m| m.status()).unwrap_or_default()
}

/// Only the main window's confirm dialog may accept; any other webview is refused.
/// * `Err("no_such_request")` — the request already expired or was cancelled.
/// * `Err("offline")` — the relay socket is down: the accept could not reach the phone, and
///   starting the session anyway would pause the video loop for a connection that cannot be
///   negotiated. The request stays pending; the UI keeps its dialog open.
/// * `Err("too_soon")` — less than [`MIN_DECISION_DELAY`] after the request was shown; the
///   request stays pending.
/// * `Err("not_ready")` / `Err("forbidden")`.
#[tauri::command]
pub async fn remote_stream_accept(window: tauri::WebviewWindow, session_id: String) -> Result<(), String> {
    if window.label() != crate::tray::MAIN_WINDOW {
        return Err("forbidden".to_string());
    }
    let manager = manager()?;
    {
        let shown = LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
        if accept_too_soon(shown.as_ref(), &session_id, Instant::now()) {
            return Err("too_soon".to_string());
        }
    }
    if !crate::remote::client::is_connected() {
        return Err("offline".to_string());
    }
    manager.accept(&session_id).await.map_err(|_| "no_such_request".to_string())?;
    crate::tray::clear_attention(window.app_handle());
    Ok(())
}

#[tauri::command]
pub async fn remote_stream_reject(app: AppHandle, session_id: String) -> Result<(), String> {
    crate::tray::clear_attention(&app);
    manager()?.reject(&session_id).await.map_err(|_| "no_such_request".to_string())
}

/// Stops a stream, declines a pending request; a repeat while a stop hangs forces it (see
/// `FORCE_STOP_AFTER` in machine.rs). Also what the tray's "Stop phone camera" calls.
#[tauri::command]
pub async fn remote_stream_stop() -> Result<(), String> {
    manager()?.stop(StopReason::User).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accept_right_after_the_request_is_refused() {
        let t = Instant::now();
        let shown = Some(("s1".to_string(), t));
        assert!(accept_too_soon(shown.as_ref(), "s1", t));
        assert!(accept_too_soon(shown.as_ref(), "s1", t + MIN_DECISION_DELAY - Duration::from_millis(1)));
        assert!(!accept_too_soon(shown.as_ref(), "s1", t + MIN_DECISION_DELAY));
        // Another session, or nothing shown: left to the state machine to refuse.
        assert!(!accept_too_soon(shown.as_ref(), "s2", t));
        assert!(!accept_too_soon(None, "s1", t));
    }

    #[test]
    fn the_tray_follows_the_phase() {
        use crate::tray::PhoneTray;
        let live = StreamMode::Live;
        assert_eq!(tray_state(Phase::Requested, live), PhoneTray::Idle);
        assert_eq!(tray_state(Phase::Connecting, live), PhoneTray::Connecting);
        assert_eq!(tray_state(Phase::Live, live), PhoneTray::Live);
        assert_eq!(tray_state(Phase::Stopping, live), PhoneTray::Stopping);
        assert_eq!(tray_state(Phase::Error, live), PhoneTray::Idle);
        // A file transfer can be stopped from the tray, but it is never "camera live";
        // idle with the last transfer's outcome still in the status is idle.
        // A record session is the phone's camera, live, until it ends.
        assert_eq!(tray_state(Phase::Live, StreamMode::Record), PhoneTray::Live);
        assert_eq!(tray_state(Phase::Idle, StreamMode::Record), PhoneTray::Idle);
        assert_eq!(tray_state(Phase::Live, StreamMode::File), PhoneTray::Connecting);
        assert_eq!(tray_state(Phase::Stopping, StreamMode::File), PhoneTray::Stopping);
        assert_eq!(tray_state(Phase::Idle, StreamMode::File), PhoneTray::Idle);
    }
}
