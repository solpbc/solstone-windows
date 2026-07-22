# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$VersionGate = Join-Path $PSScriptRoot "smoke-version-gate.ps1"
$SmokeScript = Join-Path $RepoRoot "scripts\smoke.ps1"
$Assertions = 0

function Assert-True([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "smoke-version-gate.test.ps1: assertion failed: $Label" }
    $script:Assertions++
}

function Assert-Throws([scriptblock]$Action, [string]$Expected, [string]$Label) {
    $Actual = $null
    try {
        & $Action
    } catch {
        $Actual = $_.Exception.Message
    }
    Assert-True ($Actual -eq $Expected) $Label
}

. $VersionGate

$MatchPassed = $true
try {
    Assert-NativeProofHealthVersion -Body '{"version":"0.2.11"}' -ExpectedVersion "0.2.11"
} catch {
    $MatchPassed = $false
}
Assert-True $MatchPassed "matching version passes"

$MismatchDiagnostic = "native-proof launched app version does not match ExpectedVersion; rebuild and reinstall the source-bound candidate"
$EmptyDiagnostic = "native-proof launched app /healthz returned an empty body; restore the canonical health response and retry"
$MalformedDiagnostic = "native-proof launched app /healthz returned malformed JSON; restore the canonical health response and retry"
$MissingDiagnostic = "native-proof launched app /healthz omitted version; restore the canonical health response and retry"
$UnreachableDiagnostic = "native-proof launched app /healthz was unreachable after the health/render gate passed; inspect the launched Session-1 app and retry"

Assert-Throws { Assert-NativeProofHealthVersion -Body '{"version":"0.2.12"}' -ExpectedVersion "0.2.11" } $MismatchDiagnostic "wrong version exact diagnostic"
Assert-Throws { Assert-NativeProofHealthVersion -Body '' -ExpectedVersion "0.2.11" } $EmptyDiagnostic "empty body exact diagnostic"
Assert-Throws { Assert-NativeProofHealthVersion -Body '   ' -ExpectedVersion "0.2.11" } $EmptyDiagnostic "whitespace body exact diagnostic"
Assert-Throws { Assert-NativeProofHealthVersion -Body '{' -ExpectedVersion "0.2.11" } $MalformedDiagnostic "malformed JSON exact diagnostic"
Assert-Throws { Assert-NativeProofHealthVersion -Body '{"phase":"observing"}' -ExpectedVersion "0.2.11" } $MissingDiagnostic "missing version exact diagnostic"
Assert-Throws { Assert-NativeProofHealthVersion -Body '{"version":null}' -ExpectedVersion "0.2.11" } $MissingDiagnostic "null version exact diagnostic"

$SmokeSource = Get-Content -LiteralPath $SmokeScript -Raw -Encoding UTF8
$KillIndex = $SmokeSource.IndexOf('Get-Process solstone-windows-app')
$LaunchIndex = $SmokeSource.IndexOf('Invoke-InSession1 "solstone-smoke-app" $AppLaunchCmd ""', $KillIndex)
$GateBannerIndex = $SmokeSource.IndexOf('Write-Host "=== run health/render gate in Session 0 ==="', $LaunchIndex)
$GateIndex = $SmokeSource.IndexOf('& $DriverExe @GateArgs', $GateBannerIndex)
$GateFailureIndex = $SmokeSource.IndexOf('if ($GateExit -ne 0)', $GateIndex)
$GateExitIndex = $SmokeSource.IndexOf('exit $GateExit', $GateFailureIndex)
$VersionIndex = $SmokeSource.IndexOf('Assert-NativeProofHealthVersion', $GateExitIndex)
$Tier1Index = $SmokeSource.IndexOf('if (-not $FailInject)', $VersionIndex)
Assert-True (
    $KillIndex -ge 0 -and
    $KillIndex -lt $LaunchIndex -and
    $LaunchIndex -lt $GateIndex -and
    $GateIndex -lt $GateFailureIndex -and
    $GateFailureIndex -lt $GateExitIndex -and
    $GateExitIndex -lt $VersionIndex -and
    $VersionIndex -lt $Tier1Index
) "smoke keeps kill-launch-gate-failure-version-tier1 order"
Assert-True (-not $SmokeSource.Contains('$AppExe --dump-state')) "smoke has no explicit-app dump-state invocation"
$FetchCatchPattern = '(?s)try\s*\{\s*\$HealthResponse = Invoke-WebRequest.*?\}\s*catch\s*\{\s*throw "' + [regex]::Escape($UnreachableDiagnostic) + '"\s*\}'
Assert-True ([regex]::IsMatch($SmokeSource, $FetchCatchPattern)) "fetch catch has exact unreachable diagnostic"

Write-Host "smoke-version-gate.test.ps1: $Assertions assertions passed"
