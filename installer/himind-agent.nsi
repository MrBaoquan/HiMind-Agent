Unicode true

!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"

!ifndef API_BASE
  !define API_BASE "https://himind.andcrane.com"
!endif
!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef PRODUCT_VERSION
  !define PRODUCT_VERSION "${VERSION}"
!endif
!ifndef ASSET_DIR
  !define ASSET_DIR "generated"
!endif
!ifndef VSCODE_EXTENSION_VSIX
  !error "VSCODE_EXTENSION_VSIX is required"
!endif

!define PRODUCT_NAME "HiMind Agent"
!define PRODUCT_PUBLISHER "HiMind"
!define PRODUCT_REGISTRY_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\HiMindAgent"
!define PRODUCT_RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define PRODUCT_PROTOCOL_KEY "Software\Classes\himind-agent"
!define PRODUCT_LAUNCH_ARGS '--api ${API_BASE} --local-app --local-port 18181'

Name "${PRODUCT_NAME}"
Caption "${PRODUCT_NAME} 安装程序"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\HiMindAgent"
InstallDirRegKey HKCU "${PRODUCT_REGISTRY_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
SetCompressorDictSize 32
ManifestDPIAware true
ManifestSupportedOS all
XPStyle on
BrandingText "HiMind  ·  Agent ${PRODUCT_VERSION}"
ShowInstDetails nevershow
ShowUninstDetails nevershow
Icon "${ASSET_DIR}\himind-agent.ico"
UninstallIcon "${ASSET_DIR}\himind-agent.ico"

!define MUI_ICON "${ASSET_DIR}\himind-agent.ico"
!define MUI_UNICON "${ASSET_DIR}\himind-agent.ico"
!define MUI_ABORTWARNING
!define MUI_UNABORTWARNING
!define MUI_BGCOLOR "FFFFFF"
!define MUI_TEXTCOLOR "102033"
!define MUI_FONT "Microsoft YaHei UI"
!define MUI_FONTSIZE 9

!define MUI_WELCOMEFINISHPAGE_BITMAP "${ASSET_DIR}\installer-welcome.bmp"
!define MUI_WELCOMEPAGE_TITLE "欢迎安装 HiMind Agent"
!define MUI_WELCOMEPAGE_TEXT "HiMind Agent 将安装到当前 Windows 用户，用于连接 HiMind 工作台与本机能力。$\r$\n$\r$\n安装无需管理员权限，通常只需几秒。现有版本会被安全替换，本机配置保持不变。"
!insertmacro MUI_PAGE_WELCOME

!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "${ASSET_DIR}\installer-header.bmp"
!define MUI_HEADERIMAGE_BITMAP_STRETCH FitControl
!define MUI_HEADERIMAGE_UNBITMAP "${ASSET_DIR}\installer-header.bmp"
!define MUI_HEADERIMAGE_UNBITMAP_STRETCH FitControl
Page custom OptionsPage OptionsPageLeave

!define MUI_INSTFILESPAGE_FINISHHEADER_TEXT "核心组件已安装"
!define MUI_INSTFILESPAGE_FINISHHEADER_SUBTEXT "正在完成本机注册与启动配置。"
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_TITLE "HiMind Agent 已准备就绪"
!define MUI_FINISHPAGE_TEXT "安装已完成。启动后，HiMind Agent 会在系统托盘中保持运行，并自动连接工作台。"
!define MUI_FINISHPAGE_RUN "$INSTDIR\himind-agent-launcher.exe"
!define MUI_FINISHPAGE_RUN_PARAMETERS "${PRODUCT_LAUNCH_ARGS}"
!define MUI_FINISHPAGE_RUN_TEXT "立即启动 HiMind Agent"
!define MUI_FINISHPAGE_NOAUTOCLOSE
!insertmacro MUI_PAGE_FINISH

!define MUI_UNCONFIRMPAGE_TEXT_TOP "HiMind Agent 将从当前 Windows 用户中移除。为便于后续恢复，本机配置数据会继续保留。"
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!define MUI_UNFINISHPAGE_TITLE "HiMind Agent 已卸载"
!define MUI_UNFINISHPAGE_TEXT "程序文件和启动项已移除，本机配置数据仍保留在原安装目录。"
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "SimpChinese"

Var StartWithWindowsCheckbox
Var DesktopShortcutCheckbox
Var StartWithWindowsState
Var DesktopShortcutState

Function .onInit
  StrCpy $StartWithWindowsState ${BST_CHECKED}
  StrCpy $DesktopShortcutState ${BST_CHECKED}
FunctionEnd

