//! The remote's single background task (an actor driven by an mpsc channel) plus the shared
//! state the Tauri commands read.
//!
//! Lifecycle: `init` loads `remote.json`. Without a pairing the task parks on the control
//! channel and nothing network-related exists — not even the HTTP client. `link` performs the
//! claim (the first connection the app ever makes), persists the pairing and wakes the task,
//! which keeps a WebSocket to the relay with exponential-backoff reconnects. `unlink`, a
//! `pair_revoked` answer or an `unpaired` frame tear everything down and park the task again.
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use once_cell::sync::{Lazy, OnceCell};
use rand::Rng;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http::HeaderValue, Message};
use url::Url;

use super::meeting_url::{provider, validate_meeting_url};
use super::protocol::{ApiErrorBody, ClientMessage, PairClaimRequest, PairClaimResponse, ServerMessage};
use super::store::{Pairing, Store, STORE_VERSION};

const DEFAULT_API_BASE: &str = "https://api.camlooper.com/";
/// Dev override, e.g. `CAMLOOPER_API_URL=http://127.0.0.1:8787` (wrangler dev).
const API_BASE_ENV: &str = "CAMLOOPER_API_URL";
const PING_INTERVAL: Duration = Duration::from_secs(30);
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BACKOFF_SECS: u64 = 60;
/// A command re-flushed after a long outage must not pop a meeting hours later.
const STALE_COMMAND_MS: i64 = 10 * 60 * 1000;
const RECENT_IDS: usize = 32;
const MAX_DESKTOP_NAME: usize = 64;

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// `CAMLOOPER_API_URL` is set to something unusable; the feature is off.
    Disabled,
    #[default]
    Unlinked,
    Linking,
    Linked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteState {
    pub status: Status,
    pub connected: bool,
    pub mobile_device_name: Option<String>,
    pub desktop_name: Option<String>,
    pub autostart: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenMeetingCommand {
    pub command_id: String,
    pub url: String,
    pub provider: &'static str,
    pub mobile_device_name: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug)]
pub enum Control {
    Reconnect,
    Ack(String),
    Shutdown,
}

#[derive(Debug)]
pub enum LinkError {
    Disabled,
    CodeInvalid,
    RateLimited,
    Network,
    Server,
}

impl LinkError {
    pub fn code(&self) -> &'static str {
        match self {
            LinkError::Disabled => "disabled",
            LinkError::CodeInvalid => "code_invalid",
            LinkError::RateLimited => "rate_limited",
            LinkError::Network => "network",
            LinkError::Server => "server",
        }
    }
}

#[derive(Default)]
struct Inner {
    store: Option<Store>,
    status: Status,
    connected: bool,
    pairing: Option<Pairing>,
    pending: Option<OpenMeetingCommand>,
    recent_ids: VecDeque<String>,
    last_error: Option<String>,
    control_tx: Option<mpsc::UnboundedSender<Control>>,
    autostart: bool,
}

static STATE: Lazy<Mutex<Inner>> = Lazy::new(|| Mutex::new(Inner::default()));
/// Built on the first `link()`. Its absence while unlinked is the "no network" guarantee.
static HTTP: OnceCell<reqwest::Client> = OnceCell::new();

fn state() -> MutexGuard<'static, Inner> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn http() -> &'static reqwest::Client {
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(format!("camlooper/{} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS))
            .build()
            .expect("reqwest client with rustls")
    })
}

pub fn api_base() -> Option<Url> {
    let raw = std::env::var(API_BASE_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
    let mut base = Url::parse(raw.trim()).ok()?;
    if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
        return None;
    }
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base.set_query(None);
    base.set_fragment(None);
    Some(base)
}

pub fn ws_url_for(base: &Url) -> Option<Url> {
    let mut ws = base.clone();
    let scheme = if base.scheme() == "https" { "wss" } else { "ws" };
    ws.set_scheme(scheme).ok()?;
    ws.set_path(&format!("{}v1/desktop/ws", base.path()));
    Some(ws)
}

pub fn snapshot() -> RemoteState {
    let s = state();
    RemoteState {
        status: s.status,
        connected: s.connected,
        mobile_device_name: s.pairing.as_ref().and_then(|p| p.mobile_device_name.clone()),
        desktop_name: s.pairing.as_ref().map(|p| p.desktop_name.clone()),
        autostart: s.autostart,
        last_error: s.last_error.clone(),
    }
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit("remote://status", snapshot());
}

