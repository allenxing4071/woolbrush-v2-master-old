@echo off
chcp 65001 >nul

echo ========================================
echo  WoolBrush V2 - x86_64 Dev Mode
echo  (Native ARM64 toolchain, cross-compile x64)
echo ========================================

REM Initialize x64 MSVC environment (sets LIB/INCLUDE for x64)
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64
if errorlevel 1 (
    echo [ERROR] vcvarsall.bat failed
    exit /b 1
)

REM Use x64 toolchain so build scripts also compile as x64
set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-msvc

REM Run tauri dev for x86_64
cd /d "D:\DuMate\Polymarket\WoolBrush-羊毛刷-V2"
pnpm tauri dev --target x86_64-pc-windows-msvc
