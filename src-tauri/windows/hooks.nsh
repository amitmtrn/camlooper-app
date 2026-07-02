; CamLooper NSIS installer hooks.
;
; Register / unregister the bundled softcam DirectShow virtual-camera driver so the
; "CamLooper Virtual Camera" appears in Zoom / Teams / OBS. softcam.dll exports the
; standard COM DllRegisterServer/DllUnregisterServer, so regsvr32 records the driver
; at its installed absolute path. The installer runs elevated (perMachine).

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering CamLooper virtual camera driver (softcam)..."
  ExecWait 'regsvr32 /s "$INSTDIR\softcam.dll"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Unregistering CamLooper virtual camera driver (softcam)..."
  ExecWait 'regsvr32 /s /u "$INSTDIR\softcam.dll"'
!macroend