pub fn is_linked() -> bool {
    state().pairing.is_some()
}

pub fn pending_command() -> Option<OpenMeetingCommand> {
    state().pending.clone()
}

pub fn ack(command_id: String) {
    let tx = {
        let mut s = state();
        if s.pending.as_ref().map(|c| c.command_id == command_id).unwrap_or(false) {
            s.pending = None;
        }
        s.control_tx.clone()
    };
    if let Some(tx) = tx {
        let _ = tx.send(Control::Ack(command_id));
    }
}

pub fn autostart_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

pub fn set_autostart(app: &AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let result = if enabled { manager.enable() } else { manager.disable() };
    match result {
        Ok(()) => {
            state().autostart = enabled;
            crate::tray::set_autostart_checked(app, enabled);
            emit_status(app);
            Ok(enabled)
        }
        Err(e) => {
            let actual = manager.is_enabled().unwrap_or(false);
            state().autostart = actual;
            crate::tray::set_autostart_checked(app, actual);
            emit_status(app);
            eprintln!("remote: autostart {} failed: {e}", if enabled { "enable" } else { "disable" });
            Err(e.to_string())
        }
    }
}

pub fn toggle_autostart_from_tray(app: &AppHandle) {
    let desired = crate::tray::autostart_checked(app).unwrap_or_else(|| !state().autostart);
    let _ = set_autostart(app, desired);
}

pub fn init(app: AppHandle) {
    // Both reqwest and tokio-tungstenite use rustls; make ring the process-wide provider
    // once, so neither picks a provider lazily on a background thread.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let store = app.path().app_data_dir().ok().map(|dir| Store::at(dir.join("remote.json")));
    let pairing = store.as_ref().and_then(|s| s.load());
    let autostart = autostart_enabled(&app);
    let (tx, rx) = mpsc::unbounded_channel();
    {
        let mut s = state();
        s.store = store;
        s.pairing = pairing.clone();
        s.autostart = autostart;
        s.control_tx = Some(tx);
        s.status = if api_base().is_none() {
            Status::Disabled
        } else if pairing.is_some() {
            Status::Linked
        } else {
            Status::Unlinked
        };
    }
    match &pairing {
        Some(p) => eprintln!(
            "remote: linked with {}, connecting",
            p.mobile_device_name.as_deref().unwrap_or("phone")
        ),
        None => eprintln!("remote: idle, no network"),
    }
    tauri::async_runtime::spawn(run_task(app.clone(), rx));

    // Dev/test aid only (debug builds): `CAMLOOPER_REMOTE_LINK_CODE=123456 npm run tauri dev`
    // claims that code shortly after start, so the whole phone → server → desktop path can be
    // exercised from a terminal without typing into the UI. Never compiled into release builds.
    #[cfg(debug_assertions)]
    if let Ok(code) = std::env::var("CAMLOOPER_REMOTE_LINK_CODE") {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            match link(app, code).await {
                Ok(_) => eprintln!("remote: dev autolink succeeded"),
                Err(e) => eprintln!("remote: dev autolink failed ({})", e.code()),
            }
        });
    }
}

