@echo off
rem Runs cargo inside Visual Studio's developer environment, which the app
rem needs on Windows since speech recognition (whisper-rs): CMake builds
rem whisper.cpp, and bindgen (libclang) reads the MSVC and Windows SDK headers
rem that vcvars64 puts in INCLUDE.
rem
rem Needs Visual Studio 2022 (any edition, or the Build Tools) with the
rem "Desktop development with C++" workload, "C++ CMake tools for Windows" and
rem "C++ Clang Compiler for Windows" (or LLVM installed on its own).
rem
rem Usage, from windows\:   scripts\cargo-vs.cmd test --workspace --locked
setlocal
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%i"
if not defined VSROOT (
  echo Visual Studio with the C++ tools was not found. 1>&2
  exit /b 1
)
call "%VSROOT%\VC\Auxiliary\Build\vcvars64.bat" >nul
if not defined LIBCLANG_PATH if exist "%VSROOT%\VC\Tools\Llvm\x64\bin\libclang.dll" set "LIBCLANG_PATH=%VSROOT%\VC\Tools\Llvm\x64\bin"
if not defined LIBCLANG_PATH if exist "%ProgramFiles%\LLVM\bin\libclang.dll" set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
rem No MSBuild worker left running after the build.
set "MSBUILDDISABLENODEREUSE=1"
cargo %*
