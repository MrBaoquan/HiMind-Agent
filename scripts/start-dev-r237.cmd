@echo off
setlocal
set "TEST_SCRIPT=%~dp0start-test-agent.ps1"
where pwsh.exe >nul 2>nul
if not errorlevel 1 goto use_pwsh
set "PSModulePath="
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%TEST_SCRIPT%"
goto done
:use_pwsh
pwsh.exe -NoProfile -File "%TEST_SCRIPT%"
:done
if errorlevel 1 exit /b 1
exit /b 0

