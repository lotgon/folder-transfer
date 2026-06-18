@echo off
rem ====================================================================
rem  folder-transfer - thin launcher. Runs ft-server.ps1 sitting next to
rem  this file. No temp extraction, no embedding - just a one-line wrapper
rem  so you can start it as a .bat. The real, readable script is ft-server.ps1.
rem
rem  Usage:   folder-transfer.bat <FOLDER> [options]    Help: folder-transfer.bat --help
rem  Example: folder-transfer.bat D:\db -Cutover    (two-phase sync; default is single-phase)
rem  Requires ft-server.ps1 and ft-client.ps1 in this same folder.
rem ====================================================================
setlocal EnableExtensions
if not exist "%~dp0ft-server.ps1" ( echo ERROR: ft-server.ps1 not found next to folder-transfer.bat & echo. & pause & exit /b 1 )
rem  No arguments is NOT help anymore - ft-server.ps1 then asks interactively.
set "HELP="
if "%~1"=="/?" set "HELP=1"
if /i "%~1"=="-h" set "HELP=1"
if /i "%~1"=="--help" set "HELP=1"
if /i "%~1"=="help" set "HELP=1"
if defined HELP (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0ft-server.ps1" -Help
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0ft-server.ps1" %*
)
set "RC=%errorlevel%"
echo.
echo Server stopped (exit code %RC%). Review the log above.
pause
exit /b %RC%
