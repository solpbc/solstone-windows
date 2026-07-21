# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$PowerShellPath = (Get-Process -Id $PID).Path
$CmdPath = $env:ComSpec
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("solstone-package-entrypoints-" + [Guid]::NewGuid().ToString("N"))
$FakeBin = Join-Path $Temp "selected"
$PoisonBin = Join-Path $Temp "poison"
$Witness = Join-Path $Temp "witness.txt"
$PoisonWitness = Join-Path $Temp "poison-witness.txt"
$Assertions = 0
$SavedEnvironment = @{}

function Assert-True([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "package-entrypoints.test.ps1: assertion failed: $Label" }
    $script:Assertions++
}

function Write-Ascii([string]$Path, [string]$Text) {
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    [IO.File]::WriteAllText($Path, $Text, [Text.Encoding]::ASCII)
}

function Set-TestEnvironment([string]$Name, [string]$Value) {
    if (-not $SavedEnvironment.ContainsKey($Name)) {
        $SavedEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name)
    }
    [Environment]::SetEnvironmentVariable($Name, $Value)
}

function Reset-Case {
    Write-Ascii $Witness ""
    Write-Ascii $PoisonWitness ""
    foreach ($path in @((Join-Path $Temp "Releases"), (Join-Path $Temp "target\vpk-stage"))) {
        if (Test-Path $path) { Remove-Item -Recurse -Force $path }
    }
}

function Run-Process([string]$File, [string]$Arguments) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $File
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

