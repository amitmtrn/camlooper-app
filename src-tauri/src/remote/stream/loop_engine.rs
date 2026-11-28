//! The desktop's video loop — the app's one `video_processor` — as a [`LoopPlayer`].
//!
//! This is the whole of "play a video that came from the phone": read its header (held to
//! the container its first bytes showed, and to the size limits), make sure the camera is
//! on, and only then stop what plays, load the file and start the loop — going back to the
//! earlier video if that fails. It needs no window, so the headless CLI
//! (`examples/phone_stream_cli.rs`) runs the very same code as the app. What differs between
//! the two is behind [`LoopHost`]: which camera the loop writes to, and who is told about
//! the result.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures_util::future::BoxFuture;

use super::file_receive::{LoopPlayer, LoopState, PlayError, PlayRequest, Played, LOOP_BUSY, LOOP_CAMERA_OFF};
use super::signal::LoopAction;
use crate::video_processor::{self, LoopSettings, ReplaceError, Replaced};

pub use crate::video_processor::VideoInfo;

/// Reading a received video's header may take this long.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Turning the virtual camera on for a received video may take this long. It can be a
/// system prompt (polkit, UAC) that the user has to read and answer.
const CAMERA_TIMEOUT: Duration = Duration::from_secs(120);

/// One loop operation at a time, app-wide: the phone's `stop` / `start`, a received video
/// being loaded and started, and the camera handoff of a live stream (acquire and release)
/// all change "what plays on the camera" in several steps, and none of them may run between
/// the steps of another.
static LOOP_OPS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Take the loop-operation lock. Held by [`EngineLoop`] for each of its operations and by
/// `handoff::AppCamera` around acquire and release.
pub async fn loop_operation() -> tokio::sync::MutexGuard<'static, ()> {
    LOOP_OPS.lock().await
}

/// What surrounds the loop engine.
pub trait LoopHost: Send + Sync {
    /// Make sure the virtual camera is running (the app: driver or kernel module first,
    /// then the platform camera — `handoff::ensure_virtual_camera`). Only for a video the
    /// user accepted in a dialog; never for a `loop_control`.
    fn ensure_camera(&self) -> BoxFuture<'_, Result<()>>;
    /// Whether the virtual camera is running right now.
    fn camera_active(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { crate::virtual_camera::get_virtual_camera_status().await.is_active })
    }
    /// The phone stopped or started the loop.
    fn loop_changed(&self, playing: bool);
    /// A video from the phone was loaded: looping (`playing`), or loaded but not started.
    /// `video` is what a desktop-side load returns.
    fn video_loaded(&self, video: &VideoInfo, playing: bool);
}

/// The app's loop engine, driven the way the UI drives it.
pub struct EngineLoop {
    host: Arc<dyn LoopHost>,
}

impl EngineLoop {
    pub fn new(host: Arc<dyn LoopHost>) -> Self {
        Self { host }
    }

    /// The in-app camera off the virtual camera — never under a recording.
    async fn free_camera_from_capture(&self) -> Result<()> {
        let (capture_running, recording) = crate::camera_capture::capture_activity().await;
        if recording {
            return Err(anyhow!("A camera recording is in progress."));
        }
        if capture_running {
            let _ = crate::camera_capture::stop_camera_preview().await;
        }
        Ok(())
    }

    async fn stop_locked(&self) -> Result<()> {
        video_processor::stop_video_stream().await?;
        self.host.loop_changed(false);
        Ok(())
    }

    /// Start the loaded video on a camera that is already on. Nothing here can prompt.
    async fn start_locked(&self) -> Result<()> {
        if !video_processor::has_video_loaded().await {
            return Err(anyhow!("No video is loaded."));
        }
        if !self.host.camera_active().await {
            return Err(anyhow!("The virtual camera is off."));
        }
        self.free_camera_from_capture().await?;
        video_processor::start_video_stream().await?;
        self.host.loop_changed(true);
        Ok(())
    }

