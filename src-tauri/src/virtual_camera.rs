use anyhow::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

#[cfg(target_os = "linux")]
use v4l::Device;
#[cfg(target_os = "linux")]
use v4l::capability;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::process::Stdio;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use tokio::io::AsyncWriteExt;

#[cfg(target_os = "windows")]
use tokio::io::AsyncReadExt;

#[cfg(target_os = "windows")]
use crate::windows_virtual_camera::WindowsVirtualCamera;

use tauri::AppHandle;

/// Human-readable label we set on our v4l2loopback device (via the modprobe.d
/// `card_label=` option). Used both to create the device with the right name and to pick
/// it out from any other loopback devices (OBS, Discord) at runtime.
#[cfg(target_os = "linux")]
const CARD_LABEL: &str = "CamLooper Virtual Camera";

/// Path to the modprobe options file the deb/rpm installs. When present, a plain
/// `modprobe v4l2loopback` already applies `exclusive_caps=1` + the card label, so we don't
/// pass them explicitly. On AppImage (file absent) we pass them on the command line so
/// browsers/Zoom still list the device.
#[cfg(target_os = "linux")]
const MODPROBE_CONF: &str = "/etc/modprobe.d/camlooper-v4l2loopback.conf";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualCameraConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub camera_name: String,
}

