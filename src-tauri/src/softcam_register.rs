//! On-demand, elevated registration of the bundled softcam DirectShow virtual-camera
//! driver (Windows only).
//!
//! Background: the perMachine ("direct download") installer registers `softcam.dll` at
//! install time via `regsvr32` in an elevated NSIS hook. The Microsoft Store build is a
//! per-user (`currentUser`) install that runs silently and non-elevated so it can pass
//! certification — it therefore cannot register the DirectShow filter at install time,
//! because that writes to HKLM and requires admin.
//!
//! This module fills that gap uniformly for BOTH builds: before the virtual camera is
//! first used, [`ensure_registered`] probes whether the softcam filter is registered and,
//! if not, runs `regsvr32` **elevated** (a single UAC prompt). On the perMachine build the
//! probe already finds it registered, so no prompt appears; on the Store build it registers
//! on demand.
//!
//! The softcam filter CLSID `{AEF3B972-5FA5-4647-9571-358EB472BC9E}` and friendly name
//! ("DirectShow Softcam") come from tshino/softcam v1.8.1 (`src/softcam/softcam.cpp`) and
//! were cross-verified against the bytes of the committed `drivers/windows/softcam.dll`.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// Result of an attempt to ensure the softcam driver is registered.
///
/// Serialized (camelCase) to the frontend so the UI can decide whether to proceed with
/// enabling the camera or to surface a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RegisterOutcome {
    /// The filter was already registered (typical for the perMachine install).
    AlreadyRegistered,
    /// The filter was not registered and we just registered it (one UAC prompt accepted).
    JustRegistered,
    /// The user dismissed the UAC elevation prompt.
    Declined,
    /// Registration was attempted but failed (regsvr32 error, missing DLL, etc.).
    Failed,
}

/// Returns true if the softcam DirectShow filter is registered on this machine/user.
///
/// `HKEY_CLASSES_ROOT` is a merged view of `HKLM\Software\Classes` and
/// `HKCU\Software\Classes`, so this detects both a per-machine (installer) registration
/// and a per-user (in-app) one. The app is 64-bit, so it reads the 64-bit registry view
/// where the x64 softcam.dll registers — no WOW64 flag needed.
pub fn is_registered() -> bool {
    let subkey = w!("CLSID\\{AEF3B972-5FA5-4647-9571-358EB472BC9E}\\InprocServer32");
    unsafe {
        let mut hkey = HKEY::default();
        let status = RegOpenKeyExW(HKEY_CLASSES_ROOT, subkey, 0, KEY_READ, &mut hkey);
        if status == ERROR_SUCCESS {
            let _ = RegCloseKey(hkey);
            true
        } else {
            false
        }
    }
}

/// Absolute path to the bundled softcam.dll, which the installer places next to the exe
/// (the same primary path `windows_virtual_camera::load_softcam_dll` resolves).
fn softcam_dll_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dll = exe.parent()?.join("softcam.dll");
    if dll.exists() {
        Some(dll.to_string_lossy().into_owned())
    } else {
        None
    }
}

/// Full path to the 64-bit regsvr32, from a known system location (not PATH). Launching an
/// elevated process, we resolve it explicitly to avoid a PATH-hijack of the elevated child.
fn regsvr32_path() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    format!("{}\\System32\\regsvr32.exe", root)
}

/// Runs `regsvr32 /s "<softcam.dll>"` elevated via ShellExecuteEx (verb "runas"), waits for
/// it to finish, and maps the result. Produces exactly one UAC prompt.
fn register_elevated() -> RegisterOutcome {
    let dll_path = match softcam_dll_path() {
        Some(p) => p,
        None => return RegisterOutcome::Failed,
    };

    // Keep the wide strings alive for the whole call — the PCWSTRs borrow their buffers.
    let verb = HSTRING::from("runas");
    let file = HSTRING::from(regsvr32_path());
    let params = HSTRING::from(format!("/s \"{}\"", dll_path));

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    unsafe {
        match ShellExecuteExW(&mut info) {
            Ok(()) => {
                if info.hProcess.0.is_null() {
                    return RegisterOutcome::Failed;
                }
                let process = HANDLE(info.hProcess.0);
                WaitForSingleObject(process, INFINITE);
                let mut exit_code: u32 = 0;
                let got_code = GetExitCodeProcess(process, &mut exit_code).is_ok();
                let _ = CloseHandle(process);
                if got_code && exit_code == 0 {
                    RegisterOutcome::JustRegistered
                } else {
                    RegisterOutcome::Failed
                }
            }
            Err(e) => {
                // The user dismissing the UAC prompt surfaces as ERROR_CANCELLED (1223).
                if e.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) {
                    RegisterOutcome::Declined
                } else {
                    RegisterOutcome::Failed
                }
            }
        }
    }
}

