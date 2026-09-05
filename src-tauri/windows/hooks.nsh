; CamLooper NSIS installer hooks.
;
; Register / unregister the bundled softcam DirectShow virtual-camera driver so the
; "CamLooper Virtual Camera" appears in Zoom / Teams / OBS. softcam.dll exports the
; standard COM DllRegisterServer/DllUnregisterServer, so regsvr32 records the driver
; at its installed absolute path. The installer runs elevated (perMachine).

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering CamLooper virtual camera driver (softcam)..."
  ExecWait 'regsvr32 /s "$INSTDIR\softcam.dll"'

  ; Open the thank-you page as the logged-in user. This installer is elevated, so ExecShell
  ; here would launch the browser as Administrator; going through the already-running
  ; desktop shell hands the URL back to the normal user. $LANGUAGE is the LCID picked in
  ; the language dialog — thank-you.html maps it, so fixing a mapping is a site deploy
  ; rather than a new installer. Skipped under /S so automated deployments stay silent.
  IfSilent +2
  Exec '"$WINDIR\explorer.exe" "https://camlooper.com/thank-you?ch=direct&lcid=$LANGUAGE"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Unregistering CamLooper virtual camera driver (softcam)..."
  ExecWait 'regsvr32 /s /u "$INSTDIR\softcam.dll"'
!macroend
