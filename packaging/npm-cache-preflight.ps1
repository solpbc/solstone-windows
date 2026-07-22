# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

param(
    [Parameter(Mandatory = $true)]
    [string]$Root,
    [Parameter(Mandatory = $true)]
    [string]$NpmPath
)

$ErrorActionPreference = "Stop"
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
Push-Location $Root
try {
    & $NpmPath --prefix ui ci --offline --dry-run
    $probeStatus = $LASTEXITCODE
} finally {
    Pop-Location
    $ErrorActionPreference = $previousPreference
}
if ($probeStatus -ne 0) {
    throw "npm offline cache preflight failed; run 'make install' on the build box with network access, then rerun the source-bound package command."
}
