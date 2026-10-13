@echo off
rem Builds a coucou.exe that runs on its own (its pages inside it), from any
rem terminal or a double-click. No installer: see `npm run pack` for that.
rem
rem   scripts\build-exe.cmd        speech recognition on the CPU
rem   scripts\build-exe.cmd gpu    speech recognition on the graphics card (Vulkan)
setlocal
cd /d "%~dp0.."
call "%~dp0vs-env.cmd" %1 || goto :failed

if /i "%~1"=="gpu" (
  call npx tauri build --no-bundle --features gpu-vulkan || goto :failed
) else (
  call npx tauri build --no-bundle || goto :failed
)
set "OUT=%CARGO_TARGET_DIR%"
if not defined OUT set "OUT=%CD%\target"
echo.
echo Done: %OUT%\release\coucou.exe
echo It can be started by double-clicking it.
pause
exit /b 0

:failed
echo.
echo The build failed. See the messages above.
pause
exit /b 1
