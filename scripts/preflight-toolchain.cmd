@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc

setlocal enableextensions
set "EXPECTED_RELEASE="
set "EXPECTED_HOST=x86_64-pc-windows-msvc"
set "ACTUAL_RELEASE="
set "ACTUAL_HOST="
set "RUSTC_TEMP_ROOT=%TEMP%\solstone-rustc-vv-%RANDOM%-%RANDOM%"
set "RUSTC_OUTPUT=%RUSTC_TEMP_ROOT%\rustc-vv.txt"

if defined RUSTC (
  set "RUSTC_BIN=%RUSTC%"
) else (
  set "RUSTC_BIN=rustc"
)

for /f "tokens=3" %%V in ('findstr /b /c:"channel = " "%~dp0..\rust-toolchain.toml"') do if not defined EXPECTED_RELEASE set "EXPECTED_RELEASE=%%~V"

if exist "%RUSTC_TEMP_ROOT%" goto rustc_unavailable
mkdir "%RUSTC_TEMP_ROOT%" >nul 2>&1 || goto rustc_unavailable
call "%RUSTC_BIN%" -Vv >"%RUSTC_OUTPUT%" 2>nul
set "RUSTC_STATUS=%ERRORLEVEL%"
for /f "tokens=2" %%V in ('findstr /b /c:"release:" "%RUSTC_OUTPUT%" 2^>nul') do if not defined ACTUAL_RELEASE set "ACTUAL_RELEASE=%%V"
for /f "tokens=2" %%V in ('findstr /b /c:"host:" "%RUSTC_OUTPUT%" 2^>nul') do if not defined ACTUAL_HOST set "ACTUAL_HOST=%%V"
rmdir /s /q "%RUSTC_TEMP_ROOT%" >nul 2>&1
goto rustc_output_ready

:rustc_unavailable
set "RUSTC_STATUS=1"

:rustc_output_ready

if not defined EXPECTED_RELEASE set "EXPECTED_RELEASE=unavailable"
if not defined ACTUAL_RELEASE set "ACTUAL_RELEASE=unavailable"
if not defined ACTUAL_HOST set "ACTUAL_HOST=unavailable"

if not "%RUSTC_STATUS%"=="0" goto toolchain_mismatch
if "%EXPECTED_RELEASE%"=="unavailable" goto toolchain_mismatch
if "%ACTUAL_RELEASE%"=="unavailable" goto toolchain_mismatch
if "%ACTUAL_HOST%"=="unavailable" goto toolchain_mismatch
if not "%EXPECTED_RELEASE%"=="%ACTUAL_RELEASE%" goto toolchain_mismatch
if not "%EXPECTED_HOST%"=="%ACTUAL_HOST%" goto toolchain_mismatch

exit /b 0

:toolchain_mismatch
>&2 echo ERROR: Rust toolchain mismatch: expected release %EXPECTED_RELEASE%, actual release %ACTUAL_RELEASE%; expected host %EXPECTED_HOST%, actual host %ACTUAL_HOST%. Run 'make rust-toolchain'.
exit /b 1
