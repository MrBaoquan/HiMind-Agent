Unicode true

!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

!ifndef API_BASE
  !define API_BASE "https://himind.andcrane.com"
!endif
!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef PRODUCT_VERSION
  !define PRODUCT_VERSION "${VERSION}"
!endif
!ifndef DEFAULT_MODE
  !define DEFAULT_MODE "independent"
!endif
!ifndef ASSET_DIR
  !define ASSET_DIR "generated"
!endif
!ifndef VSCODE_EXTENSION_VSIX
  !error "VSCODE_EXTENSION_VSIX is required"
!endif
!ifndef RUNTIME_DIR
  !error "RUNTIME_DIR is required: installer must carry the signed HiMind AI Runtime"
!endif
!ifndef RUNTIME_PACKAGE
  !error "RUNTIME_PACKAGE is required: installer must carry the signed HiMind AI Runtime"
!endif

; Staging the bundled Runtime and extracting it both live under $INSTDIR, so the
; installer requires real headroom instead of failing half way through. Roughly:
; staged package + agent binaries (~560 MB) plus the extracted Runtime (~456 MB).
!define MIN_FREE_SPACE_MB 1536

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
SetCompressorDictSize 64
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
!define MUI_WELCOMEPAGE_TEXT "HiMind Agent 将安装到当前 Windows 用户，提供独立本机能力，并可按需连接 HiMind 工作台。$\r$\n$\r$\n安装无需管理员权限，通常只需几秒。现有版本会被安全替换，本机配置保持不变。"
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
!define MUI_FINISHPAGE_TEXT "安装已完成。HiMind Agent 已在后台启动；首次运行模式已按安装来源配置，之后可在设置中切换。"
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
  !ifdef TRUSTED_EXTENSION_PUBLIC_KEY
    !ifndef EXTENSION_SIGNING_KEY_ID
      !error "EXTENSION_SIGNING_KEY_ID is required with TRUSTED_EXTENSION_PUBLIC_KEY"
    !endif
    SetOutPath "$INSTDIR\trusted-keys"
    File /oname=${EXTENSION_SIGNING_KEY_ID}.pem "${TRUSTED_EXTENSION_PUBLIC_KEY}"
    SetOutPath "$INSTDIR"
  !endif

  SetOutPath "$INSTDIR\versions\${PRODUCT_VERSION}"
  File "${RELEASE}\himind-agent.exe"
  File "${RELEASE}\himind-agent-mcp.exe"
  SetOutPath "$INSTDIR"
  FileOpen $0 "$INSTDIR\active-version" w
  FileWrite $0 "${PRODUCT_VERSION}$\r$\n"
  FileClose $0

  SetOutPath "$INSTDIR\resources\vscode"
  File /oname=himind-ai.vsix "${VSCODE_EXTENSION_VSIX}"

  ; Runtime is shipped with the installer. First launch does not depend on
  ; Dashboard availability or a separate Node/Office installation.
  ; Clear the directory first: the package from an earlier install is kept for
  ; offline repair, so without this an upgrade would leave the old version's
  ; archive behind next to the new one.
  RMDir /r "$INSTDIR\resources\runtime"
  SetOutPath "$INSTDIR\resources\runtime"
  File "${RUNTIME_DIR}\himind-runtime-release.json"
  File "${RUNTIME_DIR}\${RUNTIME_PACKAGE}"
  File "${RUNTIME_DIR}\${RUNTIME_PACKAGE}.signature.json"

  SetOutPath "$INSTDIR\data"
  ; Installation source only selects the first-run default. Reinstall and
  ; update must preserve the user's persisted mode choice.
  IfFileExists "$INSTDIR\data\agent-preferences.json" mode_preferences_done
  FileOpen $0 "$INSTDIR\data\agent-preferences.json" w
  FileWrite $0 "{$\r$\n  $\"mode$\": $\"${DEFAULT_MODE}$\"$\r$\n}$\r$\n"
  FileClose $0
  mode_preferences_done:

  ; Reserve disk space before staging the Runtime.
  ${GetRoot} "$INSTDIR" $R8
  ${DriveSpace} "$R8" "/D=F /S=M" $R9
  StrCmp $R9 "" runtime_space_checked
  ${If} $R9 < ${MIN_FREE_SPACE_MB}
    MessageBox MB_ICONSTOP|MB_OK "磁盘可用空间不足，无法安装 HiMind Agent。$\r$\n$\r$\n$INSTDIR 所在磁盘剩余约 $R9 MB，需要至少 ${MIN_FREE_SPACE_MB} MB。$\r$\n请清理磁盘空间后重新运行安装程序。"
    Abort
  ${EndIf}
  runtime_space_checked:

  ; Import the bundled signed Runtime before installation reports success.
  ; Agent verifies size, SHA-256, RSA-PSS and the package contents, then runs the
  ; CLI / Office / screen-control preflight. Capabilities that depend on this
  ; machine (screen control) degrade instead of aborting and are reported with
  ; exit code 2; an unusable Runtime still fails closed.
  SetOutPath "$INSTDIR"
  nsExec::ExecToLog /TIMEOUT=900000 '"$INSTDIR\versions\${PRODUCT_VERSION}\himind-agent.exe" runtime install --manifest "$INSTDIR\resources\runtime\himind-runtime-release.json"'
  Pop $0

  ; The Agent mirrors its output into runtime-install.log. Read the last
  ; non-empty line so the dialog can name the real reason: the NSIS details pane
  ; is not persisted, which previously left field failures undiagnosable.
  StrCpy $R7 ""
  ${If} ${FileExists} "$INSTDIR\logs\runtime-install.log"
    FileOpen $R6 "$INSTDIR\logs\runtime-install.log" r
    ${Do}
      ClearErrors
      FileRead $R6 $R5
      ${If} ${Errors}
        ${Break}
      ${EndIf}
      ${If} $R5 != ""
        StrCpy $R7 $R5
      ${EndIf}
    ${Loop}
    FileClose $R6
  ${EndIf}
  StrCmp $R7 "" 0 runtime_reason_ready
  StrCpy $R7 "未生成安装日志，请展开本页的「详细信息」查看 Agent 输出。"
  runtime_reason_ready:

  ; On any failure, collect a diagnostics bundle before showing the dialog, so
  ; the user has something to send without being walked through it by hand. This
  ; does not change the outcome - it only produces a file - and a failure to
  ; collect is ignored, because the dialog still names the reason below.
  StrCmp $0 "0" runtime_bundle_skipped
  StrCmp $0 "2" runtime_bundle_skipped
  nsExec::ExecToLog '"$INSTDIR\versions\${PRODUCT_VERSION}\himind-agent.exe" diagnostics export'
  Pop $R3
  runtime_bundle_skipped:

  ${If} $0 == "error"
    MessageBox MB_ICONSTOP|MB_OK "无法启动 HiMind AI 运行时安装程序，安装已中止，Agent 未启动。$\r$\n$\r$\n请检查安全软件是否拦截了 $INSTDIR 下的程序，然后重新运行安装程序。$\r$\n$\r$\n诊断包目录：$LOCALAPPDATA\HiMindAgent\diagnostics"
    Abort
  ${ElseIf} $0 == 2
    MessageBox MB_ICONEXCLAMATION|MB_OK "HiMind Agent 已安装，但部分本机能力暂不可用。$\r$\n$\r$\n原因：$R7$\r$\n$\r$\n完整日志：$INSTDIR\logs\runtime-install.log$\r$\n诊断包目录：$LOCALAPPDATA\HiMindAgent\diagnostics"
  ${ElseIf} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "HiMind AI 运行时安装失败，安装已中止，Agent 未启动。$\r$\n$\r$\n原因：$R7$\r$\n$\r$\n完整日志：$INSTDIR\logs\runtime-install.log$\r$\n$\r$\n诊断包（可直接发给技术支持）：$LOCALAPPDATA\HiMindAgent\diagnostics"
    Abort
  ${EndIf}
  ; Keep the staged Runtime package. The Runtime is already imported into
  ; $INSTDIR\runtimes\ above, but the signed archive stays here as the offline
  ; repair source: if a later launch finds the Runtime missing or unusable
  ; (antivirus quarantining a file, a half-finished copy, a corrupt state.json)
  ; the Agent reinstalls from this package without needing the Dashboard or the
  ; network. Agent backups deliberately exclude `resources/` and `runtimes/`
  ; (see src/app/backup.rs), so keeping it does not inflate them, and a reinstall
  ; overwrites it with the current version.

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
  Exec '"$INSTDIR\himind-agent-launcher.exe" ${PRODUCT_LAUNCH_ARGS}'
FunctionEnd

Section "Uninstall"
  nsExec::Exec /TIMEOUT=10000 'taskkill /IM himind-agent.exe /T /F'
  DeleteRegValue HKCU "${PRODUCT_RUN_KEY}" "HiMindAgent"
  DeleteRegKey HKCU "${PRODUCT_PROTOCOL_KEY}"
  DeleteRegKey HKCU "${PRODUCT_REGISTRY_KEY}"
  Delete "$DESKTOP\HiMind Agent.lnk"
  RMDir /r "$INSTDIR\current"
  RMDir /r "$INSTDIR\versions"
  RMDir /r "$INSTDIR\previous"
  RMDir /r "$INSTDIR\logs"
  RMDir /r "$INSTDIR\resources"
  RMDir /r "$INSTDIR\trusted-keys"
  Delete "$INSTDIR\himind-agent-launcher.exe"
  Delete "$INSTDIR\himind-agent-updater.exe"
  Delete "$INSTDIR\himind-agent.ico"
  Delete "$INSTDIR\active-version"
  Delete "$INSTDIR\uninstall.exe"
SectionEnd
