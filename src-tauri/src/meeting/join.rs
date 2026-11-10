//! Driving the pre-join screens: launch the browser, find the tab, and run the provider's join
//! script once a second until it reports `joined` (or we give up after `JOIN_TIMEOUT`).

use super::browser::{self, Browser};
use super::cdp::{self, PageSession};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::AppHandle;
use tokio::time::{sleep, Instant};

const COMMON: &str = include_str!("scripts/common.js");
const MEET: &str = include_str!("scripts/meet.js");
const TEAMS: &str = include_str!("scripts/teams.js");
const ZOOM: &str = include_str!("scripts/zoom.js");
const DUMP: &str = include_str!("scripts/dump.js");

pub const JOIN_TIMEOUT: Duration = Duration::from_secs(180);
const PORT_TIMEOUT: Duration = Duration::from_secs(20);
const TAB_TIMEOUT: Duration = Duration::from_secs(30);

/// What to hand to the browser. Zoom's `/j/<id>` page exists to start the native client; the
/// web client lives at `app.zoom.us/wc/<id>/join`, so go there directly (keeping the passcode).
pub fn launch_url(provider: &str, url: &url::Url) -> String {
    if provider == "zoom" {
        if let Some(web) = zoom_web_client_url(url) {
            return web;
        }
    }
    url.to_string()
}

pub fn zoom_web_client_url(url: &url::Url) -> Option<String> {
    let segs: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let id = match segs.as_slice() {
        ["j", id] | ["w", id] => *id,
        _ => return None,
    };
    if !(9..=11).contains(&id.len()) || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut out = url::Url::parse(&format!("https://app.zoom.us/wc/{id}/join")).ok()?;
    if let Some((_, pwd)) = url.query_pairs().find(|(k, _)| k == "pwd") {
        out.query_pairs_mut().append_pair("pwd", &pwd);
    }
    Some(out.to_string())
}

/// Guest name typed into the pre-join screen when the profile is not signed in.
pub fn default_display_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "CamLooper".to_string())
}

fn script_for(provider: &str) -> Option<&'static str> {
    match provider {
        "meet" => Some(MEET),
        "teams" => Some(TEAMS),
        "zoom" => Some(ZOOM),
        _ => None,
    }
}

/// Hosts the meeting tab may be on while joining (launch pages redirect across these).
pub fn host_matches(provider: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    match provider {
        "zoom" => host == "zoom.us" || host.ends_with(".zoom.us"),
        "teams" => {
            ["teams.microsoft.com", "teams.live.com", "teams.cloud.microsoft", "teams.microsoft.us"]
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        }
        "meet" => host == "meet.google.com",
        _ => false,
    }
}

fn build_script(provider: &str, display_name: &str) -> Result<String> {
    let body = script_for(provider).context("no join script for this provider")?;
    let name_literal = serde_json::to_string(display_name)?;
    let common = COMMON.replace("__DISPLAY_NAME__", &name_literal);
    Ok(format!("(() => {{\n{common}\n{body}\n}})()"))
}

pub async fn run(
    app: AppHandle,
    browser: Browser,
    profile: PathBuf,
    open_url: String,
    provider: String,
    display_name: String,
) {
    super::emit_status(&app, &provider, "launching", Some(browser.name.to_string()));
    if let Err(e) = drive(&app, &browser, &profile, &open_url, &provider, &display_name).await {
        eprintln!("meeting: auto-join failed: {e:#}");
        super::emit_status(&app, &provider, "failed", Some(e.to_string()));
    }
}

