use anyhow::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::pipeline::{CameraSink, Geometry};

#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use v4l::Device;
#[cfg(target_os = "linux")]
use v4l::capability;

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

/// A running virtual camera.
///
/// This type *owns the sink and nothing else*. It used to also run a frame pump: it took
/// MJPEG on a channel, spawned a second ffmpeg to decode it back to raw, and fed the result
/// to the platform camera. That decode existed only because the producer had encoded MJPEG
/// in the first place — a round trip costing ~13 ms of CPU per frame at 720p, which also
/// re-subsampled chroma on the way through. The producer now renders raw frames directly,
/// so all this has to do is hold the device open and say where frames should go.
pub struct VirtualCamera {
    config: VirtualCameraConfig,
    status: Arc<Mutex<VirtualCameraStatus>>,
    is_running: Arc<Mutex<bool>>,
    /// Needed on Windows to locate the bundled ffmpeg.exe under the Tauri resource dir.
    #[allow(dead_code)]
    app_handle: Option<AppHandle>,
    /// Frames delivered since start. Atomic rather than a field of `status`, because the UI
    /// polls status on an interval and the old layout made that poll take the same mutex the
    /// frame path needed — once per frame, purely to increment a counter.
    frame_count: Arc<AtomicU64>,
    /// Where the video pipeline must deliver frames while this camera runs.
    sink: CameraSink,
    /// Windows: the softcam instance raw frames are pushed into. Held here rather than moved
    /// into a pump task, because the pump now lives with the pipeline producing the frames.
    #[cfg(target_os = "windows")]
    softcam: Option<Arc<Mutex<WindowsVirtualCamera>>>,
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
            is_running: Arc::new(Mutex::new(false)),
            app_handle: None,
            frame_count: Arc::new(AtomicU64::new(0)),
            sink: CameraSink::None,
            #[cfg(target_os = "windows")]
            softcam: None,
        }
    }

    pub fn set_app_handle(&mut self, app_handle: AppHandle) {
        self.app_handle = Some(app_handle);
    }

    pub fn geometry(&self) -> Geometry {
        Geometry { width: self.config.width, height: self.config.height, fps: self.config.fps }
    }

    pub fn sink(&self) -> CameraSink {
        self.sink.clone()
    }

    pub async fn start(&mut self) -> Result<()> {
        {
            let is_running = self.is_running.lock().await;
            if *is_running {
                return Ok(());
            }
        }

        self.frame_count.store(0, Ordering::Relaxed);
        self.sink = self.acquire_sink().await?;

        {
            let mut status = self.status.lock().await;
            status.is_active = true;
            status.frame_count = 0;
        }
        {
            let mut is_running = self.is_running.lock().await;
            *is_running = true;
        }

        println!("Virtual camera started: {} -> {:?}", self.config.camera_name, self.sink);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        {
            let is_running = self.is_running.lock().await;
            if !*is_running {
                return Ok(());
            }
        }

        self.release_sink().await;
        self.sink = CameraSink::None;

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
        let mut status = self.status.lock().await.clone();
        status.frame_count = self.frame_count.load(Ordering::Relaxed);
        status
    }

    pub async fn is_active(&self) -> bool {
        self.status.lock().await.is_active
    }

    /// Hand one raw frame to the platform camera.
    ///
    /// Only meaningful for push sinks (Windows softcam). On Linux ffmpeg writes into the
    /// device node itself and Rust never sees the pixels.
    #[cfg(target_os = "windows")]
    pub async fn push_raw_frame(&self, frame: &[u8]) -> Result<()> {
        let softcam = self
            .softcam
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Virtual camera is not started"))?;
        softcam.lock().await.send_frame(frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    pub async fn push_raw_frame(&self, _frame: &[u8]) -> Result<()> {
        Err(anyhow::anyhow!("This platform's camera does not take pushed frames"))
    }

    /// Count a frame that ffmpeg delivered to the device without passing through Rust.
    pub fn note_frame(&self) {
        self.frame_count.fetch_add(1, Ordering::Relaxed);
    }

    // --- acquiring and releasing the platform sink -------------------------------------

    #[cfg(target_os = "windows")]
    async fn acquire_sink(&mut self) -> Result<CameraSink> {
        println!("Starting softcam DirectShow virtual camera...");

        // Created synchronously so a failure is reported to the caller (and surfaced in the
        // UI) instead of being swallowed inside a background task. softcam.dll is bundled
        // with the app; it is registered at install time (perMachine build) or on first use
        // via `ensure_softcam_registered` (per-user Store build).
        let mut vcam = WindowsVirtualCamera::new(
            &self.config.camera_name,
            self.config.width,
            self.config.height,
            self.config.fps,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create softcam virtual camera: {}", e))?;

        vcam.start()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start softcam virtual camera: {}", e))?;

        self.softcam = Some(Arc::new(Mutex::new(vcam)));
        Ok(CameraSink::RawBgr24Stdout)
    }

    #[cfg(target_os = "linux")]
    async fn acquire_sink(&mut self) -> Result<CameraSink> {
        // Resolve the device before reporting success, so a missing or unloaded module
        // surfaces as an error the UI can show. The frontend calls `ensure_v4l2loopback`
        // first, so the module is normally loaded by now; this is belt and braces.
        let (device_path, _index) = Self::find_loopback_device().map_err(|e| {
            anyhow::anyhow!(
                "No CamLooper virtual camera device found ({}). The v4l2loopback kernel module \
                 may not be loaded — try starting the camera again to load it, or run \
                 `sudo modprobe v4l2loopback`.",
                e
            )
        })?;
        println!("Virtual camera will write to {}", device_path);
        Ok(CameraSink::V4l2 { device: device_path })
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    async fn acquire_sink(&mut self) -> Result<CameraSink> {
        // macOS ships a CMIO Camera Extension, but it cannot load until the app is signed
        // and notarised, and its FrameReceiver is still a stub. Failing here is the honest
        // answer: the previous code logged every frame and slept, so the UI reported a
        // running camera that no application could ever see.
        Err(anyhow::anyhow!(
            "The CamLooper virtual camera is not available on this platform yet. On macOS it \
             needs a signed and notarised build before the system extension will load."
        ))
    }

    #[cfg(target_os = "windows")]
    async fn release_sink(&mut self) {
        if let Some(softcam) = self.softcam.take() {
            // scDeleteCamera runs here rather than in a pump task, so by the time stop()
            // returns the handle is genuinely gone and a subsequent start() cannot race a
            // second instance against it. That race is what the old unconditional 150 ms
            // sleep in stop_platform_camera was papering over.
            if let Err(e) = softcam.lock().await.stop() {
                eprintln!("Failed to stop softcam virtual camera cleanly: {}", e);
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    async fn release_sink(&mut self) {
        // Nothing to release: ffmpeg holds the device node open, and the pipeline closes it
        // when the pipeline stops.
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

/// Where the running camera wants frames, and at what geometry.
///
/// `CameraSink::None` means no camera is running — the pipeline should still be built (the
/// user may just be previewing), it simply gets no camera leg.
pub async fn active_sink() -> (CameraSink, Option<Geometry>) {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    match global_camera.as_ref() {
        Some(camera) => (camera.sink(), Some(camera.geometry())),
        None => (CameraSink::None, None),
    }
}

/// Forward one raw frame to a push sink (Windows softcam).
///
/// Kept as a free function so the pipeline does not have to hold the camera lock across a
/// frame's worth of work — it takes it, pushes, and drops it.
pub async fn push_raw_frame(frame: &[u8]) -> Result<()> {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    match global_camera.as_ref() {
        Some(camera) => camera.push_raw_frame(frame).await,
        None => Err(anyhow::anyhow!("Virtual camera not running")),
    }
}

/// Count a frame ffmpeg wrote straight to the device, so the UI's frame counter still moves
/// on platforms where the pixels never pass through Rust.
pub async fn note_frame_delivered() {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(camera) = global_camera.as_ref() {
        camera.note_frame();
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