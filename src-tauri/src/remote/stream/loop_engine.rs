//! The desktop's video loop — the app's one `video_processor` — as a [`LoopPlayer`].
//!
//! This is the whole of "play a video that came from the phone": stop what plays, load the
//! file with `load_video_file_with_original_name`, make sure the camera is on, start the
//! loop. It needs no window, so the headless CLI (`examples/phone_stream_cli.rs`) runs the
//! very same code as the app. What differs between the two is behind [`LoopHost`]: which
//! camera the loop writes to, and who is told about the result.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;

use super::file_receive::{LoopPlayer, LoopState};
use crate::video_processor::{self, LoopSettings};

pub use crate::video_processor::VideoInfo;

/// What surrounds the loop engine.
pub trait LoopHost: Send + Sync {
    /// Make sure the virtual camera is running (the app: driver or kernel module first,
    /// then the platform camera — `handoff::ensure_virtual_camera`).
    fn ensure_camera(&self) -> BoxFuture<'_, Result<()>>;
    /// The phone stopped or started the loop.
    fn loop_changed(&self, playing: bool);
    /// A video from the phone was loaded and is looping. `video` is what a desktop-side
    /// load returns.
    fn video_loaded(&self, video: &VideoInfo);
}

/// The app's loop engine, driven the way the UI drives it.
pub struct EngineLoop {
    host: Arc<dyn LoopHost>,
}

impl EngineLoop {
    pub fn new(host: Arc<dyn LoopHost>) -> Self {
        Self { host }
    }

    /// What the UI does before it starts playback: the in-app camera off the virtual camera
    /// (never under a recording), the virtual camera on.
    async fn prepare_camera(&self) -> Result<()> {
        let (capture_running, recording) = crate::camera_capture::capture_activity().await;
        if recording {
            return Err(anyhow!("A camera recording is in progress."));
        }
        if capture_running {
            let _ = crate::camera_capture::stop_camera_preview().await;
        }
        self.host.ensure_camera().await
    }
}

impl LoopPlayer for EngineLoop {
    fn state(&self) -> BoxFuture<'_, LoopState> {
        Box::pin(async {
            let playing = video_processor::get_video_stream_status().await.is_playing;
            match video_processor::loaded_video().await {
                Some((path, name)) => LoopState { playing, name: Some(name), path: Some(path) },
                None => LoopState { playing, name: None, path: None },
            }
        })
    }

    fn stop(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            video_processor::stop_video_stream().await?;
            self.host.loop_changed(false);
            Ok(())
        })
    }

    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            if !video_processor::has_video_loaded().await {
                return Err(anyhow!("No video is loaded."));
            }
            self.prepare_camera().await?;
            video_processor::start_video_stream().await?;
            self.host.loop_changed(true);
            Ok(())
        })
    }

    fn play_file<'a>(&'a self, path: &'a Path, display_name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // The loop that is playing, if any, and its ffmpeg: gone before the next starts.
            video_processor::stop_video_stream().await?;
            if !video_processor::wait_for_playback_exit(Duration::from_secs(5)).await {
                return Err(anyhow!("video playback did not stop"));
            }
            let video = video_processor::load_video_file_with_original_name(
                path.to_string_lossy().into_owned(),
                display_name.to_string(),
            )
            .await?;
            self.prepare_camera().await?;
            video_processor::start_video_stream().await?;
            self.host.video_loaded(&video);
            Ok(())
        })
    }
}

/// The playback options the main window pushes when nothing was changed in it: loop
/// forever, natural motion on. (The engine's own default before the window's first push
/// is ten passes.) For a host without a window.
pub async fn apply_window_default_settings() -> Result<()> {
    video_processor::set_video_loop_settings(LoopSettings { loop_count: 0, auto_start: true, natural_motion: true }).await
}

/// Playback as the main window polls it, for a host without a window.
#[derive(Debug, Clone, PartialEq)]
pub struct Playback {
    pub playing: bool,
    /// Natural motion is still decoding its first frames.
    pub preparing: bool,
    pub fps: f64,
    pub seconds: f64,
    /// Frames delivered to the virtual camera since it started, where Rust sees them
    /// (push sinks); 0 on Linux, where ffmpeg writes the device itself.
    pub camera_frames: u64,
}

pub async fn playback() -> Playback {
    let s = video_processor::get_video_stream_status().await;
    let camera = crate::virtual_camera::get_virtual_camera_status().await;
    Playback {
        playing: s.is_playing,
        preparing: s.is_preparing,
        fps: s.actual_fps,
        seconds: s.current_time,
        camera_frames: camera.frame_count,
    }
}

/// Stop the loop and wait for its ffmpeg to be gone (a host that is about to delete the
/// video, or exit). `false` when playback is still alive after `timeout`.
pub async fn stop_and_wait(timeout: Duration) -> bool {
    let _ = video_processor::stop_video_stream().await;
    video_processor::wait_for_playback_exit(timeout).await
}

/// Pause the loop for a live phone stream on a camera that has no handoff of its own (the
/// CLI's fixed device; the app's handoff does this itself) and wait for its ffmpeg to let
/// go of the device. Returns whether a loop was playing, for [`resume_after_live`].
pub async fn pause_for_live(timeout: Duration) -> Result<bool> {
    let was_playing = video_processor::get_video_stream_status().await.is_playing;
    if was_playing {
        video_processor::pause_video_stream().await?;
    }
    if !video_processor::wait_for_playback_exit(timeout).await {
        return Err(anyhow!("video playback did not stop"));
    }
    Ok(was_playing)
}

/// Start the loop again after [`pause_for_live`] said it was playing.
pub async fn resume_after_live() -> Result<()> {
    if video_processor::has_video_loaded().await {
        video_processor::start_video_stream().await?;
    }
    Ok(())
}
