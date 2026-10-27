@echo off
setlocal
title PC Guardian Deployment
%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\deploy-windows.ps1"
if errorlevel 1 (
  echo.
  echo Deployment failed. Please check the deployment log shown above.
  pause
)
endlocal
