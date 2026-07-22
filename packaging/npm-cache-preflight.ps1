# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

param(
    [Parameter(Mandatory = $true)]
    [string]$Root,
    [Parameter(Mandatory = $true)]
    [string]$NpmPath
)

$ErrorActionPreference = "Stop"
$locationPushed = $false
try {
    Push-Location $Root
    $locationPushed = $true
    # npm writes notices to stderr even on success; keep native stderr
    # non-terminating and judge the probe solely by its exit code.
    $ErrorActionPreference = "Continue"
    & $NpmPath --prefix ui ci --offline --dry-run
    $probeStatus = $LASTEXITCODE
} finally {
    $ErrorActionPreference = "Stop"
    if ($locationPushed) {
        Pop-Location
    }
}
if ($probeStatus -ne 0) {
    throw "npm offline cache preflight failed; run 'make install' on the build box with network access, then rerun the source-bound package command."
}
