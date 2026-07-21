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

param([switch]$Sign)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$SignEnabled = $Sign -or -not [string]::IsNullOrWhiteSpace($env:SOLSTONE_SIGN)

# Deterministic gates run before credentials, dependency resolution, output
# creation, or any other byte-changing work. Redundant wrapper execution is
# intentional: this protects direct package.ps1 invocation and the pack boundary.
$Preflight = Join-Path $Root "packaging\preflight-release-tools.ps1"
$SelectionJson = if ($SignEnabled) { & $Preflight -Sign } else { & $Preflight }
$Selection = $SelectionJson | ConvertFrom-Json

$OldCargoOverride = $env:SOLSTONE_VERSION_GATE_CARGO
$env:SOLSTONE_VERSION_GATE_CARGO = $Selection.tools.cargo.path
Push-Location $Root
try {
    $VersionOutput = @(& $Selection.tools.cargo.path run --locked -q -p xtask -- version-gate)
    if ($LASTEXITCODE -ne 0 -or $VersionOutput.Count -ne 1 -or [string]::IsNullOrWhiteSpace($VersionOutput[0])) {
        throw "version gate failed"
    }
    $Version = $VersionOutput[0].Trim()
} finally {
    Pop-Location
    $env:SOLSTONE_VERSION_GATE_CARGO = $OldCargoOverride
}

& (Join-Path $Root "packaging\lock-guard.ps1") -Root $Root

# Signing seam. Empty = unsigned. -Sign populates it (release-only) after the
# credential preflight passes. The keypair alias is env-supplied so no DigiCert
# account identifier lands in this public source; the smctl form is the KeyLocker
# signing path validated on the build box (signtool + KSP, RFC3161 timestamp).
$SignTemplate = ""
$Vpk = $Selection.tools.vpk.path

$Exe = Join-Path $Root "target\release\solstone-windows-app.exe"
if (-not (Test-Path $Exe)) {
    throw "release binary not found at $Exe - run ``make package`` (it builds --release first)."
}

if ($SignEnabled) {
    $SmctlPath = $Selection.tools.smctl.path
    & (Join-Path $Root "packaging\signing\preflight-auth.ps1") -SmctlPath $SmctlPath
    $SignTemplate = "`"$SmctlPath`" sign --keypair-alias $($env:SM_KEYPAIR_ALIAS) --input {{file}}"
    Write-Host "package.ps1: signing ENABLED - release artifacts will be signed via selected smctl/KeyLocker."
} else {
    Write-Host "package.ps1: signing disabled (unsigned pack). Pass -Sign or set SOLSTONE_SIGN for a release."
}

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
# under target/ (NOT Releases/, which is retained as aggregate publisher input). Velopack
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
    if ($SignEnabled) {
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
    "--packTitle", "sol",
    "--packAuthors", "sol pbc",
    "--icon", $Icon,
    "--channel", "win",
    # Ensure the Evergreen WebView2 runtime. Setup.exe detects it and, only when
    # absent, downloads Microsoft's Evergreen bootstrapper and installs it silently
    # (a no-op where WebView2 is already present, i.e. Win11 / most Win10). The app
    # renders its UI via WebView2, and vpk packaging bypasses Tauri's own installer,
    # so this is where the runtime dependency gets ensured for direct Setup.exe
    # installs on clean/minimal machines. Requires network at install time.
    "--framework", "webview2"
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
$VersionedSetupName = "solstone-setup-$Version.exe"
$VersionedSetup = Join-Path $Releases $VersionedSetupName
Move-Item -Force $DefaultSetup $VersionedSetup
Write-Host "package.ps1: renamed $DefaultSetupName -> $VersionedSetupName"

Write-Host "package.ps1: done. Releases/ carries $VersionedSetupName + full nupkg (+ delta when a prior release was present) + releases.win.json$(if ($NotesFile) { ' (with release notes)' })."
