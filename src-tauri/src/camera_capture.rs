#![allow(dead_code)]

use std::sync::Arc;
use tokio::sync::Mutex;
use tauri::AppHandle;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use base64::prelude::*;
use uuid::Uuid;

use once_cell::sync::Lazy;

static CAPTURE_STATE: Lazy<Arc<Mutex<CaptureState>>> = Lazy::new(|| Arc::new(Mutex::new(CaptureState::default())));

#[derive(Default)]
struct CaptureState {
    ffmpeg_task: Option<tokio::task::JoinHandle<()>>,
    ffmpeg_stdin: Option<tokio::process::ChildStdin>,
    is_recording: bool,
    recording_path: Option<String>,
    latest_frame: Option<String>,
}

/// `(running, recording)` for the in-app camera, which also feeds the virtual camera.
pub(crate) async fn capture_activity() -> (bool, bool) {
    let state = CAPTURE_STATE.lock().await;
    let running = state.ffmpeg_task.as_ref().is_some_and(|t| !t.is_finished());
    (running, state.is_recording)
}

#[tauri::command]
pub async fn start_camera_preview(app: AppHandle, device_id: Option<String>) -> Result<(), String> {
    start_ffmpeg_process(app, false, device_id).await
}

#[tauri::command]
pub async fn stop_camera_preview() -> Result<(), String> {
    let mut state = CAPTURE_STATE.lock().await;
    if let Some(task) = state.ffmpeg_task.take() {
        task.abort();
    }
    state.is_recording = false;
    Ok(())
}

#[tauri::command]
pub async fn start_camera_recording(app: AppHandle, device_id: Option<String>) -> Result<(), String> {
    let state = CAPTURE_STATE.lock().await;
    if state.is_recording {
        return Err("Already recording".into());
    }
    
    // Drop the lock before calling start_ffmpeg_process because it acquires the lock
    drop(state);
    
    start_ffmpeg_process(app, true, device_id).await
}

#[tauri::command]
pub async fn stop_camera_recording() -> Result<String, String> {
    let mut state = CAPTURE_STATE.lock().await;
    
    if !state.is_recording {
        return Err("Not recording".into());
    }
    
    // Gracefully stop ffmpeg by sending 'q' to its stdin
    if let Some(mut stdin) = state.ffmpeg_stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(b"q").await;
        let _ = stdin.flush().await;
    }
    
    // Give ffmpeg time to finalize the WebM file (write metadata/footer)
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
    
    if let Some(task) = state.ffmpeg_task.take() {
        task.abort();
    }
    
    state.is_recording = false;
    
    let path = state.recording_path.take().ok_or_else(|| "No recording path found".to_string())?;
    Ok(path)
}

/// Resolve the ffmpeg binary path. On Windows the shared build is bundled as a
/// tauri resource (see tauri.windows.conf.json + scripts/fetch-windows-ffmpeg.sh);
/// on macOS it sits beside the app binary (tauri.macos.conf.json +
/// scripts/build-macos-ffmpeg.sh); on Linux we assume system ffmpeg is on PATH.
pub fn resolve_ffmpeg_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        use tauri::Manager;
        let resource_dir = app
            .path()
            .resource_dir()
            .map_err(|e| format!("resource_dir(): {e}"))?;
        let exe = resource_dir.join("ffmpeg").join("ffmpeg.exe");
        if !exe.exists() {
            return Err(format!(
                "bundled ffmpeg.exe not found at {}",
                exe.display()
            ));
        }
        Ok(exe)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Ok(system_ffmpeg())
    }
}

/// The ffmpeg to run on platforms where it is not a Tauri resource.
///
/// Linux finds the distribution's ffmpeg on PATH. macOS prefers the LGPL build shipped
/// inside the app, and only falls back to a system one for builds made without it.
#[cfg(not(target_os = "windows"))]
pub fn system_ffmpeg() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        let on_path = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join("ffmpeg").is_file()))
            .unwrap_or(false);
        let exe = std::env::current_exe().ok();
        if let Some(found) = macos_ffmpeg_candidates(exe.as_deref(), on_path)
            .into_iter()
            .find(|candidate| candidate.is_file())
        {
            return found;
        }
    }
    std::path::PathBuf::from("ffmpeg")
}

