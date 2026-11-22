//! macOS virtual camera: the Rust face of `macos/camera_bridge.m`.
//!
//! The camera itself is a CMIO Camera Extension (`macos-extension/`), a separately signed
//! system extension that macOS runs on our behalf. This module asks macOS to activate the
//! copy embedded in the app bundle, then writes BGRA frames into the extension's sink
//! stream. Zoom, Meet and friends read the same frames from its source stream.

use std::ffi::{c_char, c_double, c_int, CStr, CString};
use std::time::{Duration, Instant};

/// The name camera pickers show. Must match `Shared.cameraName` in the extension.
pub const DEVICE_NAME: &str = "CamLooper Virtual Camera";

/// The one format the extension advertises (`Shared.width/height/frameRate`). The app
/// renders to this regardless of the Resolution setting, because a sink buffer that does
/// not match the device's declared format is not something consumers can be trusted with.
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;

// Status codes shared with camera_bridge.m.
const CL_OK: c_int = 0;
const CL_NEEDS_APPROVAL: c_int = 1;
const CL_DEVICE_NOT_FOUND: c_int = 5;
const CL_CAMERA_DENIED: c_int = 6;

/// How long to wait for macOS to answer an activation request. An already-approved
/// extension answers in well under a second; first-time approval is reported immediately
/// as "needs approval" rather than waited out.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);
/// After activation the device takes a moment to appear in CoreMediaIO.
const DEVICE_WAIT: Duration = Duration::from_secs(6);

extern "C" {
    fn camlooper_extension_needs_activation() -> c_int;
    fn camlooper_extension_activate(timeout_s: c_double, err: *mut c_char, errlen: usize) -> c_int;
    fn camlooper_extension_bundled() -> c_int;
    fn camlooper_open_extension_settings();
    fn camlooper_sink_open(
        name: *const c_char,
        width: c_int,
        height: c_int,
        fps: c_int,
        err: *mut c_char,
        errlen: usize,
    ) -> c_int;
    fn camlooper_sink_push(bgra: *const u8, len: usize) -> c_int;
    fn camlooper_sink_close();
}

const APPROVAL_HINT: &str = "Open System Settings > General > Login Items & Extensions > \
    Camera Extensions, switch on CamLooper, then start the camera again.";

/// What to tell the user when the sink could not be opened.
///
/// `activation` is the status of the activation request made just before, if one was made.
fn failure_message(open_status: c_int, activation: Option<c_int>, detail: &str) -> String {
    if activation == Some(CL_NEEDS_APPROVAL) {
        return format!("macOS needs your approval to add the CamLooper camera. {APPROVAL_HINT}");
    }
    match open_status {
        CL_CAMERA_DENIED => detail.to_string(),
        CL_DEVICE_NOT_FOUND if activation.is_none() => format!(
            "The CamLooper camera is not installed. It is added by the signed CamLooper app in \
             the Applications folder; this build cannot install it. If you have just installed \
             it: {APPROVAL_HINT}"
        ),
        CL_DEVICE_NOT_FOUND => format!(
            "The CamLooper camera did not appear after macOS accepted it. {APPROVAL_HINT}"
        ),
        _ => format!("Could not open the CamLooper camera: {detail}"),
    }
}

/// Where the camera extension stands, as the launch gate in the UI needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionState {
    /// Installed and approved for this app version.
    Ready,
    /// macOS is waiting for the user to switch it on in System Settings.
    NeedsApproval,
    /// This build carries no extension (`tauri dev`), so there is nothing to approve.
    Unavailable,
    /// macOS refused the request outright; approval would not help.
    Failed,
}

