# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Velopack packaging: `vpk pack` the already-built RELEASE binary into Releases/
# (full + delta nupkg + Setup.exe + releases.win.json feed). Per-user install to
# %LocalAppData%, no UAC.
#
# Release notes: the matching CHANGELOG.md "## [<version>]" section is threaded
# into the pack via Velopack's --releaseNotes, so releases.win.json carries real
# per-release notes (NotesMarkdown/NotesHtml) - the Windows analog of the macOS
# appcast <description>, rendered by the in-app Updates pane and
# solstone.app/releases/windows. A signed release pack REQUIRES the section
# (parity with the macOS publish-appcast, which dies without it); an unsigned
# dev/local pack warns and packs note-less so iteration stays frictionless.
#
# Signing is OPT-IN and release-only. Pass -Sign to sign the packed artifacts with
# the DigiCert KeyLocker certificate via Velopack's --signTemplate (smctl). Without
# -Sign the pack is unsigned - the dev/local default - so iterate and delta-update
# validation packs do NOT burn the certificate's finite signature quota or churn
# the binary's SmartScreen reputation hashes. No secret or account identifier is
# hard-coded here: the signing credentials and the keypair alias come from the
# build box's signing environment (SM_HOST / SM_API_KEY / SM_CLIENT_CERT_FILE /
# SM_CLIENT_CERT_PASSWORD / SM_KEYPAIR_ALIAS), never committed. See
# docs/release-runbook.md.
#
# This script does NOT build - `make package` builds target/release/ first and
# this consumes it. Tauri embeds the webview (ui/dist) into the exe at compile
# time, so the pack input is the self-contained exe, not loose webview files.

param(
    # Override the packed version. Default: read it from the built exe's own
    # --dump-state so the feed version equals the binary's reported version by
    # construction (the monotonic feed + Wave-3 delta gate depend on this identity).
    [string]$Version = "",

    # Sign the packed artifacts (release-only). Default off = unsigned dev/local
    # pack. When set, the credential preflight must pass and the signing
    # environment must be provisioned on the build box.
    [switch]$Sign
)

$ErrorActionPreference = "Stop"

# Signing seam. Empty = unsigned. -Sign populates it (release-only) after the
# credential preflight passes. The keypair alias is env-supplied so no DigiCert
# account identifier lands in this public source; the smctl form is the KeyLocker
# signing path validated on the build box (signtool + KSP, RFC3161 timestamp).
$SignTemplate = ""
if ($Sign) {
    & (Join-Path $PSScriptRoot "..\packaging\signing\preflight-auth.ps1")
    $SignTemplate = "smctl sign --keypair-alias $($env:SM_KEYPAIR_ALIAS) --input {{file}}"
    Write-Host "package.ps1: signing ENABLED - release artifacts will be signed via smctl/KeyLocker."
} else {
    Write-Host "package.ps1: signing disabled (unsigned pack). Pass -Sign for a release."
}

$Root = Split-Path -Parent $PSScriptRoot

# Explicit tool path first (box PATH may not carry freshly-installed dotnet tools).
$Vpk = "$env:USERPROFILE\.dotnet\tools\vpk.exe"
if (-not (Test-Path $Vpk)) { $Vpk = "vpk" }

$Exe = Join-Path $Root "target\release\solstone-windows-app.exe"
if (-not (Test-Path $Exe)) {
    throw "release binary not found at $Exe - run ``make package`` (it builds --release first)."
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
    throw "Solstone $Version already packed in $Releases - bump the version before re-packing."
}

# Stage the self-contained exe only. If box validation shows a required sidecar
# (e.g. WebView2Loader.dll) sitting next to the built exe, copy it into the stage
# here too.
$Stage = Join-Path $Root "target\vpk-stage"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item $Exe (Join-Path $Stage "solstone-windows-app.exe")

$Icon = Join-Path $Root "src-tauri\icons\icon.ico"