/// Where to look for ffmpeg on macOS, best first.
///
/// 1. Beside the app binary — `camlooper.app/Contents/MacOS/ffmpeg`, the bundled build.
/// 2. Debug builds only: the source tree `scripts/build-macos-ffmpeg.sh` builds into, so
///    `tauri dev` exercises the same ffmpeg the bundle ships.
/// 3. Homebrew's prefixes, when a bare `ffmpeg` would not resolve: an app launched from
///    Finder or the Dock gets a minimal PATH that does not include them.
pub fn macos_ffmpeg_candidates(
    current_exe: Option<&std::path::Path>,
    ffmpeg_on_path: bool,
) -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = current_exe.and_then(|exe| exe.parent()) {
        candidates.push(dir.join("ffmpeg"));
    }
    if cfg!(debug_assertions) {
        candidates.push(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("drivers/macos/ffmpeg/bin/ffmpeg"),
        );
    }
    if !ffmpeg_on_path {
        candidates.push("/opt/homebrew/bin/ffmpeg".into());
        candidates.push("/usr/local/bin/ffmpeg".into());
    }
    candidates
}

/// ffmpeg input arguments for a macOS AVFoundation camera.
///
/// `device_id` is the camera's *name*, as listed by [`parse_avfoundation_video_devices`].
/// Never its index: AVFoundation renumbers devices while the app is running — the moment
/// our own virtual camera starts streaming it moves to index 0 — so an index captured when
/// the picker was filled points at a different camera by the time recording restarts the
/// capture. The `:none` suffix declines the audio device, so ffmpeg does not also open
/// (and ask permission for) the microphone.
fn avfoundation_input_args(device_id: Option<String>) -> Vec<String> {
    let name = device_id.unwrap_or_else(|| "default".to_string());
    vec![
        "-f".to_string(), "avfoundation".to_string(),
        "-framerate".to_string(), "30".to_string(),
        "-video_size".to_string(), "640x480".to_string(),
        "-i".to_string(), format!("{}:none", name),
    ]
}

/// Pull the camera names out of `ffmpeg -f avfoundation -list_devices true -i ""` stderr.
///
/// Names only, with the listing's index dropped (see [`avfoundation_input_args`] for why).
/// Screen-capture pseudo devices are left out: they are not cameras. So is any device with
/// a `:` in its name — ffmpeg reads `video:audio` from the input string and the frontend
/// takes the text before the first `:` as the id, so such a name could not be opened.
pub fn parse_avfoundation_video_devices(stderr: &str) -> Vec<String> {
    let mut devices = Vec::new();
    let mut in_video_section = false;
    for line in stderr.lines() {
        if line.contains("AVFoundation video devices") {
            in_video_section = true;
            continue;
        }
        if line.contains("AVFoundation audio devices") {
            break;
        }
        if !in_video_section {
            continue;
        }
        // "[AVFoundation indev @ 0x1234] [0] MacBook Pro Camera" — skip the log prefix,
        // then read "[index] name".
        let Some(rest) = line.split_once("] ").map(|(_, rest)| rest.trim()) else { continue };
        let Some(rest) = rest.strip_prefix('[') else { continue };
        let Some((index, name)) = rest.split_once("] ") else { continue };
        let name = name.trim();
        if index.parse::<u32>().is_err() || name.is_empty() {
            continue;
        }
        if name.starts_with("Capture screen") || name.contains(':') {
            continue;
        }
        devices.push(name.to_string());
    }
    devices
}

/// The first listed camera that is not CamLooper's own virtual camera. Capturing from that
/// one would feed the camera its own output.
pub fn first_real_camera(devices: &[String]) -> Option<String> {
    devices.iter().find(|name| !name.to_lowercase().contains("camlooper")).cloned()
}

