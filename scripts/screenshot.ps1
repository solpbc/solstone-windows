# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Capture Settings/About screenshots (light + dark) of the Windows observer for
# visual validation. Live target only (the build box).
#
# WHY THIS SHAPE: window enumeration and screen capture must run INSIDE the
# interactive Session 1 -- an SSH/Session-0 process cannot see Session 1's windows
# or composited desktop (it gets a Win32Exception / empty window list, the way the
# FlaUI smoke can only reach the app over loopback /healthz). So this script is a
# Session-0 DRIVER that generates a self-contained capture routine, schedules it
# into Session 1 (the validated scheduled-task mechanism), and collects the PNGs.
# The capture finds the app window by PID + largest-visible (robust to a missing/
# late window title), brings it to the foreground, and grabs its rect via
# CopyFromScreen so DWM compositing (Mica) is captured.
#
# ASCII-only by policy: Windows PowerShell 5.1 reads non-BOM .ps1 in the system
# codepage, so smart punctuation can corrupt and break parsing.

param(
    [int]$TimeoutSecs = 90,
    [string]$OutputDir = "",
    # Override the app exe (default: the installed Velopack app). Lets a session
    # capture a freshly-built dev exe (target\release|debug\...) for pre-release
    # visual iteration, not only the installed release.
    [string]$AppExe = ""
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
if (-not $OutputDir) { $OutputDir = Join-Path $Root "target\screenshots" }
New-Item -Path $OutputDir -ItemType Directory -Force | Out-Null

# Resolve the app exe (override -> installed -> recursive search).
if (-not ($AppExe -and (Test-Path $AppExe))) {
    $AppExe = Join-Path $env:LOCALAPPDATA "Solstone\current\solstone-windows-app.exe"
    if (-not (Test-Path $AppExe)) {
        $found = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Solstone") -Recurse -Filter "solstone-windows-app.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) { $AppExe = $found.FullName }
        else { throw "app exe not found - pass -AppExe <path> or install the Velopack app first" }
    }
}
$AppExe = (Resolve-Path $AppExe).Path
Write-Host "screenshot: app = $AppExe"
Write-Host "screenshot: out = $OutputDir"

# The Session-1 capture routine (literal body; params injected as a prelude so the
# body needs no escaping). Runs entirely inside Session 1.
$body = @'
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class W {
  public delegate bool Enum(IntPtr h, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Rr, B; }
  [DllImport("user32.dll")] public static extern bool EnumWindows(Enum f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
try { [void][W]::SetProcessDPIAware() } catch {}
$tk = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
$health = "http://127.0.0.1:49247/healthz"
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
Remove-Item "$OutputDir\DONE" -Force -ErrorAction SilentlyContinue

function Find-AppWindow {
  $pids = @{}; Get-Process solstone-windows-app -EA SilentlyContinue | % { $pids["$($_.Id)"] = $true }
  $script:best = $null; $script:bestArea = -1; $script:bestRect = $null
  $cb = [W+Enum]{ param($h, $l)
    if (-not [W]::IsWindowVisible($h)) { return $true }
    [uint32]$p = 0; [void][W]::GetWindowThreadProcessId($h, [ref]$p)
    if (-not $pids.ContainsKey("$p")) { return $true }
    $r = New-Object W+R; if (-not [W]::GetWindowRect($h, [ref]$r)) { return $true }
    $w = $r.Rr - $r.L; $hh = $r.B - $r.T; $a = $w * $hh
    if ($w -gt 0 -and $hh -gt 0 -and $a -gt $script:bestArea) { $script:bestArea = $a; $script:best = $h; $script:bestRect = $r }
    return $true
  }
  [void][W]::EnumWindows($cb, [IntPtr]::Zero)
  if ($script:best) { return @{ H = $script:best; R = $script:bestRect } }
  return $null
}

foreach ($t in @(@{n = "light"; v = 1 }, @{n = "dark"; v = 0 })) {
  New-Item -Path $tk -Force | Out-Null
  New-ItemProperty -Path $tk -Name "AppsUseLightTheme" -Value $t.v -PropertyType DWord -Force | Out-Null
  New-ItemProperty -Path $tk -Name "SystemUsesLightTheme" -Value $t.v -PropertyType DWord -Force | Out-Null
  foreach ($v in @("settings", "about")) {
    Get-Process solstone-windows-app -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    Start-Sleep -Seconds 2
    Start-Process $AppExe -ArgumentList "--open-view", $v
    $dl = (Get-Date).AddSeconds($TimeoutSecs)
    while ((Get-Date) -lt $dl) {
      try { $h = Invoke-RestMethod $health -TimeoutSec 2; if ($h.views."$v" -eq "rendered") { break } } catch {}
      Start-Sleep -Milliseconds 500
    }
    Start-Sleep -Seconds 2
    $win = Find-AppWindow
    if ($win) {
      [void][W]::ShowWindow($win.H, 5); [void][W]::SetForegroundWindow($win.H); Start-Sleep -Milliseconds 900
      $win = Find-AppWindow; $r = $win.R; $w = $r.Rr - $r.L; $hh = $r.B - $r.T
      $bmp = New-Object System.Drawing.Bitmap($w, $hh); $g = [System.Drawing.Graphics]::FromImage($bmp)
      try { $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size); $bmp.Save("$OutputDir\$v-$($t.n).png", [System.Drawing.Imaging.ImageFormat]::Png) }
      finally { $g.Dispose(); $bmp.Dispose() }
    }
  }
}
Get-Process solstone-windows-app -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Set-Content "$OutputDir\DONE" -Value "ok" -Encoding ASCII
'@

$prelude = "`$AppExe = '$AppExe'`r`n`$OutputDir = '$OutputDir'`r`n`$TimeoutSecs = $TimeoutSecs`r`n"
$s1 = Join-Path $env:TEMP "solstone-capture-session1.ps1"
$s1cmd = Join-Path $env:TEMP "solstone-capture-session1.cmd"
Set-Content -Path $s1 -Value ($prelude + $body) -Encoding ASCII
Set-Content -Path $s1cmd -Value @("@echo off", "start `"`" /b powershell -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$s1`"") -Encoding ASCII

$TaskName = "solstone-screenshot"
Remove-Item "$OutputDir\DONE" -Force -ErrorAction SilentlyContinue
$start = (Get-Date).AddMinutes(5)
schtasks /Create /TN $TaskName /TR "`"$s1cmd`"" /SC ONCE /ST $start.ToString("HH:mm") /SD $start.ToString("MM/dd/yyyy", [System.Globalization.CultureInfo]::InvariantCulture) /RL LIMITED /IT /F | Out-Null
schtasks /Run /TN $TaskName | Out-Null

# Poll for the DONE marker the Session-1 routine writes (4 captures ~ 30-40s).
$deadline = (Get-Date).AddSeconds(($TimeoutSecs * 4) + 60)
while ((Get-Date) -lt $deadline -and -not (Test-Path "$OutputDir\DONE")) { Start-Sleep -Seconds 2 }
schtasks /Delete /TN $TaskName /F 2>$null | Out-Null
if (-not (Test-Path "$OutputDir\DONE")) { throw "capture did not finish before deadline (no DONE marker in $OutputDir)" }

Write-Host "SCREENSHOT_OK"
Get-ChildItem "$OutputDir\*.png" | ForEach-Object { Write-Host $_.FullName }
