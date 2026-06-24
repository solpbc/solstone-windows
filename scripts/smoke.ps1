# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# FlaUI smoke against the *installed* observer. Runs on the Windows build box only
# (live target). Two stages:
#   1. Build + publish the net48 FlaUI driver (Accessibility.dll in the publish
#      layout; see ../harness/README.md).
#   2. Launch the installed app in interactive Session 1.
#   3. Run the deterministic health/render driver directly in Session 0, then run
#      the FlaUI/UIA Tier 1 pass separately as advisory.
#
# The deterministic gate is the health oracle (Tier 0): poll /healthz until
# app_state reaches the contract's `observing` token, then require the per-view
# render beacon (Tier R). Tier 1 drives native chrome by AutomationId, but it is
# advisory and never decides SMOKE_OK/SMOKE_FAIL.
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
    [int]$Tier1TimeoutSecs = 15,
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
    $start = (Get-Date).AddMinutes(5)
    $startDate = $start.ToString("MM/dd/yyyy", [System.Globalization.CultureInfo]::InvariantCulture)
    $startTime = $start.ToString("HH:mm", [System.Globalization.CultureInfo]::InvariantCulture)
    schtasks /Create /TN $name /TR "`"$exe`" $args" /SC ONCE /ST $startTime /SD $startDate /RL LIMITED /IT /F | Out-Null
    schtasks /Run /TN $name | Out-Null
}
function Remove-Task([string]$name) { schtasks /Delete /TN $name /F 2>$null | Out-Null }
function ConvertTo-PsSingleQuoted([string]$value) { return "'" + $value.Replace("'", "''") + "'" }

# Launch the observer in Session 1 with --open-view settings so the Settings
# webview actually opens -- the Tier-R render gate then polls /healthz for the
# per-view render beacon (views.settings == rendered). The single-instance mutex
# makes a second launch a no-op, so the harness must own a clean slate: any prior
# instance is killed below before launch (a stray holder would swallow the arg and
# the gate would time out, indistinguishable from a render failure).
Write-Host "=== ensure no prior instance holds the single-instance mutex ==="
# Get-Process|Stop-Process, NOT taskkill: under $ErrorActionPreference='Stop' a
# native command's stderr (taskkill's "process not found" when none is running)
# becomes a terminating NativeCommandError and aborts the smoke.
Get-Process solstone-windows-app -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Write-Host "=== launch observer in Session 1 (--open-view settings) ==="
$AppLaunchCmd = Join-Path $env:TEMP "solstone-smoke-app.cmd"
if (Test-Path $AppLaunchCmd) { Remove-Item $AppLaunchCmd -Force }
Set-Content -Path $AppLaunchCmd -Value @("@echo off", "`"$AppExe`" --open-view settings") -Encoding ASCII
Invoke-InSession1 "solstone-smoke-app" $AppLaunchCmd ""

$GateArgs = @(
    "--contract", $Contract,
    "--health-url", $HealthUrl,
    "--timeout-secs", "$TimeoutSecs",
    "--render-view", "settings",
    "--skip-tier1"
)
if ($FailInject) {
    # Wait for observing, then kill system audio (stop the audio service), then
    # run the driver in --fail-inject mode to assert the honest drop.
    Write-Host "=== fail-inject: waiting for observing, then stopping Windows Audio ==="
    & $DriverExe @GateArgs
    if ($LASTEXITCODE -ne 0) { throw "precondition (reach observing + render) failed ($LASTEXITCODE)" }
    try { Stop-Service -Name Audiosrv -Force -ErrorAction Stop; Write-Host "stopped Audiosrv" }
    catch { Write-Warning "could not stop Audiosrv ($_): live fail-injection needs privilege; selftest already proved the decision logic"; Remove-Task "solstone-smoke-app"; exit 0 }
    $GateArgs += "--fail-inject"
}

# Run the load-bearing gate directly in Session 0. /healthz is loopback-reachable
# from SSH, so the gate can use the actual driver process exit instead of racing
# Task Scheduler's transient Last Result (0x00041301 = still running).
Write-Host "=== run health/render gate in Session 0 ==="
& $DriverExe @GateArgs
$GateExit = $LASTEXITCODE

# Restore audio if we stopped it.
if ($FailInject) { try { Start-Service -Name Audiosrv } catch {} }

if ($GateExit -ne 0) {
    Remove-Task "solstone-smoke-app"
    Write-Host "SMOKE_FAIL (gate exit $GateExit)"
    exit $GateExit
}

if (-not $FailInject) {
    Write-Host "=== run Tier 1 FlaUI advisory in Session 1 ==="
    $Tier1Log = Join-Path $env:TEMP "solstone-smoke-tier1.log"
    $Tier1Err = Join-Path $env:TEMP "solstone-smoke-tier1.err"
    $Tier1Result = Join-Path $env:TEMP "solstone-smoke-tier1.exit"
    $Tier1Script = Join-Path $env:TEMP "solstone-smoke-tier1.ps1"
    foreach ($path in @($Tier1Log, $Tier1Err, $Tier1Result, $Tier1Script)) {
        if (Test-Path $path) { Remove-Item $path -Force }
    }

    $Tier1Lines = @(
        '$ErrorActionPreference = "Continue"',
        '$p = Start-Process -FilePath ' + (ConvertTo-PsSingleQuoted $DriverExe) + ' -ArgumentList @("--contract",' + (ConvertTo-PsSingleQuoted $Contract) + ',"--tier1-only","--tier1-timeout-secs",' + (ConvertTo-PsSingleQuoted "$Tier1TimeoutSecs") + ') -Wait -PassThru -RedirectStandardOutput ' + (ConvertTo-PsSingleQuoted $Tier1Log) + ' -RedirectStandardError ' + (ConvertTo-PsSingleQuoted $Tier1Err),
        '("exit={0}" -f $p.ExitCode) | Set-Content -Path ' + (ConvertTo-PsSingleQuoted $Tier1Result) + ' -Encoding ASCII'
    )
    Set-Content -Path $Tier1Script -Value $Tier1Lines -Encoding ASCII
    Invoke-InSession1 "solstone-smoke-tier1" "powershell.exe" "-NoProfile -ExecutionPolicy Bypass -File `"$Tier1Script`""

    $deadline = (Get-Date).AddSeconds($Tier1TimeoutSecs + 20)
    while ((Get-Date) -lt $deadline -and -not (Test-Path $Tier1Result)) {
        Start-Sleep -Seconds 1
    }

    if (Test-Path $Tier1Log) { Write-Host "--- tier1 advisory log ---"; Get-Content $Tier1Log; Write-Host "--- end tier1 advisory log ---" }
    if (Test-Path $Tier1Err) {
        $err = Get-Content $Tier1Err
        if ($err) { Write-Host "--- tier1 advisory stderr ---"; $err; Write-Host "--- end tier1 advisory stderr ---" }
    }
    if (Test-Path $Tier1Result) {
        $tier1Exit = Get-Content $Tier1Result | Select-Object -First 1
        if ($tier1Exit -ne "exit=0") { Write-Warning "Tier 1 advisory did not pass ($tier1Exit); health/render gate already passed" }
    } else {
        Write-Warning "Tier 1 advisory did not finish before the advisory timeout; health/render gate already passed"
        schtasks /End /TN "solstone-smoke-tier1" 2>$null | Out-Null
    }
    Remove-Task "solstone-smoke-tier1"
}

Remove-Task "solstone-smoke-app"

Write-Host "SMOKE_OK"
exit 0
