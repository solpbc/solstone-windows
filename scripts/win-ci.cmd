@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Windows build-box CI runner. Activates the MSVC dev environment, then runs the
:: native preflight self-test, toolchain preflight, build, workspace tests, contract
:: drift check, and purity check, then emits WIN_CI_HEAD for identity verification.
:: This is the remote-mill ship-gate (the live FlaUI smoke + lifecycle matrix are
:: operator-direct per the wave plan, not part of this run).
::
:: Invoked on the build box via `cmd.exe /c` (the box default SSH shell is
:: PowerShell, and vcvars only sets env in a cmd session). Run from the repo root
:: by the box-side bootstrap, or directly during a manual build.
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

:: rustup installs cargo under the user profile; non-interactive SSH PATH may not
:: carry it, so add it explicitly (the winget/PATH-refresh gotcha from the spikes).
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
call scripts\lib\preflight-toolchain.test.cmd || exit /b 1
call scripts\preflight-toolchain.cmd || exit /b 1

:: Locate + activate VS Build Tools (the MSVC linker the platform-tier crates need).
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" ( echo ERROR: vswhere not found at "%VSWHERE%" & exit /b 1 )
set "VSINSTALL="
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%i"
if not defined VSINSTALL ( echo ERROR: VS Build Tools with VC.Tools.x86.x64 not found & exit /b 1 )
call "%VSINSTALL%\VC\Auxiliary\Build\vcvarsall.bat" x64 >nul || ( echo ERROR: vcvarsall failed & exit /b 1 )

:: The fast iterate-loop gate: build + test the library/platform crates (incl. the
:: windows-rs tier), then check contract drift and purity. The Tauri app crate is
:: excluded here because its build needs the npm frontend + an icon asset - those
:: build in the heavier operator-direct `make package` path, not this iterate loop.
echo === cargo build --locked (workspace, minus app) ===
cargo build --locked --workspace --exclude solstone-windows-app || exit /b 1
echo === cargo test --locked (workspace, minus app) ===
cargo test --locked --workspace --exclude solstone-windows-app || exit /b 1
echo === cargo xtask contract --locked --check ===
cargo run --locked -q -p xtask -- contract --check || exit /b 1
echo === cargo xtask purity-check --locked ===
cargo run --locked -q -p xtask -- purity-check || exit /b 1

git rev-parse HEAD >nul 2>&1 || ( echo ERROR: git rev-parse HEAD failed & exit /b 1 )
set "WIN_CI_HEAD="
for /f "usebackq tokens=*" %%i in (`git rev-parse HEAD`) do set "WIN_CI_HEAD=%%i"
if not defined WIN_CI_HEAD ( echo ERROR: git rev-parse HEAD returned no commit & exit /b 1 )
echo WIN_CI_HEAD=%WIN_CI_HEAD%
echo === WIN_CI_OK: native Windows build and test passed for workspace excluding solstone-windows-app; contract and purity checks passed; app package install sign and smoke not run ===
exit /b 0
