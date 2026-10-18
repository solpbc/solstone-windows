# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

function Test-SmokeRunValueEqual($Left, $Right) {
    if ($null -eq $Left -or $null -eq $Right) { return ($null -eq $Left -and $null -eq $Right) }
    return ($Left.Kind -eq $Right.Kind -and
        [Collections.StructuralComparisons]::StructuralEqualityComparer.Equals($Left.Data, $Right.Data))
}

function Assert-SmokeNoMedia([string]$SegmentsRoot) {
    if (Test-Path -LiteralPath $SegmentsRoot -ErrorAction Stop) {
        if (-not (Test-Path -LiteralPath $SegmentsRoot -PathType Container -ErrorAction Stop) -or
            @(Get-ChildItem -LiteralPath $SegmentsRoot -Force -ErrorAction Stop).Count -ne 0) {
            throw "fresh all-off proof found segment material at $SegmentsRoot"
        }
    }
}

# Bind the endpoint to the one exact app process. Callers pass measured OS facts;
# tests can exercise wrong-owner/missing/ambiguous cases without a live app.
function Get-SmokeHealthOwner([string]$AppExe, [int]$Port, [object[]]$Processes, [object[]]$Listeners) {
    $apps = @($Processes | Where-Object { $_.ExecutablePath -and
        ([string]$_.ExecutablePath).Equals($AppExe, [StringComparison]::OrdinalIgnoreCase) })
    $ports = @($Listeners | Where-Object { $_.LocalPort -eq $Port })
    if ($apps.Count -ne 1 -or $ports.Count -eq 0 -or
        @($ports | Where-Object { $_.OwningProcess -ne $apps[0].ProcessId }).Count -ne 0) {
        throw "health listener is not owned by the exact smoke candidate"
    }
    return [int]$apps[0].ProcessId
}
