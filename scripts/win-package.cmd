@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Thin build-box bootstrap for the source-bound release transaction. All
:: preflight, build, pack, signing, evidence, and promotion work is owned by
:: scripts\package.ps1 and its single xtask finalizer invocation.
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "SIGN_ARG="
if defined SOLSTONE_SIGN set "SIGN_ARG=-Sign"

powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 %SIGN_ARG%
if errorlevel 1 exit /b 1

echo === WIN_PACKAGE_OK: source-bound release candidate transaction passed ===
exit /b 0
