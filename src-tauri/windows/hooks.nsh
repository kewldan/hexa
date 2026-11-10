; «Открыть в Hexa» в контекстном меню Проводника для любого файла (UI-13).
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Classes\*\shell\Hexa" "" "Открыть в Hexa"
  WriteRegStr HKCU "Software\Classes\*\shell\Hexa" "Icon" "$INSTDIR\${MAINBINARYNAME}.exe"
  WriteRegStr HKCU "Software\Classes\*\shell\Hexa\command" "" '"$INSTDIR\${MAINBINARYNAME}.exe" "%1"'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCU "Software\Classes\*\shell\Hexa"
!macroend
