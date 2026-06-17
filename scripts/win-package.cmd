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
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" ( echo ERROR: vswhere not found at "%VSWHERE%" & exit /b 1 )
set "VSINSTALL="
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%i"
if not defined VSINSTALL ( echo ERROR: VS Build Tools with VC.Tools.x86.x64 not found & exit /b 1 )
call "%VSINSTALL%\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul || ( echo ERROR: vcvarsall failed & exit /b 1 )

echo === npm install (ui) ===
call npm --prefix ui install || exit /b 1
echo === npm run build (ui -^> ui/dist) ===
call npm --prefix ui run build || exit /b 1
echo === cargo build -p solstone-windows-app --release ===
cargo build -p solstone-windows-app --release || exit /b 1
echo === vpk pack (scripts\package.ps1) ===
powershell -ExecutionPolicy Bypass -File scripts\package.ps1 || exit /b 1

echo === WIN_PACKAGE_OK ===
exit /b 0
