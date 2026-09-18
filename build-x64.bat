@echo off
chcp 65001 >nul

echo ========================================
echo  WoolBrush V2 - x86_64 Windows Build
echo  (Native ARM64 toolchain, cross-compile x64)
echo ========================================

REM Initialize x64 MSVC environment (sets LIB/INCLUDE for x64)
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64
if errorlevel 1 (
    echo [ERROR] vcvarsall.bat failed
    exit /b 1
)

REM Disable LTO - link.exe silently fails with LTO in cross-compile
set CARGO_PROFILE_RELEASE_LTO=false

REM Build frontend and Tauri for x86_64 using native ARM64 cargo
cd /d "D:\DuMate\Polymarket\WoolBrush-羊毛刷-V2"
pnpm tauri build --target x86_64-pc-windows-msvc

if errorlevel 1 (
    echo [ERROR] Build failed
    exit /b 1
)

echo.
echo [SUCCESS] Build completed
echo Output: src-tauri\target\x86_64-pc-windows-msvc\release\bundle\
