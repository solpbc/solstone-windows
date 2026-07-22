# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Thin Windows bootstrap for the source-bound Rust release transaction. The
# selected actions are executed only by xtask; this script performs redundant
# preflight/version/lock/cache defense-in-depth and delegates exactly once.

param(
    [switch]$Sign,
    [string[]]$DeltaBaseFull = @()
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$SignEnvironment = [string]$env:SOLSTONE_SIGN
if ($SignEnvironment.Length -ne 0 -and $SignEnvironment -ne "1") {
    throw "SOLSTONE_SIGN must be exactly 1 when signing is requested; unset it for unsigned finalization and retry."
}
$SignEnabled = $Sign -or $SignEnvironment -eq "1"

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
if ($null -eq $Selection.tools.npm -or [string]::IsNullOrWhiteSpace($Selection.tools.npm.path)) {
    throw "release-tool selection omitted npm.path; rerun the pinned release-tool preflight and retry."
}
$CargoPath = [string]$Selection.tools.cargo.path
$NpmPath = [string]$Selection.tools.npm.path

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
$AdvisoryTreeSha256 = [string]$env:SOLSTONE_ADVISORY_TREE_SHA256
if ($AdvisoryTreeSha256 -notmatch "^[0-9a-f]{64}$") {
    throw "SOLSTONE_ADVISORY_TREE_SHA256 is required as 64 lowercase hex; supply the reviewed isolated RustSec archive digest and retry."
}

& (Join-Path $Root "packaging\npm-cache-preflight.ps1") -Root $Root -NpmPath $NpmPath

$GitCommand = if ([string]::IsNullOrWhiteSpace($env:GIT)) { "git" } else { [string]$env:GIT }
$GitSelection = Get-Command -Name $GitCommand -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($null -eq $GitSelection -or [string]::IsNullOrWhiteSpace($GitSelection.Source)) {
    throw "Git executable selection failed; install Git or set GIT to its executable path and retry."
}
$GitPath = [System.IO.Path]::GetFullPath([string]$GitSelection.Source)
if (-not [System.IO.Path]::IsPathRooted($GitPath) -or -not (Test-Path -LiteralPath $GitPath -PathType Leaf)) {
    throw "Git executable selection is not one absolute regular file; set GIT to the exact executable and retry."
}
$env:GIT = $GitPath

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
