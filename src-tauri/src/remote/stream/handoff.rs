//! The app's [`CameraControl`]: hands the CamLooper virtual camera to the phone stream and
//! back, using the same start/stop functions as the UI.
//!
//! Acquire: pause the video loop (and wait for its ffmpeg to exit), stop the in-app camera
//! preview, start the virtual camera if it is off (Linux: load v4l2loopback first — at most
//! one polkit prompt; Windows: make sure softcam is registered — at most one UAC prompt on
//! the Store build). Release: undo exactly what acquire did, in reverse.
//!
//! While the phone holds the camera ([`phone_owns_camera`]) the other writers refuse to
//! start: video playback (`video_processor::start_streaming`) and the in-app camera preview
//! and recording (`camera_capture::start_capture`).
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;
use tokio::sync::Mutex;

use super::runner::{CameraControl, CameraTarget};
use crate::virtual_camera::{self, VirtualCameraConfig};

static PHONE_OWNS_CAMERA: AtomicBool = AtomicBool::new(false);

/// True while a phone session holds the camera; video playback refuses to start meanwhile.
pub fn phone_owns_camera() -> bool {
    PHONE_OWNS_CAMERA.load(Ordering::SeqCst)
}

#[derive(Default, Debug)]
struct Undo {
    /// Playback was running and we paused it.
    resume_playback: bool,
    /// The camera was off and we started it.
    stop_camera: bool,
    claimed: bool,
}

#[derive(Default)]
pub struct AppCamera {
    undo: Mutex<Undo>,
}

impl AppCamera {
    async fn acquire_inner(&self) -> Result<CameraTarget> {
        let mut undo = self.undo.lock().await;
        if undo.claimed {
            // A previous session was given up on (stop watchdog) and has not released yet.
            // Its release will find nothing left: this session's release undoes it all.
            return Err(anyhow!("the previous phone session is still shutting down"));
        }
        // Claim first, look second: the in-app camera checks the claim under its own lock
        // before it starts (camera_capture::start_capture), so either it sees the claim and
        // refuses, or it started before and is seen here. Checking first would leave a window
        // for a recording to start in between.
        PHONE_OWNS_CAMERA.store(true, Ordering::SeqCst);
        undo.claimed = true;
        let (capture_running, recording) = crate::camera_capture::capture_activity().await;
        if recording {
            return Err(anyhow!("a camera recording is in progress"));
        }

        if crate::video_processor::get_video_stream_status().await.is_playing {
            undo.resume_playback = true;
            crate::video_processor::pause_video_stream().await?;
        }
        if !crate::video_processor::wait_for_playback_exit(std::time::Duration::from_secs(5)).await {
            // The old playback task is still alive. Starting playback again on release would
            // put two writers on the camera, so it is not resumed.
            undo.resume_playback = false;
            return Err(anyhow!("video playback did not stop"));
        }
        if capture_running {
            // The in-app camera preview writes to the virtual camera too. It is not resumed
            // afterwards: the UI restarts it when the recorder is opened again.
            let _ = crate::camera_capture::stop_camera_preview().await;
        }

        if !virtual_camera::get_virtual_camera_status().await.is_active {
            prepare_platform_camera().await?;
            let (width, height) = virtual_camera::output_resolution();
            let config = VirtualCameraConfig { width, height, fps: 30, ..VirtualCameraConfig::default() };
            // Noted before the start, not after: a start that fails half way (or a session
            // that ends meanwhile) must still be undone. `release` copes with a camera that
            // never came up.
            undo.stop_camera = true;
            virtual_camera::start_virtual_camera_with(Some(config), None).await?;
        }
        let (sink, geometry) = virtual_camera::active_sink().await;
        let geometry = geometry.ok_or_else(|| anyhow!("the virtual camera did not start"))?;
        Ok(CameraTarget { sink, geometry, preview: true })
    }
}

impl CameraControl for AppCamera {
    fn acquire(&self) -> BoxFuture<'_, Result<CameraTarget>> {
        Box::pin(self.acquire_inner())
    }

    fn release(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut undo = self.undo.lock().await;
            let u = std::mem::take(&mut *undo);
            if u.stop_camera && virtual_camera::get_virtual_camera_status().await.is_active {
                if let Err(e) = virtual_camera::stop_virtual_camera().await {
                    eprintln!("phone stream: could not stop the virtual camera: {e}");
                }
            }
            if u.claimed {
                PHONE_OWNS_CAMERA.store(false, Ordering::SeqCst);
            }
            if u.resume_playback && crate::video_processor::has_video_loaded().await {
                if let Err(e) = crate::video_processor::start_video_stream().await {
                    eprintln!("phone stream: could not resume the video loop: {e}");
                }
            }
        })
    }
}

#[cfg(target_os = "linux")]
async fn prepare_platform_camera() -> Result<()> {
    use crate::virtual_camera::VcamSetupOutcome;
    match virtual_camera::ensure_loopback_ready().await {
        VcamSetupOutcome::Ready | VcamSetupOutcome::JustLoaded => Ok(()),
        other => Err(anyhow!("the virtual camera is not available ({other:?})")),
    }
}

#[cfg(target_os = "windows")]
async fn prepare_platform_camera() -> Result<()> {
    use crate::softcam_register::{ensure_registered, RegisterOutcome};
    match tokio::task::spawn_blocking(ensure_registered).await {
        Ok(RegisterOutcome::AlreadyRegistered | RegisterOutcome::JustRegistered) => Ok(()),
        Ok(other) => Err(anyhow!("the virtual camera driver is not registered ({other:?})")),
        Err(e) => Err(anyhow!("driver registration task failed: {e}")),
    }
}

/// macOS: the Camera Extension's activation happens inside `start_virtual_camera`.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
async fn prepare_platform_camera() -> Result<()> {
    Ok(())
}
