//! Auto-join: open the meeting in a Chromium browser that CamLooper controls.
//!
//! The phone remote delivers a meeting link. With auto-join on, instead of handing the URL to
//! the default browser we launch Chrome / Edge / Brave / Chromium with a **dedicated profile**
//! (`<app_data_dir>/meeting-browser`), remote debugging on a random localhost port and
//! `--use-fake-ui-for-media-stream`, which makes the browser accept camera and microphone
//! prompts by itself. A small DevTools Protocol client (`cdp.rs`) then evaluates a per-provider
//! script (`scripts/*.js`) in the meeting tab once a second until it reports that the call is
//! joined (`join.rs`). The dedicated profile keeps the user's Google / Microsoft / Zoom sign-ins,
//! so Meet offers "Join now" instead of a guest "Ask to join". Chrome 136+ refuses remote
//! debugging on the default profile anyway, so the separate profile is mandatory.
//!
//! Network: everything in this module talks to 127.0.0.1 only. The browser itself reaches the
//! meeting provider exactly as it would when the user opens the link by hand. Nothing here
//! contacts api.camlooper.com.
pub mod browser;
pub mod cdp;
pub mod join;

use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInfo {
    pub available: bool,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingOpenResult {
    /// `auto` (CamLooper drives the join), `default` (auto-join is off: caller opens the URL in
    /// the default browser) or `unavailable` (auto-join is on but no Chromium browser exists).
    pub mode: &'static str,
    pub browser: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingStatus {
    pub provider: String,
    /// `launching` | `joining` | `waiting_admission` | `joined` | `blocked` | `timeout` | `failed`
    pub phase: &'static str,
    pub detail: Option<String>,
}

pub(crate) fn emit_status(app: &AppHandle, provider: &str, phase: &'static str, detail: Option<String>) {
    let _ = app.emit(
        "meeting://status",
        MeetingStatus { provider: provider.to_string(), phase, detail },
    );
}

fn profile_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_data_dir()?.join("meeting-browser");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Which Chromium browser auto-join would use, if any. No I/O beyond `stat`.
#[tauri::command]
pub async fn meeting_browser_info() -> BrowserInfo {
    match browser::find() {
        Some(b) => BrowserInfo { available: true, name: Some(b.name.to_string()) },
        None => BrowserInfo { available: false, name: None },
    }
}

/// Open a meeting link that arrived from the phone. Returns immediately; progress arrives as
/// `meeting://status` events. The URL is re-validated against the allow-list here so a bug in
/// the frontend can never make this module launch a browser at an arbitrary address.
#[tauri::command]
pub async fn meeting_open(
    app: AppHandle,
    url: String,
    auto_join: bool,
    display_name: Option<String>,
) -> Result<MeetingOpenResult, String> {
    let parsed = crate::remote::meeting_url::validate_meeting_url(&url)
        .map_err(|e| format!("meeting link rejected: {e:?}"))?;
    let provider = crate::remote::meeting_url::provider(&parsed).to_string();
    if !auto_join {
        return Ok(MeetingOpenResult { mode: "default", browser: None });
    }
    let Some(b) = browser::find() else {
        return Ok(MeetingOpenResult { mode: "unavailable", browser: None });
    };
    let profile = profile_dir(&app).map_err(|e| e.to_string())?;
    let open_url = join::launch_url(&provider, &parsed);
    let name = b.name.to_string();
    let display_name = display_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(join::default_display_name);
    tauri::async_runtime::spawn(async move {
        join::run(app, b, profile, open_url, provider, display_name).await;
    });
    Ok(MeetingOpenResult { mode: "auto", browser: Some(name) })
}

/// Open the dedicated profile on the three providers' home pages so the user can sign in once
/// and choose the CamLooper camera in each service's settings. Not driven; just a window.
#[tauri::command]
pub async fn meeting_open_profile(app: AppHandle) -> Result<String, String> {
    let b = browser::find().ok_or_else(|| "no_browser".to_string())?;
    let profile = profile_dir(&app).map_err(|e| e.to_string())?;
    browser::launch(
        &b,
        &profile,
        &["https://meet.google.com/", "https://teams.microsoft.com/", "https://zoom.us/signin"],
    )
    .map_err(|e| e.to_string())?;
    Ok(b.name.to_string())
}

/// Debug builds only: `CAMLOOPER_TEST_OPEN_MEETING=<url>` drives a join a few seconds after
/// startup without needing a phone, so the browser automation can be exercised on its own.
/// Compiled out of release builds.
pub fn init(app: AppHandle) {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("CAMLOOPER_TEST_OPEN_MEETING") {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            match meeting_open(app, url, true, None).await {
                Ok(r) => eprintln!("meeting: test open → mode {} ({:?})", r.mode, r.browser),
                Err(e) => eprintln!("meeting: test open failed: {e}"),
            }
        });
    }
    #[cfg(not(debug_assertions))]
    let _ = app;
}