/// List the cameras AVFoundation can see, by asking ffmpeg — the same ffmpeg that will
/// open the chosen one, so the indices are guaranteed to agree.
#[cfg(target_os = "macos")]
pub async fn list_avfoundation_devices() -> Result<Vec<String>, String> {
    let ffmpeg = system_ffmpeg();
    // ffmpeg exits non-zero here by design: the empty input name can never open. The
    // listing is on stderr regardless.
    let output = tokio::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("Failed to run ffmpeg at {}: {e}", ffmpeg.display()))?;
    Ok(parse_avfoundation_video_devices(&String::from_utf8_lossy(&output.stderr)))
}

/// Build the ffmpeg input arguments for the platform. Windows uses DirectShow
/// (`-f dshow -i video=<FriendlyName>`); Linux uses V4L2 (`-f v4l2 -i /dev/videoN`);
/// macOS uses AVFoundation (`-f avfoundation -i <name>:none`).
fn build_input_args(device_id: Option<String>) -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        // dshow needs the FriendlyName wrapped in `video=` (audio would be `audio=`).
        // If frontend didn't pass one, use "0" to pick the first video device.
        let name = device_id.unwrap_or_else(|| "0".to_string());
        vec![
            "-f".to_string(), "dshow".to_string(),
            "-framerate".to_string(), "30".to_string(),
            "-video_size".to_string(), "640x480".to_string(),
            "-i".to_string(), format!("video={}", name),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        avfoundation_input_args(device_id)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let device = device_id.unwrap_or_else(|| "/dev/video0".to_string());
        vec![
            "-f".to_string(), "v4l2".to_string(),
            "-framerate".to_string(), "30".to_string(),
            "-video_size".to_string(), "640x480".to_string(),
            "-i".to_string(), device,
        ]
    }
}

async fn start_ffmpeg_process(app: AppHandle, record: bool, device_id: Option<String>) -> Result<(), String> {
    let ffmpeg_path = resolve_ffmpeg_path(&app)?;
    start_capture(ffmpeg_path, record, device_id).await
}

