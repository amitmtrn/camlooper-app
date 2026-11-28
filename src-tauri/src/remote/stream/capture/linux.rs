//! Linux capture for a meeting session.
//!
//! * **Sound**: the *monitor* of the default sink — what the speakers or headphones play,
//!   which in a meeting is the other participants. Never a microphone: when the default sink
//!   cannot be asked for (`pactl` missing), PulseAudio's own `@DEFAULT_MONITOR@` is used,
//!   not ffmpeg's `default` (that is the default *source*, a microphone).
//! * **Picture**: the xdg-desktop-portal ScreenCast picker — the system's own dialog, the one
//!   a browser shows for screen sharing — lets the user choose a window or a screen. The
//!   portal hands back a PipeWire node; `gst-launch-1.0 pipewiresrc` reads it (the ffmpeg
//!   builds around have no PipeWire input) and pipes raw 1280x720 I420 frames into the
//!   encoder's stdin. The portal session lives as long as the D-Bus connection that made
//!   it, which is kept with the capture process and dropped with it.
//!
//! Nothing here runs unless a meeting session was accepted in the confirm dialog.
use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::remote::stream::meeting::{MediaInput, MeetingSource, MEETING_FPS, MEETING_HEIGHT, MEETING_WIDTH};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
/// `SelectSources.types`: monitors and windows.
const SOURCE_MONITOR_AND_WINDOW: u32 = 1 | 2;
/// `SelectSources.cursor_mode`: the pointer is drawn into the picture.
const CURSOR_EMBEDDED: u32 = 2;
/// The portal answers the two calls that show nothing within this long.
const PORTAL_CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct LinuxMeetingSource;

impl MeetingSource for LinuxMeetingSource {
    fn audio(&self) -> BoxFuture<'_, Result<MediaInput>> {
        Box::pin(async {
            let sink = default_sink().await;
            let mut input = MediaInput::args(&["-f", "pulse", "-fragment_size", "3840", "-i", &monitor_of(sink.as_deref())]);
            input.device = sink;
            Ok(input)
        })
    }

    fn video(&self) -> BoxFuture<'_, Result<Option<MediaInput>>> {
        Box::pin(async {
            let Some(stream) = pick_window().await? else { return Ok(None) };
            window_input(stream).map(Some)
        })
    }

    fn audio_device(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(default_sink())
    }
}

/// The PulseAudio source that carries what `sink` plays.
pub fn monitor_of(sink: Option<&str>) -> String {
    match sink {
        Some(sink) => format!("{sink}.monitor"),
        None => "@DEFAULT_MONITOR@".to_string(),
    }
}

/// The default sink's name (`pactl get-default-sink`), if it can be asked for.
async fn default_sink() -> Option<String> {
    let run = tokio::process::Command::new("pactl").arg("get-default-sink").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_secs(2), run).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    parse_sink_name(&String::from_utf8_lossy(&out.stdout))
}

/// A sink name as `pactl` prints it: one line of printable characters without blanks.
pub fn parse_sink_name(output: &str) -> Option<String> {
    let name = output.trim();
    (!name.is_empty() && name.len() <= 256 && name.chars().all(|c| c.is_ascii_graphic())).then(|| name.to_string())
}

/// What the user picked in the portal: a PipeWire remote and the node that carries it.
pub struct PortalStream {
    fd: OwnedFd,
    node: u32,
    /// The portal session ends when this connection does.
    _session: zbus::Connection,
}

/// The `gst-launch-1.0` arguments that turn `source` (an element description) into raw
/// I420 frames of the meeting size on stdout: converted, letterboxed (square pixels), and
/// brought to a steady frame rate — a window that does not change sends no frames at all.
pub fn gst_pipeline(source: &[String]) -> Vec<String> {
    let caps = format!(
        "video/x-raw,format=I420,width={MEETING_WIDTH},height={MEETING_HEIGHT},pixel-aspect-ratio=1/1,framerate={MEETING_FPS}/1"
    );
    let mut args: Vec<String> = vec!["-q".into()];
    args.extend(source.iter().cloned());
    for element in ["videoconvert", "videoscale", "videorate"] {
        args.extend(["!".to_string(), element.to_string()]);
    }
    args.extend(["!".into(), caps, "!".into(), "fdsink".into(), "fd=1".into(), "sync=false".into()]);
    args
}

/// The ffmpeg input that reads [`gst_pipeline`]'s output on stdin.
pub fn raw_input_args() -> Vec<String> {
    let size = format!("{MEETING_WIDTH}x{MEETING_HEIGHT}");
    let rate = MEETING_FPS.to_string();
    ["-f", "rawvideo", "-pixel_format", "yuv420p", "-video_size", &size, "-framerate", &rate, "-i", "pipe:0"].iter().map(|a| a.to_string()).collect()
}

