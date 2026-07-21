# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Live validation for the native Journal window. VPE-run only on the Windows
# build box; not part of make ci / win-host-ci.
#
# Rerun: make journal-live
# Success marker: JOURNAL_WINDOW_LIVE_OK
# Artifacts: target\journal-window-live\<timestamp>\ contains pairing backup or
# absence marker, mock stdout/stderr, mock-transcript.ndjson, window-evidence.json,
# windows.json, journal.png, and result.txt.
#
# The trigger is the app-native --open-journal control verb. It calls the same
# windows::open_journal path as tray/UI actions; the harness never fetches mock
# dashboard routes directly.
#
# ASCII-only by policy: Windows PowerShell 5.1 reads non-BOM .ps1 in the system
# codepage, so smart punctuation can corrupt and break parsing.

param(
    [int]$TimeoutSecs = 120,
    [string]$OutputDir = "",
    [string]$AppExe = ""
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
if (-not $OutputDir) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutputDir = Join-Path $Root "target\journal-window-live\$stamp"
}
New-Item -Path $OutputDir -ItemType Directory -Force | Out-Null

$Contract = Join-Path $Root "automation-contract.json"
$DriverProject = Join-Path $Root "harness\driver\Driver.csproj"
$DriverPublish = Join-Path $Root "harness\driver\bin\publish"
$DriverExe = Join-Path $DriverPublish "solstone-driver.exe"
$PairingPath = Join-Path $env:LOCALAPPDATA "Solstone\pairing.json"
$PairingBackup = Join-Path $OutputDir "pairing.backup"
$PairingAbsent = Join-Path $OutputDir "pairing.absent"
$Transcript = Join-Path $OutputDir "mock-transcript.ndjson"
$ReadyFile = Join-Path $OutputDir "mock-ready.json"
$MockOut = Join-Path $OutputDir "mock.stdout.log"
$MockErr = Join-Path $OutputDir "mock.stderr.log"
$WindowEvidence = Join-Path $OutputDir "window-evidence.json"
$WindowList = Join-Path $OutputDir "windows.json"
$Screenshot = Join-Path $OutputDir "journal.png"
$Done = Join-Path $OutputDir "DONE"
$Result = Join-Path $OutputDir "result.txt"
$TriggerLog = Join-Path $OutputDir "trigger.log"
$TriggerErr = Join-Path $OutputDir "trigger.err"
$TriggerExit = Join-Path $OutputDir "trigger.exit"
$Session1Out = Join-Path $OutputDir "session1.stdout.log"
$Session1Err = Join-Path $OutputDir "session1.stderr.log"
$Session1Trace = Join-Path $OutputDir "session1.trace"
$Marker = "SOLSTONE_JOURNAL_LIVE_$([guid]::NewGuid().ToString('N'))"
$MockProc = $null
$HadPairing = $false

function ConvertTo-PsSingleQuoted([string]$value) {
    return "'" + $value.Replace("'", "''") + "'"
}

function ConvertTo-CmdArg([string]$value) {
    if ($value -match '[\s"]') {
        return '"' + $value.Replace('"', '\"') + '"'
    }
    return $value
}

function Remove-Task([string]$name) {
    $oldPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        cmd /c "schtasks /Delete /TN `"$name`" /F >NUL 2>NUL" | Out-Null
    } catch {
    } finally {
        $ErrorActionPreference = $oldPreference
    }
}

function Invoke-InSession1([string]$name, [string]$exe, [string]$args) {
    $start = (Get-Date).AddMinutes(5)
    $startDate = $start.ToString("MM/dd/yyyy", [System.Globalization.CultureInfo]::InvariantCulture)
    $startTime = $start.ToString("HH:mm", [System.Globalization.CultureInfo]::InvariantCulture)
    schtasks /Create /TN $name /TR "`"$exe`" $args" /SC ONCE /ST $startTime /SD $startDate /RL LIMITED /IT /F | Out-Null
    schtasks /Run /TN $name | Out-Null
}

