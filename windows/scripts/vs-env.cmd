@echo off
rem Sets up, in the calling script, what building Coucou needs on Windows since
rem speech recognition (whisper.cpp through whisper-rs): Visual Studio's
rem developer environment (CMake, and the MSVC headers bindgen reads) and
rem libclang. With `gpu`, also the Vulkan SDK and a short target folder: the
rem Vulkan sub-build goes past Windows' 260-character path limit under
rem windows\target.
rem
rem Use with `call scripts\vs-env.cmd [gpu]`; errorlevel 1 when something is missing.
rem Needs Visual Studio 2022 (any edition, or the Build Tools) with "Desktop
rem development with C++", "C++ CMake tools for Windows" and "C++ Clang
rem Compiler for Windows" (or LLVM installed on its own).

set "VSROOT="
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%i"
if not defined VSROOT (
  echo Visual Studio 2022 with the C++ tools was not found.
  exit /b 1
)
call "%VSROOT%\VC\Auxiliary\Build\vcvars64.bat" >nul 2>nul
if not defined LIBCLANG_PATH if exist "%VSROOT%\VC\Tools\Llvm\x64\bin\libclang.dll" set "LIBCLANG_PATH=%VSROOT%\VC\Tools\Llvm\x64\bin"
if not defined LIBCLANG_PATH if exist "%ProgramFiles%\LLVM\bin\libclang.dll" set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
if not defined LIBCLANG_PATH (
  echo libclang was not found: add "C++ Clang Compiler for Windows" in the Visual Studio Installer.
  exit /b 1
)
rem No MSBuild worker left running after the build.
set "MSBUILDDISABLENODEREUSE=1"
if exist "%USERPROFILE%\.cargo\bin" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

if /i not "%~1"=="gpu" exit /b 0
rem A terminal opened before the SDK was installed doesn't have VULKAN_SDK:
rem take the newest one in its default folder.
if not defined VULKAN_SDK for /d %%d in ("%SystemDrive%\VulkanSDK\*") do set "VULKAN_SDK=%%d"
if not defined VULKAN_SDK (
  echo The Vulkan SDK was not found: winget install KhronosGroup.VulkanSDK
  exit /b 1
)
set "CARGO_TARGET_DIR=%~d0\cv"
exit /b 0
