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

use super::runner::{CameraControl, CameraTarget, Cancel};
use crate::virtual_camera::{self, VirtualCameraConfig};

static PHONE_OWNS_CAMERA: AtomicBool = AtomicBool::new(false);

/// True while a phone session holds the camera; video playback refuses to start meanwhile.
pub fn phone_owns_camera() -> bool {
    PHONE_OWNS_CAMERA.load(Ordering::SeqCst)
}

#[derive(Default, Debug, PartialEq, Eq)]
struct Undo {
    /// Playback was running and we paused it.
    resume_playback: bool,
    /// The camera was off and we started it.
    stop_camera: bool,
    claimed: bool,
    /// The session that holds the claim.
    epoch: u64,
}

impl Undo {
    /// Take the camera for `epoch`. Refused while another session's claim has not been
    /// released (it was given up on by the stop watchdog and is still shutting down);
    /// nothing is changed then, so that session's own release still undoes what it did.
    fn claim(&mut self, epoch: u64) -> Result<()> {
        if self.claimed {
            return Err(anyhow!("the previous phone session is still shutting down"));
        }
        *self = Undo { claimed: true, epoch, ..Undo::default() };
        Ok(())
    }

    /// What `epoch`'s release has to undo: everything if it holds the claim, nothing if
    /// another session does — a session whose acquire was refused must not hand back (or
    /// stop) a camera it never had.
    fn take_for(&mut self, epoch: u64) -> Option<Undo> {
        (self.claimed && self.epoch == epoch).then(|| std::mem::take(self))
    }
}

/// How many early releases are remembered; see [`Claims::released_early`].
const RELEASED_EARLY_KEPT: usize = 32;

/// The claim on the camera, plus the sessions that were released before they claimed it.
#[derive(Default, Debug)]
struct Claims {
    undo: Undo,
    /// Sessions whose release found nothing of theirs to undo. Usually that is a session
    /// whose acquire was refused. It can also be one that ended (a panic, say) before its
    /// acquire — which runs in a task of its own — got as far as claiming: that acquire is
    /// still on its way, and nobody is left to release what it would claim.
    released_early: std::collections::VecDeque<u64>,
}

impl Claims {
    /// [`Undo::claim`], refused as well for a session that was already released.
    fn claim(&mut self, epoch: u64) -> Result<()> {
        if let Some(i) = self.released_early.iter().position(|e| *e == epoch) {
            self.released_early.remove(i);
            return Err(anyhow!("the session ended before it took the camera"));
        }
        self.undo.claim(epoch)
    }

    /// [`Undo::take_for`]; a release with nothing to undo is remembered.
    fn release(&mut self, epoch: u64) -> Option<Undo> {
        let taken = self.undo.take_for(epoch);
        if taken.is_none() && !self.released_early.contains(&epoch) {
            if self.released_early.len() == RELEASED_EARLY_KEPT {
                self.released_early.pop_front();
            }
            self.released_early.push_back(epoch);
        }
        taken
    }
}

/// Clears the process-wide claim when dropped, so it is cleared even if stopping the camera
/// panics: with the undo state already taken, nothing else would ever clear it, and video
/// playback would be refused until the app is restarted.
struct ClaimGuard;

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        PHONE_OWNS_CAMERA.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
pub struct AppCamera {
    claims: Mutex<Claims>,
}

impl AppCamera {
    async fn acquire_inner(&self, epoch: u64, cancel: Cancel) -> Result<CameraTarget> {
        let mut claims = self.claims.lock().await;
        claims.claim(epoch)?;
        let undo = &mut claims.undo;
        // Claim first, look second: the in-app camera checks the claim under its own lock
        // before it starts (camera_capture::start_capture), so either it sees the claim and
        // refuses, or it started before and is seen here. Checking first would leave a window
        // for a recording to start in between.
        PHONE_OWNS_CAMERA.store(true, Ordering::SeqCst);
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
            // (The same steps as `ensure_virtual_camera`, spelled out here because each is
            // noted in `undo` and the wait can be cancelled.)
            // The step that can wait on the user (polkit / UAC). When the session ends
            // meanwhile, do not sit in it: dropping the Linux future kills pkexec, which
            // closes the prompt. The Windows registration runs on a blocking thread and its
            // UAC prompt cannot be closed from here; it is left to finish on its own.
            tokio::select! {
                r = prepare_platform_camera() => r?,
                _ = cancel.cancelled() => return Err(anyhow!("the session ended while the camera was being set up")),
            }
            if cancel.is_cancelled() {
                return Err(anyhow!("the session ended while the camera was being set up"));
            }
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
    fn acquire(&self, epoch: u64, cancel: Cancel) -> BoxFuture<'_, Result<CameraTarget>> {
        Box::pin(self.acquire_inner(epoch, cancel))
    }