/// Claim the code the phone is showing. Replaces (and revokes) any existing pairing.
pub async fn link(app: AppHandle, code: String) -> Result<RemoteState, LinkError> {
    let code = code.trim().to_string();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return fail(&app, LinkError::CodeInvalid);
    }
    let Some(base) = api_base() else {
        return fail(&app, LinkError::Disabled);
    };

    let previous = { state().pairing.clone() };
    if let Some(old) = previous {
        // Revoke the old phone first so it cannot keep pushing links to this computer.
        let _ = delete_pair(&base, &old).await;
        forget_pairing(&app, None);
    }

    {
        let mut s = state();
        s.status = Status::Linking;
        s.last_error = None;
    }
    emit_status(&app);

    let desktop_name = desktop_name();
    let request = PairClaimRequest {
        code: &code,
        platform: std::env::consts::OS,
        app_version: env!("CARGO_PKG_VERSION"),
        desktop_name: &desktop_name,
    };
    let endpoint = match base.join("v1/desktop/pair/claim") {
        Ok(u) => u,
        Err(_) => return fail(&app, LinkError::Disabled),
    };

    let response = match http().post(endpoint).json(&request).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("remote: claim request failed: {e}");
            return fail(&app, LinkError::Network);
        }
    };
    let claim: PairClaimResponse = match response.status().as_u16() {
        200 => match response.json().await {
            Ok(c) => c,
            Err(_) => return fail(&app, LinkError::Server),
        },
        404 | 409 => return fail(&app, LinkError::CodeInvalid),
        429 => return fail(&app, LinkError::RateLimited),
        other => {
            let body = response.json::<ApiErrorBody>().await.ok();
            eprintln!(
                "remote: claim answered HTTP {other} ({})",
                body.map(|b| format!("{} {}", b.error, b.message.unwrap_or_default())).unwrap_or_default()
            );
            return fail(&app, LinkError::Server);
        }
    };

    let pairing = Pairing {
        version: STORE_VERSION,
        pair_id: claim.pair_id,
        desktop_token: claim.desktop_token,
        desktop_name,
        mobile_device_name: claim.mobile_device_name,
        paired_at: format_rfc3339(now_ms()),
    };
    let tx = {
        let mut s = state();
        if let Some(store) = &s.store {
            if let Err(e) = store.save(&pairing) {
                eprintln!("remote: could not save pairing: {e:#}");
            }
        }
        s.pairing = Some(pairing);
        s.status = Status::Linked;
        s.last_error = None;
        s.control_tx.clone()
    };
    let autostart_enabled = set_autostart(&app, true).is_ok();
    if let Some(tx) = tx {
        let _ = tx.send(Control::Reconnect);
    }
    emit_status(&app);
    let _ = app.emit(
        "remote://linked",
        serde_json::json!({
            "mobileDeviceName": snapshot().mobile_device_name,
            "autostartEnabled": autostart_enabled,
        }),
    );
    Ok(snapshot())
}

fn fail(app: &AppHandle, err: LinkError) -> Result<RemoteState, LinkError> {
    {
        let mut s = state();
        s.status = if s.pairing.is_some() {
            Status::Linked
        } else if api_base().is_none() {
            Status::Disabled
        } else {
            Status::Unlinked
        };
        s.last_error = Some(err.code().to_string());
    }
    emit_status(app);
    Err(err)
}

/// Revoke on the server (best effort, one request), forget locally, go silent.
pub async fn unlink(app: AppHandle) -> anyhow::Result<RemoteState> {
    let pairing = { state().pairing.clone() };
    if let (Some(p), Some(base)) = (pairing, api_base()) {
        let _ = delete_pair(&base, &p).await;
    }
    forget_pairing(&app, None);
    Ok(snapshot())
}

async fn delete_pair(base: &Url, pairing: &Pairing) -> Result<(), ()> {
    let endpoint = base.join("v1/desktop/pair").map_err(|_| ())?;
    let response = http()
        .delete(endpoint)
        .header("authorization", format!("Bearer {}", pairing.desktop_token))
        .header("x-camlooper-pair", &pairing.pair_id)
        .send()
        .await
        .map_err(|_| ())?;
    if response.status().is_success() || response.status().as_u16() == 401 {
        Ok(())
    } else {
        Err(())
    }
}

/// Local teardown shared by unlink, revocation and replacement. `server_reason` is set when
/// the server ended the pairing (phone unlinked, code room expired, idle cleanup).
fn forget_pairing(app: &AppHandle, server_reason: Option<&str>) {
    let tx = {
        let mut s = state();
        if let Some(store) = &s.store {
            if let Err(e) = store.delete() {
                eprintln!("remote: could not delete pairing file: {e:#}");
            }
        }
        s.pairing = None;
        s.pending = None;
        s.connected = false;
        s.status = if api_base().is_some() { Status::Unlinked } else { Status::Disabled };
        s.control_tx.clone()
    };
    if let Some(tx) = tx {
        let _ = tx.send(Control::Shutdown);
    }
    // Start-at-login exists only to keep a linked computer reachable.
    let _ = set_autostart(app, false);
    emit_status(app);
    match server_reason {
        Some(reason) => {
            eprintln!("remote: pairing ended by the server ({reason}); idle, no network");
            // The user may be looking at a tray icon and nothing else; tell them.
            crate::tray::show_main_window(app);
            let _ = app.emit("remote://unlinked-by-phone", serde_json::json!({ "reason": reason }));
        }
        None => eprintln!("remote: unlinked; idle, no network"),
    }
}