impl Default for VirtualCameraConfig {
    fn default() -> Self {
        // 720p: what conferencing apps actually request, and half the per-frame copy of
        // 1080p. The frontend overrides this from the user's Resolution setting.
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            camera_name: "CamLooper Virtual Camera".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualCameraStatus {
    pub is_active: bool,
    pub camera_name: String,
    pub resolution: String,
    pub fps: u32,
    pub frame_count: u64,
}

pub struct VirtualCamera {
    config: VirtualCameraConfig,
    status: Arc<Mutex<VirtualCameraStatus>>,
    frame_sender: Option<mpsc::UnboundedSender<Vec<u8>>>,
    is_running: Arc<Mutex<bool>>,
    /// Needed on Windows to locate the bundled ffmpeg.exe under the Tauri resource dir.
    app_handle: Option<AppHandle>,
    /// Flipped to `true` on stop so the platform frame pump exits instead of running
    /// forever. Without this the Windows pump leaked a task (and a softcam handle) per
    /// camera toggle.
    shutdown: Arc<tokio::sync::Notify>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
}

impl VirtualCamera {
    pub fn new(config: VirtualCameraConfig) -> Self {
        let status = VirtualCameraStatus {
            is_active: false,
            camera_name: config.camera_name.clone(),
            resolution: format!("{}x{}", config.width, config.height),
            fps: config.fps,
            frame_count: 0,
        };

        Self {
            config,
            status: Arc::new(Mutex::new(status)),
            frame_sender: None,
            is_running: Arc::new(Mutex::new(false)),
            app_handle: None,
            shutdown: Arc::new(tokio::sync::Notify::new()),
            stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn set_app_handle(&mut self, app_handle: AppHandle) {
        self.app_handle = Some(app_handle);
    }

    pub async fn start(&mut self) -> Result<()> {
        {
            let is_running = self.is_running.lock().await;
            if *is_running {
                return Ok(());
            }
        }

        // Clear any shutdown request left over from a previous run, so the pump we are
        // about to spawn doesn't immediately exit.
        self.stopping
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Create frame channel
        let (frame_sender, frame_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
        self.frame_sender = Some(frame_sender);

        // Start platform-specific virtual camera
        self.start_platform_camera(frame_receiver).await?;

        // Update status
        {
            let mut status = self.status.lock().await;
            status.is_active = true;
            status.frame_count = 0;
        }

        {
            let mut is_running = self.is_running.lock().await;
            *is_running = true;
        }
        
        println!("Virtual camera started: {}", self.config.camera_name);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        {
            let is_running = self.is_running.lock().await;
            if !*is_running {
                return Ok(());
            }
        }

        // Stop frame sender
        self.frame_sender = None;

        // Stop platform-specific virtual camera
        self.stop_platform_camera().await?;

        // Update status
        {
            let mut status = self.status.lock().await;
            status.is_active = false;
        }

        {
            let mut is_running = self.is_running.lock().await;
            *is_running = false;
        }
        
        println!("Virtual camera stopped: {}", self.config.camera_name);
        Ok(())
    }

    pub async fn get_status(&self) -> VirtualCameraStatus {
        self.status.lock().await.clone()
    }

    pub async fn send_frame(&self, frame_data: Vec<u8>) -> Result<()> {
        if let Some(sender) = &self.frame_sender {
            if let Err(e) = sender.send(frame_data) {
                return Err(anyhow::anyhow!("Failed to send frame to virtual camera: channel closed. This usually means the virtual camera process has stopped. Error: {}", e));
            }
            
            // Update frame count
            let mut status = self.status.lock().await;
            status.frame_count += 1;
        } else {
            return Err(anyhow::anyhow!("Virtual camera not started - no frame sender available"));
        }
        Ok(())
    }

    pub async fn is_active(&self) -> bool {
        self.status.lock().await.is_active
    }

    #[cfg(target_os = "windows")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Windows implementation using the softcam DirectShow virtual camera.
        //
        // Frames arrive as MJPEG and softcam wants raw BGR24 at exactly config.width x
        // config.height. We hand that whole conversion (decode + scale + RGB->BGR) to the
        // bundled ffmpeg, exactly as the Linux leg does, instead of doing it by hand in
        // Rust. The previous in-process version decoded with the `image` crate and ran a
        // scalar bilinear resize, which measured ~99 ms/frame upscaling 640x360 to
        // 1920x1080 — against a 33 ms frame budget, so most ticks were skipped and the
        // camera delivered ~10 fps. swscale does strictly more work in ~1.6 ms.
        let config = self.config.clone();
        let status = self.status.clone();
        let stopping = self.stopping.clone();

        let app = self
            .app_handle
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Virtual camera has no AppHandle; cannot locate bundled ffmpeg"))?;
        let ffmpeg_path = crate::camera_capture::resolve_ffmpeg_path(app)
            .map_err(|e| anyhow::anyhow!("Failed to resolve bundled ffmpeg: {}", e))?;

        println!("Starting softcam DirectShow virtual camera...");

        // Create and start the softcam-backed virtual camera synchronously so a failure is
        // reported to the caller (and surfaced in the UI) instead of being swallowed inside
        // the background frame loop. softcam.dll is bundled with the app; it is registered at
        // install time (perMachine build) or on first use via `ensure_softcam_registered`
        // (per-user Store build).
        let mut vcam = WindowsVirtualCamera::new(
            &config.camera_name,
            config.width,
            config.height,
            config.fps,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create softcam virtual camera: {}", e))?;

        vcam.start()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start softcam virtual camera: {}", e))?;

        // MJPEG in on stdin, tightly-packed BGR24 out on stdout. rawvideo has no padding
        // or stride, so every frame is exactly width*height*3 bytes and the reader below
        // can use a fixed-size read_exact.
        let mut converter = {
            // tokio's Command exposes creation_flags directly on Windows.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;

            let mut cmd = tokio::process::Command::new(&ffmpeg_path);
            cmd.args([
                "-hide_banner",
                "-loglevel", "error",
                "-analyzeduration", "0",
                "-probesize", "32768",
                "-f", "mjpeg",
                "-i", "pipe:0",
                "-vf", &format!("scale={}:{}", config.width, config.height),
                "-f", "rawvideo",
                "-pix_fmt", "bgr24",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true);

            cmd.spawn()
                .map_err(|e| anyhow::anyhow!("Failed to start ffmpeg converter for the virtual camera: {}", e))?
        };

        let mut conv_stdin = converter
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open ffmpeg converter stdin"))?;
        let mut conv_stdout = converter
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open ffmpeg converter stdout"))?;

        let frame_size = (config.width as usize) * (config.height as usize) * 3;

        // Writer: feed JPEGs to ffmpeg as they arrive. Repeats the last frame when the
        // source is idle so the camera never goes stale, and exits when the channel is
        // dropped (stop) — the old loop had no exit condition at all, so every camera
        // toggle leaked a pump and a softcam handle.
        let writer_stopping = stopping.clone();
        let writer_fps = config.fps.max(1);
        tokio::spawn(async move {
            // Two frame intervals, not one: at one interval this races with normal 30 fps
            // arrival and writes a duplicate alongside almost every real frame. softcam
            // already serves its last frame to consumers on its own, so this only has to
            // be often enough to cover a genuinely idle source.
            let idle_interval =
                tokio::time::Duration::from_millis(2000 / writer_fps as u64);
            let mut last_frame: Option<Vec<u8>> = None;

            loop {
                if writer_stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                let jpeg = match tokio::time::timeout(idle_interval, frame_receiver.recv()).await {
                    // New frame from the producer.
                    Ok(Some(frame)) => {
                        last_frame = Some(frame.clone());
                        Some(frame)
                    }
                    // Producer went away (camera stopped) — shut the pipe down.
                    Ok(None) => break,
                    // Nothing new this interval; repeat the last frame to hold the feed.
                    Err(_) => last_frame.clone(),
                };

                if let Some(jpeg) = jpeg {
                    if conv_stdin.write_all(&jpeg).await.is_err() {
                        break;
                    }
                    if conv_stdin.flush().await.is_err() {
                        break;
                    }
                }
            }

            // Dropping stdin gives ffmpeg EOF so it exits and the reader below unblocks.
            drop(conv_stdin);
        });

        // Reader: pull finished BGR24 frames and push them straight into softcam's shared
        // memory. One reusable buffer, so there is no per-frame allocation.
        tokio::spawn(async move {
            let mut buf = vec![0u8; frame_size];
            let mut frame_count = 0u64;

            loop {
                if stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                match conv_stdout.read_exact(&mut buf).await {
                    Ok(_) => {}
                    // EOF or a short read means ffmpeg exited — normal on stop.
                    Err(e) => {
                        if !stopping.load(std::sync::atomic::Ordering::SeqCst) {
                            eprintln!("Virtual camera converter ended: {}", e);
                        }
                        break;
                    }
                }

                if let Err(e) = vcam.send_frame(&buf) {
                    eprintln!("Failed to send frame to softcam virtual camera: {}", e);
                    break;
                }

                frame_count += 1;
                {
                    let mut status_lock = status.lock().await;
                    status_lock.frame_count = frame_count;
                }
            }

            // Drops `vcam`, which calls scDeleteCamera and releases the softcam instance.
            if let Err(e) = vcam.stop() {
                eprintln!("Failed to stop softcam virtual camera cleanly: {}", e);
            }
            let _ = converter.kill().await;
            println!("Virtual camera frame pump stopped after {} frames", frame_count);
        });

        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Linux v4l2loopback implementation using FFmpeg with frame rate control
        let config = self.config.clone();
        let status = self.status.clone();
        
        // Resolve the device BEFORE spawning so a missing/unloaded module surfaces as an
        // error to the caller (and thus the UI) instead of failing silently inside the task.
        // The frontend calls `ensure_v4l2loopback` first, so the module is normally loaded by
        // now; this is the belt-and-suspenders path.
        let (device_path, _device_index) = Self::find_loopback_device().map_err(|e| {
            anyhow::anyhow!(
                "No CamLooper virtual camera device found ({}). The v4l2loopback kernel module \
                 may not be loaded — try starting the camera again to load it, or run \
                 `sudo modprobe v4l2loopback`.",
                e
            )
        })?;

        tokio::spawn(async move {
            // Device was resolved before the spawn (see above).
            println!("Starting Linux v4l2loopback virtual camera on {}", device_path);
            
            // Start FFmpeg process to write to the v4l2loopback device
            let mut ffmpeg_process = match tokio::process::Command::new("ffmpeg")
                .args([
                    "-analyzeduration", "0",
                    "-probesize", "32768",
                    "-f", "mjpeg",
                    "-i", "pipe:0",
                    "-f", "v4l2",
                    "-pix_fmt", "rgb24", // Must match v4l2loopback's current device lock!

                    "-vf", &format!("scale={}:{},fps={}", config.width, config.height, config.fps),
                    &device_path,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn() {
                Ok(process) => process,
                Err(e) => {
                    eprintln!("Failed to start FFmpeg process: {}", e);
                    eprintln!("Make sure FFmpeg is installed: sudo apt install ffmpeg");
                    return;
                }
            };
            
            // Get the stdin handle
            let mut stdin = match ffmpeg_process.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    eprintln!("Failed to get FFmpeg stdin");
                    return;
                }
            };
            
            let mut frame_count = 0u64;
            let mut last_frame_jpeg: Option<Vec<u8>> = None;
            let target_frame_duration = tokio::time::Duration::from_millis(1000 / config.fps as u64);
            let mut frame_timer = tokio::time::interval(target_frame_duration);
            frame_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            // Create a frame buffer to handle incoming frames
            let mut frame_buffer = std::collections::VecDeque::new();
            const MAX_BUFFER_SIZE: usize = 10;
            
            // Spawn a task to collect incoming frames
            let (buffer_sender, mut buffer_receiver) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Some(frame_data) = frame_receiver.recv().await {
                    if buffer_sender.send(frame_data).is_err() {
                        break;
                    }
                }
            });
            
            println!("Virtual camera frame processing started with resolution {}x{} at {} FPS", 
                config.width, config.height, config.fps);
            
            loop {
                // Wait for the next frame time
                frame_timer.tick().await;
                
                // Collect any available frames (non-blocking)
                let mut is_disconnected = false;
                loop {
                    match buffer_receiver.try_recv() {
                        Ok(frame_data) => {
                            if frame_buffer.len() >= MAX_BUFFER_SIZE {
                                frame_buffer.pop_front();
                            }
                            frame_buffer.push_back(frame_data);
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                            break;
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            is_disconnected = true;
                            break;
                        }
                    }
                }
                
                if is_disconnected {
                    println!("Virtual camera channel disconnected, stopping FFmpeg writer");
                    break;
                }
                
                // Get the most recent frame to send
                let current_frame_data = if let Some(frame_data) = frame_buffer.pop_back() {
                    // Clear any remaining buffered frames to use the latest
                    frame_buffer.clear();
                    Some(frame_data)
                } else {
                    None
                };
                
                // Use the new frame or repeat the last frame
                let jpeg_to_send = if let Some(frame_data) = current_frame_data {
                    last_frame_jpeg = Some(frame_data.clone());
                    Some(frame_data)
                } else {
                    last_frame_jpeg.clone()
                };
                
                // Send frame to FFmpeg
                if let Some(jpeg_data) = jpeg_to_send {
                    if let Err(e) = stdin.write_all(&jpeg_data).await {
                        eprintln!("Failed to write frame to FFmpeg: {}", e);
                        break;
                    }
                } else {
                    // No frames have been received yet, just wait.
                    continue;
                }
                
                // Flush periodically (not every frame for better performance)
                if frame_count % 5 == 0 {
                    if let Err(e) = stdin.flush().await {
                        eprintln!("Failed to flush frame data to FFmpeg: {}", e);
                        break;
                    }
                }
                
                frame_count += 1;
                if frame_count % 30 == 0 {
                    println!("Sent {} frames to virtual camera via FFmpeg", frame_count);
                }
                
                // Update status
                {
                    let mut status_lock = status.lock().await;
                    status_lock.frame_count = frame_count;
                }
            }
            
            // Close stdin to signal end of stream
            drop(stdin);
            
            // Wait for FFmpeg process to finish
            match ffmpeg_process.wait().await {
                Ok(exit_status) => {
                    println!("FFmpeg process finished with status: {}", exit_status);
                }
                Err(e) => {
                    eprintln!("Error waiting for FFmpeg process: {}", e);
                }
            }
            
            println!("Linux virtual camera stopped after {} frames", frame_count);
        });
        
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // macOS AVFoundation implementation
        let _config = self.config.clone();
        let _status = self.status.clone();
        
        tokio::spawn(async move {
            println!("Starting macOS AVFoundation virtual camera...");
            
            // TODO: Implement macOS AVFoundation virtual camera
            // This would involve:
            // 1. Creating an AVCaptureDevice
            // 2. Setting up AVCaptureSession
            // 3. Streaming frames through AVSampleBufferDisplayLayer
            
            while let Some(frame_data) = frame_receiver.recv().await {
                // Process frame for AVFoundation
                println!("Processing frame for macOS virtual camera: {} bytes", frame_data.len());
                
                // Simulate frame processing
                tokio::time::sleep(tokio::time::Duration::from_millis(33)).await; // ~30 FPS
            }
            
            println!("macOS virtual camera stopped");
        });
        
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Fallback implementation for unsupported platforms
        tokio::spawn(async move {
            println!("Virtual camera not supported on this platform");
            while let Some(frame_data) = frame_receiver.recv().await {
                println!("Would process frame: {} bytes", frame_data.len());
                tokio::time::sleep(tokio::time::Duration::from_millis(33)).await;
            }
        });
        Ok(())
    }

    async fn stop_platform_camera(&self) -> Result<()> {
        println!("Stopping platform-specific virtual camera...");

        // Tell the frame pump to exit. `stop()` has already dropped `frame_sender`, which
        // is the primary signal (the writer sees a closed channel); this flag covers the
        // window where the pump is mid-iteration and makes the intent explicit.
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.shutdown.notify_waiters();

        // Give the pump a moment to unwind and release the platform camera handle before
        // a subsequent start() tries to create a second one.
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn find_loopback_device() -> Result<(String, usize)> {
        // Prefer the device we labelled "CamLooper Virtual Camera"; if we only find some
        // other app's loopback (OBS, Discord), keep it as a fallback.
        let mut fallback: Option<(String, usize)> = None;
        for i in 0..20 {
            let device_path = format!("/dev/video{}", i);
            if std::path::Path::new(&device_path).exists() {
                // Try to open the device and check if it's a v4l2loopback device
                match Device::new(i) {
                    Ok(device) => {
                        // Check if it's a v4l2loopback device by checking capabilities and driver info
                        if let Ok(caps) = device.query_caps() {
                            println!("Checking device {}: driver='{}', card='{}', bus='{}'", 
                                device_path, caps.driver, caps.card, caps.bus);
                            
                            // Prefer our own labelled device; otherwise remember the first
                            // loopback-like device as a fallback.
                            if caps.card.trim() == CARD_LABEL {
                                println!("Found CamLooper virtual camera device: {}", device_path);
                                return Ok((device_path, i));
                            }
                            if fallback.is_none() && Self::is_v4l2loopback_device(&caps) {
                                fallback = Some((device_path.clone(), i));
                            }
                        }
                    }
                    Err(e) => {
                        println!("Failed to open device {}: {}", device_path, e);
                        continue;
                    }
                }
            }
        }
        fallback.ok_or_else(|| anyhow::anyhow!("No v4l2loopback device found"))
    }

    #[cfg(target_os = "linux")]
    fn is_v4l2loopback_device(caps: &capability::Capabilities) -> bool {
        // Check if the driver is v4l2loopback
        let driver_name = caps.driver.to_lowercase();
        if driver_name.contains("v4l2loopback") || driver_name.contains("v4l2 loopback") {
            return true;
        }
        
        // Check if the card name indicates it's a loopback device
        let card_name = caps.card.to_lowercase();
        if card_name.contains("loopback") || card_name.contains("dummy") {
            return true;
        }
        
        // Check capabilities - v4l2loopback devices typically support both capture and output
        let has_video_capture = caps.capabilities.contains(capability::Flags::VIDEO_CAPTURE);
        let has_video_output = caps.capabilities.contains(capability::Flags::VIDEO_OUTPUT);
        let has_streaming = caps.capabilities.contains(capability::Flags::STREAMING);
        
        // v4l2loopback devices usually have both capture and output capabilities
        if has_video_capture && has_video_output && has_streaming {
            println!("Device has loopback-like capabilities (capture + output + streaming)");
            return true;
        }
        
        // Check if it's a writable device (can be used as output)
        if has_video_output && has_streaming {
            println!("Device has output capabilities, might be usable as virtual camera");
            return true;
        }
        
        false
    }
}

/// Result of making sure the v4l2loopback module is loaded and a usable device exists.
/// Serialized (camelCase) to the frontend — mirrors the Windows `RegisterOutcome` pattern —
/// so the UI can either proceed or surface an actionable message. Unit-only so it reaches the
/// frontend as a plain string; detailed failure reasons are logged server-side.
#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VcamSetupOutcome {
    /// A CamLooper virtual-camera device is already present — nothing to do, no prompt.
    Ready,
    /// The module wasn't loaded; we loaded it just now (one polkit prompt accepted).
    JustLoaded,
    /// The user dismissed the polkit authentication dialog.
    Declined,
    /// The v4l2loopback module isn't built for the running kernel (missing kernel headers /
    /// DKMS never built it, or it isn't installed at all).
    NotInstalled,
    /// The module exists but the kernel refused to load it because of Secure Boot (unsigned
    /// DKMS module — needs MOK enrollment; not auto-fixable).
    SecureBootBlocked,
    /// No pkexec / polkit agent available to escalate privileges.
    NoPkexec,
    /// A device node exists but isn't accessible (user not in the `video` group).
    PermissionDenied,
    /// Any other failure — details are logged to stderr.
    Failed,
}

/// Guards against re-prompting: once we've successfully ensured the device this session,
/// later calls report state without another polkit prompt.
#[cfg(target_os = "linux")]
static VCAM_ENSURED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True if the v4l2loopback module is currently loaded into the kernel.
#[cfg(target_os = "linux")]
fn is_module_loaded() -> bool {
    std::path::Path::new("/sys/module/v4l2loopback").exists()
}

/// Whether a v4l2loopback `.ko` is built for the running kernel: `Some(true)`/`Some(false)`
/// if we could run `modinfo`, or `None` if `modinfo` couldn't be found/executed (in which
/// case the caller should attempt the load rather than assume absence). Tries absolute paths
/// first because a desktop-launched app may not have `/usr/sbin` on PATH.
#[cfg(target_os = "linux")]
async fn module_ko_exists() -> Option<bool> {
    for bin in ["/usr/sbin/modinfo", "/sbin/modinfo", "modinfo"] {
        match tokio::process::Command::new(bin)
            .arg("-n")
            .arg("v4l2loopback")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(status) => return Some(status.success()),
            Err(_) => continue,
        }
    }
    None
}

/// True if any `/dev/videoN` node exists but can't be opened for read/write because of
/// permissions — i.e. the current user is not in the `video` group.
#[cfg(target_os = "linux")]
fn any_video_node_permission_denied() -> bool {
    for i in 0..20 {
        let path = format!("/dev/video{}", i);
        if std::path::Path::new(&path).exists() {
            if let Err(e) = std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    return true;
                }
            }
        }
    }
    false
}

/// Best-effort Secure Boot detection via `mokutil --sb-state`.
#[cfg(target_os = "linux")]
async fn is_secure_boot_enabled() -> bool {
    for bin in ["/usr/bin/mokutil", "/bin/mokutil", "mokutil"] {
        if let Ok(output) = tokio::process::Command::new(bin)
            .arg("--sb-state")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await
        {
            return String::from_utf8_lossy(&output.stdout)
                .to_lowercase()
                .contains("enabled");
        }
    }
    false
}

/// Module loaded but no usable device: distinguish a permissions problem from anything else.
#[cfg(target_os = "linux")]
async fn classify_no_device() -> VcamSetupOutcome {
    if any_video_node_permission_denied() {
        VcamSetupOutcome::PermissionDenied
    } else {
        eprintln!("camlooper: v4l2loopback is loaded but no usable device was found");
        VcamSetupOutcome::Failed
    }
}

/// Load the v4l2loopback module on demand, escalating with pkexec (one polkit prompt). Falls
/// back to a plain `modprobe` (works only if already privileged) when pkexec is absent.
#[cfg(target_os = "linux")]
async fn load_loopback_module() -> VcamSetupOutcome {
    let pkexec = ["/usr/bin/pkexec", "/bin/pkexec"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists());

    // deb/rpm ship the modprobe.d options file; AppImage doesn't, so pass options explicitly.
    let pass_options = !std::path::Path::new(MODPROBE_CONF).exists();

    let mut cmd = match pkexec {
        Some(p) => {
            let mut c = tokio::process::Command::new(p);
            c.arg("modprobe").arg("v4l2loopback");
            c
        }
        None => {
            let mut c = tokio::process::Command::new("modprobe");
            c.arg("v4l2loopback");
            c
        }
    };
    if pass_options {
        cmd.arg("exclusive_caps=1");
        cmd.arg(format!("card_label={}", CARD_LABEL));
    }

    let output = match cmd.stdin(Stdio::null()).output().await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("camlooper: failed to run modprobe: {}", e);
            return VcamSetupOutcome::NoPkexec;
        }
    };

    if output.status.success() {
        // udev needs a moment to create /dev/videoN and apply permissions.
        for _ in 0..15 {
            if VirtualCamera::find_loopback_device().is_ok() {
                return VcamSetupOutcome::JustLoaded;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        return classify_no_device().await;
    }

    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();

    // pkexec's own exit codes: 126 = dialog dismissed, 127 = auth couldn't proceed / no agent.
    if pkexec.is_some() && code == 126 {
        return VcamSetupOutcome::Declined;
    }
    if pkexec.is_some() && code == 127 {
        return VcamSetupOutcome::NoPkexec;
    }
    if stderr.contains("not found") || stderr.contains("not currently installed") {
        return VcamSetupOutcome::NotInstalled;
    }
    if stderr.contains("key was rejected")
        || stderr.contains("required key not available")
        || is_secure_boot_enabled().await
    {
        return VcamSetupOutcome::SecureBootBlocked;
    }
    eprintln!(
        "camlooper: modprobe v4l2loopback failed (exit {}): {}",
        code,
        stderr.trim()
    );
    VcamSetupOutcome::Failed
}

/// Make sure a usable CamLooper virtual-camera device exists, loading the module on demand.
/// Called from the frontend before enabling the camera. At most one polkit prompt per run.
#[cfg(target_os = "linux")]
pub async fn ensure_loopback_ready() -> VcamSetupOutcome {
    use std::sync::atomic::Ordering;

    // Fast path: a usable device already exists (no prompt).
    if VirtualCamera::find_loopback_device().is_ok() {
        return VcamSetupOutcome::Ready;
    }

    // Already ensured earlier this session: report state without prompting again.
    if VCAM_ENSURED.load(Ordering::Relaxed) {
        return if is_module_loaded() {
            classify_no_device().await
        } else {
            VcamSetupOutcome::Failed
        };
    }

    // Module loaded but no usable device (e.g. devices=0 or a permissions issue).
    if is_module_loaded() {
        return classify_no_device().await;
    }

    // Not loaded. If we can tell the .ko isn't built for this kernel, say so without a prompt.
    if module_ko_exists().await == Some(false) {
        return VcamSetupOutcome::NotInstalled;
    }

    let outcome = load_loopback_module().await;
    if matches!(
        outcome,
        VcamSetupOutcome::Ready | VcamSetupOutcome::JustLoaded
    ) {
        VCAM_ENSURED.store(true, Ordering::Relaxed);
    }
    outcome
}

/// Non-Linux, non-Windows (macOS): the virtual camera is provided by a different mechanism,
/// so there's no kernel module to load — always report ready.
#[cfg(all(not(target_os = "windows"), not(target_os = "linux")))]
pub async fn ensure_loopback_ready() -> VcamSetupOutcome {
    VcamSetupOutcome::Ready
}

// Global virtual camera instance
static VIRTUAL_CAMERA: Lazy<Arc<Mutex<Option<VirtualCamera>>>> = Lazy::new(|| {
    Arc::new(Mutex::new(None))
});

/// The output resolution the user picked, shared with the video processor.
///
/// Both ends must agree: the MJPEG stage renders at this size and the camera emits it, so
/// there is no rescaling in between. They used to disagree — the source was hardcoded to
/// 640 wide and the camera to 1920x1080, so every frame was upscaled from 360p to 1080p
/// for no gain in detail. Defaults match `VirtualCameraConfig::default`, which covers the
/// case where video playback starts before the camera is enabled.
static OUTPUT_WIDTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1280);
static OUTPUT_HEIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(720);

