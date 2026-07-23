# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Direct package.ps1 execution is host-testable when pwsh is available. Actual
# win-package.cmd execution is Windows build-box-only post-ship evidence because
# cmd.exe and Windows PowerShell 5.1 are required.

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$PowerShellPath = (Get-Process -Id $PID).Path
$CmdPath = $env:ComSpec
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("solstone-package-entrypoints-" + [Guid]::NewGuid().ToString("N"))
$Witness = Join-Path $Temp "witness.txt"
$SelectionStdin = Join-Path $Temp "selection-stdin.txt"
$EmittedSelection = Join-Path $Temp "selection-emitted.txt"
$Assertions = 0
$SavedEnvironment = @{}
$ExpectedCommit = "0123456789abcdef0123456789abcdef01234567"
$AdvisoryTreeSha256 = "a" * 64
$MirrorLocator = "https://private-token@mirror.example.invalid/advisory-db"
$MirrorReceipt = "C:\operator\packet\freshness.json"
$MirrorPublicKey = "C:\operator\keys\advisory-mirror.pub"

function Assert-True([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "package-entrypoints.test.ps1: assertion failed: $Label" }
    $script:Assertions++
}

function Write-Ascii([string]$Path, [string]$Text) {
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    [IO.File]::WriteAllText($Path, $Text, [Text.Encoding]::ASCII)
}

function Set-TestEnvironment([string]$Name, [AllowNull()][string]$Value) {
    if (-not $SavedEnvironment.ContainsKey($Name)) {
        $SavedEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name)
    }
    [Environment]::SetEnvironmentVariable($Name, $Value)
}

function Reset-Case {
    [IO.File]::WriteAllText($Witness, "", [Text.Encoding]::ASCII)
    foreach ($path in @($SelectionStdin, $EmittedSelection)) {
        if (Test-Path $path) { Remove-Item -LiteralPath $path -Force }
    }
    Set-TestEnvironment "PACKAGE_TEST_FAIL" ""
    Set-TestEnvironment "SOLSTONE_SIGN" $null
    Set-TestEnvironment "EXPECTED_RELEASE_COMMIT" $ExpectedCommit
    Set-TestEnvironment "SOLSTONE_ADVISORY_TREE_SHA256" $AdvisoryTreeSha256
    Set-TestEnvironment "SOLSTONE_ADVISORY_MIRROR_LOCATOR" $MirrorLocator
    Set-TestEnvironment "SOLSTONE_ADVISORY_RECEIPT" $MirrorReceipt
    Set-TestEnvironment "SOLSTONE_ADVISORY_MIRROR_PUB" $MirrorPublicKey
}

function Run-Process([string]$FileName, [string]$Arguments) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $FileName
    $info.Arguments = $Arguments
    $info.WorkingDirectory = $Temp
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    return [pscustomobject]@{ status = $process.ExitCode; stdout = $stdout; stderr = $stderr }
}

function Run-Direct([switch]$Sign) {
    $signArg = if ($Sign) { " -Sign" } else { "" }
    return Run-Process $PowerShellPath "-NoProfile -ExecutionPolicy Bypass -File `"$Temp\scripts\package.ps1`"$signArg"
}

function Run-Wrapper {
    return Run-Process $CmdPath "/d /c `"`"$Temp\scripts\win-package.cmd`"`""
}

function Witness-Text {
    $raw = Get-Content -LiteralPath $Witness -Raw
    if ($null -eq $raw) { return "" }
    return $raw.Trim()
}