async fn drive(
    app: &AppHandle,
    browser: &Browser,
    profile: &Path,
    open_url: &str,
    provider: &str,
    display_name: &str,
) -> Result<()> {
    let http = cdp::local_http()?;
    // A running instance keeps serving the port in DevToolsActivePort; a stale file from a
    // crashed one must go, or we would wait on a dead port.
    let existing = match browser::read_devtools_port(profile) {
        Some(p) if cdp::is_alive(&http, p).await => Some(p),
        _ => {
            let _ = std::fs::remove_file(profile.join("DevToolsActivePort"));
            None
        }
    };
    let mut before: HashSet<String> = HashSet::new();
    if let Some(p) = existing {
        if let Ok(targets) = cdp::list_targets(&http, p).await {
            before = targets.into_iter().map(|t| t.id).collect();
        }
    }
    eprintln!(
        "meeting: opening {provider} meeting in {} ({})",
        browser.name,
        if existing.is_some() { "running instance" } else { "new instance" }
    );
    browser::launch(browser, profile, &[open_url])?;

    let started = Instant::now();
    let port = loop {
        if let Some(p) = existing {
            break p;
        }
        if let Some(p) = browser::read_devtools_port(profile) {
            if cdp::is_alive(&http, p).await {
                break p;
            }
        }
        if started.elapsed() > PORT_TIMEOUT {
            bail!("{} did not open its DevTools port", browser.name);
        }
        sleep(Duration::from_millis(300)).await;
    };

    let matcher = |h: &str| host_matches(provider, h);
    let started = Instant::now();
    let target = loop {
        let targets = cdp::list_targets(&http, port).await.unwrap_or_default();
        if let Some(t) = cdp::pick_target(&targets, &matcher, &before) {
            break t.clone();
        }
        if started.elapsed() > TAB_TIMEOUT {
            bail!("the meeting tab did not appear");
        }
        sleep(Duration::from_millis(500)).await;
    };
    let ws_url = target.web_socket_debugger_url.clone().context("tab has no debugger url")?;
    let mut session = PageSession::connect(&ws_url).await?;
    super::emit_status(app, provider, "joining", None);

    let script = build_script(provider, display_name)?;
    let deadline = Instant::now() + JOIN_TIMEOUT;
    let mut last_status = String::new();
    let mut announced_admission = false;
    loop {
        match session.evaluate(&script).await {
            Ok(v) => {
                let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("unknown").to_string();
                let detail = v.get("detail").and_then(|s| s.as_str()).map(|s| s.to_string());
                if status != last_status {
                    eprintln!(
                        "meeting: {provider} join step: {status}{}",
                        detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default()
                    );
                    last_status = status.clone();
                }
                match status.as_str() {
                    "joined" => {
                        super::emit_status(app, provider, "joined", None);
                        return Ok(());
                    }
                    "blocked" => {
                        super::emit_status(app, provider, "blocked", detail);
                        return Ok(());
                    }
                    "waiting_admission" if !announced_admission => {
                        announced_admission = true;
                        super::emit_status(app, provider, "waiting_admission", None);
                    }
                    _ => {}
                }
            }
            Err(e) => {
                // Navigations (Zoom launch page → web client) can drop the session; the tab keeps
                // its id, so reattach to it, or to whichever matching tab exists now.
                eprintln!("meeting: devtools evaluate failed ({e}); reattaching");
                sleep(Duration::from_secs(1)).await;
                let targets = cdp::list_targets(&http, port).await.unwrap_or_default();
                let Some(t) = targets
                    .iter()
                    .find(|t| t.id == target.id)
                    .or_else(|| cdp::pick_target(&targets, &matcher, &before))
                else {
                    bail!("the meeting tab was closed");
                };
                let Some(u) = t.web_socket_debugger_url.clone() else {
                    bail!("the meeting tab is no longer debuggable");
                };
                session = PageSession::connect(&u).await?;
            }
        }
        if Instant::now() >= deadline {
            let dump = session
                .evaluate(DUMP)
                .await
                .ok()
                .map(|v| v.to_string())
                .unwrap_or_default();
            eprintln!(
                "meeting: gave up on {provider} after {}s (last step: {last_status}); visible controls: {dump}",
                JOIN_TIMEOUT.as_secs()
            );
            super::emit_status(app, provider, "timeout", Some(last_status));
            return Ok(());
        }
        sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn zoom_join_links_go_straight_to_the_web_client() {
        assert_eq!(
            zoom_web_client_url(&u("https://zoom.us/j/12345678901?pwd=abc.1")).as_deref(),
            Some("https://app.zoom.us/wc/12345678901/join?pwd=abc.1")
        );
        assert_eq!(
            zoom_web_client_url(&u("https://us02web.zoom.us/j/123456789")).as_deref(),
            Some("https://app.zoom.us/wc/123456789/join")
        );
        assert_eq!(
            zoom_web_client_url(&u("https://zoom.us/w/98765432109?pwd=x&uname=y")).as_deref(),
            Some("https://app.zoom.us/wc/98765432109/join?pwd=x")
        );
        // Not a plain join link: leave it alone.
        assert!(zoom_web_client_url(&u("https://zoom.us/my/someone")).is_none());
        assert!(zoom_web_client_url(&u("https://zoom.us/j/not-digits")).is_none());
        assert!(zoom_web_client_url(&u("https://zoom.us/j/123")).is_none());
        assert_eq!(launch_url("zoom", &u("https://zoom.us/my/someone")), "https://zoom.us/my/someone");
        assert_eq!(launch_url("meet", &u("https://meet.google.com/abc-defg-hij")), "https://meet.google.com/abc-defg-hij");
    }

    #[test]
    fn host_matching_follows_each_provider_redirects() {
        assert!(host_matches("zoom", "app.zoom.us"));
        assert!(host_matches("zoom", "zoom.us"));
        assert!(!host_matches("zoom", "zoom.us.evil.com"));
        assert!(host_matches("teams", "teams.microsoft.com"));
        assert!(host_matches("teams", "teams.cloud.microsoft"));
        assert!(host_matches("teams", "teams.live.com"));
        assert!(!host_matches("teams", "login.microsoftonline.com"));
        assert!(host_matches("meet", "meet.google.com"));
        assert!(!host_matches("meet", "accounts.google.com"));
        assert!(!host_matches("generic", "example.com"));
    }

    #[test]
    fn scripts_embed_the_display_name_as_a_json_literal() {
        let s = build_script("meet", "Amit \"A\" <x>").unwrap();
        assert!(s.starts_with("(() => {"));
        assert!(s.ends_with("})()"));
        assert!(s.contains(r#"const displayName = "Amit \"A\" <x>";"#));
        assert!(!s.contains("__DISPLAY_NAME__"));
        assert!(build_script("generic", "x").is_err());
        for p in ["meet", "teams", "zoom"] {
            assert!(build_script(p, "n").unwrap().contains("return __cl.result("));
        }
    }
}
