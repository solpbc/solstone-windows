@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Operator-direct RELEASE build + Velopack pack on the Windows box. The box-side
:: analog of `make package` (the Makefile target runs the LOCAL toolchain, which
:: cannot cross-build the Windows app from Linux). Activates the MSVC dev env,
:: builds the webview bundle, builds the RELEASE app binary (windowless tray exe),
:: then vpk-packs into Releases/. vpk emits a DELTA nupkg automatically when a
:: prior full nupkg for an earlier version is already present in Releases/, so the
:: delta-update flow is exercised by packaging a bumped version against the
:: installed baseline's full nupkg. Invoke via `cmd /c` after a tree sync.
::
:: The box has Windows PowerShell 5.1 only (no pwsh 7), so package.ps1 runs under
:: `powershell` here (the Makefile uses `pwsh` for the local path); package.ps1 is
:: ASCII-only precisely so it parses under 5.1.
::
:: Signing is opt-in and release-only: set SOLSTONE_SIGN=1 in the environment to
:: pass -Sign through to package.ps1 (a release). Leave it unset for dev/local and
:: delta-update validation packs so they stay unsigned (no signature-quota burn,
:: stable SmartScreen hashes). The signing credentials are supplied by the box's
:: signing environment; package.ps1 / preflight-auth.ps1 read them, never this file.
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "SIGN_ARG="
if defined SOLSTONE_SIGN set "SIGN_ARG=-Sign"

echo === release-tool preflight ===
set "RELEASE_SELECTION="
for /f "usebackq delims=" %%i in (`powershell -NoProfile -File packaging\preflight-release-tools.ps1 %SIGN_ARG%`) do set "RELEASE_SELECTION=%%i"
if errorlevel 1 exit /b 1
if not defined RELEASE_SELECTION ( echo ERROR: release-tool preflight returned no selection record & exit /b 1 )

set "SELECTED_CARGO="
set "SELECTED_NPM="
set "SELECTED_POWERSHELL="
set "SELECTED_VCVARSALL="
set "SELECTED_VCVARS_VERSION_ARG="
for /f "usebackq delims=" %%i in (`powershell -NoProfile -Command "($env:RELEASE_SELECTION | ConvertFrom-Json).tools.cargo.path"`) do set "SELECTED_CARGO=%%i"
for /f "usebackq delims=" %%i in (`powershell -NoProfile -Command "($env:RELEASE_SELECTION | ConvertFrom-Json).tools.npm.path"`) do set "SELECTED_NPM=%%i"
for /f "usebackq delims=" %%i in (`powershell -NoProfile -Command "($env:RELEASE_SELECTION | ConvertFrom-Json).tools.powershell.path"`) do set "SELECTED_POWERSHELL=%%i"
for /f "usebackq delims=" %%i in (`powershell -NoProfile -Command "($env:RELEASE_SELECTION | ConvertFrom-Json).tools.'msvc-cl'.vcvarsallPath"`) do set "SELECTED_VCVARSALL=%%i"
for /f "usebackq delims=" %%i in (`powershell -NoProfile -Command "($env:RELEASE_SELECTION | ConvertFrom-Json).tools.'msvc-cl'.vcvarsVersionArg"`) do set "SELECTED_VCVARS_VERSION_ARG=%%i"
if not defined SELECTED_CARGO ( echo ERROR: selection record omitted cargo.path & exit /b 1 )
if not defined SELECTED_NPM ( echo ERROR: selection record omitted npm.path & exit /b 1 )
if not defined SELECTED_POWERSHELL ( echo ERROR: selection record omitted powershell.path & exit /b 1 )
if not defined SELECTED_VCVARSALL ( echo ERROR: selection record omitted msvc-cl.vcvarsallPath & exit /b 1 )
if not defined SELECTED_VCVARS_VERSION_ARG ( echo ERROR: selection record omitted msvc-cl.vcvarsVersionArg & exit /b 1 )

echo === version gate ===
set "SOLSTONE_VERSION_GATE_CARGO=%SELECTED_CARGO%"
call "%SELECTED_CARGO%" run --locked -q -p xtask -- version-gate || exit /b 1
echo === lock guard ===
"%SELECTED_POWERSHELL%" -NoProfile -File packaging\lock-guard.ps1 || exit /b 1

call "%SELECTED_VCVARSALL%" x64 %SELECTED_VCVARS_VERSION_ARG% >nul || ( echo ERROR: selected vcvarsall failed & exit /b 1 )

echo === npm ci --offline (ui) ===
call "%SELECTED_NPM%" --prefix ui ci --offline || exit /b 1
echo === npm run build (ui -^> ui/dist) ===
call "%SELECTED_NPM%" --prefix ui run build || exit /b 1
:: --features custom-protocol is REQUIRED for a shipping build. Without it Tauri
:: serves the webview from devUrl (the Vite dev server, build.devUrl in
:: tauri.conf.json) instead of the embedded ui/dist, so the Settings and About
:: windows load "localhost refused to connect" in the installed app. cargo tauri
:: build enables it automatically; a plain cargo build does not. Do not remove.
echo === cargo build --locked -p solstone-windows-app --release --features custom-protocol ===
call "%SELECTED_CARGO%" build --locked -p solstone-windows-app --release --features custom-protocol || exit /b 1
echo === vpk pack (scripts\package.ps1) ===
"%SELECTED_POWERSHELL%" -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 %SIGN_ARG% || exit /b 1

echo === WIN_PACKAGE_OK: native Windows release build and vpk pack passed; signing only when SOLSTONE_SIGN is set; install and smoke not run ===
exit /b 0
