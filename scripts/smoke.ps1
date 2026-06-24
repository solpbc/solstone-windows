# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# FlaUI smoke against the *installed* observer. Runs on the Windows build box only
# (live target). Two stages:
#   1. Build + publish the net48 FlaUI driver (Accessibility.dll in the publish
#      layout; see ../harness/README.md).
#   2. Launch the installed app and run the driver in interactive Session 1 (FlaUI
#      UIA needs the interactive desktop), then assert on the health dump.
#
# The deterministic gate is the health oracle (Tier 0): poll /healthz until
# app_state reaches the contract's `observing` token. Tier 1 drives the native
# tray chrome by AutomationId. The webview DOM (Tier 2) is never load-bearing.
#
# -FailInject: after the app reaches observing, stop the Windows Audio service so
# the (required) system-audio source faults, then assert the observer honestly
# leaves observing. Needs privilege to stop the service; if unavailable, the
# driver's --selftest still proves the drop-detection logic.
#
# ASCII-only by policy: Windows PowerShell 5.1 reads non-BOM .ps1 in the system
# codepage, so smart punctuation can corrupt and break parsing.

param(
    [switch]$FailInject,
    [int]$TimeoutSecs = 90,
    [switch]$SelftestOnly
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
$Driver = Join-Path $Root "harness\driver\Driver.csproj"
$Contract = Join-Path $Root "automation-contract.json"
$Publish = Join-Path $Root "harness\driver\bin\publish"
$HealthUrl = "http://127.0.0.1:49247/healthz"

# Resolve the dotnet SDK host (PATH first, then the default install location;
# a non-interactive SSH PATH may not carry it).
$Dotnet = "dotnet"
if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
    $cand = "C:\Program Files\dotnet\dotnet.exe"
    if (Test-Path $cand) { $Dotnet = $cand } else { throw "dotnet SDK not found on PATH or at $cand" }
}

Write-Host "=== build + publish the net48 driver ==="
& $Dotnet publish $Driver -c Release -o $Publish
if ($LASTEXITCODE -ne 0) { throw "driver publish failed" }
$DriverExe = Join-Path $Publish "solstone-driver.exe"
if (-not (Test-Path $DriverExe)) { throw "driver exe not found at $DriverExe" }

# Stage 0: pure-logic selftest (no live target) - proves the driver + its
# contract-parse / token-match / fail-inject decision logic.
Write-Host "=== driver --selftest (pure logic) ==="
& $DriverExe --selftest
if ($LASTEXITCODE -ne 0) { throw "driver --selftest failed ($LASTEXITCODE)" }
if ($SelftestOnly) { Write-Host "SMOKE_SELFTEST_OK"; exit 0 }

# Locate the installed app (Velopack per-user layout).
$AppExe = Join-Path $env:LOCALAPPDATA "Solstone\current\solstone-windows-app.exe"
if (-not (Test-Path $AppExe)) {
    $found = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Solstone") -Recurse -Filter "solstone-windows-app.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) { $AppExe = $found.FullName } else { throw "installed app not found under $env:LOCALAPPDATA\Solstone - run make package + install Setup.exe first" }
}
Write-Host "app: $AppExe"

# Helper: register + fire a low-privilege scheduled task into the interactive
# Session 1 (the validated mechanism; an SSH/Session-0 process cannot start a GUI
# or drive UIA in Session 1 directly).
function Invoke-InSession1([string]$name, [string]$exe, [string]$args) {
    schtasks /Create /TN $name /TR "`"$exe`" $args" /SC ONCE /ST 00:00 /RL LIMITED /IT /F | Out-Null
    schtasks /Run /TN $name | Out-Null
}
function Remove-Task([string]$name) { schtasks /Delete /TN $name /F 2>$null | Out-Null }

# Launch the observer in Session 1 with --open-view settings so the Settings
# webview actually opens -- the Tier-R render gate then polls /healthz for the
# per-view render beacon (views.settings == rendered). The single-instance mutex
# makes a second launch a no-op, so the harness must own a clean slate: any prior
# instance is killed below before launch (a stray holder would swallow the arg and
# the gate would time out, indistinguishable from a render failure).
Write-Host "=== ensure no prior instance holds the single-instance mutex ==="
taskkill /IM solstone-windows-app.exe /F 2>$null | Out-Null
Start-Sleep -Seconds 2
Write-Host "=== launch observer in Session 1 (--open-view settings) ==="
Invoke-InSession1 "solstone-smoke-app" $AppExe "--open-view settings"

$DriverLog = Join-Path $env:TEMP "solstone-smoke-driver.log"
if (Test-Path $DriverLog) { Remove-Item $DriverLog -Force }

$DriverArgs = "--contract `"$Contract`" --health-url $HealthUrl --timeout-secs $TimeoutSecs --render-view settings"
if ($FailInject) {
    # Wait for observing, then kill system audio (stop the audio service), then
    # run the driver in --fail-inject mode to assert the honest drop.
    Write-Host "=== fail-inject: waiting for observing, then stopping Windows Audio ==="
    & $DriverExe --contract $Contract --health-url $HealthUrl --timeout-secs $TimeoutSecs
    if ($LASTEXITCODE -ne 0) { throw "precondition (reach observing) failed ($LASTEXITCODE)" }
    try { Stop-Service -Name Audiosrv -Force -ErrorAction Stop; Write-Host "stopped Audiosrv" }
    catch { Write-Warning "could not stop Audiosrv ($_): live fail-injection needs privilege; selftest already proved the decision logic"; Remove-Task "solstone-smoke-app"; exit 0 }
    $DriverArgs = "$DriverArgs --fail-inject"
}

# Run the driver in Session 1 so FlaUI UIA can see the interactive desktop.
Write-Host "=== run FlaUI driver in Session 1 ==="
$DriverWrap = "/c `"`"$DriverExe`" $DriverArgs > `"$DriverLog`" 2>&1`""
Invoke-InSession1 "solstone-smoke-driver" "cmd.exe" $DriverWrap

# Wait for the driver task to finish + surface its log/exit.
$deadline = (Get-Date).AddSeconds($TimeoutSecs + 30)
do {
    Start-Sleep -Seconds 3
    $info = schtasks /Query /TN "solstone-smoke-driver" /FO LIST /V 2>$null | Select-String "Status:|Last Result:"
} while ((Get-Date) -lt $deadline -and ($info -match "Running"))

if (Test-Path $DriverLog) { Write-Host "--- driver log ---"; Get-Content $DriverLog; Write-Host "--- end driver log ---" }

# Restore audio if we stopped it.
if ($FailInject) { try { Start-Service -Name Audiosrv } catch {} }

# The Last Result of the driver scheduled task is the driver's exit code.
$last = (schtasks /Query /TN "solstone-smoke-driver" /FO LIST /V 2>$null | Select-String "Last Result:")
Remove-Task "solstone-smoke-driver"
Remove-Task "solstone-smoke-app"

if ($last -match "Last Result:\s+(\d+)") {
    $code = [int]$Matches[1]
    if ($code -eq 0) { Write-Host "SMOKE_OK"; exit 0 }
    Write-Host "SMOKE_FAIL (driver exit $code)"; exit $code
}
Write-Host "SMOKE_FAIL (could not read driver result)"; exit 1
