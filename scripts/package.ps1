# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Thin Windows bootstrap for the source-bound Rust release transaction. The
# selected actions are executed only by xtask; this script performs redundant
# preflight/version/lock defense-in-depth and delegates exactly once.

param(
    [switch]$Sign,
    [string[]]$DeltaBaseFull = @()
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$SignEnabled = $Sign -or -not [string]::IsNullOrWhiteSpace($env:SOLSTONE_SIGN)

$Preflight = Join-Path $Root "packaging\preflight-release-tools.ps1"
$PowerShellPath = (Get-Process -Id $PID).Path
$PreflightArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $Preflight)
if ($SignEnabled) { $PreflightArgs += "-Sign" }

$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
    $SelectionLines = @(& $PowerShellPath @PreflightArgs)
    $preflightStatus = $LASTEXITCODE
} finally {
    $ErrorActionPreference = $previousPreference
}
if ($preflightStatus -ne 0) {
    throw "release-tool preflight failed (exit $preflightStatus); repair the reported pinned-tool mismatch and retry."
}
if ($SelectionLines.Count -ne 1 -or [string]::IsNullOrWhiteSpace($SelectionLines[0])) {
    throw "release-tool preflight returned no single selection record; rerun the current preflight and retry."
}
$SelectionJson = [string]$SelectionLines[0]
try {
    $Selection = $SelectionJson | ConvertFrom-Json
} catch {
    throw "release-tool preflight returned malformed selection JSON; rerun the current preflight and retry."
}
if ($null -eq $Selection.tools.cargo -or [string]::IsNullOrWhiteSpace($Selection.tools.cargo.path)) {
    throw "release-tool selection omitted cargo.path; rerun the pinned release-tool preflight and retry."
}
$CargoPath = [string]$Selection.tools.cargo.path

$OldCargoOverride = $env:SOLSTONE_VERSION_GATE_CARGO
$env:SOLSTONE_VERSION_GATE_CARGO = $CargoPath
Push-Location $Root
try {
    $VersionOutput = @(& $CargoPath run --locked -q -p xtask -- version-gate)
    if ($LASTEXITCODE -ne 0 -or $VersionOutput.Count -ne 1 -or [string]::IsNullOrWhiteSpace($VersionOutput[0])) {
        throw "version gate failed; align all committed version surfaces and retry."
    }
} finally {
    Pop-Location
    $env:SOLSTONE_VERSION_GATE_CARGO = $OldCargoOverride
}

& (Join-Path $Root "packaging\lock-guard.ps1") -Root $Root
if ($LASTEXITCODE -ne 0) {
    throw "lock guard failed (exit $LASTEXITCODE); restore both tracked lockfiles and retry."
}

$ExpectedCommit = $env:EXPECTED_RELEASE_COMMIT
if ([string]::IsNullOrWhiteSpace($ExpectedCommit)) {
    throw "EXPECTED_RELEASE_COMMIT is required; set it to the full lowercase 40-hex release commit and retry."
}
if ($ExpectedCommit -notmatch "^[0-9a-f]{40}$") {
    throw "EXPECTED_RELEASE_COMMIT is not a full lowercase 40-hex commit; correct it and retry."
}

$FinalizeArgs = @(
    "run",
    "--locked",
    "-q",
    "-p",
    "xtask",
    "--",
    "rust-release-manifest",
    "finalize",
    "--expected-release-commit",
    $ExpectedCommit
)
if ($SignEnabled) { $FinalizeArgs += "--sign" }
foreach ($basename in $DeltaBaseFull) {
    if ([string]::IsNullOrWhiteSpace($basename)) {
        throw "delta-base full basename is empty; pass a canonical historical full-package basename and retry."
    }
    $FinalizeArgs += @("--delta-base-full", $basename)
}

Push-Location $Root
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
    $SelectionJson | & $CargoPath @FinalizeArgs
    $finalizeStatus = $LASTEXITCODE
} finally {
    $ErrorActionPreference = $previousPreference
    Pop-Location
}
if ($finalizeStatus -ne 0) {
    throw "release finalization failed (exit $finalizeStatus); repair the reported transaction gate and rerun from the source-bound entry point."
}
