//! System tray, close-to-tray and hidden start for the phone remote.
//!
//! The tray icon is always present so "Link phone…" is discoverable. Closing the window only
//! hides to the tray while a phone is linked (that is what keeps the computer reachable);
//! unlinked, close still quits as it always did. `--hidden` (passed by the autostart entry)
//! starts without showing the window — unless nothing is linked, in which case hiding would
//! leave the user with an icon and no explanation.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::OnceCell;
use serde::Deserialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

pub const HIDDEN_FLAG: &str = "--hidden";
pub const MAIN_WINDOW: &str = "main";

static STARTED_HIDDEN: OnceCell<bool> = OnceCell::new();

pub struct TrayHandles {
    show: MenuItem<Wry>,
    link: MenuItem<Wry>,
    /// "Stop phone camera": enabled while the phone holds the camera (connecting, live, or a
    /// stop that has not finished — a second click after a few seconds forces it).
    stop_phone: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
    phone: Mutex<PhoneTrayState>,
}

/// What the tray shows for the phone stream; kept so a language change can re-apply the
/// tooltip without waiting for the next stream transition.
struct PhoneTrayState {
    /// What the tray was last *successfully* switched to; `None` until the first update and
    /// after a failed one, so the next status retries instead of trusting a stale flag.
    applied: Option<PhoneTray>,
    wanted: PhoneTray,
    live_tooltip: String,
    connecting_tooltip: String,
}

/// The phone stream as the tray shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneTray {
    Idle,
    /// Accepted, no video yet: Stop works, but nothing is "live".
    Connecting,
    Live,
    /// Shutting down: Stop stays available as the way out of a hung teardown.
    Stopping,
}

impl PhoneTray {
    fn stop_enabled(self) -> bool {
        self != PhoneTray::Idle
    }
}

const IDLE_TOOLTIP: &str = "CamLooper";
const DEFAULT_LIVE_TOOLTIP: &str = "CamLooper — phone camera live";
const DEFAULT_CONNECTING_TOOLTIP: &str = "CamLooper — connecting to the phone…";

static QUITTING: AtomicBool = AtomicBool::new(false);

pub fn init(app: &tauri::App) -> tauri::Result<()> {
    let hidden = std::env::args().skip(1).any(|a| a == HIDDEN_FLAG);
    let _ = STARTED_HIDDEN.set(hidden);
    let handle = app.handle();
    let autostart_on = crate::remote::client::snapshot().autostart;

    // English defaults; the frontend pushes translated labels via `set_tray_labels`.
    let show = MenuItem::with_id(handle, "show", "Show CamLooper", true, None::<&str>)?;
    let link = MenuItem::with_id(handle, "link", "Link phone…", true, None::<&str>)?;
    let stop_phone = MenuItem::with_id(handle, "stop_phone", "Stop phone camera", false, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(handle, "autostart", "Start at login", true, autostart_on, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        handle,
        &[
            &show,
            &link,
            &stop_phone,
            &PredefinedMenuItem::separator(handle)?,
            &autostart,
            &PredefinedMenuItem::separator(handle)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip(IDLE_TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "link" => {
                show_main_window(app);
                let _ = app.emit("remote://show-linking", ());
            }
            "stop_phone" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::remote::stream::commands::remote_stream_stop().await {
                        // No window may be open to see it; the frontend toasts when it is.
                        eprintln!("phone stream: stop from the tray failed: {e}");
                        let _ = app.emit("remote://stream-stop-failed", e);
                    }
                });
            }
            "autostart" => crate::remote::toggle_autostart_from_tray(app),
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    app.manage(TrayHandles {
        show,
        link,
        stop_phone,
        autostart,
        quit,
        phone: Mutex::new(PhoneTrayState {
            applied: None,
            wanted: PhoneTray::Idle,
            live_tooltip: DEFAULT_LIVE_TOOLTIP.to_string(),
            connecting_tooltip: DEFAULT_CONNECTING_TOOLTIP.to_string(),
        }),
    });

    if !hidden || !crate::remote::is_linked() {
        show_main_window(handle);
    } else {
        eprintln!("remote: started hidden (autostart); waiting in the tray");
    }
    Ok(())
}

/// Bring the main window back. Order matters on Windows: tao only forces the window to the
/// foreground when it is already visible and not minimized.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = app.emit("app://shown", ());
    }
}

