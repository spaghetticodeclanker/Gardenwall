; Gardenwall's installer hooks (release plan feature 3).
; Included by Tauri's NSIS template before its pages are defined.

; The uninstall confirm page says where the data is. The checkbox under it
; ("Also delete my chats, settings and models", see English.nsh) is unticked
; by default, so the data stays unless the user asks. Updates never show it.
; The text area holds about three lines (a longer text was cut off on a
; clean Windows Sandbox, 2026-11-24): the install folder is already in the
; "Uninstalling from" box, so this names only the data folder, on its own
; line so a long user name can wrap without hiding anything.
!define MUI_UNCONFIRMPAGE_TEXT_TOP "Your chats, agent sessions, settings and models are kept in this folder:$\r$\n$LOCALAPPDATA\ai.gardenwall.desktop"

!macro NSIS_HOOK_PREUNINSTALL
  ; Close Gardenwall first (the template's own check that follows then finds
  ; nothing), so no sandbox is still running. Then revoke any sandbox file
  ; permissions left on the user's folders by a session that didn't close
  ; cleanly. This runs whatever the data choice, before the data folder
  ; (with its permission records) can be deleted.
  !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  nsExec::Exec '"$INSTDIR\${MAINBINARYNAME}.exe" --revoke-sandbox-grants'
  Pop $0
!macroend