/// Start (or restart) the capture process. Split from the command so the whole
/// preview -> record -> preview sequence can be exercised without a Tauri app.
async fn start_capture(
    ffmpeg_path: std::path::PathBuf,
    record: bool,
    device_id: Option<String>,
) -> Result<(), String> {
    let mut state = CAPTURE_STATE.lock().await;

    // Stop existing process
    if let Some(task) = state.ffmpeg_task.take() {
        task.abort();

        // Give the OS time to release the camera device handle
        // Wait asynchronously while holding the lock is okay for 500ms during init
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    }

    // macOS with no camera chosen: do not let AVFoundation pick its "default", which is
    // our own virtual camera whenever that is streaming. Take the first real one.
    #[cfg(target_os = "macos")]
    let device_id = match device_id {
        Some(id) => Some(id),
        None => list_avfoundation_devices()
            .await
            .ok()
            .and_then(|devices| first_real_camera(&devices)),
    };

    let mut args = build_input_args(device_id);

    if record {
        let temp_dir = std::env::temp_dir();
        let filename = format!("recording_{}.webm", Uuid::new_v4());
        let filepath = temp_dir.join(filename).to_string_lossy().to_string();

        // Output 1: WebM file.
        // Force yuv420p — dshow typically emits YUY2/NV12, libvpx-vp9 needs I420 and
        // ffmpeg's implicit conversion had a habit of smearing chroma. BT.709 tags
        // keep players from second-guessing the colorspace.
        args.extend(vec![
            "-c:v".to_string(), "libvpx-vp9".to_string(),
            "-pix_fmt".to_string(), "yuv420p".to_string(),
            "-color_primaries".to_string(), "bt709".to_string(),
            "-color_trc".to_string(), "bt709".to_string(),
            "-colorspace".to_string(), "bt709".to_string(),
            "-color_range".to_string(), "tv".to_string(),
            "-b:v".to_string(), "2M".to_string(),
            "-deadline".to_string(), "realtime".to_string(),
            "-cpu-used".to_string(), "4".to_string(),
            filepath.clone()
        ]);
        state.recording_path = Some(filepath.clone());
        state.is_recording = true;
    } else {
        state.recording_path = None;
        state.is_recording = false;
    }

    // Remaining outputs: the virtual-camera leg and the UI preview.
    //
    // The camera leg used to be absent here. Live frames were encoded to MJPEG for the UI,
    // then handed to the virtual camera, which spawned a *second* ffmpeg to decode that same
    // MJPEG straight back to raw — a full JPEG round trip on live video. Now one process
    // feeds both, and on a raw-stdout sink the preview is derived from the frames Rust is
    // already forwarding rather than encoded a second time.
    let (sink, camera_geometry) = crate::virtual_camera::active_sink().await;
    let geometry = camera_geometry.unwrap_or_else(|| {
        let (w, h) = crate::virtual_camera::output_resolution();
        crate::pipeline::Geometry { width: w, height: h, fps: 30 }
    });
    let camera_spec = crate::pipeline::PipelineSpec {
        source: crate::pipeline::Source::Device { args: Vec::new() },
        sink: sink.clone(),
        geometry,
        preview: Some(crate::pipeline::PreviewSpec::default()),
        realtime: false,
    };
    args.extend(crate::pipeline::build_outputs(&camera_spec));

    println!("Spawning ffmpeg ({}) with args: {:?}", ffmpeg_path.display(), args);
    let mut cmd = tokio::process::Command::new(&ffmpeg_path);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // See what ffmpeg is complaining about
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW — keep the console hidden.
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg at {}: {e}", ffmpeg_path.display()))?;

    let stdin = child.stdin.take().expect("Failed to open stdin");
    let mut stdout = child.stdout.take().expect("Failed to open stdout");

    state.ffmpeg_stdin = Some(stdin);

    let frame_bytes = sink.frame_bytes(&geometry);

    let task = tokio::spawn(async move {
        // Keep child alive as long as this task is alive
        let mut _child = child;

        // Raw-stdout sink (Windows softcam): stdout carries camera frames, and the preview
        // is decimated out of the frames we are already forwarding.
        if let Some(size) = frame_bytes {
            let mut buf = vec![0u8; size];
            let mut encoder = crate::preview::PreviewEncoder::new(
                &geometry,
                crate::pipeline::PreviewSpec::default(),
            )
            .for_sink(&sink);
            loop {
                if stdout.read_exact(&mut buf).await.is_err() {
                    break;
                }
                crate::virtual_camera::push_raw_frame(&buf).await.ok();
                if let Some(jpeg) = encoder.sample(&buf) {
                    let base64_frame = BASE64_STANDARD.encode(&jpeg);
                    if let Ok(mut state) = CAPTURE_STATE.try_lock() {
                        state.latest_frame = Some(base64_frame);
                    }
                }
            }
            println!("Camera capture task ended");
            return;
        }

        // Device sink or preview-only: ffmpeg has already delivered the camera frames, so
        // stdout is the preview MJPEG.
        let mut reader = crate::preview::JpegStreamReader::new();
        let mut chunk = [0u8; 8192];

        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    reader.extend(&chunk[..n]);

                    while let Some(jpeg) = reader.next_frame() {
                        let base64_frame = BASE64_STANDARD.encode(&jpeg);

                        // Store the latest frame instead of emitting
                        if let Ok(mut state) = CAPTURE_STATE.try_lock() {
                            state.latest_frame = Some(base64_frame);
                        }
                        crate::virtual_camera::note_frame_delivered().await;
                    }
                },
                Err(e) => {
                    eprintln!("Failed to read from ffmpeg stdout: {}", e);
                    break;
                }
            }
        }
    });

    state.ffmpeg_task = Some(task);
    Ok(())
}

#[tauri::command]
pub async fn get_camera_preview_frame() -> Result<String, String> {
    let state = CAPTURE_STATE.lock().await;
    if let Some(frame) = &state.latest_frame {
        Ok(frame.clone())
    } else {
        Err("No frame available".to_string())
    }
}