pub fn set_autostart_checked(app: &AppHandle, checked: bool) {
    if let Some(handles) = app.try_state::<TrayHandles>() {
        let _ = handles.autostart.set_checked(checked);
    }
}

/// A phone asks to stream: put the window on screen and ask for attention, but do **not**
/// take keyboard focus. Focus arriving mid-keystroke is how an Enter meant for another app
/// would land on the confirm dialog. (Showing a hidden window may still activate it on some
/// platforms; the dialog guards against that itself.)
pub fn raise_main_window_for_request(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Critical));
        let _ = app.emit("app://shown", ());
    }
}

/// The request was answered or went away: stop flashing the taskbar / bouncing the Dock.
pub fn clear_attention(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.request_user_attention(None);
    }
}

/// Quit from the tray (or the app menu): end a phone stream first — or decline a pending
/// request — so the phone is told instead of being left to time out.
pub fn quit_app(app: &AppHandle) {
    if QUITTING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::remote::stream::commands::stop_for_exit().await;
        app.exit(0);
    });
}

/// For `RunEvent::ExitRequested`: `true` when the exit must wait for [`quit_app`] (which was
/// just started); `false` when it may proceed (nothing to stop, or it is already under way).
pub fn defer_exit_for_phone_stream(app: &AppHandle) -> bool {
    if QUITTING.load(Ordering::SeqCst) || !crate::remote::stream::commands::stream_busy() {
        return false;
    }
    quit_app(app);
    true
}

/// Follow the phone stream: "Stop phone camera" enabled while the phone holds the camera,
/// and a tooltip that says connecting or live. Cheap to call on every status update (only
/// acts on a change), and a failed update is retried on the next call.
pub fn set_phone_stream(app: &AppHandle, state: PhoneTray) {
    let Some(handles) = app.try_state::<TrayHandles>() else { return };
    {
        let mut phone = handles.phone.lock().unwrap_or_else(|e| e.into_inner());
        phone.wanted = state;
        if phone.applied == Some(state) {
            return;
        }
    }
    if let Err(e) = apply_phone_tray(app, &handles) {
        eprintln!("tray: could not update the phone stream state: {e}");
    }
}

fn apply_phone_tray(app: &AppHandle, handles: &TrayHandles) -> tauri::Result<()> {
    sync_phone_tray(&handles.phone, |state, tooltip| {
        let enabled = handles.stop_phone.set_enabled(state.stop_enabled());
        let tooltip = match app.tray_by_id("main") {
            Some(tray) => tray.set_tooltip(Some(tooltip)),
            None => Ok(()),
        };
        enabled.and(tooltip)
    })
}

/// How often [`sync_phone_tray`] starts over because the state changed under it.
const TRAY_SYNC_ATTEMPTS: usize = 4;

/// Bring the tray in line with `phone.wanted` and the current tooltip texts.
///
/// `apply` is called **without** the lock held. Tray and menu calls block until the main
/// thread has run them, and the main thread may itself be waiting for this lock
/// ([`set_tray_labels`] is a sync command): holding it across the call would freeze the app.
/// Because the lock is released, the wanted state or the texts can change during the call;
/// what was applied is then out of date and is applied again.
fn sync_phone_tray<E>(
    phone: &Mutex<PhoneTrayState>,
    mut apply: impl FnMut(PhoneTray, &str) -> Result<(), E>,
) -> Result<(), E> {
    let mut result = Ok(());
    for _ in 0..TRAY_SYNC_ATTEMPTS {
        let (state, tooltip) = {
            let phone = phone.lock().unwrap_or_else(|e| e.into_inner());
            (phone.wanted, tooltip_for(&phone).to_owned())
        };
        result = apply(state, &tooltip);
        let mut phone = phone.lock().unwrap_or_else(|e| e.into_inner());
        if phone.wanted == state && tooltip_for(&phone) == tooltip {
            phone.applied = result.is_ok().then_some(state);
            return result;
        }
        // Changed meanwhile: not known to be applied until the next round says so.
        phone.applied = None;
    }
    result
}

