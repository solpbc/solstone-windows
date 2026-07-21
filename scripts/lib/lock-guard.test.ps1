# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Guard = Join-Path $Root "packaging\lock-guard.ps1"
$PowerShellPath = (Get-Process -Id $PID).Path
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("solstone-lock-guard-" + [Guid]::NewGuid().ToString("N"))
$Assertions = 0

function Assert-True([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "lock-guard.test.ps1: assertion failed: $Label" }
    $script:Assertions++
}

function Run-Guard([string]$Repo, [string]$PathOverride = "") {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $PowerShellPath
    $info.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$Guard`" -Root `"$Repo`""
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    if ($PathOverride -ne "") { $info.EnvironmentVariables["PATH"] = $PathOverride }
    $process = [Diagnostics.Process]::Start($info)
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    return [pscustomobject]@{ status = $process.ExitCode; stdout = $stdout.Trim(); stderr = $stderr.Trim() }
}

function Git([string[]]$Arguments) {
    & git -C $Temp @Arguments | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git failed: $($Arguments -join ' ')" }
}

function Reset-Repo {
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
    New-Item -ItemType Directory -Path (Join-Path $Temp "ui") -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $Temp "Cargo.lock") -Value "cargo-lock" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $Temp "ui\package-lock.json") -Value "{}" -Encoding ASCII
    & git -C $Temp init -q
    & git -C $Temp config user.name "solstone lock test"
    & git -C $Temp config user.email "lock-test@example.invalid"
    Git @("add", "Cargo.lock", "ui/package-lock.json")
    Git @("commit", "-qm", "locks")
}

try {
    Reset-Repo
    $beforeCargo = (Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash
    $beforeUi = (Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash
    $beforeStatus = (& git -C $Temp status --short | Out-String)
    $result = Run-Guard $Temp
    Assert-True ($result.status -eq 0) "tracked locks succeed"
    Assert-True ($result.stdout -eq "" -and $result.stderr -eq "") "success is silent"
    Assert-True ((Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash -eq $beforeCargo) "Cargo.lock bytes stable"
    Assert-True ((Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash -eq $beforeUi) "UI lock bytes stable"
    Assert-True ((& git -C $Temp status --short | Out-String) -eq $beforeStatus) "git status stable"

    foreach ($case in @(,
        @("Cargo.lock", "Run 'cargo update -p <crate>', "Cargo.lock"),
        ,@("ui/package-lock.json", "Run 'make ui-deps-update'", "ui\package-lock.json")
    )) {
        Reset-Repo
        Remove-Item -LiteralPath (Join-Path $Temp $case[2])
        $result = Run-Guard $Temp
        Assert-True ($result.status -ne 0) "$($case[0]) missing fails"
        Assert-True ($result.stderr.Contains("ERROR: lock guard: $($case[0]) is missing.")) "$($case[0]) missing diagnostic"
        Assert-True ($result.stderr.Contains($case[1])) "$($case[0]) missing repair"

        Reset-Repo
        Git @("rm", "--cached", "--", $case[0])
        $result = Run-Guard $Temp
        Assert-True ($result.status -ne 0) "$($case[0]) untracked fails"
        Assert-True ($result.stderr.Contains("ERROR: lock guard: $($case[0]) is untracked.")) "$($case[0]) untracked diagnostic"
        Assert-True ($result.stderr.Contains($case[1])) "$($case[0]) untracked repair"
    }

    Reset-Repo
    Git @("rm", "--cached", "--", "ui/package-lock.json")
    Add-Content -LiteralPath (Join-Path $Temp ".gitignore") -Value "ui/package-lock.json" -Encoding ASCII
    $result = Run-Guard $Temp
    Assert-True ($result.status -ne 0) "ignored present lock fails"
    Assert-True ($result.stderr.Contains("ui/package-lock.json is untracked")) "ignored present reports untracked"

    Reset-Repo
    $result = Run-Guard $Temp (Join-Path $Temp "no-tools")
    Assert-True ($result.status -ne 0) "missing git fails closed"
    Assert-True ($result.stderr.Contains("ERROR: lock guard: unable to verify tracked lockfiles: git is unavailable.")) "missing git has distinct diagnostic"
    Assert-True ($result.stderr.Contains("Install Git, ensure it is runnable, and run from a Git checkout.")) "missing git has actionable repair"

    Reset-Repo
    Remove-Item -LiteralPath (Join-Path $Temp ".git") -Recurse -Force
    $result = Run-Guard $Temp
    Assert-True ($result.status -ne 0) "non-repository fails closed"
    Assert-True ($result.stderr.Contains("unable to verify tracked lockfiles: git exited")) "non-repository has distinct diagnostic"
    Assert-True ($result.stderr.Contains("run from a Git checkout")) "non-repository has actionable repair"

    Write-Host "lock-guard.test.ps1: $Assertions assertions passed"
} finally {
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
}
