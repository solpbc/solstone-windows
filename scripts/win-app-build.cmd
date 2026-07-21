@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Operator-direct app-crate build for the Windows box. The Tauri app crate
:: (solstone-windows-app) is excluded from the win-ci.cmd ship-gate because its
:: build needs the npm frontend (ui/dist, embedded at cargo-compile time) and the
:: icon asset. This verb builds the webview bundle, then the app binary, so a
:: session can verify the shell + IPC integration compiles + links on the box.
:: Mirrors win-ci.cmd's MSVC activation. Invoke via `cmd /c`.
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
call scripts\preflight-toolchain.cmd || exit /b 1

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" ( echo ERROR: vswhere not found at "%VSWHERE%" & exit /b 1 )
set "VSINSTALL="
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%i"
if not defined VSINSTALL ( echo ERROR: VS Build Tools with VC.Tools.x86.x64 not found & exit /b 1 )
call "%VSINSTALL%\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul || ( echo ERROR: vcvarsall failed & exit /b 1 )

echo === npm ci --offline (ui) ===
call npm --prefix ui ci --offline || exit /b 1
echo === npm run build (ui -^> ui/dist) ===
call npm --prefix ui run build || exit /b 1
:: --features custom-protocol so this verify build serves the embedded ui/dist
:: (not the Vite devUrl) and the Settings/About windows actually render - a plain
:: cargo build leaves the webview pointed at a dead dev server. See win-package.cmd.
echo === cargo build --locked -p solstone-windows-app --features custom-protocol ===
cargo build --locked -p solstone-windows-app --features custom-protocol || exit /b 1

echo === WIN_APP_BUILD_OK: native Windows app build passed after UI build; package install sign and smoke not run ===
exit /b 0
