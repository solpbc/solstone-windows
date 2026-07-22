@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Windows build-box CI runner. Activates the MSVC dev environment, then runs the
:: source binding, native preflight self-test, toolchain preflight, build,
:: workspace tests, contract drift check, and purity check, then emits the three
:: source-binding acknowledgements for identity verification.
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

:: The driver passes only these three validated lowercase values. Verify the
:: transferred checkout before any build, test, or package action can write bytes.
if not defined EXPECTED_RELEASE_COMMIT ( echo ERROR: EXPECTED_RELEASE_COMMIT is required; rerun through win-host-ci & exit /b 1 )
if not defined EXPECTED_CARGO_LOCK_SHA256 ( echo ERROR: EXPECTED_CARGO_LOCK_SHA256 is required; rerun through win-host-ci & exit /b 1 )
if not defined EXPECTED_UI_PACKAGE_LOCK_SHA256 ( echo ERROR: EXPECTED_UI_PACKAGE_LOCK_SHA256 is required; rerun through win-host-ci & exit /b 1 )
powershell -NoProfile -Command "if ($env:EXPECTED_RELEASE_COMMIT -notmatch '^[0-9a-f]{40}$' -or $env:EXPECTED_CARGO_LOCK_SHA256 -notmatch '^[0-9a-f]{64}$' -or $env:EXPECTED_UI_PACKAGE_LOCK_SHA256 -notmatch '^[0-9a-f]{64}$') { exit 1 }" || ( echo ERROR: source-binding values must be lowercase full commit and SHA-256 hex; rerun through win-host-ci & exit /b 1 )

git rev-parse HEAD >nul 2>&1 || ( echo ERROR: git rev-parse HEAD failed; restore the transferred checkout and retry & exit /b 1 )
set "WIN_CI_HEAD="
for /f "usebackq tokens=*" %%i in (`git rev-parse HEAD`) do set "WIN_CI_HEAD=%%i"
if not defined WIN_CI_HEAD ( echo ERROR: git rev-parse HEAD returned no commit; restore the transferred checkout and retry & exit /b 1 )
if not "%WIN_CI_HEAD%"=="%EXPECTED_RELEASE_COMMIT%" ( echo ERROR: transferred HEAD does not match EXPECTED_RELEASE_COMMIT; restore the transferred bundle and retry & exit /b 1 )

git status --porcelain=v1 --untracked-files=all --ignore-submodules=none >nul 2>&1 || ( echo ERROR: git status failed; restore the transferred checkout and retry & exit /b 1 )
set "WIN_CI_DIRTY="
for /f "usebackq delims=" %%i in (`git status --porcelain=v1 --untracked-files=all --ignore-submodules=none`) do set "WIN_CI_DIRTY=1"
if defined WIN_CI_DIRTY ( echo ERROR: transferred checkout is dirty; restore the exact clean bundle and retry & exit /b 1 )

set "WIN_CI_CARGO_LOCK_SHA256="
for /f "usebackq tokens=*" %%i in (`powershell -NoProfile -Command "(Get-FileHash -LiteralPath 'Cargo.lock' -Algorithm SHA256).Hash.ToLowerInvariant()"`) do set "WIN_CI_CARGO_LOCK_SHA256=%%i"
if not defined WIN_CI_CARGO_LOCK_SHA256 ( echo ERROR: Cargo.lock SHA-256 could not be computed; restore the tracked lockfile and retry & exit /b 1 )
if not "%WIN_CI_CARGO_LOCK_SHA256%"=="%EXPECTED_CARGO_LOCK_SHA256%" ( echo ERROR: Cargo.lock SHA-256 does not match the transferred binding; restore the exact lockfile and retry & exit /b 1 )

set "WIN_CI_UI_LOCK_SHA256="
for /f "usebackq tokens=*" %%i in (`powershell -NoProfile -Command "(Get-FileHash -LiteralPath 'ui/package-lock.json' -Algorithm SHA256).Hash.ToLowerInvariant()"`) do set "WIN_CI_UI_LOCK_SHA256=%%i"
if not defined WIN_CI_UI_LOCK_SHA256 ( echo ERROR: ui/package-lock.json SHA-256 could not be computed; restore the tracked lockfile and retry & exit /b 1 )
if not "%WIN_CI_UI_LOCK_SHA256%"=="%EXPECTED_UI_PACKAGE_LOCK_SHA256%" ( echo ERROR: ui/package-lock.json SHA-256 does not match the transferred binding; restore the exact lockfile and retry & exit /b 1 )

:: The expected commit and lock variables remain in this process environment so
:: any later package transaction invoked here receives the same binding.
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\lib\preflight-release-tools.test.ps1 || exit /b 1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\lib\lock-guard.test.ps1 || exit /b 1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\lib\package-entrypoints.test.ps1 || exit /b 1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\lib\smoke-version-gate.test.ps1 || exit /b 1
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

echo WIN_CI_HEAD=%WIN_CI_HEAD%
echo WIN_CI_CARGO_LOCK_SHA256=%WIN_CI_CARGO_LOCK_SHA256%
echo WIN_CI_UI_LOCK_SHA256=%WIN_CI_UI_LOCK_SHA256%
echo === WIN_CI_OK: native Windows build and test passed for workspace excluding solstone-windows-app; contract and purity checks passed; app package install sign and smoke not run ===
exit /b 0