#[tauri::command]
pub async fn get_recorded_video_base64(path: String) -> Result<String, String> {
    let data = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    Ok(BASE64_STANDARD.encode(data))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "\
[AVFoundation indev @ 0xb39014140] AVFoundation video devices:
[AVFoundation indev @ 0xb39014140] [0] MacBook Pro Camera
[AVFoundation indev @ 0xb39014140] [1] MacBook Pro Desk View Camera
[AVFoundation indev @ 0xb39014140] [2] Capture screen 0
[AVFoundation indev @ 0xb39014140] [3] Logitech [C920] HD Pro
[AVFoundation indev @ 0xb39014140] [4] Odd: Name
[AVFoundation indev @ 0xb39014140] AVFoundation audio devices:
[AVFoundation indev @ 0xb39014140] [0] MacBook Pro Microphone
[in#0 @ 0xb39014000] Error opening input: Input/output error
";

    /// The same cameras after the virtual camera starts streaming: macOS moves it to the
    /// front and every other index shifts.
    const LISTING_REORDERED: &str = "\
[AVFoundation indev @ 0xb39014140] AVFoundation video devices:
[AVFoundation indev @ 0xb39014140] [0] CamLooper Virtual Camera
[AVFoundation indev @ 0xb39014140] [1] MacBook Pro Camera
[AVFoundation indev @ 0xb39014140] [2] MacBook Pro Desk View Camera
[AVFoundation indev @ 0xb39014140] AVFoundation audio devices:
";

    #[test]
    fn avfoundation_listing_yields_camera_names_only() {
        assert_eq!(
            parse_avfoundation_video_devices(LISTING),
            vec!["MacBook Pro Camera", "MacBook Pro Desk View Camera", "Logitech [C920] HD Pro"]
        );
    }

    #[test]
    fn avfoundation_listing_ignores_microphones_screens_and_unopenable_names() {
        let devices = parse_avfoundation_video_devices(LISTING);
        assert!(devices.iter().all(|d| !d.contains("Microphone")), "{devices:?}");
        assert!(devices.iter().all(|d| !d.contains("Capture screen")), "{devices:?}");
        assert!(devices.iter().all(|d| !d.contains(':')), "{devices:?}");
    }

    #[test]
    fn avfoundation_listing_is_empty_when_ffmpeg_prints_no_devices() {
        assert!(parse_avfoundation_video_devices("").is_empty());
        assert!(parse_avfoundation_video_devices("ffmpeg: command not found").is_empty());
    }

    /// The frontend takes the text before the first `:` as the device id, so that prefix
    /// must be exactly what `avfoundation_input_args` hands back to ffmpeg.
    #[test]
    fn avfoundation_device_id_round_trips_into_the_input_args() {
        let devices = parse_avfoundation_video_devices(LISTING);
        let id = devices[2].split(':').next().unwrap().to_string();
        let args = avfoundation_input_args(Some(id));
        assert_eq!(args[0..2], ["-f", "avfoundation"]);
        assert_eq!(args.last().unwrap(), "Logitech [C920] HD Pro:none");
    }

    /// Regression: recording froze because the picker's index 0 meant the MacBook camera
    /// when it was filled and CamLooper's own virtual camera once that started streaming.
    #[test]
    fn avfoundation_device_id_survives_macos_renumbering_the_cameras() {
        let before = parse_avfoundation_video_devices(LISTING);
        let after = parse_avfoundation_video_devices(LISTING_REORDERED);
        let picked = before[0].clone();
        assert!(after.contains(&picked), "the id must still name a listed camera");
        let args = avfoundation_input_args(Some(picked));
        assert_eq!(args.last().unwrap(), "MacBook Pro Camera:none");
    }

    #[test]
    fn the_fallback_camera_is_never_the_virtual_camera_itself() {
        let after = parse_avfoundation_video_devices(LISTING_REORDERED);
        assert_eq!(after[0], "CamLooper Virtual Camera");
        assert_eq!(first_real_camera(&after).as_deref(), Some("MacBook Pro Camera"));
        assert_eq!(first_real_camera(&["CamLooper Virtual Camera".to_string()]), None);
    }

    #[test]
    fn avfoundation_falls_back_to_the_system_default_camera_without_audio() {
        let args = avfoundation_input_args(None);
        assert_eq!(args.last().unwrap(), "default:none");
    }

    /// The Step 1 -> Step 2 flow against real hardware, without the UI: virtual camera on,
    /// live preview, start recording, stop recording. This is the sequence in which the
    /// preview froze when cameras were opened by index.
    ///
    ///   cargo test --lib -- --ignored --nocapture live_preview_keeps_moving_while_recording
    ///
    /// Needs a physical camera, camera permission for the terminal, and the CamLooper
    /// camera extension installed and approved. Turns the camera on for about 15 seconds.
    #[cfg(target_os = "macos")]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real camera and the installed camera extension"]
    async fn live_preview_keeps_moving_while_recording() {
        use std::time::Duration;

        async fn camera_frames() -> u64 {
            crate::virtual_camera::get_virtual_camera_status().await.frame_count
        }
        /// Frames delivered to the virtual camera over `secs`, and whether the preview
        /// image changed in that time.
        async fn observe(secs: u64) -> (u64, bool) {
            let frames_before = camera_frames().await;
            let preview_before = get_camera_preview_frame().await.ok();
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let preview_after = get_camera_preview_frame().await.ok();
            (camera_frames().await - frames_before, preview_after.is_some() && preview_after != preview_before)
        }

        // What the picker does: list, drop our own camera, take the first.
        let devices = crate::virtual_camera::list_video_devices().await.expect("list cameras");
        let device = first_real_camera(&devices).expect("a physical camera");
        let device_id = device.split(':').next().unwrap().to_string();
        println!("camera: {device_id}");

        crate::virtual_camera::start_virtual_camera_with(None, None)
            .await
            .expect("start the virtual camera");
        let ffmpeg = system_ffmpeg();
        println!("ffmpeg: {}", ffmpeg.display());

        start_capture(ffmpeg.clone(), false, Some(device_id.clone())).await.expect("start preview");
        tokio::time::sleep(Duration::from_secs(3)).await; // camera warm-up
        let (frames, moved) = observe(3).await;
        println!("preview only: {frames} camera frames in 3s, preview changed: {moved}");
        assert!(frames > 45, "live preview should feed the camera at ~30 fps, got {frames} in 3s");
        assert!(moved, "preview image should change while live");

        // The virtual camera has now been streaming for a while — the state in which macOS
        // has renumbered the devices. This is the restart that used to fail.
        assert!(!CAPTURE_STATE.lock().await.is_recording);
        start_capture(ffmpeg.clone(), true, Some(device_id.clone())).await.expect("start recording");
        tokio::time::sleep(Duration::from_secs(3)).await; // restart + encoder warm-up
        let (frames, moved) = observe(4).await;
        println!("while recording: {frames} camera frames in 4s, preview changed: {moved}");
        assert!(frames > 60, "recording must not stall the camera, got {frames} frames in 4s");
        assert!(moved, "preview froze while recording");

        let path = stop_camera_recording().await.expect("stop recording");
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        println!("recording: {path} ({size} bytes)");
        assert!(size > 50_000, "recording file is missing or nearly empty: {size} bytes");

        // The UI resumes the plain preview after a recording.
        start_capture(ffmpeg, false, Some(device_id)).await.expect("resume preview");
        tokio::time::sleep(Duration::from_secs(3)).await;
        let (frames, moved) = observe(2).await;
        println!("after recording: {frames} camera frames in 2s, preview changed: {moved}");
        assert!(frames > 30 && moved, "preview did not resume after recording");

        stop_camera_preview().await.unwrap();
        crate::virtual_camera::stop_virtual_camera().await.unwrap();
        println!("RECORDING_PATH={path}");
    }

    #[test]
    fn macos_prefers_the_ffmpeg_bundled_beside_the_app_binary() {
        let exe = std::path::Path::new("/Applications/camlooper.app/Contents/MacOS/camlooper");
        let candidates = macos_ffmpeg_candidates(Some(exe), true);
        assert_eq!(
            candidates[0],
            std::path::Path::new("/Applications/camlooper.app/Contents/MacOS/ffmpeg")
        );
    }

    #[test]
    fn macos_only_guesses_at_homebrew_when_ffmpeg_is_not_on_path() {
        let has_homebrew = |c: &[std::path::PathBuf]| c.iter().any(|p| p.starts_with("/opt/homebrew"));
        assert!(has_homebrew(&macos_ffmpeg_candidates(None, false)));
        assert!(!has_homebrew(&macos_ffmpeg_candidates(None, true)));
    }
}