Function OptionsPage
  !insertmacro MUI_HEADER_TEXT "安装偏好" "确认本机启动方式"
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}

  ${NSD_CreateLabel} 0 2u 100% 24u "HiMind Agent 将安装到当前用户目录，并在后台提供本机服务。"
  Pop $0

  ${NSD_CreateCheckbox} 0 42u 100% 14u "登录 Windows 时自动启动（推荐）"
  Pop $StartWithWindowsCheckbox
  ${NSD_SetState} $StartWithWindowsCheckbox $StartWithWindowsState

  ${NSD_CreateLabel} 18u 59u 92% 22u "保持 Agent 在线，工作台可以随时调用已授权的本机能力。"
  Pop $0

  ${NSD_CreateCheckbox} 0 92u 100% 14u "在桌面创建 HiMind Agent 快捷方式"
  Pop $DesktopShortcutCheckbox
  ${NSD_SetState} $DesktopShortcutCheckbox $DesktopShortcutState

  ${NSD_CreateLabel} 0 104u 100% 12u "安装位置"
  Pop $0
  ${NSD_CreateText} 0 118u 100% 20u "$INSTDIR"
  Pop $0
  SendMessage $0 ${EM_SETREADONLY} 1 0

  nsDialogs::Show
FunctionEnd

Function OptionsPageLeave
  ${NSD_GetState} $StartWithWindowsCheckbox $StartWithWindowsState
  ${NSD_GetState} $DesktopShortcutCheckbox $DesktopShortcutState
FunctionEnd

Section "HiMind Agent 核心组件" SEC_AGENT
  SectionIn RO

  nsExec::Exec /TIMEOUT=10000 'taskkill /IM himind-agent.exe /T /F'

  SetOutPath "$INSTDIR"
  File "${RELEASE}\himind-agent-launcher.exe"
  File "${RELEASE}\himind-agent-updater.exe"
  File /oname=himind-agent.ico "${ASSET_DIR}\himind-agent.ico"
  !ifdef TRUSTED_PUBLIC_KEY
    !ifndef SIGNING_KEY_ID
      !error "SIGNING_KEY_ID is required with TRUSTED_PUBLIC_KEY"
    !endif
    SetOutPath "$INSTDIR\trusted-keys"
    File /oname=${SIGNING_KEY_ID}.pem "${TRUSTED_PUBLIC_KEY}"
    SetOutPath "$INSTDIR"
  !endif

  SetOutPath "$INSTDIR\current"
  File "${RELEASE}\himind-agent.exe"

  SetOutPath "$INSTDIR\resources\vscode"
  File /oname=himind-ai.vsix "${VSCODE_EXTENSION_VSIX}"

  SetOutPath "$INSTDIR\data"
  SetOutPath "$INSTDIR\previous"
  SetOutPath "$INSTDIR\logs"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "DisplayIcon" "$INSTDIR\himind-agent.ico"
  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${PRODUCT_REGISTRY_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${PRODUCT_REGISTRY_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${PRODUCT_REGISTRY_KEY}" "NoRepair" 1

  WriteRegStr HKCU "${PRODUCT_PROTOCOL_KEY}" "" "URL:HiMind Agent Protocol"
  WriteRegStr HKCU "${PRODUCT_PROTOCOL_KEY}" "URL Protocol" ""
  WriteRegStr HKCU "${PRODUCT_PROTOCOL_KEY}\DefaultIcon" "" "$INSTDIR\himind-agent.ico"
  WriteRegStr HKCU "${PRODUCT_PROTOCOL_KEY}\shell\open\command" "" '"$INSTDIR\himind-agent-launcher.exe" ${PRODUCT_LAUNCH_ARGS} --protocol-url "%1"'

  ${If} $StartWithWindowsState == ${BST_CHECKED}
    WriteRegStr HKCU "${PRODUCT_RUN_KEY}" "HiMindAgent" '"$INSTDIR\himind-agent-launcher.exe" ${PRODUCT_LAUNCH_ARGS}'
  ${Else}
    DeleteRegValue HKCU "${PRODUCT_RUN_KEY}" "HiMindAgent"
  ${EndIf}

  ${If} $DesktopShortcutState == ${BST_CHECKED}
    CreateShortcut "$DESKTOP\HiMind Agent.lnk" "$INSTDIR\himind-agent-launcher.exe" "${PRODUCT_LAUNCH_ARGS}" "$INSTDIR\himind-agent.ico"
  ${Else}
    Delete "$DESKTOP\HiMind Agent.lnk"
  ${EndIf}
SectionEnd

Function .onInstSuccess
  IfSilent 0 done
  Exec '"$INSTDIR\himind-agent-launcher.exe" ${PRODUCT_LAUNCH_ARGS}'
done:
FunctionEnd

Section "Uninstall"
  nsExec::Exec /TIMEOUT=10000 'taskkill /IM himind-agent.exe /T /F'
  DeleteRegValue HKCU "${PRODUCT_RUN_KEY}" "HiMindAgent"
  DeleteRegKey HKCU "${PRODUCT_PROTOCOL_KEY}"
  DeleteRegKey HKCU "${PRODUCT_REGISTRY_KEY}"
  Delete "$DESKTOP\HiMind Agent.lnk"
  RMDir /r "$INSTDIR\current"
  RMDir /r "$INSTDIR\previous"
  RMDir /r "$INSTDIR\logs"
  RMDir /r "$INSTDIR\resources"
  RMDir /r "$INSTDIR\trusted-keys"
  Delete "$INSTDIR\himind-agent-launcher.exe"
  Delete "$INSTDIR\himind-agent-updater.exe"
  Delete "$INSTDIR\himind-agent.ico"
  Delete "$INSTDIR\uninstall.exe"
SectionEnd
