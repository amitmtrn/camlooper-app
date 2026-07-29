; CamLooper NSIS installer hooks — Microsoft Store (per-user) build.
;
; Intentionally EMPTY. This variant is a `currentUser` install (RequestExecutionLevel
; user) so it can install silently and non-elevated, which is required to pass Store
; certification policies 10.2.9.2 (silent install) and 10.3.4 (installer can write its
; files without admin).
;
; Because a per-user install cannot elevate, it must NOT run the `regsvr32` that the
; perMachine build does in windows/hooks.nsh — HKLM registration of the softcam
; DirectShow filter requires admin. Instead the softcam driver is registered on first
; use from inside the running app (one UAC prompt), via the `ensure_softcam_registered`
; command in src/softcam_register.rs.
;
; Tauri's generated installer.nsi guards every hook with `!ifmacrodef`, so leaving the
; NSIS_HOOK_* macros undefined here simply skips install-time registration.
