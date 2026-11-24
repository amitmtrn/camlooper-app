//! System tray, close-to-tray and hidden start for the phone remote.
//!
//! The tray icon is always present so "Link phone…" is discoverable. Closing the window only
//! hides to the tray while a phone is linked (that is what keeps the computer reachable);
//! unlinked, close still quits as it always did. `--hidden` (passed by the autostart entry)
//! starts without showing the window — unless nothing is linked, in which case hiding would
//! leave the user with an icon and no explanation.
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
    /// "Stop phone camera": enabled only while a phone stream is connecting or live.
    stop_phone: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    quit: MenuItem<Wry>,
    phone: Mutex<PhoneTrayState>,
}

/// What the tray shows for the phone stream; kept so a language change can re-apply the
/// tooltip without waiting for the next stream transition.
struct PhoneTrayState {
    active: bool,
    live_tooltip: String,
}

const IDLE_TOOLTIP: &str = "CamLooper";
const DEFAULT_LIVE_TOOLTIP: &str = "CamLooper — phone camera live";

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
                tauri::async_runtime::spawn(async {
                    let _ = crate::remote::stream::commands::remote_stream_stop().await;
                });
            }
            "autostart" => crate::remote::toggle_autostart_from_tray(app),
            "quit" => app.exit(0),
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
        phone: Mutex::new(PhoneTrayState { active: false, live_tooltip: DEFAULT_LIVE_TOOLTIP.to_string() }),
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

/// Phone stream connecting/live: enable "Stop phone camera" and change the tooltip; idle
/// reverts both. Cheap to call on every status update (only acts on a change).
pub fn set_phone_stream_active(app: &AppHandle, active: bool) {
    let Some(handles) = app.try_state::<TrayHandles>() else { return };
    let mut phone = handles.phone.lock().unwrap_or_else(|e| e.into_inner());
    if phone.active == active {
        return;
    }
    phone.active = active;
    let _ = handles.stop_phone.set_enabled(active);
    apply_tooltip(app, &phone);
}

fn apply_tooltip(app: &AppHandle, phone: &PhoneTrayState) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(if phone.active { phone.live_tooltip.as_str() } else { IDLE_TOOLTIP }));
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
}

#[tauri::command]
pub fn set_tray_labels(app: AppHandle, labels: TrayLabels) -> Result<(), String> {
    let handles = app.try_state::<TrayHandles>().ok_or("tray not initialised")?;
    handles.show.set_text(&labels.show).map_err(|e| e.to_string())?;
    handles.link.set_text(&labels.link).map_err(|e| e.to_string())?;
    handles.autostart.set_text(&labels.autostart).map_err(|e| e.to_string())?;
    handles.quit.set_text(&labels.quit).map_err(|e| e.to_string())?;
    if let Some(text) = &labels.stop_phone {
        handles.stop_phone.set_text(text).map_err(|e| e.to_string())?;
    }
    if let Some(text) = labels.phone_live {
        let mut phone = handles.phone.lock().unwrap_or_else(|e| e.into_inner());
        phone.live_tooltip = text;
        apply_tooltip(&app, &phone);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::TrayLabels;

    #[test]
    fn tray_labels_accept_old_and_new_payloads() {
        // An older frontend sends only the four menu labels.
        let old: TrayLabels =
            serde_json::from_str(r#"{"show":"S","link":"L","autostart":"A","quit":"Q"}"#).unwrap();
        assert_eq!(old.stop_phone, None);
        assert_eq!(old.phone_live, None);

        let new: TrayLabels = serde_json::from_str(
            r#"{"show":"S","link":"L","autostart":"A","quit":"Q","stopPhone":"Stop","phoneLive":"Live"}"#,
        )
        .unwrap();
        assert_eq!(new.stop_phone.as_deref(), Some("Stop"));
        assert_eq!(new.phone_live.as_deref(), Some("Live"));
    }
}
