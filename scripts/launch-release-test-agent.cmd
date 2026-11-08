@echo off
rem 启动一份 release 构建的验收实例（development profile，端口 18082）。
rem
rem 依赖仓库内的相对布局，任何机器上都能直接用；WebView2 调试端口 9311 用于
rem 通过 DevTools 协议读界面、点按钮做 e2e 验收（见 AGENTS.md §1.1）。
setlocal
set "ROOT=%~dp0.."
set "LOG_DIR=%ROOT%\.himind\test-agent-logs"
if not exist "%LOG_DIR%" mkdir "%LOG_DIR%"

set HIMIND_AGENT_PROFILE=development
set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9311
set WEBVIEW2_USER_DATA_FOLDER=%ROOT%\.himind\e2e-webview-18082

start "" /b "%ROOT%\target\release\himind-agent.exe" --profile development --local-app --local-port 18082 1>"%LOG_DIR%\medium-release.stdout.log" 2>"%LOG_DIR%\medium-release.stderr.log"
exit /b 0
