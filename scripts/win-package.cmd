@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc
::
:: Thin build-box bootstrap for the source-bound release transaction. All
:: preflight, build, pack, signing, evidence, and promotion work is owned by
:: scripts\package.ps1 and its single xtask finalizer invocation.
:: In signed mode the finalizer constructs child environments from the pinned
:: selection record; MSVC/vcvars activation is not what supplies SignTool.
setlocal enableextensions
cd /d "%~dp0.." || exit /b 1

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
if not defined EXPECTED_RELEASE_COMMIT (
  echo ERROR: EXPECTED_RELEASE_COMMIT is required; set the full lowercase 40-hex release commit and retry. 1>&2
  exit /b 1
)
if not defined SOLSTONE_ADVISORY_TREE_SHA256 (
  echo ERROR: SOLSTONE_ADVISORY_TREE_SHA256 is required; supply the reviewed isolated RustSec archive digest and retry. 1>&2
  exit /b 1
)
set "SIGN_ARG="
if defined SOLSTONE_SIGN if not "%SOLSTONE_SIGN%"=="1" (
  echo ERROR: SOLSTONE_SIGN must be exactly 1 when signing is requested; unset it for unsigned finalization and retry. 1>&2
  exit /b 1
)
if "%SOLSTONE_SIGN%"=="1" set "SIGN_ARG=-Sign"

powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 %SIGN_ARG%
if errorlevel 1 exit /b 1

echo === WIN_PACKAGE_OK: source-bound release candidate transaction passed ===
exit /b 0
