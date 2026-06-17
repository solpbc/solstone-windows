@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Windows build-box CI runner. Activates the MSVC dev environment, then runs the
:: Session-0-safe gate: build + workspace tests + contract drift check. This is the
:: remote-mill ship-gate (the live FlaUI smoke + lifecycle matrix are operator-direct
:: per the wave plan, not part of this run).
::
:: Invoked on the build box via `cmd.exe /c` (the box default SSH shell is
:: PowerShell, and vcvars only sets env in a cmd session). Run from the repo root
:: by the box-side bootstrap, or directly during a manual build.
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

:: rustup installs cargo under the user profile; non-interactive SSH PATH may not
:: carry it, so add it explicitly (the winget/PATH-refresh gotcha from the spikes).
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

:: Locate + activate VS Build Tools (the MSVC linker the platform-tier crates need).
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" ( echo ERROR: vswhere not found at "%VSWHERE%" & exit /b 1 )
set "VSINSTALL="
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%i"
if not defined VSINSTALL ( echo ERROR: VS Build Tools with VC.Tools.x86.x64 not found & exit /b 1 )
call "%VSINSTALL%\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul || ( echo ERROR: vcvarsall failed & exit /b 1 )

:: The fast iterate-loop gate: build + test the library/platform crates (incl. the
:: windows-rs tier) + the contract drift check. The Tauri app crate is excluded here
:: because its build needs the npm frontend + an icon asset — those build in the
:: heavier operator-direct `make package` path, not the lode iterate loop.
echo === cargo build (workspace, minus app) ===
cargo build --workspace --exclude solstone-windows-app || exit /b 1
echo === cargo test (workspace, minus app) ===
cargo test --workspace --exclude solstone-windows-app || exit /b 1
echo === cargo xtask contract --check ===
cargo run -q -p xtask -- contract --check || exit /b 1

echo === WIN_CI_OK ===
exit /b 0