function Assert-OneFinalizer([string]$Label, [switch]$Signed) {
    $lines = @(Get-Content -LiteralPath $Witness | Where-Object { $_.StartsWith("finalize|") })
    Assert-True ($lines.Count -eq 1) "$Label invokes exactly one xtask finalizer"
    Assert-True ($lines[0].Contains("rust-release-manifest finalize --expected-release-commit $ExpectedCommit")) "$Label passes expected commit"
    Assert-True ($lines[0].Contains("git=$env:PACKAGE_TEST_GIT")) "$Label passes one absolute Git executable"
    Assert-True ($lines[0].Contains("advisory=$AdvisoryTreeSha256")) "$Label passes reviewed advisory digest"
    Assert-True ($lines[0].Contains("--sign") -eq [bool]$Signed) "$Label translates signed mode"
    Assert-True ((Get-Content -LiteralPath $SelectionStdin -Raw).Trim() -eq (Get-Content -LiteralPath $EmittedSelection -Raw).Trim()) "$Label passes exact selection JSON on stdin"
}

try {
    New-Item -ItemType Directory -Path (Join-Path $Temp "scripts"), (Join-Path $Temp "packaging") -Force | Out-Null
    Copy-Item (Join-Path $RepoRoot "scripts\package.ps1") (Join-Path $Temp "scripts\package.ps1") -Force
    Copy-Item (Join-Path $RepoRoot "scripts\win-package.cmd") (Join-Path $Temp "scripts\win-package.cmd") -Force
    Copy-Item (Join-Path $RepoRoot "packaging\npm-cache-preflight.ps1") (Join-Path $Temp "packaging\npm-cache-preflight.ps1") -Force
    Write-Ascii (Join-Path $Temp "Cargo.lock") "fake cargo lock`r`n"
    Write-Ascii (Join-Path $Temp "ui\package-lock.json") "{}`r`n"

    Write-Ascii (Join-Path $Temp "packaging\preflight-release-tools.ps1") @'
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
param([switch]$Sign)
[IO.File]::AppendAllText($env:PACKAGE_TEST_WITNESS, "preflight`r`n", [Text.Encoding]::ASCII)
if ($env:PACKAGE_TEST_FAIL -eq "preflight") { exit 30 }
$record = [ordered]@{
    schema = "solstone.release-tool-selection.v1"
    mode = $(if ($Sign -or $env:SOLSTONE_SIGN -eq "1") { "signed" } else { "unsigned" })
    tools = [ordered]@{
        cargo = [ordered]@{ path = $env:PACKAGE_TEST_CARGO }
        npm = [ordered]@{ path = $env:PACKAGE_TEST_NPM }
    }
}
$json = $record | ConvertTo-Json -Depth 6 -Compress
[IO.File]::WriteAllText($env:PACKAGE_TEST_EMITTED_SELECTION, $json, [Text.Encoding]::UTF8)
[Console]::Out.WriteLine($json)
'@
    Write-Ascii (Join-Path $Temp "packaging\lock-guard.ps1") @'
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
param([string]$Root = "")
[IO.File]::AppendAllText($env:PACKAGE_TEST_WITNESS, "lock-guard`r`n", [Text.Encoding]::ASCII)
if ($env:PACKAGE_TEST_FAIL -eq "lock") { exit 32 }
'@

    $cargo = Join-Path $Temp "fake-cargo.cmd"
    Write-Ascii $cargo @'
@echo off
setlocal enableextensions enabledelayedexpansion
echo %*| findstr /c:"-- version-gate" >nul
if not errorlevel 1 (
  echo version-gate>>"%PACKAGE_TEST_WITNESS%"
  if "%PACKAGE_TEST_FAIL%"=="version" exit /b 31
  echo 0.2.11
  exit /b 0
)
echo %*| findstr /c:"-- rust-release-manifest finalize" >nul
if not errorlevel 1 (
  set /p "SELECTION="
  echo finalize^|git=%GIT%^|advisory=%SOLSTONE_ADVISORY_TREE_SHA256%^|%*>>"%PACKAGE_TEST_WITNESS%"
  >"%PACKAGE_TEST_SELECTION_STDIN%" echo !SELECTION!
  if "%PACKAGE_TEST_FAIL%"=="finalize" exit /b 33
  exit /b 0
)
exit /b 90
'@

    $npm = Join-Path $Temp "fake-npm.cmd"
    Write-Ascii $npm @'
@echo off
if not "%*"=="--prefix ui ci --offline --dry-run" exit /b 91
echo npm-cache>>"%PACKAGE_TEST_WITNESS%"
if "%PACKAGE_TEST_FAIL%"=="npm-cache" exit /b 34
exit /b 0
'@

    Set-TestEnvironment "PACKAGE_TEST_WITNESS" $Witness
    Set-TestEnvironment "PACKAGE_TEST_SELECTION_STDIN" $SelectionStdin
    Set-TestEnvironment "PACKAGE_TEST_EMITTED_SELECTION" $EmittedSelection
    Set-TestEnvironment "PACKAGE_TEST_CARGO" $cargo
    Set-TestEnvironment "PACKAGE_TEST_NPM" $npm
    $git = Join-Path $Temp "fake-git.cmd"
    Write-Ascii $git "@echo off`r`nexit /b 0`r`n"
    Set-TestEnvironment "PACKAGE_TEST_GIT" $git
    Set-TestEnvironment "GIT" $git

    $packageSource = Get-Content (Join-Path $RepoRoot "scripts\package.ps1") -Raw
    $wrapperSource = Get-Content (Join-Path $RepoRoot "scripts\win-package.cmd") -Raw
    $legacyCargoBuild = "cargo " + "build"
    foreach ($forbidden in @("solstone-windows-app.exe", "vpk pack", "npm ci", $legacyCargoBuild, "vcvarsall")) {
        Assert-True (-not $packageSource.ToLowerInvariant().Contains($forbidden)) "package.ps1 omits legacy '$forbidden' action"
        Assert-True (-not $wrapperSource.ToLowerInvariant().Contains($forbidden)) "win-package.cmd omits legacy '$forbidden' action"
    }

    foreach ($entry in @(
        ,@("preflight", "preflight")
        ,@("version", "preflight`r`nversion-gate")
        ,@("lock", "preflight`r`nversion-gate`r`nlock-guard")
        ,@("npm-cache", "preflight`r`nversion-gate`r`nlock-guard`r`nnpm-cache")
    )) {
        Reset-Case
        Set-TestEnvironment "PACKAGE_TEST_FAIL" $entry[0]
        $result = Run-Direct
        Assert-True ($result.status -ne 0) "direct $($entry[0]) failure exits nonzero"
        Assert-True ((Witness-Text) -eq $entry[1]) "direct $($entry[0]) stops at the expected gate"
        Assert-True (-not (Witness-Text).Contains("finalize|")) "direct $($entry[0]) invokes no finalizer"
        if ($entry[0] -eq "npm-cache") {
            $failureOutput = $result.stdout + $result.stderr
            Assert-True ($failureOutput.Contains("npm offline cache preflight failed")) "npm cache failure keeps its stable prefix"
            Assert-True ($failureOutput.Contains("make install")) "npm cache failure names the warm command"
        }
    }

    foreach ($missing in @(
        "SOLSTONE_ADVISORY_MIRROR_LOCATOR",
        "SOLSTONE_ADVISORY_RECEIPT",
        "SOLSTONE_ADVISORY_MIRROR_PUB"
    )) {
        Reset-Case
        Set-TestEnvironment $missing $null
        $result = Run-Direct
        Assert-True ($result.status -ne 0) "direct invocation requires $missing"
        Assert-True ((Witness-Text) -eq "") "direct missing $missing stops before tool preflight"
    }

    Reset-Case
    Set-TestEnvironment "EXPECTED_RELEASE_COMMIT" $null
    $result = Run-Direct
    Assert-True ($result.status -ne 0) "direct invocation requires EXPECTED_RELEASE_COMMIT"
    Assert-True ((Witness-Text) -eq "preflight`r`nversion-gate`r`nlock-guard") "direct missing commit stops before finalizer"

    Reset-Case
    Set-TestEnvironment "SOLSTONE_ADVISORY_TREE_SHA256" $null
    $result = Run-Direct
    Assert-True ($result.status -ne 0) "direct invocation requires SOLSTONE_ADVISORY_TREE_SHA256"
    Assert-True ((Witness-Text) -eq "preflight`r`nversion-gate`r`nlock-guard") "direct missing advisory digest stops before npm cache probe"
    Assert-True (-not (Witness-Text).Contains("finalize|")) "direct missing advisory digest invokes no finalizer"

    foreach ($invalidSign in @("0", "false", " ")) {
        Reset-Case
        Set-TestEnvironment "SOLSTONE_SIGN" $invalidSign
        $result = Run-Direct
        Assert-True ($result.status -ne 0) "direct rejects SOLSTONE_SIGN '$invalidSign'"
        Assert-True ((Witness-Text) -eq "") "invalid SOLSTONE_SIGN fails before preflight"
    }

    Reset-Case
    $result = Run-Direct
    Assert-True ($result.status -eq 0) "direct unsigned delegation succeeds"
    Assert-True ((Witness-Text).StartsWith("preflight`r`nversion-gate`r`nlock-guard`r`nnpm-cache`r`nfinalize|")) "direct unsigned gate order"
    Assert-OneFinalizer "direct unsigned"

    Reset-Case
    $result = Run-Direct -Sign
    Assert-True ($result.status -eq 0) "direct signed delegation succeeds"
    Assert-OneFinalizer "direct signed" -Signed

    Reset-Case
    Set-TestEnvironment "EXPECTED_RELEASE_COMMIT" $null
    $result = Run-Wrapper
    Assert-True ($result.status -ne 0) "cmd wrapper requires EXPECTED_RELEASE_COMMIT"
    Assert-True (-not (Witness-Text).Contains("finalize|")) "cmd wrapper missing commit invokes no finalizer"

    Reset-Case
    Set-TestEnvironment "SOLSTONE_ADVISORY_TREE_SHA256" $null
    $result = Run-Wrapper
    Assert-True ($result.status -ne 0) "cmd wrapper requires advisory digest"
    Assert-True ((Witness-Text) -eq "") "cmd wrapper missing advisory digest stops before package.ps1"
    Assert-True (-not (Witness-Text).Contains("finalize|")) "cmd wrapper missing advisory digest invokes no finalizer"

    foreach ($invalidSign in @("0", "false", " ")) {
        Reset-Case
        Set-TestEnvironment "SOLSTONE_SIGN" $invalidSign
        $result = Run-Wrapper
        Assert-True ($result.status -ne 0) "cmd wrapper rejects SOLSTONE_SIGN '$invalidSign'"
        Assert-True (-not (Witness-Text).Contains("finalize|")) "cmd invalid SOLSTONE_SIGN invokes no finalizer"
    }

    Reset-Case
    $result = Run-Wrapper
    Assert-True ($result.status -eq 0) "cmd wrapper unsigned delegation succeeds"
    Assert-True ((Witness-Text).StartsWith("preflight`r`nversion-gate`r`nlock-guard`r`nnpm-cache`r`nfinalize|")) "cmd wrapper unsigned gate order"
    Assert-OneFinalizer "cmd wrapper unsigned"

    Reset-Case
    Set-TestEnvironment "SOLSTONE_SIGN" "1"
    $result = Run-Wrapper
    Assert-True ($result.status -eq 0) "cmd wrapper signed delegation succeeds"
    Assert-OneFinalizer "cmd wrapper signed" -Signed

    Assert-True (-not (Test-Path (Join-Path $Temp "target\release\solstone-windows-app.exe"))) "entry points never inspect a pre-existing release exe"
    Write-Host "package-entrypoints.test.ps1: $Assertions assertions passed"
} finally {
    foreach ($name in $SavedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $SavedEnvironment[$name])
    }
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
}
