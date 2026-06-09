@echo off
REM Claude Monitor - browser mode (Windows double-click launcher).
REM
REM Double-click this file (or run it from a terminal) to build (if needed) and
REM launch the full dashboard in your default browser. No Tauri window/tray.
REM
REM Usage:
REM   serve.bat                  build + run on the default port (8788)
REM   set CM_PORT=9000 ^&^& serve.bat   pick a port
REM   set CM_NO_OPEN=1  ^&^& serve.bat  don't auto-open a browser
REM
REM Requires a Rust toolchain (https://rustup.rs). If dist\ is missing it is
REM built via "npm run build" (needs Node.js).
setlocal enabledelayedexpansion
cd /d "%~dp0"

where cargo >nul 2>nul
if errorlevel 1 (
  echo error: 'cargo' not found. Install Rust from https://rustup.rs and retry.
  pause
  exit /b 1
)

if not exist "%~dp0dist\index.html" (
  echo dist\ not found - building the frontend ^(npm run build^)...
  where npm >nul 2>nul
  if errorlevel 1 (
    echo error: dist\ is missing and 'npm' is not installed to build it.
    echo        Install Node.js, run "npm install ^&^& npm run build", then retry.
    pause
    exit /b 1
  )
  call npm install
  call npm run build
)

echo Starting Claude Monitor ^(browser mode^)...
cargo run -p cm-serve --release
REM Keep the window open if launched by double-click and the server exits.
pause