fn set_connected(app: &AppHandle, connected: bool) {
    state().connected = connected;
    emit_status(app);
}

enum ConnectError {
    Revoked,
    Other(String),
}

enum SessionEnd {
    Dropped,
    Shutdown,
    Revoked(String),
}

async fn run_task(app: AppHandle, mut rx: mpsc::UnboundedReceiver<Control>) {
    let mut backoff = Backoff::default();
    loop {
        let pairing = { state().pairing.clone() };
        let Some(pairing) = pairing else {
            // Unlinked: park. No client, no DNS, no socket until a code is entered.
            match rx.recv().await {
                None => return,
                Some(_) => {
                    backoff.reset();
                    continue;
                }
            }
        };
        let Some(base) = api_base() else {
            match rx.recv().await {
                None => return,
                Some(_) => continue,
            }
        };

        match connect(&base, &pairing).await {
            Ok(ws) => {
                backoff.reset();
                set_connected(&app, true);
                let end = run_session(ws, &mut rx, &app).await;
                set_connected(&app, false);
                match end {
                    SessionEnd::Revoked(reason) => {
                        forget_pairing(&app, Some(&reason));
                        continue;
                    }
                    SessionEnd::Shutdown => continue,
                    SessionEnd::Dropped => {}
                }
            }
            Err(ConnectError::Revoked) => {
                forget_pairing(&app, Some("revoked"));
                continue;
            }
            Err(ConnectError::Other(msg)) => eprintln!("remote: connect failed: {msg}"),
        }

        let delay = backoff.next();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            ctrl = rx.recv() => match ctrl {
                None => return,
                Some(Control::Reconnect) | Some(Control::Shutdown) => backoff.reset(),
                Some(Control::Ack(_)) => {}
            }
        }
    }
}

async fn connect(base: &Url, pairing: &Pairing) -> Result<WsStream, ConnectError> {
    let ws_url = ws_url_for(base).ok_or_else(|| ConnectError::Other("unusable API URL".into()))?;
    let mut request = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| ConnectError::Other(e.to_string()))?;
    let headers = request.headers_mut();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", pairing.desktop_token))
            .map_err(|e| ConnectError::Other(e.to_string()))?,
    );
    headers.insert(
        "x-camlooper-pair",
        HeaderValue::from_str(&pairing.pair_id).map_err(|e| ConnectError::Other(e.to_string()))?,
    );
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)).await {
        Ok(Ok((ws, _response))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(response))) => {
            if response.status().as_u16() == 401 {
                Err(ConnectError::Revoked)
            } else {
                Err(ConnectError::Other(format!("HTTP {}", response.status())))
            }
        }
        Ok(Err(e)) => Err(ConnectError::Other(e.to_string())),
        Err(_) => Err(ConnectError::Other("timeout".into())),
    }
}

async fn run_session(mut ws: WsStream, rx: &mut mpsc::UnboundedReceiver<Control>, app: &AppHandle) -> SessionEnd {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // the first tick completes immediately
    let idle = tokio::time::sleep(READ_IDLE_TIMEOUT);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            incoming = ws.next() => {
                idle.as_mut().reset(tokio::time::Instant::now() + READ_IDLE_TIMEOUT);
                match incoming {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMessage>(text.as_str()) {
                        Ok(msg) => {
                            if let Some(end) = handle_server_message(msg, &mut ws, app).await {
                                return end;
                            }
                        }
                        Err(e) => eprintln!("remote: ignoring unreadable frame: {e}"),
                    },
                    Some(Ok(Message::Ping(payload))) => {
                        if ws.send(Message::Pong(payload)).await.is_err() {
                            return SessionEnd::Dropped;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(frame) = frame {
                            match u16::from(frame.code) {
                                4003 => return SessionEnd::Revoked("revoked".into()),
                                4004 => return SessionEnd::Revoked("expired".into()),
                                _ => {}
                            }
                        }
                        return SessionEnd::Dropped;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        eprintln!("remote: socket error: {e}");
                        return SessionEnd::Dropped;
                    }
                    None => return SessionEnd::Dropped,
                }
            }
            _ = ping.tick() => {
                let frame = serde_json::to_string(&ClientMessage::Ping).unwrap_or_default();
                if ws.send(Message::text(frame)).await.is_err() {
                    return SessionEnd::Dropped;
                }
            }
            _ = &mut idle => {
                eprintln!("remote: no traffic for {}s, reconnecting", READ_IDLE_TIMEOUT.as_secs());
                let _ = ws.close(None).await;
                return SessionEnd::Dropped;
            }
            ctrl = rx.recv() => match ctrl {
                Some(Control::Ack(id)) => {
                    if send_ack(&mut ws, &id).await.is_err() {
                        return SessionEnd::Dropped;
                    }
                }
                Some(Control::Shutdown) | None => {
                    let _ = ws.close(None).await;
                    return SessionEnd::Shutdown;
                }
                Some(Control::Reconnect) => {
                    let _ = ws.close(None).await;
                    return SessionEnd::Dropped;
                }
            }
        }
    }
}

