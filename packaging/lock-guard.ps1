# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

param([string]$Root = "")

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($Root)) {
    $Root = Split-Path -Parent $PSScriptRoot
}
$Root = [IO.Path]::GetFullPath($Root)

function Fail-Lock([string]$Path, [string]$State, [string]$Repair) {
    [Console]::Error.WriteLine("ERROR: lock guard: $Path is $State. $Repair")
    exit 1
}

function Fail-Git([string]$Detail) {
    [Console]::Error.WriteLine("ERROR: lock guard: unable to verify tracked lockfiles: $Detail. Install Git, ensure it is runnable, and run from a Git checkout.")
    exit 1
}

$locks = @(
    [ordered]@{
        path = "Cargo.lock"
        repair = "Run 'cargo update -p <crate>', review Cargo.lock, and commit it."
    },
    [ordered]@{
        path = "ui/package-lock.json"
        repair = "Run 'make ui-deps-update', review ui/package-lock.json, and commit it."
    }
)

foreach ($lock in $locks) {
    $absolute = Join-Path $Root ($lock.path -replace "/", "\")
    if (-not (Test-Path -LiteralPath $absolute -PathType Leaf)) {
        Fail-Lock $lock.path "missing" $lock.repair
    }
}

try {
    $gitPath = (Get-Command git -CommandType Application -ErrorAction Stop).Source
} catch {
    Fail-Git "git is unavailable"
}

foreach ($lock in $locks) {
    try {
        $output = @(& $gitPath -C $Root ls-files --error-unmatch -- $lock.path 2>$null)
        $status = $LASTEXITCODE
    } catch {
        Fail-Git "git could not inspect $($lock.path)"
    }
    if ($status -eq 1) {
        Fail-Lock $lock.path "untracked" $lock.repair
    }
    if ($status -ne 0) {
        Fail-Git "git exited $status while inspecting $($lock.path)"
    }
    if ($output.Count -ne 1 -or $output[0] -ne $lock.path) {
        Fail-Git "git returned an invalid result for $($lock.path)"
    }
}