/// Start reading the picked node and describe it as an encoder input.
fn window_input(stream: PortalStream) -> Result<MediaInput> {
    let fd = stream.fd.as_raw_fd();
    // `keepalive-time`: resend the last frame of a window that stands still, so the phone
    // gets a picture at all and the encoder keeps its rate.
    let source =
        vec!["pipewiresrc".to_string(), format!("fd={fd}"), format!("path={}", stream.node), "do-timestamp=true".into(), "keepalive-time=100".into()];
    let mut cmd = tokio::process::Command::new("gst-launch-1.0");
    cmd.args(gst_pipeline(&source)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true);
    // The PipeWire descriptor is close-on-exec; the capture process is the one that needs it.
    // SAFETY: runs in the child between fork and exec and only calls fcntl, which is
    // async-signal-safe; `fd` is owned by `stream`, which outlives the spawn.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("could not start gst-launch-1.0 (GStreamer with the PipeWire plugin is needed to share a window)")?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("the capture process has no output"))?;
    let stdin: Stdio = stdout.try_into().context("could not hand the capture to the encoder")?;
    Ok(MediaInput { args: raw_input_args(), stdin: Some(stdin), keep: Some(Box::new((child, stream))), device: None })
}

/// The unique part of a portal request or session path.
fn token(what: &str) -> String {
    format!("camlooper_{what}_{:08x}", rand::random::<u32>())
}

/// Call a ScreenCast method that answers through a `Request` object, and wait for the
/// answer. `Ok(None)`: the user cancelled. `within`: how long the answer may take (`None`
/// for the calls that wait for the user).
async fn request<B>(
    conn: &zbus::Connection,
    portal: &zbus::Proxy<'_>,
    handle_token: &str,
    method: &str,
    body: &B,
    within: Option<Duration>,
) -> Result<Option<HashMap<String, OwnedValue>>>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let sender = conn.unique_name().ok_or_else(|| anyhow!("no D-Bus name"))?.as_str().trim_start_matches(':').replace('.', "_");
    // Listen before calling: the answer may come at once.
    let path = format!("{PORTAL_PATH}/request/{sender}/{handle_token}");
    let request = zbus::Proxy::new(conn, PORTAL, path, "org.freedesktop.portal.Request").await?;
    let mut responses = request.receive_signal("Response").await?;
    portal.call_method(method, body).await.with_context(|| format!("the screen sharing portal refused {method}"))?;
    let answer = async { responses.next().await.ok_or_else(|| anyhow!("the screen sharing portal went away")) };
    let message = match within {
        Some(limit) => tokio::time::timeout(limit, answer).await.map_err(|_| anyhow!("the screen sharing portal did not answer {method}"))??,
        None => answer.await?,
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    match code {
        0 => Ok(Some(results)),
        1 => Ok(None),
        _ => bail!("the screen sharing portal ended {method} with an error"),
    }
}

/// The node id of the first stream in a `Start` response (`streams`: `a(ua{sv})`).
fn first_node(streams: &Value<'_>) -> Option<u32> {
    let Value::Array(streams) = streams else { return None };
    streams.iter().find_map(|stream| match stream {
        Value::Structure(s) => match s.fields().first() {
            Some(Value::U32(node)) => Some(*node),
            _ => None,
        },
        _ => None,
    })
}

async fn screencast(conn: &zbus::Connection) -> Result<zbus::Proxy<'static>> {
    zbus::Proxy::new(conn, PORTAL, PORTAL_PATH, "org.freedesktop.portal.ScreenCast").await.context("no screen sharing portal (xdg-desktop-portal)")
}

/// The part of the handshake that shows nothing: a portal session that is told what may be
/// picked (one window or screen). `Ok(None)`: refused as a cancel. Returns the session path.
async fn prepare_session(conn: &zbus::Connection, portal: &zbus::Proxy<'_>) -> Result<Option<String>> {
    let handle = token("r");
    let session_token = token("s");
    let options: HashMap<&str, Value<'_>> =
        HashMap::from([("handle_token", Value::from(handle.as_str())), ("session_handle_token", Value::from(session_token.as_str()))]);
    let created = request(conn, portal, &handle, "CreateSession", &(options,), Some(PORTAL_CALL_TIMEOUT))
        .await?
        .ok_or_else(|| anyhow!("the screen sharing portal refused a session"))?;
    let session = match created.get("session_handle").map(|v| &**v) {
        Some(Value::Str(path)) => path.to_string(),
        Some(Value::ObjectPath(path)) => path.to_string(),
        _ => bail!("the screen sharing portal gave no session"),
    };
    let session_path = ObjectPath::try_from(session.as_str()).map_err(|_| anyhow!("the screen sharing portal gave an unusable session"))?;

    // The pointer is part of what a meeting shows; asked for only where the portal has it.
    let cursor_modes: u32 = portal.get_property("AvailableCursorModes").await.unwrap_or(0);
    let handle = token("r");
    let mut options: HashMap<&str, Value<'_>> = HashMap::from([
        ("handle_token", Value::from(handle.as_str())),
        ("types", Value::from(SOURCE_MONITOR_AND_WINDOW)),
        ("multiple", Value::from(false)),
    ]);
    if cursor_modes & CURSOR_EMBEDDED != 0 {
        options.insert("cursor_mode", Value::from(CURSOR_EMBEDDED));
    }
    if request(conn, portal, &handle, "SelectSources", &(&session_path, options), Some(PORTAL_CALL_TIMEOUT)).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(session))
}