async fn send_ack(ws: &mut WsStream, command_id: &str) -> Result<(), ()> {
    let frame = serde_json::to_string(&ClientMessage::Ack { command_id: command_id.to_string() }).map_err(|_| ())?;
    ws.send(Message::text(frame)).await.map_err(|_| ())
}

async fn handle_server_message(msg: ServerMessage, ws: &mut WsStream, app: &AppHandle) -> Option<SessionEnd> {
    match msg {
        ServerMessage::Hello { mobile_device_name, .. } => {
            if let Some(name) = mobile_device_name {
                let changed = {
                    let mut s = state();
                    match s.pairing.as_mut() {
                        Some(p) if p.mobile_device_name.as_deref() != Some(name.as_str()) => {
                            p.mobile_device_name = Some(name);
                            true
                        }
                        _ => false,
                    }
                };
                if changed {
                    persist_current();
                }
            }
            emit_status(app);
            None
        }
        ServerMessage::OpenMeeting { command_id, url, created_at, .. } => {
            let seen_before = { state().recent_ids.contains(&command_id) };
            if seen_before {
                // The server re-flushes until it sees an ack; we already have this one.
                let _ = send_ack(ws, &command_id).await;
                return None;
            }
            remember_id(&command_id);

            let age_ms = created_at.as_deref().and_then(parse_rfc3339_ms).map(|t| now_ms() - t);
            if let Some(age) = age_ms {
                if age > STALE_COMMAND_MS {
                    eprintln!("remote: ignoring a {}-minute-old command", age / 60_000);
                    let _ = send_ack(ws, &command_id).await;
                    return None;
                }
            }

            match validate_meeting_url(&url) {
                Ok(parsed) => {
                    let cmd = OpenMeetingCommand {
                        command_id,
                        url: parsed.as_str().to_string(),
                        provider: provider(&parsed),
                        mobile_device_name: { state().pairing.as_ref().and_then(|p| p.mobile_device_name.clone()) },
                        created_at,
                    };
                    {
                        state().pending = Some(cmd.clone());
                    }
                    eprintln!("remote: opening {} meeting sent from the phone", cmd.provider);
                    crate::tray::show_main_window(app);
                    let _ = app.emit("remote://open-meeting", &cmd);
                }
                Err(reason) => {
                    // Never log the URL itself.
                    eprintln!("remote: rejected a link from the phone ({reason})");
                    let _ = send_ack(ws, &command_id).await;
                    let _ = app.emit(
                        "remote://command-rejected",
                        serde_json::json!({ "commandId": command_id, "reason": reason.as_str() }),
                    );
                }
            }
            None
        }
        ServerMessage::Unpaired { reason } => Some(SessionEnd::Revoked(reason.unwrap_or_else(|| "revoked".into()))),
        ServerMessage::Pong | ServerMessage::Unknown => None,
    }
}

fn remember_id(command_id: &str) {
    let mut s = state();
    if s.recent_ids.len() >= RECENT_IDS {
        s.recent_ids.pop_front();
    }
    s.recent_ids.push_back(command_id.to_string());
}

fn persist_current() {
    let s = state();
    if let (Some(store), Some(pairing)) = (&s.store, &s.pairing) {
        if let Err(e) = store.save(pairing) {
            eprintln!("remote: could not update pairing file: {e:#}");
        }
    }
}