pub fn set_output_resolution(width: u32, height: u32) {
    OUTPUT_WIDTH.store(width, std::sync::atomic::Ordering::Relaxed);
    OUTPUT_HEIGHT.store(height, std::sync::atomic::Ordering::Relaxed);
}

pub fn output_resolution() -> (u32, u32) {
    (
        OUTPUT_WIDTH.load(std::sync::atomic::Ordering::Relaxed),
        OUTPUT_HEIGHT.load(std::sync::atomic::Ordering::Relaxed),
    )
}

// Public API functions
pub async fn start_virtual_camera(
    config: Option<VirtualCameraConfig>,
    app: AppHandle,
) -> Result<VirtualCameraStatus> {
    let config = config.unwrap_or_default();

    // Keep the MJPEG stage in step with what the camera will emit.
    set_output_resolution(config.width, config.height);

    let mut global_camera = VIRTUAL_CAMERA.lock().await;

    // Tear down any camera that is still running before replacing it. Without this a
    // second start (e.g. the user changing resolution) left the previous frame pump alive
    // and its platform camera handle unreleased, so both pumps kept writing frames.
    if let Some(ref mut existing) = global_camera.as_mut() {
        existing.stop().await?;
    }
    *global_camera = None;

    let mut camera = VirtualCamera::new(config);
    camera.set_app_handle(app);

    camera.start().await?;
    let status = camera.get_status().await;

    // Store the camera instance
    *global_camera = Some(camera);

    Ok(status)
}