    async fn play_locked(&self, request: PlayRequest) -> Result<Played, PlayError> {
        let PlayRequest { path, display_name, container, cancel } = request;
        if cancel.is_cancelled() {
            return Err(PlayError::Cancelled);
        }
        // 1. Is it a video this computer can play, within the limits? Nothing is touched
        //    yet: a file that fails here leaves the loop that is playing alone.
        let video = tokio::select! {
            r = video_processor::probe_phone_video(path.clone(), display_name, container, PROBE_TIMEOUT) => r?,
            _ = cancel.cancelled() => return Err(PlayError::Cancelled),
        };
        // 2. Can it be shown? Not under a recording; and the camera has to be on — started
        //    here if it is off, which can be a system prompt. Still nothing of the loop is
        //    touched. Ending the wait (cancel, deadline) drops the Linux prompt; a Windows
        //    UAC prompt cannot be closed from here and is left to finish on its own.
        self.free_camera_from_capture().await?;
        tokio::select! {
            r = tokio::time::timeout(CAMERA_TIMEOUT, self.host.ensure_camera()) => match r {
                Ok(r) => r?,
                Err(_) => return Err(PlayError::Failed(anyhow!("turning the virtual camera on took too long"))),
            },
            _ = cancel.cancelled() => return Err(PlayError::Cancelled),
        }
        // 3. Swap and start, in one step as far as the window's own commands can tell.
        let was_playing = video_processor::get_video_stream_status().await.is_playing;
        let cancelled = || cancel.is_cancelled();
        let replaced = video_processor::replace_with_phone_video(path.clone(), video.clone(), container, &cancelled).await;
        let now_playing = video_processor::get_video_stream_status().await.is_playing;
        match replaced {
            Ok(Replaced::Playing) => {
                self.host.video_loaded(&video, true);
                Ok(Played::Looping { path: video_processor::loaded_video().await.map(|(p, _)| p).unwrap_or(path) })
            }
            Ok(Replaced::LoadedNotPlaying(e)) => {
                eprintln!("phone video: loaded, but playback did not start: {e:#}");
                self.host.video_loaded(&video, false);
                Ok(Played::LoadedNotPlaying { path: video_processor::loaded_video().await.map(|(p, _)| p).unwrap_or(path) })
            }
            // The earlier video is loaded again. If it did not come back playing, the
            // window has to hear that its loop stopped.
            Err(e) => {
                if was_playing != now_playing {
                    self.host.loop_changed(now_playing);
                }
                Err(match e {
                    ReplaceError::Cancelled => PlayError::Cancelled,
                    ReplaceError::Failed(e) => PlayError::Failed(e),
                })
            }
        }
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
            let _op = loop_operation().await;
            self.stop_locked().await
        })
    }

    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            let _op = loop_operation().await;
            self.start_locked().await
        })
    }

    fn camera_ready(&self) -> BoxFuture<'_, bool> {
        self.host.camera_active()
    }

    fn control<'a>(&'a self, action: LoopAction, refused: &'a (dyn Fn() -> bool + Send + Sync)) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // The lock first, the question second: a session that took the camera while
            // this request waited is seen here.
            let _op = loop_operation().await;
            if refused() {
                return Err(LOOP_BUSY.to_string());
            }
            match action {
                LoopAction::Status => Ok(()),
                LoopAction::Stop => self.stop_locked().await.map_err(|e| e.to_string()),
                LoopAction::Start => {
                    if !self.host.camera_active().await {
                        return Err(LOOP_CAMERA_OFF.to_string());
                    }
                    self.start_locked().await.map_err(|e| e.to_string())
                }
            }
        })
    }

    fn play_file(&self, request: PlayRequest) -> BoxFuture<'_, Result<Played, PlayError>> {
        Box::pin(async move {
            // Waiting for the lock is cancellable too (a live session's handoff can hold it
            // through a system prompt).
            let cancel = request.cancel.clone();
            let _op = tokio::select! {
                op = loop_operation() => op,
                _ = cancel.cancelled() => return Err(PlayError::Cancelled),
            };
            self.play_locked(request).await
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

/// Tests that drive the process-wide loop engine (or the flag it obeys) take this.
#[cfg(test)]
pub(crate) static ENGINE_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Unix only: the test reaches its data directory through a symlink.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::remote::stream::file_receive::{FileDeps, PhoneContainer, PhoneVideoStore};
    use crate::remote::stream::runner::Cancel;
    use crate::remote::stream::signal::FileOffer;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// A host without a window or a camera: the loop plays preview-only, as it does in the
    /// app while the virtual camera is off.
    #[derive(Default)]
    struct TestHost {
        camera_on: AtomicBool,
        events: Mutex<Vec<String>>,
    }

    impl TestHost {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    impl LoopHost for TestHost {
        fn ensure_camera(&self) -> BoxFuture<'_, Result<()>> {
            Box::pin(async {
                self.events.lock().unwrap().push("ensure_camera".into());
                self.camera_on.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
        fn camera_active(&self) -> BoxFuture<'_, bool> {
            Box::pin(async { self.camera_on.load(Ordering::SeqCst) })
        }
        fn loop_changed(&self, playing: bool) {
            self.events.lock().unwrap().push(format!("loop_changed {playing}"));
        }
        fn video_loaded(&self, video: &VideoInfo, playing: bool) {
            self.events.lock().unwrap().push(format!("video_loaded {} {}x{} {playing}", video.filename, video.width, video.height));
        }
    }

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Put `bytes` into the store the way a transfer does.
    async fn stored(store: &PhoneVideoStore, bytes: &[u8]) -> PathBuf {
        let offer = FileOffer { name: "clip.mp4".into(), size: bytes.len() as u64, mime: "video/mp4".into() };
        let mut part = store.create(&offer).await.unwrap();
        part.write(bytes).await.unwrap();
        part.commit("mp4").await.unwrap().0
    }

    fn request(path: &std::path::Path, name: &str) -> (crate::remote::stream::runner::CancelHandle, PlayRequest) {
        let (handle, cancel) = Cancel::new();
        (handle, PlayRequest { path: path.to_path_buf(), display_name: name.into(), container: PhoneContainer::IsoBmff, cancel })
    }

    /// The real loader and the real playback (ffmpeg), without a window: one test, because
    /// the engine is one per process. Covers what the fake loop of the session tests cannot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_real_engine_plays_a_phone_video_and_keeps_what_it_had_when_it_cannot() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let _engine = ENGINE_TEST.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        // The app's data directory, known through a symlink (so a canonicalised path would
        // be spelled differently).
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("data");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let store = PhoneVideoStore::in_app_data(&link);

        let fixture = tmp.path().join("fixture.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=1"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&fixture)
            .status()
            .unwrap();
        assert!(made.success(), "ffmpeg could not build the fixture");
        let video = std::fs::read(&fixture).unwrap();

        let host = Arc::new(TestHost::default());
        let engine = Arc::new(EngineLoop::new(host.clone()));
        let files = FileDeps::new(store.clone(), engine.clone());
        assert!(!engine.state().await.playing);

        // --- a good video: loaded, playing, and still the store's file afterwards (M6) ----
        let first = stored(&store, &video).await;
        let older = stored(&store, b"an earlier phone video").await;
        let (_c, req) = request(&first, "First clip.mp4");
        let played = engine.play_file(req).await.expect("a real video plays");
        let Played::Looping { path: loaded } = played else { panic!("{played:?}") };
        assert_eq!(host.take(), vec!["ensure_camera", "video_loaded First clip.mp4 320x240 true"]);
        let state = engine.state().await;
        assert!(state.playing);
        assert_eq!(state.name.as_deref(), Some("First clip.mp4"));
        assert!(store.contains(&loaded) && store.contains(state.path.as_deref().unwrap()), "fromPhone");
        assert_eq!(files.sweep_unused_keeping(Some(&loaded)).await, 1, "only the older video is swept");
        assert!(first.exists() && !older.exists(), "the looping video survives the sweep");

        // --- a file the loader cannot read: nothing is touched (M3) -----------------------
        let broken = stored(&store, b"\0\0\0\x18ftypisom\0\0\x02\0isommp41 and then nothing a video has").await;
        let (_c, req) = request(&broken, "Broken.mp4");
        assert!(matches!(engine.play_file(req).await, Err(PlayError::Failed(_))));
        let state = engine.state().await;
        assert!(state.playing, "the first video was never stopped");
        assert_eq!(state.name.as_deref(), Some("First clip.mp4"));
        assert!(host.take().is_empty(), "the window was told nothing, because nothing changed");
        assert_eq!(files.sweep_unused().await, 1, "the unreadable file goes");
        assert!(first.exists());

        // --- cancelled before it started (H1): the same ----------------------------------
        let second = stored(&store, &video).await;
        let (cancel, req) = request(&second, "Second clip.mp4");
        cancel.cancel();
        assert!(matches!(engine.play_file(req).await, Err(PlayError::Cancelled)));
        assert_eq!(engine.state().await.name.as_deref(), Some("First clip.mp4"));
        assert!(engine.state().await.playing);
        assert!(host.take().is_empty());

        // --- loop_control (M1, M2) --------------------------------------------------------
        let (idle, busy) = (|| false, || true);
        assert_eq!(engine.control(LoopAction::Stop, &busy).await, Err(LOOP_BUSY.to_string()));
        assert!(engine.state().await.playing, "refused under the lock: nothing was stopped");
        assert_eq!(engine.control(LoopAction::Stop, &idle).await, Ok(()));
        assert!(!engine.state().await.playing);
        assert_eq!(host.take(), vec!["loop_changed false"]);
        // The camera went off meanwhile: the phone cannot turn it on.
        host.camera_on.store(false, Ordering::SeqCst);
        assert_eq!(engine.control(LoopAction::Start, &idle).await, Err(LOOP_CAMERA_OFF.to_string()));
        assert!(engine.start().await.is_err());
        assert!(!engine.state().await.playing);
        assert!(host.take().is_empty(), "no ensure_camera, no event");
        host.camera_on.store(true, Ordering::SeqCst);
        assert_eq!(engine.control(LoopAction::Start, &idle).await, Ok(()));
        assert!(engine.state().await.playing);
        assert_eq!(host.take(), vec!["loop_changed true"]);

        // --- a second good video replaces the first ---------------------------------------
        let (_c, req) = request(&second, "Second clip.mp4");
        assert!(matches!(engine.play_file(req).await, Ok(Played::Looping { .. })));
        assert_eq!(engine.state().await.name.as_deref(), Some("Second clip.mp4"));
        assert_eq!(files.sweep_unused().await, 1);
        assert!(second.exists() && !first.exists());

        assert!(stop_and_wait(Duration::from_secs(10)).await, "playback stops");
    }

    /// Record mode's last step with nothing faked: real H.264 teed into a take, remuxed by
    /// ffmpeg, and loaded and looped by the real engine (natural motion, the default) —
    /// which keeps playing with no session and no phone anywhere.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_real_engine_loops_a_recorded_take() {
        use crate::remote::stream::record::{self, testing::h264_frames, Take, TakeStep};
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let _engine = ENGINE_TEST.lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let store = PhoneVideoStore::in_app_data(tmp.path());
        // Three seconds of take.
        let mut part = record::create_take_file(&store).await.unwrap();
        let mut take = Take::new();
        for f in h264_frames(90, Duration::from_micros(33_333), std::time::Instant::now()) {
            if let TakeStep::Write(bytes) = take.push(&f) {
                part.write(&bytes).await.unwrap();
            }
        }
        let summary = take.verdict().expect("a good take");
        let recorded = record::finalize(std::path::Path::new("ffmpeg"), &store, part, summary).await.expect("remux");

        let host = Arc::new(TestHost::default());
        let engine = Arc::new(EngineLoop::new(host.clone()));
        let files = FileDeps::new(store.clone(), engine.clone());
        let (_cancel, cancel) = Cancel::new();
        let request = PlayRequest { path: recorded.path.clone(), display_name: recorded.display_name.clone(), container: recorded.container, cancel };
        let played = engine.play_file(request).await.expect("the take plays");
        let Played::Looping { path: loaded } = played else { panic!("{played:?}") };
        assert_eq!(host.take(), vec!["ensure_camera", "video_loaded Phone recording.mp4 640x360 true"]);
        drop(recorded.hold);
        // The session's sweep keeps the looping take; the loop is the engine's own now.
        assert_eq!(files.sweep_unused_keeping(Some(&loaded)).await, 0);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let state = engine.state().await;
        assert!(state.playing, "still looping");
        assert_eq!(state.name.as_deref(), Some("Phone recording.mp4"));
        assert!(store.contains(state.path.as_deref().unwrap()) && loaded.exists(), "fromPhone, and on disk");
        let playback = playback().await;
        assert!(playback.playing, "{playback:?}");

        assert!(stop_and_wait(Duration::from_secs(10)).await, "playback stops");
    }
}