/// Ensures the softcam driver is registered, elevating only if necessary. Safe to call on
/// every "enable virtual camera" action: it is a no-op (no prompt) once registered.
pub fn ensure_registered() -> RegisterOutcome {
    if is_registered() {
        return RegisterOutcome::AlreadyRegistered;
    }
    match register_elevated() {
        // regsvr32 can report success even when DllRegisterServer failed; re-probe to be sure.
        RegisterOutcome::JustRegistered => {
            if is_registered() {
                RegisterOutcome::JustRegistered
            } else {
                RegisterOutcome::Failed
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frontend string-matches these values (`src/App.tsx`: `outcome === 'declined'`,
    /// `outcome === 'justRegistered'`). Dropping the `rename_all` attribute or renaming a
    /// variant would compile fine and silently stop the UAC-declined warning from ever
    /// showing, so pin the wire format here.
    #[test]
    fn outcome_serializes_to_the_names_the_frontend_matches() {
        let json = |o: RegisterOutcome| serde_json::to_string(&o).unwrap();
        assert_eq!(json(RegisterOutcome::AlreadyRegistered), "\"alreadyRegistered\"");
        assert_eq!(json(RegisterOutcome::JustRegistered), "\"justRegistered\"");
        assert_eq!(json(RegisterOutcome::Declined), "\"declined\"");
        assert_eq!(json(RegisterOutcome::Failed), "\"failed\"");
    }

    /// regsvr32 is resolved from %SystemRoot% rather than PATH specifically so that a
    /// hijacked PATH cannot substitute a binary that we then launch *elevated*. Assert the
    /// path stays absolute and lands in System32.
    #[test]
    fn regsvr32_path_is_absolute_and_under_system32() {
        let path = regsvr32_path();
        assert!(
            path.to_ascii_lowercase().ends_with("\\system32\\regsvr32.exe"),
            "expected a System32-qualified regsvr32, got {path}"
        );
        assert!(
            path.contains(":\\"),
            "expected an absolute path so PATH lookup is never consulted, got {path}"
        );
    }

    /// Probing the registry must be side-effect free and safe to call before anything is
    /// installed -- `ensure_registered` calls it on every "enable camera" action.
    #[test]
    fn is_registered_probe_does_not_panic() {
        let _ = is_registered();
    }

    /// The CLSID in `is_registered` is only correct as long as the committed
    /// `drivers/windows/softcam.dll` is the build that exports it. Swapping the DLL for a
    /// softcam release with a different CLSID would leave the probe permanently reporting
    /// "not registered" -- producing a UAC prompt on every camera enable, forever.
    ///
    /// So verify against the artefact rather than restating the constant: a COM server
    /// embeds its CLSID as a little-endian GUID, which must appear in the DLL's bytes.
    #[test]
    fn softcam_clsid_matches_the_committed_driver() {
        // Little-endian layout of {AEF3B972-5FA5-4647-9571-358EB472BC9E}.
        const CLSID_LE: [u8; 16] = [
            0x72, 0xb9, 0xf3, 0xae, 0xa5, 0x5f, 0x47, 0x46,
            0x95, 0x71, 0x35, 0x8e, 0xb4, 0x72, 0xbc, 0x9e,
        ];

        let dll = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("drivers/windows/softcam.dll");
        let bytes = std::fs::read(&dll)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dll.display()));

        assert!(
            bytes.windows(16).any(|w| w == CLSID_LE),
            "the CLSID probed by is_registered() is absent from {} -- the bundled softcam \
             build does not export it, so registration detection would never succeed",
            dll.display()
        );
    }
}