pub async fn stop_virtual_camera() -> Result<VirtualCameraStatus> {
    let mut global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(ref mut camera) = global_camera.as_mut() {
        camera.stop().await?;
        let status = camera.get_status().await;
        *global_camera = None;
        Ok(status)
    } else {
        Err(anyhow::anyhow!("Virtual camera not running"))
    }
}

pub async fn get_virtual_camera_status() -> VirtualCameraStatus {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(ref camera) = global_camera.as_ref() {
        camera.get_status().await
    } else {
        VirtualCameraStatus {
            is_active: false,
            camera_name: "CamLooper Virtual Camera".to_string(),
            resolution: "1920x1080".to_string(),
            fps: 30,
            frame_count: 0,
        }
    }
}

pub async fn send_frame_to_virtual_camera(frame_data: Vec<u8>) -> Result<()> {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(ref camera) = global_camera.as_ref() {
        camera.send_frame(frame_data).await
    } else {
        Err(anyhow::anyhow!("Virtual camera not running"))
    }
}

#[cfg(target_os = "windows")]
pub async fn list_video_devices() -> Result<Vec<String>> {
    // Use the softcam-backed virtual camera implementation
    crate::windows_virtual_camera::list_video_devices()
}

#[cfg(target_os = "linux")]
pub async fn list_video_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();
    
    for i in 0..20 {
        let device_path = format!("/dev/video{}", i);
        if std::path::Path::new(&device_path).exists() {
            match Device::new(i) {
                Ok(device) => {
                    if let Ok(caps) = device.query_caps() {
                        devices.push(format!("{}: {} ({})", device_path, caps.card, caps.driver));
                    } else {
                        devices.push(format!("{}: Unknown device", device_path));
                    }
                }
                Err(_) => continue,
            }
        }
    }
    
    Ok(devices)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub async fn list_video_devices() -> Result<Vec<String>> {
    Ok(vec!["Virtual camera not supported on this platform".to_string()])
} 