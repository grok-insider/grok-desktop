!if "${APP_FILENAME}" != "grok-desktop"
  !error "Grok Desktop NSIS APP_FILENAME must remain grok-desktop"
!endif
!if "${APP_PACKAGE_NAME}" != "grok-desktop"
  !error "Grok Desktop NSIS APP_PACKAGE_NAME must remain grok-desktop"
!endif

!macro customInstall
  WriteRegStr HKCU "Software\Classes\grok-desktop" "" "URL:Grok Desktop Protocol"
  WriteRegStr HKCU "Software\Classes\grok-desktop" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\grok-desktop\DefaultIcon" "" '"$INSTDIR\${APP_EXECUTABLE_FILENAME}",0'
  WriteRegStr HKCU "Software\Classes\grok-desktop\shell\open\command" "" '"$INSTDIR\${APP_EXECUTABLE_FILENAME}" "%1"'
!macroend

!macro customUnInstall
  ReadRegStr $R0 HKCU "Software\Classes\grok-desktop\shell\open\command" ""
  StrCmp $R0 '"$INSTDIR\${APP_EXECUTABLE_FILENAME}" "%1"' 0 +2
  DeleteRegKey HKCU "Software\Classes\grok-desktop"
!macroend
