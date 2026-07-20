@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc

setlocal enableextensions
set "EXPECTED="
set "ACTUAL="

for /f "tokens=3" %%V in ('findstr /b /c:"channel = " rust-toolchain.toml') do if not defined EXPECTED set "EXPECTED=%%~V"
for /f "tokens=2" %%V in ('rustc -Vv 2^>nul ^| findstr /b /c:"release:"') do if not defined ACTUAL set "ACTUAL=%%V"

if not defined EXPECTED set "EXPECTED=unavailable"
if not defined ACTUAL set "ACTUAL=unavailable"

if not "%EXPECTED%"=="%ACTUAL%" (
  >&2 echo ERROR: Rust toolchain mismatch: expected %EXPECTED%, actual %ACTUAL%. Run 'make rust-toolchain'.
  exit /b 1
)

exit /b 0