function Stop-App {
    Get-Process solstone-windows-app -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Fail([string]$message) {
    Write-Host "JOURNAL_WINDOW_LIVE_FAIL: $message"
    Set-Content -Path $Result -Value "fail: $message" -Encoding ASCII
    throw $message
}

function Resolve-AppExe {
    if ($AppExe -and (Test-Path $AppExe)) {
        return (Resolve-Path $AppExe).Path
    }
    $installed = Join-Path $env:LOCALAPPDATA "Solstone\current\solstone-windows-app.exe"
    if (Test-Path $installed) {
        return (Resolve-Path $installed).Path
    }
    $found = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Solstone") -Recurse -Filter "solstone-windows-app.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) {
        return $found.FullName
    }
    throw "installed app not found under $env:LOCALAPPDATA\Solstone - run make package + install Setup.exe first"
}

function Wait-ForMockReady {
    $deadline = (Get-Date).AddSeconds($TimeoutSecs)
    while ((Get-Date) -lt $deadline) {
        if ($MockProc -and $MockProc.HasExited) {
            throw "mock_journal exited before ready; see $MockOut and $MockErr"
        }
        $stdoutReady = $false
        if (Test-Path $MockOut) {
            $stdoutReady = (Select-String -Path $MockOut -Pattern "MOCK_JOURNAL_READY" -Quiet -ErrorAction SilentlyContinue)
        }
        if ((Test-Path $ReadyFile) -and $stdoutReady) {
            return
        }
        Start-Sleep -Milliseconds 500
    }
    throw "mock_journal did not become ready within $TimeoutSecs seconds"
}

function Wait-ForProcess([string]$name) {
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        if (Get-Process $name -ErrorAction SilentlyContinue) {
            return
        }
        Start-Sleep -Milliseconds 500
    }
    throw "$name did not start"
}

function Test-Transcript {
    if (-not (Test-Path $Transcript)) {
        return "transcript missing"
    }
    $records = @()
    foreach ($line in Get-Content $Transcript -ErrorAction SilentlyContinue) {
        if (-not $line.Trim()) { continue }
        try { $records += ($line | ConvertFrom-Json) } catch {}
    }
    if ($records.Count -eq 0) {
        return "transcript empty"
    }
    $carriers = $records | Group-Object carrier_index
    foreach ($carrier in $carriers) {
        $items = @($carrier.Group)
        $root = @($items | Where-Object { $_.method -eq "GET" -and $_.path -eq "/" -and $_.has_observer_header -eq $true -and $_.has_authorization -eq $true })
        $assetA = @($items | Where-Object { $_.method -eq "GET" -and $_.path -eq "/asset-a" -and $_.has_observer_header -eq $true -and $_.has_authorization -eq $true })
        $assetB = @($items | Where-Object { $_.method -eq "GET" -and $_.path -eq "/asset-b" -and $_.has_observer_header -eq $true -and $_.has_authorization -eq $true })
        if ($root.Count -ge 1 -and $assetA.Count -ge 1 -and $assetB.Count -ge 1) {
            return ""
        }
    }
    return "missing GET / + /asset-a + /asset-b with observer auth on one carrier"
}

Write-Host "journal-live: artifacts = $OutputDir"

