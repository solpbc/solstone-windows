# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Velopack packaging: `vpk pack` the already-built RELEASE binary into Releases/
# (full + delta nupkg + Setup.exe + releases.win.json feed). Per-user install to
# %LocalAppData%, no UAC. Unsigned now: the $SignTemplate seam below is
# intentionally empty. When the signing cert lands, populate it with the Velopack
# `--signTemplate` form and add the credential pre-check; sign release artifacts
# only. No code restructure is needed to turn signing on.
#
# This script does NOT build — `make package` builds target/release/ first and
# this consumes it. Tauri embeds the webview (ui/dist) into the exe at compile
# time, so the pack input is the self-contained exe, not loose webview files.

param(
    # Override the packed version. Default: read it from the built exe's own
    # --dump-state so the feed version equals the binary's reported version by
    # construction (the monotonic feed + Wave-3 delta gate depend on this identity).
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"

# Empty signing seam. When the cert is provisioned this becomes e.g.
#   $SignTemplate = '--signTemplate "smctl sign --fingerprint <fp> --input {{file}}"'
$SignTemplate = ""

$Root = Split-Path -Parent $PSScriptRoot

# Explicit tool path first (box PATH may not carry freshly-installed dotnet tools).
$Vpk = "$env:USERPROFILE\.dotnet\tools\vpk.exe"
if (-not (Test-Path $Vpk)) { $Vpk = "vpk" }

$Exe = Join-Path $Root "target\release\solstone-windows-app.exe"
if (-not (Test-Path $Exe)) {
    throw "release binary not found at $Exe — run ``make package`` (it builds --release first)."
}

if (-not $Version) {
    $Version = (& $Exe --dump-state | ConvertFrom-Json).version
}
if (-not $Version) { throw "could not resolve a version (from -Version or $Exe --dump-state)." }

$Releases = Join-Path $Root "Releases"
New-Item -ItemType Directory -Force -Path $Releases | Out-Null

# Re-pack guard: refuse to clobber an already-packed full release for this version
# (an un-bumped re-pack is an operator error). The glob tolerates a channel suffix.
if (Get-ChildItem $Releases -Filter "Solstone-$Version*full.nupkg" -ErrorAction SilentlyContinue) {
    throw "Solstone $Version already packed in $Releases — bump the version before re-packing."
}

# Stage the self-contained exe only. If box validation shows a required sidecar
# (e.g. WebView2Loader.dll) sitting next to the built exe, copy it into the stage
# here too.
$Stage = Join-Path $Root "target\vpk-stage"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item $Exe (Join-Path $Stage "solstone-windows-app.exe")

$Icon = Join-Path $Root "src-tauri\icons\icon.ico"

$vpkArgs = @(
    "pack",
    "--packId", "Solstone",
    "--packVersion", $Version,
    "--packDir", $Stage,
    "--mainExe", "solstone-windows-app.exe",
    "--outputDir", $Releases,
    "--packTitle", "Solstone",
    "--packAuthors", "sol pbc",
    "--icon", $Icon,
    "--channel", "win"
)
# Thread the signing seam through only when populated (unsigned path leaves it out).
if ($SignTemplate) { $vpkArgs += @("--signTemplate", $SignTemplate) }

Write-Host "package.ps1: vpk pack Solstone $Version -> $Releases"
# Delta nupkg is emitted automatically when a prior full nupkg is already present
# in the output dir; no extra flag needed.
& $Vpk @vpkArgs
if ($LASTEXITCODE -ne 0) { throw "vpk pack failed (exit $LASTEXITCODE)." }

Write-Host "package.ps1: done. Releases/ carries Setup.exe + full nupkg (+ delta when a prior release was present) + releases.win.json."
