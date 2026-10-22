; Hooks included by Tauri's NSIS installer template.

!define AUTOSTART_TASK "\clipkeep-clipboard"
!define AUTOSTART_MARKER "autostart.pending"

; First install on this machine: ask the app to enable autostart on its next launch. The elevated
; scheduled task cannot be created from here because a per-user installer is not elevated.
; Updates and reinstalls keep whatever the user chose. The history database is created on the
; first launch; settings.json only appears once a setting is changed, so it can't tell.
!macro NSIS_HOOK_POSTINSTALL
  ${If} $UpdateMode <> 1
  ${AndIfNot} ${FileExists} "$APPDATA\${BUNDLEID}\clipkeep.db"
    FileOpen $0 "$INSTDIR\${AUTOSTART_MARKER}" w
    FileClose $0
  ${EndIf}
!macroend

; A real uninstall runs from a copy in %TEMP%. The reinstall page runs it in place (`_?=`) before
; installing the new version, and the user's autostart must survive that.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
  ${AndIf} $EXEDIR != $INSTDIR
    nsExec::Exec 'schtasks.exe /Delete /TN "${AUTOSTART_TASK}" /F'
    Pop $0
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${EndIf}
  Delete "$INSTDIR\${AUTOSTART_MARKER}"
!macroend
