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
/// on Linux we assume system ffmpeg is on PATH.
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
        Ok(std::path::PathBuf::from("ffmpeg"))
    }
}

/// Build the ffmpeg input arguments for the platform. Windows uses DirectShow
/// (`-f dshow -i video=<FriendlyName>`); Linux uses V4L2 (`-f v4l2 -i /dev/videoN`).
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
    #[cfg(not(target_os = "windows"))]
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

    let mut state = CAPTURE_STATE.lock().await;

    // Stop existing process
    if let Some(task) = state.ffmpeg_task.take() {
        task.abort();

        // Give the OS time to release the camera device handle
        // Wait asynchronously while holding the lock is okay for 500ms during init
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    }

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
            );
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
