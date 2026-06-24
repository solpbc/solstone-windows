# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Capture composited Settings/About screenshots on the Windows build box. Live
# target only: launches the installed app in interactive Session 1, waits for the
# earned render beacon on /healthz, then captures the DWM-composited window rect.
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
$HealthUrl = "http://127.0.0.1:49247/healthz"
$TaskName = "solstone-screenshot-app"
$ThemeKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"

if (-not $OutputDir) {
    $OutputDir = Join-Path $Root "target\screenshots"
}
New-Item -Path $OutputDir -ItemType Directory -Force | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class SolstoneWindowNative
{
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [StructLayout(LayoutKind.Sequential)]
    public struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll", SetLastError = true)]
    public static extern bool GetWindowRect(IntPtr hWnd, out Rect lpRect);

    [DllImport("user32.dll")]
    public static extern bool SetProcessDPIAware();
}
"@

try { [void][SolstoneWindowNative]::SetProcessDPIAware() } catch {}

function Invoke-InSession1([string]$name, [string]$exe, [string]$args) {
    $start = (Get-Date).AddMinutes(5)
    $startDate = $start.ToString("MM/dd/yyyy", [System.Globalization.CultureInfo]::InvariantCulture)
    $startTime = $start.ToString("HH:mm", [System.Globalization.CultureInfo]::InvariantCulture)
    schtasks /Create /TN $name /TR "`"$exe`" $args" /SC ONCE /ST $startTime /SD $startDate /RL LIMITED /IT /F | Out-Null
    schtasks /Run /TN $name | Out-Null
}

function Remove-Task([string]$name) {
    schtasks /Delete /TN $name /F 2>$null | Out-Null
}

function Stop-AppInstance() {
    Get-Process solstone-windows-app -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
}

function Set-AppTheme([int]$value) {
    New-Item -Path $ThemeKey -Force | Out-Null
    New-ItemProperty -Path $ThemeKey -Name "AppsUseLightTheme" -Value $value -PropertyType DWord -Force | Out-Null
}

function Wait-ViewRendered([string]$view) {
    $deadline = (Get-Date).AddSeconds($TimeoutSecs)
    while ((Get-Date) -lt $deadline) {
        try {
            $health = Invoke-RestMethod -Uri $HealthUrl -TimeoutSec 2
            if ($health -and $health.views) {
                $prop = $health.views.PSObject.Properties[$view]
                if ($prop -and $prop.Value -eq "rendered") {
                    return
                }
            }
        } catch {}
        Start-Sleep -Milliseconds 500
    }
    throw "view '$view' never reached rendered within $TimeoutSecs seconds"
}

function Get-AppExe() {
    if ($AppExe -and (Test-Path $AppExe)) {
        return (Resolve-Path $AppExe).Path
    }

    $appExe = Join-Path $env:LOCALAPPDATA "Solstone\current\solstone-windows-app.exe"
    if (Test-Path $appExe) {
        return $appExe
    }

    $found = Get-ChildItem (Join-Path $env:LOCALAPPDATA "Solstone") -Recurse -Filter "solstone-windows-app.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) {
        return $found.FullName
    }

    throw "installed app not found under $env:LOCALAPPDATA\Solstone - run make package + install Setup.exe first"
}

function Get-TargetWindowRect([string]$view) {
    $processes = @(Get-Process solstone-windows-app -ErrorAction SilentlyContinue)
    if ($processes.Count -eq 0) {
        throw "solstone-windows-app process not found"
    }

    $pidSet = @{}
    foreach ($process in $processes) {
        $pidSet["$($process.Id)"] = $true
    }

    $matches = New-Object System.Collections.Generic.List[object]
    $callback = [SolstoneWindowNative+EnumWindowsProc]{
        param([IntPtr]$hWnd, [IntPtr]$lParam)

        if (-not [SolstoneWindowNative]::IsWindowVisible($hWnd)) {
            return $true
        }

        [uint32]$windowPid = 0
        [void][SolstoneWindowNative]::GetWindowThreadProcessId($hWnd, [ref]$windowPid)
        if (-not $pidSet.ContainsKey("$windowPid")) {
            return $true
        }

        $titleBuffer = New-Object System.Text.StringBuilder 256
        [void][SolstoneWindowNative]::GetWindowText($hWnd, $titleBuffer, $titleBuffer.Capacity)
        $title = $titleBuffer.ToString()
        if (-not $title.ToLowerInvariant().Contains($view)) {
            return $true
        }

        $rect = New-Object SolstoneWindowNative+Rect
        if (-not [SolstoneWindowNative]::GetWindowRect($hWnd, [ref]$rect)) {
            return $true
        }
        if (($rect.Right -le $rect.Left) -or ($rect.Bottom -le $rect.Top)) {
            return $true
        }

        $matches.Add([pscustomobject]@{
            Handle = $hWnd
            Title = $title
            Rect = $rect
        }) | Out-Null
        return $true
    }

    [void][SolstoneWindowNative]::EnumWindows($callback, [IntPtr]::Zero)
    if ($matches.Count -eq 0) {
        throw "no visible '$view' window found for solstone-windows-app"
    }

    return $matches[0].Rect
}

function Save-WindowScreenshot([object]$rect, [string]$path) {
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) {
        throw "invalid window bounds ${width}x${height}"
    }

    $bitmap = New-Object System.Drawing.Bitmap $width, $height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

$appExe = Get-AppExe
$launchCmd = Join-Path $env:TEMP "solstone-screenshot-app.cmd"
$hadOriginalTheme = $false
$originalTheme = $null
$paths = @()

try {
    try {
        $existingTheme = Get-ItemProperty -Path $ThemeKey -Name "AppsUseLightTheme" -ErrorAction Stop
        $originalTheme = [int]$existingTheme.AppsUseLightTheme
        $hadOriginalTheme = $true
    } catch {}

    $views = @("settings", "about")
    $themes = @(
        @{ Name = "light"; Value = 1 },
        @{ Name = "dark"; Value = 0 }
    )

    foreach ($view in $views) {
        foreach ($theme in $themes) {
            Stop-AppInstance
            Set-AppTheme $theme.Value

            if (Test-Path $launchCmd) {
                Remove-Item $launchCmd -Force
            }
            Set-Content -Path $launchCmd -Value @("@echo off", "`"$appExe`" --open-view $view") -Encoding ASCII
            Invoke-InSession1 $TaskName $launchCmd ""

            Wait-ViewRendered $view
            $rect = Get-TargetWindowRect $view
            $path = Join-Path $OutputDir ("{0}-{1}.png" -f $view, $theme.Name)
            Save-WindowScreenshot $rect $path
            $paths += $path
            Remove-Task $TaskName
        }
    }

    Write-Host "SCREENSHOT_OK"
    foreach ($path in $paths) {
        Write-Host $path
    }
} finally {
    Remove-Task $TaskName
    if (Test-Path $launchCmd) {
        Remove-Item $launchCmd -Force -ErrorAction SilentlyContinue
    }
    if ($hadOriginalTheme) {
        New-ItemProperty -Path $ThemeKey -Name "AppsUseLightTheme" -Value $originalTheme -PropertyType DWord -Force | Out-Null
    } else {
        Remove-ItemProperty -Path $ThemeKey -Name "AppsUseLightTheme" -ErrorAction SilentlyContinue
    }
}