try {
    New-Item -ItemType Directory -Path $FakeBin, $PoisonBin, (Join-Path $Temp "scripts") -Force | Out-Null
    Copy-Item (Join-Path $RepoRoot "scripts\package.ps1") (Join-Path $Temp "scripts\package.ps1") -Force
    Copy-Item (Join-Path $RepoRoot "scripts\win-package.cmd") (Join-Path $Temp "scripts\win-package.cmd") -Force
    Write-Ascii (Join-Path $Temp "Cargo.lock") "cargo-lock`r`n"
    Write-Ascii (Join-Path $Temp "ui\package-lock.json") "{}`r`n"
    Write-Ascii (Join-Path $Temp "target\release\solstone-windows-app.exe") "fake exe"
    Write-Ascii (Join-Path $Temp "src-tauri\icons\icon.ico") "fake icon"
    Write-Ascii (Join-Path $Temp "CHANGELOG.md") "## [0.2.11] - 2026-07-20`r`n`r`n- test notes`r`n"
    Write-Ascii (Join-Path $Temp "client-cert.pem") "fake cert"

    Write-Ascii (Join-Path $Temp "packaging\preflight-release-tools.ps1") @'
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
param([switch]$Sign)
[IO.File]::AppendAllText($env:PACKAGE_TEST_WITNESS, "preflight`r`n", [Text.Encoding]::ASCII)
if ($env:PACKAGE_TEST_FAIL -eq "preflight") { [Console]::Error.WriteLine("fake preflight failure"); exit 30 }
$record = [ordered]@{
  schema = "solstone.release-tool-selection.v1"
  mode = $(if ($Sign -or $env:SOLSTONE_SIGN) { "signed" } else { "unsigned" })
  tools = [ordered]@{
    cargo = [ordered]@{ path = $env:PACKAGE_TEST_CARGO }
    npm = [ordered]@{ path = $env:PACKAGE_TEST_NPM }
    powershell = [ordered]@{ path = $env:PACKAGE_TEST_POWERSHELL }
    vpk = [ordered]@{ path = $env:PACKAGE_TEST_VPK }
    smctl = [ordered]@{ path = $env:PACKAGE_TEST_SMCTL }
    signtool = [ordered]@{ path = $env:PACKAGE_TEST_SIGNTOOL }
    "msvc-cl" = [ordered]@{
      vcvarsallPath = $env:PACKAGE_TEST_VCVARS
      vcvarsVersionArg = $env:PACKAGE_TEST_VCVARS_ARG
    }
  }
}
[Console]::Out.WriteLine(($record | ConvertTo-Json -Depth 6 -Compress))
'@
    Write-Ascii (Join-Path $Temp "packaging\lock-guard.ps1") @'
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
param([string]$Root = "")
[IO.File]::AppendAllText($env:PACKAGE_TEST_WITNESS, "lock-guard`r`n", [Text.Encoding]::ASCII)
if ($env:PACKAGE_TEST_FAIL -eq "lock") { [Console]::Error.WriteLine("fake lock failure"); exit 32 }
'@
    Write-Ascii (Join-Path $Temp "packaging\signing\preflight-auth.ps1") @'
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
param([Parameter(Mandatory=$true)][string]$SmctlPath)
[IO.File]::AppendAllText($env:PACKAGE_TEST_WITNESS, "auth|$SmctlPath`r`n", [Text.Encoding]::ASCII)
if ($SmctlPath -ne $env:PACKAGE_TEST_SMCTL) { exit 88 }
'@

    $cargo = Join-Path $FakeBin "cargo.cmd"
    $npm = Join-Path $FakeBin "npm.cmd"
    $vpk = Join-Path $FakeBin "vpk.cmd"
    $smctl = Join-Path $FakeBin "smctl.cmd"
    $signtool = Join-Path $FakeBin "signtool.cmd"
    $vcvars = Join-Path $FakeBin "vcvarsall.bat"
    Write-Ascii $cargo @'
@echo off
if "%1"=="run" (
  echo version-gate>>"%PACKAGE_TEST_WITNESS%"
  if "%PACKAGE_TEST_FAIL%"=="version" exit /b 31
  echo 0.2.11
  exit /b 0
)
if "%1"=="build" (
  echo cargo-build>>"%PACKAGE_TEST_WITNESS%"
  exit /b 0
)
exit /b 90
'@
    Write-Ascii $npm @'
@echo off
echo %*| findstr /c:" ci --offline" >nul && echo npm-ci>>"%PACKAGE_TEST_WITNESS%" && exit /b 0
echo %*| findstr /c:" run build" >nul && echo npm-build>>"%PACKAGE_TEST_WITNESS%" && exit /b 0
exit /b 90
'@
    Write-Ascii $vpk @'
@echo off
echo vpk^|%*>>"%PACKAGE_TEST_WITNESS%"
if not exist "%PACKAGE_TEST_RELEASES%" mkdir "%PACKAGE_TEST_RELEASES%"
echo fake setup>"%PACKAGE_TEST_RELEASES%\Solstone-win-Setup.exe"
exit /b 0
'@
    Write-Ascii $smctl "@echo off`r`necho SELECTED-SMCTL>>`"%PACKAGE_TEST_WITNESS%`"`r`nexit /b 0`r`n"
    Write-Ascii $signtool "@echo off`r`necho SELECTED-SIGNTOOL>>`"%PACKAGE_TEST_WITNESS%`"`r`nexit /b 98`r`n"
    Write-Ascii $vcvars @'
@echo off
echo vcvarsall^|%*>>"%PACKAGE_TEST_WITNESS%"
if not "%1"=="x64" exit /b 89
if not "%2"=="%PACKAGE_TEST_VCVARS_ARG%" exit /b 89
exit /b 0
'@
    foreach ($tool in @("cargo", "npm", "vpk", "smctl", "signtool")) {
        Write-Ascii (Join-Path $PoisonBin "$tool.cmd") "@echo off`r`necho POISON-$tool>>`"%PACKAGE_TEST_POISON_WITNESS%`"`r`nexit /b 97`r`n"
    }

    Set-TestEnvironment "PACKAGE_TEST_WITNESS" $Witness
    Set-TestEnvironment "PACKAGE_TEST_POISON_WITNESS" $PoisonWitness
    Set-TestEnvironment "PACKAGE_TEST_CARGO" $cargo
    Set-TestEnvironment "PACKAGE_TEST_NPM" $npm
    Set-TestEnvironment "PACKAGE_TEST_POWERSHELL" $PowerShellPath
    Set-TestEnvironment "PACKAGE_TEST_VPK" $vpk
    Set-TestEnvironment "PACKAGE_TEST_SMCTL" $smctl
    Set-TestEnvironment "PACKAGE_TEST_SIGNTOOL" $signtool
    Set-TestEnvironment "PACKAGE_TEST_VCVARS" $vcvars
    Set-TestEnvironment "PACKAGE_TEST_VCVARS_ARG" "-vcvars_ver=14.44.35207"
    Set-TestEnvironment "PACKAGE_TEST_RELEASES" (Join-Path $Temp "Releases")
    Set-TestEnvironment "SM_HOST" "https://example.invalid"
    Set-TestEnvironment "SM_CLIENT_CERT_FILE" (Join-Path $Temp "client-cert.pem")
    Set-TestEnvironment "SM_KEYPAIR_ALIAS" "test-key"
    Set-TestEnvironment "PATH" "$PoisonBin;$env:PATH"
    Set-TestEnvironment "SOLSTONE_SIGN" $null

    $packageSource = Get-Content (Join-Path $RepoRoot "scripts\package.ps1") -Raw
    Assert-True (-not $packageSource.ToLowerInvariant().Contains("signtool verify")) "package.ps1 has no SignTool verification"
    Assert-True ($packageSource.Contains('$Selection.tools.smctl.path')) "package.ps1 consumes selected smctl"

    foreach ($entry in @(
        ,@("preflight", "preflight"),
        ,@("version", "preflight`r`nversion-gate"),
        ,@("lock", "preflight`r`nversion-gate`r`nlock-guard")
    )) {
        Reset-Case
        Set-TestEnvironment "PACKAGE_TEST_FAIL" $entry[0]
        $beforeCargo = (Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash
        $beforeUi = (Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash
        $result = Run-Direct
        Assert-True ($result.status -ne 0) "direct $($entry[0]) failure exits nonzero"
        Assert-True ((Get-Content $Witness -Raw).Trim() -eq $entry[1]) "direct $($entry[0]) failure order"
        Assert-True (-not (Test-Path (Join-Path $Temp "Releases"))) "direct $($entry[0]) creates no Releases"
        Assert-True ((Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash -eq $beforeCargo) "direct $($entry[0]) Cargo.lock stable"
        Assert-True ((Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash -eq $beforeUi) "direct $($entry[0]) UI lock stable"
    }

    Reset-Case
    Set-TestEnvironment "PACKAGE_TEST_FAIL" ""
    $beforeCargo = (Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash
    $beforeUi = (Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash
    $result = Run-Direct
    Assert-True ($result.status -eq 0) "direct unsigned succeeds"
    $directWitness = Get-Content $Witness -Raw
    Assert-True ($directWitness.Contains("preflight`r`nversion-gate`r`nlock-guard`r`nvpk|")) "direct gate and vpk order"
    Assert-True (-not $directWitness.Contains("auth|")) "direct unsigned omits auth"
    Assert-True (-not $directWitness.ToLowerInvariant().Contains("signtool")) "direct unsigned omits SignTool"
    Assert-True ((Get-Content $PoisonWitness -Raw) -eq "") "direct unsigned selected paths beat poisoned PATH"
    Assert-True ((Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash -eq $beforeCargo) "direct unsigned Cargo.lock stable"
    Assert-True ((Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash -eq $beforeUi) "direct unsigned UI lock stable"

    Reset-Case
    $beforeCargo = (Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash
    $beforeUi = (Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash
    $result = Run-Direct -Sign
    Assert-True ($result.status -eq 0) "direct signed succeeds"
    $signedWitness = Get-Content $Witness -Raw
    Assert-True ($signedWitness.Contains("auth|$smctl")) "direct signed auth uses selected smctl"
    $signedVpkLine = @($signedWitness -split "`r?`n" | Where-Object { $_.StartsWith("vpk|") })[0]
    Assert-True ($signedVpkLine.Contains("--signTemplate") -and $signedVpkLine.Contains($smctl)) "direct signTemplate carries selected smctl absolute path"
    Assert-True (-not $signedWitness.ToLowerInvariant().Contains("signtool")) "direct signed never invokes SignTool"
    Assert-True ((Get-Content $PoisonWitness -Raw) -eq "") "direct signed never invokes ambient SignTool"
    Assert-True ((Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash -eq $beforeCargo) "direct signed Cargo.lock stable"
    Assert-True ((Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash -eq $beforeUi) "direct signed UI lock stable"

    foreach ($case in @(
        ,@("preflight", "preflight"),
        ,@("version", "preflight`r`nversion-gate"),
        ,@("lock", "preflight`r`nversion-gate`r`nlock-guard")
    )) {
        $failure = $case[0]
        Reset-Case
        Set-TestEnvironment "PACKAGE_TEST_FAIL" $failure
        $result = Run-Wrapper
        Assert-True ($result.status -ne 0) "wrapper $failure failure exits nonzero"
        $wrapperWitness = Get-Content $Witness -Raw
        Assert-True ($wrapperWitness.Trim() -eq $case[1]) "wrapper $failure exact stop order"
        Assert-True (-not $wrapperWitness.Contains("npm-ci")) "wrapper $failure stops npm"
        Assert-True (-not $wrapperWitness.Contains("cargo-build")) "wrapper $failure stops build"
        Assert-True (-not $wrapperWitness.Contains("vpk|")) "wrapper $failure stops pack"
        Assert-True (-not (Test-Path (Join-Path $Temp "Releases"))) "wrapper $failure creates no Releases"
    }

    Reset-Case
    Set-TestEnvironment "PACKAGE_TEST_FAIL" ""
    Set-TestEnvironment "SOLSTONE_SIGN" $null
    $beforeCargo = (Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash
    $beforeUi = (Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash
    $result = Run-Wrapper
    Assert-True ($result.status -eq 0) "wrapper unsigned success"
    $wrapperWitness = Get-Content $Witness -Raw
    Assert-True ($wrapperWitness.Contains("preflight`r`nversion-gate`r`nlock-guard`r`nvcvarsall|x64 -vcvars_ver=14.44.35207`r`nnpm-ci`r`nnpm-build`r`ncargo-build`r`npreflight`r`nversion-gate`r`nlock-guard")) "wrapper unsigned defense-in-depth and pinned vcvars order"
    Assert-True (-not $wrapperWitness.Contains("auth|")) "wrapper unsigned omits auth"
    Assert-True (-not $wrapperWitness.Contains("--signTemplate")) "wrapper unsigned omits signTemplate"
    Assert-True (-not $wrapperWitness.ToLowerInvariant().Contains("signtool")) "wrapper unsigned omits SignTool"
    Assert-True ((Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash -eq $beforeCargo) "wrapper unsigned Cargo.lock stable"
    Assert-True ((Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash -eq $beforeUi) "wrapper unsigned UI lock stable"
    Assert-True ((Get-Content $PoisonWitness -Raw) -eq "") "wrapper unsigned selected paths beat poisoned PATH"

    Reset-Case
    Set-TestEnvironment "SOLSTONE_SIGN" "1"
    $beforeCargo = (Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash
    $beforeUi = (Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash
    $result = Run-Wrapper
    Assert-True ($result.status -eq 0) "wrapper signed success"
    $wrapperSignedWitness = Get-Content $Witness -Raw
    Assert-True ($wrapperSignedWitness.Contains("vcvarsall|x64 -vcvars_ver=14.44.35207")) "wrapper signed activates pinned vcvars toolset"
    Assert-True ($wrapperSignedWitness.Contains("auth|$smctl")) "wrapper signed auth uses selected smctl"
    $wrapperSignedVpkLine = @($wrapperSignedWitness -split "`r?`n" | Where-Object { $_.StartsWith("vpk|") })[0]
    Assert-True ($wrapperSignedVpkLine.Contains("--signTemplate") -and $wrapperSignedVpkLine.Contains($smctl)) "wrapper signed signTemplate carries selected smctl absolute path"
    Assert-True (-not $wrapperSignedWitness.ToLowerInvariant().Contains("signtool")) "wrapper signed never invokes SignTool"
    Assert-True ((Get-FileHash (Join-Path $Temp "Cargo.lock") -Algorithm SHA256).Hash -eq $beforeCargo) "wrapper signed Cargo.lock stable"
    Assert-True ((Get-FileHash (Join-Path $Temp "ui\package-lock.json") -Algorithm SHA256).Hash -eq $beforeUi) "wrapper signed UI lock stable"
    Assert-True ((Get-Content $PoisonWitness -Raw) -eq "") "wrapper signed selected paths beat poisoned PATH"
    Set-TestEnvironment "SOLSTONE_SIGN" $null

    Write-Host "package-entrypoints.test.ps1: $Assertions assertions passed"
} finally {
    foreach ($name in $SavedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $SavedEnvironment[$name])
    }
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
}