fn tooltip_for(phone: &PhoneTrayState) -> &str {
    match phone.wanted {
        PhoneTray::Live => phone.live_tooltip.as_str(),
        PhoneTray::Connecting => phone.connecting_tooltip.as_str(),
        PhoneTray::Idle | PhoneTray::Stopping => IDLE_TOOLTIP,
    }
}

pub fn autostart_checked(app: &AppHandle) -> Option<bool> {
    app.try_state::<TrayHandles>().and_then(|h| h.autostart.is_checked().ok())
}

/// `"hidden"` when launched by the autostart entry; the frontend gates the welcome popup and
/// the ad banner on the window having actually been shown.
#[tauri::command]
pub fn get_startup_mode() -> &'static str {
    if STARTED_HIDDEN.get().copied().unwrap_or(false) {
        "hidden"
    } else {
        "normal"
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    pub show: String,
    pub link: String,
    pub autostart: String,
    pub quit: String,
    #[serde(default)]
    pub stop_phone: Option<String>,
    /// Tooltip while the phone camera is live.
    #[serde(default)]
    pub phone_live: Option<String>,
    /// Tooltip between Accept and the first video frame.
    #[serde(default)]
    pub phone_connecting: Option<String>,
}

/// The first error of several independent updates, after all of them were attempted.
fn first_error<E: std::fmt::Display>(results: impl IntoIterator<Item = Result<(), E>>) -> Result<(), String> {
    let mut first = None;
    for r in results {
        if let Err(e) = r {
            first.get_or_insert_with(|| e.to_string());
        }
    }
    first.map_or(Ok(()), Err)
}

#[tauri::command]
pub fn set_tray_labels(app: AppHandle, labels: TrayLabels) -> Result<(), String> {
    let handles = app.try_state::<TrayHandles>().ok_or("tray not initialised")?;
    // Apply every label even if one fails: a half-translated menu with an untranslated
    // tooltip is worse than one missing label.
    let mut results = vec![
        handles.show.set_text(&labels.show),
        handles.link.set_text(&labels.link),
        handles.autostart.set_text(&labels.autostart),
        handles.quit.set_text(&labels.quit),
    ];
    if let Some(text) = &labels.stop_phone {
        results.push(handles.stop_phone.set_text(text));
    }
    if labels.phone_live.is_some() || labels.phone_connecting.is_some() {
        {
            let mut phone = handles.phone.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(text) = labels.phone_live {
                phone.live_tooltip = text;
            }
            if let Some(text) = labels.phone_connecting {
                phone.connecting_tooltip = text;
            }
        }
        // Not under the lock: see `sync_phone_tray`.
        results.push(apply_phone_tray(&app, &handles));
    }
    first_error(results)
}

#[cfg(test)]
mod tests {
    use super::{first_error, sync_phone_tray, tooltip_for, PhoneTray, PhoneTrayState, TrayLabels, IDLE_TOOLTIP};
    use std::sync::Mutex;

    fn phone_state(wanted: PhoneTray) -> Mutex<PhoneTrayState> {
        Mutex::new(PhoneTrayState {
            applied: None,
            wanted,
            live_tooltip: "LIVE".into(),
            connecting_tooltip: "CONNECTING".into(),
        })
    }