# Release notes: extract the CHANGELOG.md "## [<version>]" section (mirrors the
# macOS publish-appcast.py extract_release_notes) and write it to a notes file
# under target/ (NOT Releases/, which publish-r2 uploads wholesale). Velopack
# renders it to NotesMarkdown + NotesHtml inside releases.win.json.
$NotesFile = $null
$ChangelogPath = Join-Path $Root "CHANGELOG.md"
if (Test-Path $ChangelogPath) {
    $Changelog = Get-Content -Raw -Encoding UTF8 $ChangelogPath
    $EscVersion = [regex]::Escape($Version)
    # "## [<version>]" header (optional trailing " - date"), body up to the next
    # "## [" header or end of file. (?ms): ^ anchors per-line, . spans newlines.
    $NotesMatch = [regex]::Match($Changelog, "(?ms)^## \[$EscVersion\][^\r\n]*\r?\n(.*?)(?=^## \[|\z)")
    if ($NotesMatch.Success) {
        $NotesBody = $NotesMatch.Groups[1].Value.Trim()
        if ($NotesBody) {
            $NotesFile = Join-Path $Root "target\release-notes-$Version.md"
            # -NoNewline: write the section body verbatim, no injected trailing newline.
            Set-Content -Path $NotesFile -Value $NotesBody -Encoding UTF8 -NoNewline
            Write-Host "package.ps1: release notes from CHANGELOG.md ## [$Version] -> $NotesFile"
        }
    }
}
if (-not $NotesFile) {
    if ($Sign) {
        throw "package.ps1: no CHANGELOG.md '## [$Version]' section with notes - a signed release pack must carry release notes. Cut [Unreleased] -> [$Version] in CHANGELOG.md before signing."
    }
    Write-Host "package.ps1: no CHANGELOG.md '## [$Version]' section - packing without release notes (unsigned dev/local pack)."
}

$vpkArgs = @(
    "pack",
    "--packId", "Solstone",
    "--packVersion", $Version,
    "--packDir", $Stage,
    "--mainExe", "solstone-windows-app.exe",
    "--outputDir", $Releases,
    "--packTitle", "solstone",
    "--packAuthors", "sol pbc",
    "--icon", $Icon,
    "--channel", "win"
)
# Thread the signing seam through only when populated (unsigned path leaves it out).
if ($SignTemplate) { $vpkArgs += @("--signTemplate", $SignTemplate) }
# Thread release notes only when we have a notes file (dev packs without a
# CHANGELOG section pack note-less; a signed release already threw above if absent).
if ($NotesFile) { $vpkArgs += @("--releaseNotes", $NotesFile) }

Write-Host "package.ps1: vpk pack Solstone $Version -> $Releases"
# Delta nupkg is emitted automatically when a prior full nupkg is already present
# in the output dir; no extra flag needed.
& $Vpk @vpkArgs
if ($LASTEXITCODE -ne 0) { throw "vpk pack failed (exit $LASTEXITCODE)." }

# Versioned installer URLs avoid stale CDN cache bugs. Rename instead of copy so
# only the new name ships. Signing ran during pack; Authenticode signs file
# content, not the filename, so the post-sign rename keeps the signature valid.
$DefaultSetupName = "Solstone-win-Setup.exe"
$DefaultSetup = Join-Path $Releases $DefaultSetupName
if (-not (Test-Path $DefaultSetup)) { throw "package.ps1: expected vpk output '$DefaultSetupName' not found in $Releases after pack - vpk Setup.exe naming may have changed." }
$VersionedSetup = Join-Path $Releases "solstone-setup-$Version.exe"
Move-Item -Force $DefaultSetup $VersionedSetup
Write-Host "package.ps1: renamed $DefaultSetupName -> solstone-setup-$Version.exe"

Write-Host "package.ps1: done. Releases/ carries solstone-setup-$Version.exe + full nupkg (+ delta when a prior release was present) + releases.win.json$(if ($NotesFile) { ' (with release notes)' })."