try {
    Write-Host "=== stop existing app and back up pairing state ==="
    Stop-App
    Start-Sleep -Seconds 2
    if (Test-Path $PairingPath) {
        $HadPairing = $true
        [System.IO.File]::WriteAllBytes($PairingBackup, [System.IO.File]::ReadAllBytes($PairingPath))
    } else {
        Set-Content -Path $PairingAbsent -Value "absent" -Encoding ASCII
    }

    Write-Host "=== build + publish the net48 driver ==="
    $Dotnet = "dotnet"
    if (-not (Get-Command dotnet -ErrorAction SilentlyContinue)) {
        $cand = "C:\Program Files\dotnet\dotnet.exe"
        if (Test-Path $cand) { $Dotnet = $cand } else { throw "dotnet SDK not found on PATH or at $cand" }
    }
    & $Dotnet publish $DriverProject -c Release -o $DriverPublish
    if ($LASTEXITCODE -ne 0) { throw "driver publish failed" }
    if (-not (Test-Path $DriverExe)) { throw "driver exe not found at $DriverExe" }
    & $DriverExe --selftest
    if ($LASTEXITCODE -ne 0) { throw "driver --selftest failed ($LASTEXITCODE)" }

    $AppExe = Resolve-AppExe
    Write-Host "journal-live: app = $AppExe"
    Write-Host "journal-live: marker = $Marker"

    Write-Host "=== start mock journal ==="
    $Cargo = "cargo"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        $cand = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
        if (Test-Path $cand) { $Cargo = $cand } else { throw "cargo not found on PATH or at $cand" }
    }
    Push-Location $Root
    try {
        & $Cargo build --locked -p pl-transport-win --example mock_journal
        if ($LASTEXITCODE -ne 0) { throw "mock_journal build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
    $MockExe = Join-Path $Root "target\debug\examples\mock_journal.exe"
    if (-not (Test-Path $MockExe)) { throw "mock_journal exe not found at $MockExe" }
    $mockArgs = @(
        "--pairing-out", $PairingPath,
        "--transcript", $Transcript,
        "--ready-file", $ReadyFile,
        "--marker", $Marker
    )
    $mockArgLine = ($mockArgs | ForEach-Object { ConvertTo-CmdArg $_ }) -join " "
    $MockProc = Start-Process -FilePath $MockExe -ArgumentList $mockArgLine -WorkingDirectory $Root -RedirectStandardOutput $MockOut -RedirectStandardError $MockErr -PassThru
    Wait-ForMockReady

    Write-Host "=== launch app in Session 1 ==="
    $LaunchCmd = Join-Path $OutputDir "journal-live-launch.cmd"
    Set-Content -Path $LaunchCmd -Value @("@echo off", "start `"`" /b `"$AppExe`" --from-autostart") -Encoding ASCII
    Invoke-InSession1 "solstone-journal-live-app" $LaunchCmd ""
    Wait-ForProcess "solstone-windows-app"
    Start-Sleep -Seconds 5

    Write-Host "=== trigger journal and capture Session 1 evidence ==="
    $Session1Script = Join-Path $OutputDir "journal-live-session1.ps1"
    $Session1Cmd = Join-Path $OutputDir "journal-live-session1.cmd"
    $prelude = @(
        ("`$AppExe = " + (ConvertTo-PsSingleQuoted $AppExe))
        ("`$DriverExe = " + (ConvertTo-PsSingleQuoted $DriverExe))
        ("`$Contract = " + (ConvertTo-PsSingleQuoted $Contract))
        ("`$TriggerLog = " + (ConvertTo-PsSingleQuoted $TriggerLog))
        ("`$TriggerErr = " + (ConvertTo-PsSingleQuoted $TriggerErr))
        ("`$TriggerExit = " + (ConvertTo-PsSingleQuoted $TriggerExit))
        ("`$Session1Trace = " + (ConvertTo-PsSingleQuoted $Session1Trace))
        ("`$WindowEvidence = " + (ConvertTo-PsSingleQuoted $WindowEvidence))
        ("`$WindowList = " + (ConvertTo-PsSingleQuoted $WindowList))
        ("`$Screenshot = " + (ConvertTo-PsSingleQuoted $Screenshot))
        ("`$Done = " + (ConvertTo-PsSingleQuoted $Done))
        ("`$TimeoutSecs = $TimeoutSecs")
    )
    $body = @'
$ErrorActionPreference = "Continue"
Set-Content -Path $Session1Trace -Value ("started {0:o}" -f (Get-Date)) -Encoding ASCII
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class W {
  public delegate bool Enum(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Rr, B; }
  [DllImport("user32.dll")] public static extern bool EnumWindows(Enum f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int pv, int cb);
}
"@
try { [void][W]::SetProcessDPIAware() } catch {}
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $WindowEvidence) | Out-Null
Remove-Item $Done -Force -ErrorAction SilentlyContinue

& $AppExe --open-journal > $TriggerLog 2> $TriggerErr
("exit={0}" -f $LASTEXITCODE) | Set-Content -Path $TriggerExit -Encoding ASCII
Add-Content -Path $Session1Trace -Value ("trigger exit={0}" -f $LASTEXITCODE) -Encoding ASCII

function Find-AppWindow {
  $pids = @{}; Get-Process solstone-windows-app -EA SilentlyContinue | % { $pids["$($_.Id)"] = $true }
  $script:best = $null; $script:bestArea = -1; $script:bestRect = $null; $script:bestTitle = ""
  $script:bestMin = $false; $script:bestCloaked = $null
  $script:seen = New-Object System.Collections.ArrayList
  $cb = [W+Enum]{ param($h, $l)
    if (-not [W]::IsWindowVisible($h)) { return $true }
    [uint32]$p = 0; [void][W]::GetWindowThreadProcessId($h, [ref]$p)
    if (-not $pids.ContainsKey("$p")) { return $true }
    $r = New-Object W+R; if (-not [W]::GetWindowRect($h, [ref]$r)) { return $true }
    $w = $r.Rr - $r.L; $hh = $r.B - $r.T; $a = $w * $hh
    $title = New-Object System.Text.StringBuilder 256; [void][W]::GetWindowText($h, $title, $title.Capacity)
    $class = New-Object System.Text.StringBuilder 256; [void][W]::GetClassName($h, $class, $class.Capacity)
    $cloaked = 0; $cloakVal = $null
    try { if ([W]::DwmGetWindowAttribute($h, 14, [ref]$cloaked, 4) -eq 0) { $cloakVal = ($cloaked -ne 0) } } catch {}
    [void]$script:seen.Add([ordered]@{
      hwnd = $h.ToInt64(); pid = [int]$p; title = $title.ToString(); class = $class.ToString()
      left = $r.L; top = $r.T; width = $w; height = $hh
      visible = $true; minimized = [W]::IsIconic($h); cloaked = $cloakVal; area = $a
    })
    if ($title.ToString().ToLowerInvariant().IndexOf("settings") -ge 0) { return $true }
    if ($w -le 0 -or $hh -le 0 -or $a -le $script:bestArea) { return $true }
    $script:bestArea = $a; $script:best = $h; $script:bestRect = $r; $script:bestTitle = $title.ToString()
    $script:bestMin = [W]::IsIconic($h); $script:bestCloaked = $cloakVal
    return $true
  }
  [void][W]::EnumWindows($cb, [IntPtr]::Zero)
  if ($script:best) { return @{ H = $script:best; R = $script:bestRect; T = $script:bestTitle; M = $script:bestMin; C = $script:bestCloaked } }
  return $null
}

$deadline = (Get-Date).AddSeconds($TimeoutSecs)
$win = $null
while ((Get-Date) -lt $deadline) {
  $win = Find-AppWindow
  if ($win) {
    $r = $win.R; $w = $r.Rr - $r.L; $hh = $r.B - $r.T
    if ($w -ge 640 -and $hh -ge 480 -and -not $win.M -and $win.C -ne $true) { break }
  }
  Start-Sleep -Milliseconds 500
}

$ok = $false
$evidence = [ordered]@{ title = ""; left = 0; top = 0; width = 0; height = 0; visible = $false; minimized = $false; cloaked = $null; ok = $false; screenshot = $Screenshot }
if ($win) {
  [void][W]::ShowWindow($win.H, 5); [void][W]::SetForegroundWindow($win.H); Start-Sleep -Milliseconds 900
  $win = Find-AppWindow
  if ($win) {
    $r = $win.R; $w = $r.Rr - $r.L; $hh = $r.B - $r.T
    $ok = ($w -ge 640 -and $hh -ge 480 -and -not $win.M -and $win.C -ne $true)
    $evidence.title = $win.T; $evidence.left = $r.L; $evidence.top = $r.T
    $evidence.width = $w; $evidence.height = $hh; $evidence.visible = $true
    $evidence.minimized = $win.M; $evidence.cloaked = $win.C; $evidence.ok = $ok
    if ($w -gt 0 -and $hh -gt 0) {
      $bmp = New-Object System.Drawing.Bitmap($w, $hh); $g = [System.Drawing.Graphics]::FromImage($bmp)
      try { $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size); $bmp.Save($Screenshot, [System.Drawing.Imaging.ImageFormat]::Png) }
      finally { $g.Dispose(); $bmp.Dispose() }
    }
  }
}
$evidence | ConvertTo-Json -Depth 4 | Set-Content -Path $WindowEvidence -Encoding ASCII
if ($script:seen) {
  $script:seen | Sort-Object area -Descending | ConvertTo-Json -Depth 4 | Set-Content -Path $WindowList -Encoding ASCII
}
Set-Content -Path $Done -Value ($(if ($ok) { "ok" } else { "fail" })) -Encoding ASCII
Add-Content -Path $Session1Trace -Value ("done ok={0}" -f $ok) -Encoding ASCII
'@
    Set-Content -Path $Session1Script -Value ($prelude + ($body -split '\r?\n')) -Encoding ASCII
    Set-Content -Path $Session1Cmd -Value @("@echo off", "powershell -NoProfile -ExecutionPolicy Bypass -File `"$Session1Script`" > `"$Session1Out`" 2> `"$Session1Err`"") -Encoding ASCII
    Invoke-InSession1 "solstone-journal-live-trigger" $Session1Cmd ""

    $deadline = (Get-Date).AddSeconds($TimeoutSecs + 20)
    while ((Get-Date) -lt $deadline -and -not (Test-Path $Done)) {
        Start-Sleep -Seconds 1
    }
    if (-not (Test-Path $Done)) {
        Fail "Session 1 trigger/capture timed out (no DONE marker)"
    }
    $doneText = Get-Content $Done | Select-Object -First 1
    if ($doneText -ne "ok") {
        if (Test-Path $WindowEvidence) { Get-Content $WindowEvidence | Write-Host }
        Fail "journal window was not visible and normal-sized"
    }

    Write-Host "=== assert mock transcript causation ==="
    $deadline = (Get-Date).AddSeconds($TimeoutSecs)
    $transcriptError = "not checked"
    while ((Get-Date) -lt $deadline) {
        $transcriptError = Test-Transcript
        if (-not $transcriptError) { break }
        Start-Sleep -Seconds 1
    }
    if ($transcriptError) {
        if (Test-Path $Transcript) { Get-Content $Transcript | Write-Host }
        Fail $transcriptError
    }

    Set-Content -Path $Result -Value "ok" -Encoding ASCII
    Write-Host "JOURNAL_WINDOW_LIVE_OK"
    Write-Host "journal-live: screenshot = $Screenshot"
    Write-Host "journal-live: evidence = $WindowEvidence"
    exit 0
}
finally {
    Write-Host "=== teardown ==="
    Stop-App
    Start-Sleep -Seconds 1
    if ($HadPairing) {
        New-Item -Path (Split-Path -Parent $PairingPath) -ItemType Directory -Force | Out-Null
        Copy-Item -Path $PairingBackup -Destination $PairingPath -Force
    } else {
        Remove-Item -Path $PairingPath -Force -ErrorAction SilentlyContinue
    }
    if ($MockProc -and -not $MockProc.HasExited) {
        Stop-Process -Id $MockProc.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Task "solstone-journal-live-app"
    Remove-Task "solstone-journal-live-trigger"
}