impl ExtensionState {
    /// The string the frontend switches on.
    pub fn as_str(self) -> &'static str {
        match self {
            ExtensionState::Ready => "ready",
            ExtensionState::NeedsApproval => "needsApproval",
            ExtensionState::Unavailable => "unavailable",
            ExtensionState::Failed => "failed",
        }
    }

    /// `bundled` / `needs_activation` describe the app bundle; `activation` is the status
    /// of an activation request, made only when one was needed.
    fn resolve(bundled: bool, needs_activation: bool, activation: impl FnOnce() -> c_int) -> Self {
        if !bundled {
            return ExtensionState::Unavailable;
        }
        if !needs_activation {
            return ExtensionState::Ready;
        }
        match activation() {
            CL_OK => ExtensionState::Ready,
            CL_NEEDS_APPROVAL => ExtensionState::NeedsApproval,
            _ => ExtensionState::Failed,
        }
    }
}

/// Make sure the extension is installed, asking macOS to install it if not. When macOS
/// wants the user's approval, System Settings is opened at the right pane. Blocks — call
/// from `spawn_blocking`.
pub fn ensure_extension() -> ExtensionState {
    // SAFETY: plain C calls with no pointers retained.
    let bundled = unsafe { camlooper_extension_bundled() } == 1;
    let needs_activation = unsafe { camlooper_extension_needs_activation() } == 1;
    let state = ExtensionState::resolve(bundled, needs_activation, || {
        let mut err = [0 as c_char; 512];
        let status = unsafe {
            camlooper_extension_activate(ACTIVATION_TIMEOUT.as_secs_f64(), err.as_mut_ptr(), err.len())
        };
        println!("Camera extension activation request: status {status} {}", read_error(&err));
        status
    });
    if state == ExtensionState::NeedsApproval {
        open_extension_settings();
    }
    state
}

/// Whether the extension bundled with this app has been approved and activated. Cheap:
/// safe to poll while the approval gate is on screen.
pub fn extension_approved() -> bool {
    // SAFETY: plain C calls.
    unsafe { camlooper_extension_bundled() == 1 && camlooper_extension_needs_activation() == 0 }
}

pub fn open_extension_settings() {
    // SAFETY: plain C call; the bridge hops to the main queue itself.
    unsafe { camlooper_open_extension_settings() }
}

