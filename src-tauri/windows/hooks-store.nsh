; CamLooper NSIS installer hooks — Microsoft Store (per-user) build.
;
; This variant is a `currentUser` install (RequestExecutionLevel user) so it can install
; silently and non-elevated, which is required to pass Store certification policies
; 10.2.9.2 (silent install) and 10.3.4 (installer can write its files without admin).
;
; Because a per-user install cannot elevate, it must NOT run the `regsvr32` that the
; perMachine build does in windows/hooks.nsh — HKLM registration of the softcam
; DirectShow filter requires admin. Instead the softcam driver is registered on first
; use from inside the running app (one UAC prompt), via the `ensure_softcam_registered`
; command in src/softcam_register.rs.
;
; Tauri's generated installer.nsi guards every hook with `!ifmacrodef`, so an undefined
; NSIS_HOOK_* macro simply skips that step.

!macro NSIS_HOOK_POSTINSTALL
  ; Thank-you page, mirroring windows/hooks.nsh. Note this is a no-op in the actual Store
  ; flow: the Store runs the installer with /S, and policy 10.2.9.2 requires that silent
  ; install stay silent, so IfSilent skips it. It only fires if someone runs this build's
  ; .exe by hand. Do NOT remove the IfSilent guard — launching a browser during a silent
  ; install is exactly what 10.2.9.2 prohibits.
  IfSilent +2
  Exec '"$WINDIR\explorer.exe" "https://camlooper.com/thank-you?ch=store&lcid=$LANGUAGE"'
!macroend