/// Show the system's window/screen picker. `Ok(None)`: the user cancelled it.
pub async fn pick_window() -> Result<Option<PortalStream>> {
    let conn = zbus::Connection::session().await.context("no session bus")?;
    let portal = screencast(&conn).await?;
    let Some(session) = prepare_session(&conn, &portal).await? else { return Ok(None) };
    let session = ObjectPath::try_from(session.as_str()).map_err(|_| anyhow!("the screen sharing portal gave an unusable session"))?;

    // This is the call that shows the picker; it takes as long as the user takes.
    let handle = token("r");
    let options: HashMap<&str, Value<'_>> = HashMap::from([("handle_token", Value::from(handle.as_str()))]);
    let Some(started) = request(&conn, &portal, &handle, "Start", &(&session, "", options), None).await? else {
        return Ok(None);
    };
    let node = started.get("streams").and_then(|v| first_node(v)).ok_or_else(|| anyhow!("the screen sharing portal gave no stream"))?;

    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let fd: zbus::zvariant::OwnedFd =
        portal.call("OpenPipeWireRemote", &(&session, options)).await.context("the screen sharing portal gave no PipeWire connection")?;
    Ok(Some(PortalStream { fd: fd.into(), node, _session: conn }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{Array, StructureBuilder};

    #[test]
    fn the_sound_is_the_default_sinks_monitor_and_never_a_microphone() {
        assert_eq!(monitor_of(Some("alsa_output.pci-0000_00_1f.3.analog-stereo")), "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor");
        // Without a sink name PulseAudio resolves the monitor itself; ffmpeg's "default"
        // would be the default source (a microphone).
        assert_eq!(monitor_of(None), "@DEFAULT_MONITOR@");
        assert_eq!(parse_sink_name("alsa_output.usb-headset.analog-stereo\n").as_deref(), Some("alsa_output.usb-headset.analog-stereo"));
        assert_eq!(parse_sink_name("\n"), None);
        assert_eq!(parse_sink_name("two words\n"), None);
        assert_eq!(parse_sink_name("Connection failure: Connection refused"), None);
    }

    #[test]
    fn the_capture_pipeline_delivers_what_the_encoder_is_told_to_read() {
        let gst = gst_pipeline(&["pipewiresrc".into(), "fd=7".into(), "path=42".into()]).join(" ");
        assert!(gst.starts_with("-q pipewiresrc fd=7 path=42 ! videoconvert ! videoscale ! videorate ! video/x-raw,format=I420,width=1280,height=720"), "{gst}");
        assert!(gst.contains("pixel-aspect-ratio=1/1") && gst.contains("framerate=30/1"), "{gst}");
        assert!(gst.ends_with("! fdsink fd=1 sync=false"), "{gst}");
        let input = raw_input_args().join(" ");
        assert_eq!(input, "-f rawvideo -pixel_format yuv420p -video_size 1280x720 -framerate 30 -i pipe:0");
    }

    #[test]
    fn the_node_is_read_from_the_portals_stream_list() {
        let stream = |node: u32| {
            let props: HashMap<String, Value<'_>> = HashMap::from([("source_type".to_string(), Value::from(2u32))]);
            Value::Structure(StructureBuilder::new().add_field(node).add_field(props).build().unwrap())
        };
        let first = stream(57);
        let mut streams = Array::new(first.value_signature());
        streams.append(stream(57)).unwrap();
        streams.append(stream(58)).unwrap();
        assert_eq!(first_node(&Value::Array(streams)), Some(57));
        assert_eq!(first_node(&Value::from(57u32)), None);
        let empty = Array::new(first.value_signature());
        assert_eq!(first_node(&Value::Array(empty)), None);
    }

    /// The silent half of the portal handshake against the real portal of this desktop:
    /// `CreateSession` and `SelectSources` answer through their request objects and show
    /// nothing. (`Start` is the call that opens the picker; that one needs a person and is
    /// in MANUAL_TEST.md.) Needs a session bus with xdg-desktop-portal:
    /// `cargo test --lib portal_session -- --ignored`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "talks to this desktop's xdg-desktop-portal"]
    async fn a_portal_session_is_created_and_told_what_may_be_picked() {
        let conn = zbus::Connection::session().await.expect("session bus");
        let portal = screencast(&conn).await.expect("ScreenCast portal");
        let session = prepare_session(&conn, &portal).await.expect("handshake").expect("not cancelled");
        assert!(session.starts_with("/org/freedesktop/portal/desktop/session/"), "{session}");
        assert!(session.contains("camlooper_s_"), "{session}");
    }
}