    #[test]
    fn the_tray_is_updated_without_the_lock_held() {
        let phone = phone_state(PhoneTray::Live);
        let mut calls = vec![];
        let r: Result<(), &str> = sync_phone_tray(&phone, |state, tooltip| {
            // The main thread must be able to take the lock while this call waits for it.
            assert!(phone.try_lock().is_ok(), "lock held across the tray call");
            calls.push((state, tooltip.to_string()));
            Ok(())
        });
        assert_eq!(r, Ok(()));
        assert_eq!(calls, [(PhoneTray::Live, "LIVE".to_string())]);
        assert_eq!(phone.lock().unwrap().applied, Some(PhoneTray::Live));
    }

    #[test]
    fn a_change_during_the_tray_call_is_applied_too() {
        let phone = phone_state(PhoneTray::Connecting);
        let mut calls = vec![];
        let r: Result<(), &str> = sync_phone_tray(&phone, |state, tooltip| {
            if calls.is_empty() {
                // Meanwhile: the stream went live and the language changed.
                let mut p = phone.lock().unwrap();
                p.wanted = PhoneTray::Live;
                p.live_tooltip = "EN DIRECT".into();
            }
            calls.push((state, tooltip.to_string()));
            Ok(())
        });
        assert_eq!(r, Ok(()));
        assert_eq!(
            calls,
            [(PhoneTray::Connecting, "CONNECTING".to_string()), (PhoneTray::Live, "EN DIRECT".to_string())]
        );
        assert_eq!(phone.lock().unwrap().applied, Some(PhoneTray::Live));
    }

    #[test]
    fn a_failed_tray_update_is_not_recorded_as_applied() {
        let phone = phone_state(PhoneTray::Live);
        phone.lock().unwrap().applied = Some(PhoneTray::Connecting);
        assert_eq!(sync_phone_tray(&phone, |_, _| Err("no tray")), Err("no tray"));
        assert_eq!(phone.lock().unwrap().applied, None);
    }

    #[test]
    fn every_label_is_attempted_and_the_first_error_reported() {
        let mut attempted = 0;
        let results = ["ok", "first", "ok", "second"].map(|r| {
            attempted += 1;
            if r == "ok" { Ok(()) } else { Err(r) }
        });
        assert_eq!(first_error(results), Err("first".to_string()));
        assert_eq!(attempted, 4);
        assert_eq!(first_error([Ok::<(), &str>(()), Ok(())]), Ok(()));
    }

    #[test]
    fn the_tooltip_says_live_only_when_live() {
        let mut phone = PhoneTrayState {
            applied: None,
            wanted: PhoneTray::Idle,
            live_tooltip: "LIVE".into(),
            connecting_tooltip: "CONNECTING".into(),
        };
        let mut seen = vec![];
        for state in [PhoneTray::Idle, PhoneTray::Connecting, PhoneTray::Live, PhoneTray::Stopping] {
            phone.wanted = state;
            seen.push((tooltip_for(&phone).to_string(), state.stop_enabled()));
        }
        assert_eq!(
            seen,
            [
                (IDLE_TOOLTIP.to_string(), false),
                ("CONNECTING".to_string(), true),
                ("LIVE".to_string(), true),
                (IDLE_TOOLTIP.to_string(), true),
            ]
        );
    }

    #[test]
    fn tray_labels_accept_old_and_new_payloads() {
        // An older frontend sends only the four menu labels.
        let old: TrayLabels =
            serde_json::from_str(r#"{"show":"S","link":"L","autostart":"A","quit":"Q"}"#).unwrap();
        assert_eq!(old.stop_phone, None);
        assert_eq!(old.phone_live, None);
        assert_eq!(old.phone_connecting, None);

        let new: TrayLabels = serde_json::from_str(
            r#"{"show":"S","link":"L","autostart":"A","quit":"Q","stopPhone":"Stop","phoneLive":"Live","phoneConnecting":"Wait"}"#,
        )
        .unwrap();
        assert_eq!(new.stop_phone.as_deref(), Some("Stop"));
        assert_eq!(new.phone_live.as_deref(), Some("Live"));
        assert_eq!(new.phone_connecting.as_deref(), Some("Wait"));
    }
}