    fn release(&self, epoch: u64) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Waits for an acquire that is still running (it holds the lock throughout).
            let mut claims = self.claims.lock().await;
            let Some(u) = claims.release(epoch) else {
                if claims.undo.claimed {
                    eprintln!("phone stream: session {epoch} never had the camera (session {} holds it); nothing to hand back", claims.undo.epoch);
                }
                return;
            };
            let claim = ClaimGuard;
            if u.stop_camera && virtual_camera::get_virtual_camera_status().await.is_active {
                if let Err(e) = virtual_camera::stop_virtual_camera().await {
                    eprintln!("phone stream: could not stop the virtual camera: {e}");
                }
            }
            // Before resuming: playback refuses to start while the phone owns the camera.
            drop(claim);
            if u.resume_playback && crate::video_processor::has_video_loaded().await {
                if let Err(e) = crate::video_processor::start_video_stream().await {
                    eprintln!("phone stream: could not resume the video loop: {e}");
                }
            }
        })
    }
}

/// Make sure the virtual camera is running, starting it the way the UI does (driver or
/// kernel module first — at most one system prompt). For a loop that is started from the
/// phone: the window may be hidden in the tray, so nothing else would turn the camera on.
/// Returns whether it had to be started.
pub(super) async fn ensure_virtual_camera() -> Result<bool> {
    if virtual_camera::get_virtual_camera_status().await.is_active {
        return Ok(false);
    }
    prepare_platform_camera().await?;
    let (width, height) = virtual_camera::output_resolution();
    let config = VirtualCameraConfig { width, height, fps: 30, ..VirtualCameraConfig::default() };
    virtual_camera::start_virtual_camera_with(Some(config), None).await?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_only_undoes_its_own_sessions_claim() {
        let mut undo = Undo::default();
        // S1 acquires (started the camera, paused playback), is given up on by the watchdog.
        undo.claim(1).unwrap();
        undo.stop_camera = true;
        undo.resume_playback = true;

        // S2's acquire is refused and changes nothing; its release finds nothing of its own.
        assert!(undo.claim(2).is_err());
        assert_eq!(undo.take_for(2), None);
        assert_eq!(undo, Undo { resume_playback: true, stop_camera: true, claimed: true, epoch: 1 });

        // S1's late release still undoes everything it did — once.
        let u = undo.take_for(1).expect("S1 holds the claim");
        assert!(u.stop_camera && u.resume_playback);
        assert_eq!(undo, Undo::default());
        assert_eq!(undo.take_for(1), None);

        // S3 acquires; a stray second release from S1 or S2 cannot take the camera from it.
        undo.claim(3).unwrap();
        undo.stop_camera = true;
        assert_eq!(undo.take_for(1), None);
        assert_eq!(undo.take_for(2), None);
        assert!(undo.claimed && undo.stop_camera && undo.epoch == 3);
        assert!(undo.take_for(3).is_some());
    }

    #[test]
    fn a_session_released_before_its_acquire_ran_cannot_claim_afterwards() {
        let mut claims = Claims::default();
        // S1 panicked before its prepare task reached the claim; the guard released it.
        assert_eq!(claims.release(1), None);
        // The late acquire is refused: nobody would ever release what it claims.
        assert!(claims.claim(1).is_err());
        assert!(!claims.undo.claimed);
        // The next session is not affected, and its normal release is not remembered.
        claims.claim(2).unwrap();
        assert!(claims.release(2).is_some());
        assert!(claims.released_early.is_empty());
    }

    #[test]
    fn early_releases_are_remembered_within_bounds() {
        let mut claims = Claims::default();
        claims.claim(1).unwrap();
        // Sessions refused while S1 holds the camera: each release finds nothing.
        for epoch in 2..(2 + 2 * RELEASED_EARLY_KEPT as u64) {
            assert!(claims.claim(epoch).is_err());
            assert_eq!(claims.release(epoch), None);
            assert_eq!(claims.release(epoch), None);
        }
        assert_eq!(claims.released_early.len(), RELEASED_EARLY_KEPT);
        // S1 still holds its claim throughout.
        assert!(claims.release(1).is_some());
    }

    #[test]
    fn nothing_claimed_means_nothing_to_undo() {
        // Epoch 0 is the default; an unclaimed state must not match it.
        assert_eq!(Undo::default().take_for(0), None);
    }

    #[test]
    fn the_claim_is_cleared_even_when_the_release_unwinds() {
        // The only test that touches the process-wide flag.
        PHONE_OWNS_CAMERA.store(true, Ordering::SeqCst);
        let r = std::panic::catch_unwind(|| {
            let _claim = ClaimGuard;
            panic!("boom (expected in this test)");
        });
        assert!(r.is_err());
        assert!(!phone_owns_camera());
    }
}
