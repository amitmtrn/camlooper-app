//! Where a meeting session's media comes from, per platform. Each platform is one module
//! that implements [`super::meeting::MeetingSource`]; the session itself
//! ([`super::meeting`]) and the encoders are shared.
//!
//! * Linux ([`linux`]): the PulseAudio / PipeWire monitor of the default output device, and
//!   the xdg-desktop-portal ScreenCast picker for a window or screen.
//! * Windows (Windows.Graphics.Capture + WASAPI loopback) and macOS (ScreenCaptureKit) are
//!   not written yet; without a source the desktop does not offer `meeting`.
#[cfg(target_os = "linux")]
pub mod linux;
