@echo off
setlocal
cd /d "%~dp0"
cargo test --locked
if errorlevel 1 exit /b 1
cargo build --release --locked --bin AVT-Replenishment
if errorlevel 1 exit /b 1
echo Built: target\release\AVT-Replenishment.exe
endlocal