fn read_error(buf: &[c_char]) -> String {
    // SAFETY: the bridge always NUL-terminates within the buffer (strlcpy), and the buffer
    // is zero-initialised for the case where it writes nothing.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// Activate the extension if this bundle's copy has not been activated yet, then open the
/// sink. Blocks — call from `spawn_blocking`, never from the main thread.
pub fn open() -> Result<(), String> {
    let mut err = [0 as c_char; 512];

    // SAFETY: plain C calls; `err` outlives each call and its length is passed alongside.
    let activation = if unsafe { camlooper_extension_needs_activation() } == 1 {
        let status = unsafe {
            camlooper_extension_activate(ACTIVATION_TIMEOUT.as_secs_f64(), err.as_mut_ptr(), err.len())
        };
        println!("Camera extension activation request: status {status} {}", read_error(&err));
        if status != CL_OK && status != CL_NEEDS_APPROVAL {
            return Err(format!(
                "macOS refused to install the CamLooper camera extension: {}",
                read_error(&err)
            ));
        }
        Some(status)
    } else {
        None
    };

    let name = CString::new(DEVICE_NAME).expect("device name has no NUL");
    let deadline = Instant::now() + DEVICE_WAIT;
    loop {
        err = [0 as c_char; 512];
        let status = unsafe {
            camlooper_sink_open(
                name.as_ptr(),
                WIDTH as c_int,
                HEIGHT as c_int,
                FPS as c_int,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        if status == CL_OK {
            return Ok(());
        }
        // Only a freshly activated extension is worth waiting for.
        let worth_waiting = status == CL_DEVICE_NOT_FOUND && activation == Some(CL_OK);
        if !worth_waiting || Instant::now() >= deadline {
            return Err(failure_message(status, activation, &read_error(&err)));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Queue one BGRA frame. A frame the extension has no room for is dropped, not an error.
pub fn push(frame: &[u8]) -> Result<(), String> {
    // SAFETY: the bridge reads exactly `len` bytes and copies them before returning.
    match unsafe { camlooper_sink_push(frame.as_ptr(), frame.len()) } {
        CL_OK => Ok(()),
        _ => Err("The CamLooper camera sink is not open, or the frame has the wrong size".into()),
    }
}

pub fn close() {
    // SAFETY: safe to call with no sink open.
    unsafe { camlooper_sink_close() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_approval_points_the_user_at_system_settings() {
        let msg = failure_message(CL_DEVICE_NOT_FOUND, Some(CL_NEEDS_APPROVAL), "");
        assert!(msg.contains("needs your approval"), "{msg}");
        assert!(msg.contains("Camera Extensions"), "{msg}");
    }

    #[test]
    fn a_build_without_the_extension_says_it_cannot_install_it() {
        let msg = failure_message(CL_DEVICE_NOT_FOUND, None, "");
        assert!(msg.contains("this build cannot install it"), "{msg}");
    }

    #[test]
    fn a_camera_permission_denial_is_passed_through_verbatim() {
        let msg = failure_message(CL_CAMERA_DENIED, None, "CamLooper is not allowed to use cameras.");
        assert_eq!(msg, "CamLooper is not allowed to use cameras.");
    }

    #[test]
    fn a_build_without_the_extension_never_asks_macos_for_anything() {
        let state = ExtensionState::resolve(false, true, || panic!("must not activate"));
        assert_eq!(state, ExtensionState::Unavailable);
    }

    #[test]
    fn an_already_activated_extension_is_ready_without_a_new_request() {
        let state = ExtensionState::resolve(true, false, || panic!("must not activate"));
        assert_eq!(state, ExtensionState::Ready);
    }

    #[test]
    fn activation_statuses_map_to_the_states_the_gate_switches_on() {
        assert_eq!(ExtensionState::resolve(true, true, || CL_OK).as_str(), "ready");
        assert_eq!(ExtensionState::resolve(true, true, || CL_NEEDS_APPROVAL).as_str(), "needsApproval");
        assert_eq!(ExtensionState::resolve(true, true, || 2).as_str(), "failed");
        assert_eq!(ExtensionState::resolve(true, true, || 3).as_str(), "failed");
    }

    #[test]
    fn pushing_with_no_sink_open_is_an_error_not_a_crash() {
        close();
        assert!(push(&[0u8; 16]).is_err());
    }

    /// End to end against a real, approved extension: push a known picture into the sink
    /// for a few seconds so a reader can check what comes out of the camera.
    ///
    ///   cargo test --lib -- --ignored feeds_the_installed_extension
    ///   ffmpeg -f avfoundation -framerate 30 -video_size 1280x720 \
    ///       -i "CamLooper Virtual Camera:none" -frames:v 20 -update 1 out.png
    ///
    /// Expect: left half red, right half blue, a white band across the top.
    #[test]
    #[ignore = "needs the CamLooper camera extension installed and approved"]
    fn feeds_the_installed_extension() {
        open().expect("open the sink");
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let mut frame = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let px: [u8; 4] = if y < 60 {
                    [255, 255, 255, 255]
                } else if x < w / 2 {
                    [0, 0, 255, 255] // red, as BGRA
                } else {
                    [255, 0, 0, 255] // blue, as BGRA
                };
                frame[(y * w + x) * 4..][..4].copy_from_slice(&px);
            }
        }
        for _ in 0..(FPS * 6) {
            push(&frame).expect("push a frame");
            std::thread::sleep(Duration::from_millis(1000 / FPS as u64));
        }
        close();
    }

    #[test]
    fn the_frame_size_matches_what_the_pipeline_writes_for_bgra() {
        let geom = crate::pipeline::Geometry { width: WIDTH, height: HEIGHT, fps: FPS };
        assert_eq!(
            crate::pipeline::CameraSink::RawBgraStdout.frame_bytes(&geom),
            Some((WIDTH * HEIGHT * 4) as usize)
        );
    }
}