/// Friendly name shown on the phone. Zero-dependency hostname lookup.
fn desktop_name() -> String {
    let raw: Option<String> = {
        #[cfg(target_os = "windows")]
        {
            std::env::var("COMPUTERNAME").ok()
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("scutil")
                .args(["--get", "ComputerName"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .or_else(|| std::env::var("HOSTNAME").ok())
        }
    };
    let name = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "CamLooper".to_string());
    name.chars().take(MAX_DESKTOP_NAME).collect()
}

#[derive(Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// 1 s, 2 s, 4 s … capped at 60 s, each multiplied by a uniform jitter in [0.5, 1.5).
    pub fn next(&mut self) -> Duration {
        let base = (1u64 << self.attempt.min(6)).min(MAX_BACKOFF_SECS);
        self.attempt = self.attempt.saturating_add(1);
        let jitter: f64 = rand::thread_rng().gen_range(0.5..1.5);
        Duration::from_secs_f64(base as f64 * jitter)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse `YYYY-MM-DDTHH:MM:SS(.fff)Z` (what the server sends) into epoch milliseconds.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !(b[10] == b'T' || b[10] == b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { s.get(r)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut idx = 19;
    let mut ms = 0i64;
    if b.get(idx) == Some(&b'.') {
        idx += 1;
        let start = idx;
        while idx < b.len() && b[idx].is_ascii_digit() {
            idx += 1;
        }
        let frac: String = s[start..idx].chars().chain("000".chars()).take(3).collect();
        ms = frac.parse().ok()?;
    }
    if !(b.get(idx) == Some(&b'Z') || b.get(idx) == Some(&b'z')) || idx + 1 != b.len() {
        return None;
    }
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000 + ms)
}

pub fn format_rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1_000);
    let millis = ms.rem_euclid(1_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

// Howard Hinnant's date algorithms (public domain).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_swaps_scheme_and_sets_path() {
        let https = Url::parse("https://api.camlooper.com/").unwrap();
        assert_eq!(ws_url_for(&https).unwrap().as_str(), "wss://api.camlooper.com/v1/desktop/ws");
        let http = Url::parse("http://127.0.0.1:8787/").unwrap();
        assert_eq!(ws_url_for(&http).unwrap().as_str(), "ws://127.0.0.1:8787/v1/desktop/ws");
        let prefixed = Url::parse("https://example.test/api/").unwrap();
        assert_eq!(ws_url_for(&prefixed).unwrap().as_str(), "wss://example.test/api/v1/desktop/ws");
    }

    #[test]
    fn backoff_is_bounded_and_jittered() {
        let mut b = Backoff::default();
        for attempt in 0..12u32 {
            let base = (1u64 << attempt.min(6)).min(MAX_BACKOFF_SECS) as f64;
            let d = b.next().as_secs_f64();
            assert!(d >= base * 0.5 - 1e-9 && d < base * 1.5 + 1e-9, "attempt {attempt}: {d}");
        }
        b.reset();
        assert!(b.next().as_secs_f64() < 1.5);
    }

    #[test]
    fn rfc3339_round_trip() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2026-11-08T10:00:00.250Z"), Some(1_794_132_000_250));
        assert_eq!(parse_rfc3339_ms("2026-11-08T10:00:00Z"), Some(1_794_132_000_000));
        assert_eq!(parse_rfc3339_ms("2026-11-08 10:00:00Z"), None);
        assert_eq!(parse_rfc3339_ms("2026-11-08T10:00:00+02:00"), None);
        let now = 1_794_132_000_250;
        assert_eq!(format_rfc3339(now), "2026-11-08T10:00:00.250Z");
        assert_eq!(parse_rfc3339_ms(&format_rfc3339(now)), Some(now));
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn recent_ids_ring_is_capped() {
        for i in 0..(RECENT_IDS + 5) {
            remember_id(&format!("id-{i}"));
        }
        let s = state();
        assert_eq!(s.recent_ids.len(), RECENT_IDS);
        assert!(!s.recent_ids.contains(&"id-0".to_string()));
        assert!(s.recent_ids.contains(&format!("id-{}", RECENT_IDS + 4)));
    }
}
