@echo off
cd /d "%~dp0"

REM  CLEAN_AND_RUN.bat        -> Khoi chay ung dung ban Release doc lap, an CMD
REM  CLEAN_AND_RUN.bat dev    -> Chay che do dev (hien log Vite + Cargo trong CMD)

if /i "%~1"=="dev" goto :DEV
if /i "%~1"=="debug" goto :DEV

start "" /min powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0scripts\launcher.ps1"
exit /b

:DEV
chcp 65001 >nul
taskkill /F /IM henry-chat-pro.exe >nul 2>&1
if not exist "node_modules" call npm.cmd install
call npm.cmd run tauri dev
pause
