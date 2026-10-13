@echo off
rem Starts Coucou in development mode, from any terminal or a double-click.
rem The window this runs in is the app's console: close it (or Ctrl+C) to quit.
rem
rem   scripts\dev.cmd        speech recognition on the CPU
rem   scripts\dev.cmd gpu    speech recognition on the graphics card (Vulkan)
rem
rem A development build loads its pages from the Vite server this starts, so
rem its coucou.exe does not work on its own: use build-exe.cmd for that.
setlocal
cd /d "%~dp0.."
call "%~dp0vs-env.cmd" %1 || goto :failed

if /i "%~1"=="gpu" (
  echo Starting Coucou ^(speech on the graphics card^), building into %CARGO_TARGET_DIR%...
  call npm run tauri dev -- --features gpu-vulkan
) else (
  echo Starting Coucou ^(speech on the CPU^)...
  call npm run tauri dev
)
if errorlevel 1 goto :failed
exit /b 0

:failed
echo.
echo Coucou did not start. See the messages above.
echo If Coucou is already open, quit it first (tray icon ^> Quit): only one can run.
pause
exit /b 1
