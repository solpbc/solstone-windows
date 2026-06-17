# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Publish the Releases/ directory to GitHub Releases, which serves as the
# monotonic Velopack update feed. Operator-gated: there is no GitHub Actions
# release path by policy - the operator runs this by hand from the build box.
#
# Atomic-ish + fail-loud: `gh release create` errors if the tag already exists, so
# an un-bumped re-publish fails rather than silently overwriting the feed. The feed
# JSON (releases.win.json) is uploaded LAST so update clients never see a
# Setup.exe / nupkg without the matching feed.

param(
    # Optional "owner/name". Default: gh infers the repo from the git remote.
    # Used for the scratch-repo publish test.
    [string]$Repo = ""
)

$ErrorActionPreference = "Stop"

# Prefer gh on PATH; fall back to the default install location (box PATH gotcha).
$Gh = (Get-Command gh -ErrorAction SilentlyContinue).Source
if (-not $Gh) { $Gh = "$env:ProgramFiles\GitHub CLI\gh.exe" }

$Root = Split-Path -Parent $PSScriptRoot
$Releases = Join-Path $Root "Releases"
if (-not (Test-Path $Releases)) { throw "no Releases/ at $Releases - run ``make package`` first." }

# Derive the version (hence the tag) from the packed full nupkg already on disk -
# publish operates purely on Releases/ contents. The regex tolerates a channel
# segment in the nupkg name.
$full = Get-ChildItem $Releases -Filter "Solstone-*full.nupkg" | Select-Object -First 1
if (-not $full) { throw "no full nupkg in $Releases - run ``make package`` first." }
if ($full.Name -match 'Solstone-(.+?)(-win)?-full\.nupkg') {
    $Version = $Matches[1]
} else {
    throw "could not parse a version from $($full.Name)."
}
$Tag = "v$Version"

$repoArgs = if ($Repo) { @("--repo", $Repo) } else { @() }

$feed = Join-Path $Releases "releases.win.json"
# Every asset except the feed JSON (uploaded last, below).
$assets = Get-ChildItem $Releases -File |
    Where-Object { $_.Name -ne "releases.win.json" } |
    ForEach-Object { $_.FullName }

Write-Host "publish.ps1: creating GitHub release $Tag"
# Fail loud on an existing tag - gh errors and we do not pass --clobber, so the
# monotonic feed is never silently overwritten.
& $Gh release create $Tag @repoArgs --title $Tag --notes "Solstone $Version" @assets
if ($LASTEXITCODE -ne 0) { throw "gh release create failed for $Tag (tag may already exist; exit $LASTEXITCODE)." }

if (Test-Path $feed) {
    Write-Host "publish.ps1: uploading the update feed (releases.win.json) last"
    & $Gh release upload $Tag @repoArgs $feed
    if ($LASTEXITCODE -ne 0) { throw "feed upload failed for $Tag (exit $LASTEXITCODE)." }
}

Write-Host "publish.ps1: published $Tag."
