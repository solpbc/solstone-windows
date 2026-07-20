@echo off
:: SPDX-License-Identifier: AGPL-3.0-only
:: Copyright (c) 2026 sol pbc

setlocal enableextensions
set "REPO_ROOT=%~dp0..\.."
set "PREFLIGHT=%REPO_ROOT%\scripts\preflight-toolchain.cmd"
set "FIXTURES=%~dp0fixtures"
set "ORIGINAL_PATH=%PATH%"
set "TEST_ROOT=%TEMP%\solstone-preflight-%RANDOM%-%RANDOM%"
set "CASE_OUTPUT=%TEST_ROOT%\case-output.txt"
set "CASES_PASSED=0"

mkdir "%TEST_ROOT%" >nul 2>&1 || goto setup_failed
mkdir "%TEST_ROOT%\compiler path with spaces" >nul 2>&1 || goto setup_failed
copy /y "%FIXTURES%\rustc-valid\rustc.cmd" "%TEST_ROOT%\compiler path with spaces\rustc.cmd" >nul || goto setup_failed

set "PATH=%FIXTURES%\rustc-wrong-release;%ORIGINAL_PATH%"
set "RUSTC=%FIXTURES%\rustc-valid\rustc.cmd"
call :expect_pass "case 1 override precedence" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "PATH=%FIXTURES%\rustc-valid;%ORIGINAL_PATH%"
set "RUSTC=%FIXTURES%\rustc-wrong-release\rustc.cmd"
call :expect_failure "case 2 explicit wrong release" "9.9.9" "x86_64-pc-windows-msvc" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "PATH=%ORIGINAL_PATH%"
set "RUSTC=%TEST_ROOT%\compiler path with spaces\rustc.cmd"
call :expect_pass "case 3 compiler path with spaces" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "PATH=%FIXTURES%\rustc-valid;%ORIGINAL_PATH%"
set "RUSTC=%TEST_ROOT%\missing compiler\rustc.cmd"
call :expect_failure "case 4 unavailable compiler" "unavailable" "unavailable" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "PATH=%ORIGINAL_PATH%"
set "RUSTC=%FIXTURES%\rustc-wrong-release\rustc.cmd"
call :expect_failure "case 5 wrong release" "9.9.9" "x86_64-pc-windows-msvc" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "RUSTC=%FIXTURES%\rustc-wrong-host\rustc.cmd"
call :expect_failure "case 6 wrong host" "1.96.0" "x86_64-pc-windows-gnu" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "RUSTC=%FIXTURES%\rustc-malformed\rustc.cmd"
call :expect_failure "case 7 malformed output" "unavailable" "unavailable" || goto test_failed
set /a CASES_PASSED+=1 >nul

set "RUSTC=%FIXTURES%\rustc-missing-host\rustc.cmd"
call :expect_failure "case 8 missing host" "1.96.0" "unavailable" || goto test_failed
set /a CASES_PASSED+=1 >nul

call :cleanup
echo preflight-toolchain.test.cmd: 8 cases passed
endlocal
exit /b 0

:expect_pass
call "%PREFLIGHT%" >"%CASE_OUTPUT%" 2>&1
set "CASE_STATUS=%ERRORLEVEL%"
if not "%CASE_STATUS%"=="0" (
  >&2 echo preflight-toolchain.test.cmd: %~1 expected exit 0, actual exit %CASE_STATUS%
  >&2 type "%CASE_OUTPUT%"
  exit /b 1
)
for %%F in ("%CASE_OUTPUT%") do if not "%%~zF"=="0" (
  >&2 echo preflight-toolchain.test.cmd: %~1 expected silent success
  >&2 type "%CASE_OUTPUT%"
  exit /b 1
)
exit /b 0

:expect_failure
call "%PREFLIGHT%" >"%CASE_OUTPUT%" 2>&1
set "CASE_STATUS=%ERRORLEVEL%"
if not "%CASE_STATUS%"=="1" (
  >&2 echo preflight-toolchain.test.cmd: %~1 expected exit 1, actual exit %CASE_STATUS%
  >&2 type "%CASE_OUTPUT%"
  exit /b 1
)
call :require_output "%~1" "Rust toolchain mismatch" || exit /b 1
call :require_output "%~1" "expected release 1.96.0" || exit /b 1
call :require_output "%~1" "actual release %~2" || exit /b 1
call :require_output "%~1" "expected host x86_64-pc-windows-msvc" || exit /b 1
call :require_output "%~1" "actual host %~3" || exit /b 1
call :require_output "%~1" "make rust-toolchain" || exit /b 1
exit /b 0

:require_output
findstr /l /c:"%~2" "%CASE_OUTPUT%" >nul || (
  >&2 echo preflight-toolchain.test.cmd: %~1 missing diagnostic field: %~2
  >&2 type "%CASE_OUTPUT%"
  exit /b 1
)
exit /b 0

:setup_failed
>&2 echo preflight-toolchain.test.cmd: unable to create deterministic test files
goto test_failed

:test_failed
call :cleanup
>&2 echo preflight-toolchain.test.cmd: failed after %CASES_PASSED% cases
endlocal
exit /b 1

:cleanup
if exist "%TEST_ROOT%" rmdir /s /q "%TEST_ROOT%"
exit /b 0
