Unicode true
Name "HiMind Agent"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\HiMindAgent"
RequestExecutionLevel user
SetCompressor /SOLID lzma

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Section "Agent"
  nsExec::ExecToLog 'taskkill /IM himind-agent.exe /T /F'
  SetOutPath "$INSTDIR"
  File "${RELEASE}\himind-agent-launcher.exe"
  File "${RELEASE}\himind-agent-updater.exe"
  SetOutPath "$INSTDIR\current"
  File "${RELEASE}\himind-agent.exe"
  SetOutPath "$INSTDIR\data"
  SetOutPath "$INSTDIR\previous"
  SetOutPath "$INSTDIR\logs"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\HiMindAgent" "DisplayName" "HiMind Agent"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\HiMindAgent" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "HiMindAgent" '"$INSTDIR\himind-agent-launcher.exe" --api http://localhost:18081 --local-app --local-port 18181'
  CreateShortcut "$DESKTOP\HiMind Agent.lnk" "$INSTDIR\himind-agent-launcher.exe" "--api http://localhost:18081 --local-app --local-port 18181" "$INSTDIR\himind-agent-launcher.exe"
SectionEnd

Section "Uninstall"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "HiMindAgent"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\HiMindAgent"
  Delete "$DESKTOP\HiMind Agent.lnk"
  RMDir /r "$INSTDIR\current"
  RMDir /r "$INSTDIR\previous"
  RMDir /r "$INSTDIR\logs"
  Delete "$INSTDIR\himind-agent-launcher.exe"
  Delete "$INSTDIR\himind-agent-updater.exe"
  Delete "$INSTDIR\uninstall.exe"
SectionEnd