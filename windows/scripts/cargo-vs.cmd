@echo off
rem Runs cargo inside Visual Studio's developer environment (vs-env.cmd), which
rem the app needs on Windows since speech recognition (whisper-rs).
rem
rem Usage, from windows\:   scripts\cargo-vs.cmd test --workspace --locked
setlocal
call "%~dp0vs-env.cmd" || exit /b 1
cargo %*
