# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$Preflight = Join-Path $RepoRoot "packaging\preflight-release-tools.ps1"
$SourceContract = Join-Path $RepoRoot "packaging\release-toolchain.json"
$PowerShellPath = (Get-Process -Id $PID).Path
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("solstone-release-tools-" + [Guid]::NewGuid().ToString("N"))
$FakeBin = Join-Path $Temp "bin"
$FakeProfile = Join-Path $Temp "profile"
$FakeKits = Join-Path $Temp "kits"
$FakeVs = Join-Path $Temp "vs install"
$ContractPath = Join-Path $Temp "contract.json"
$Witness = Join-Path $Temp "witness.txt"
$VswhereOutput = Join-Path $FakeBin "vswhere-output.txt"
$Assertions = 0
$OldSign = $env:SOLSTONE_SIGN
$OldPath = $env:PATH
$OldMsvcHost = $env:VSCMD_ARG_HOST_ARCH
$OldMsvcTarget = $env:VSCMD_ARG_TGT_ARCH
$OldMsvcToolset = $env:VCToolsVersion

function Assert-True([bool]$Condition, [string]$Label) {
    if (-not $Condition) { throw "preflight-release-tools.test.ps1: assertion failed: $Label" }
    $script:Assertions++
}

function Write-Ascii([string]$Path, [string]$Text) {
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    $Text = $Text.Replace('\r\n', "`r`n").Replace('\"', '"')
    [IO.File]::WriteAllText($Path, $Text, [Text.Encoding]::ASCII)
}

function Run-Preflight([switch]$Sign) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $PowerShellPath
    $signArg = if ($Sign) { " -Sign" } else { "" }
    $info.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$Preflight`"$signArg"
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($info)
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    return [pscustomobject]@{ status = $process.ExitCode; stdout = $stdout.Trim(); stderr = $stderr.Trim() }
}

function Save-Contract($Contract) {
    [IO.File]::WriteAllText($ContractPath, ($Contract | ConvertTo-Json -Depth 12), [Text.Encoding]::UTF8)
}

function Fresh-Contract {
    $contract = Get-Content -LiteralPath $SourceContract -Raw -Encoding UTF8 | ConvertFrom-Json
    $signtoolPath = Join-Path $FakeKits "bin\10.0.26100.0\x64\signtool.exe"
    $cmdMetadata = [Diagnostics.FileVersionInfo]::GetVersionInfo($signtoolPath)
    $contract.tools.signtool.expected.path = $signtoolPath
    $contract.tools.signtool.expected.productVersion = $cmdMetadata.ProductVersion
    $contract.tools.signtool.expected.originalFilename = $cmdMetadata.OriginalFilename
    Save-Contract $contract
    return $contract
}

function Reset-Witness { [IO.File]::WriteAllText($Witness, "", [Text.Encoding]::ASCII) }

function Assert-AcceptedVsPath([string]$Root, [string]$Label) {
    Set-FakeVsLayout $Root | Out-Null
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -eq 0) "$Label MSVC path succeeds"
    $witness = Get-Content $Witness | Out-String
    $vcvars = Join-Path $Root "VC\Auxiliary\Build\vcvarsall.bat"
    $cl = Join-Path $Root "VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\cl.cmd"
    Assert-True ($witness.Contains(('vcvarsall-path|"' + $vcvars + '"'))) "$Label path runs exact vcvars"
    Assert-True ($witness.Contains(('pinned-cl-path|"' + $cl + '"'))) "$Label path runs exact compiler"
    Assert-True (-not $witness.Contains("PATH-INJECTION")) "$Label path performs no injected action"
    Assert-True (-not $witness.Contains("AMBIENT-CL")) "$Label path avoids ambient compiler"
}

function Snapshot-OwnedFiles {
    $lines = @(
        Get-ChildItem -LiteralPath $Temp -Recurse -File |
            Where-Object { $_.FullName -ne $Witness } |
            ForEach-Object {
                $relative = $_.FullName.Substring($Temp.Length)
                "$relative|$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash)"
            } |
            Sort-Object
    )
    return ($lines -join "`n")
}

function Set-FakeVsLayout([string]$Root) {
    $toolset = Join-Path $Root "VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64"
    New-Item -ItemType Directory -Path $toolset -Force | Out-Null
    Write-Ascii (Join-Path $Root "VC\Auxiliary\Build\vcvarsall.bat") '@echo off
echo vcvarsall^|%*>>"%FAKE_RELEASE_WITNESS%"
echo vcvarsall-path^|"%~f0">>"%FAKE_RELEASE_WITNESS%"
if not "%*"=="x64 -vcvars_ver=14.44.35207" (
  echo VCVARS-BAD-ARGS>>"%FAKE_RELEASE_WITNESS%"
  exit /b 87
)
if "%FAKE_VCVARS_OUTPUT_MODE%"=="compiler-like" echo Microsoft (R) C/C++ Optimizing Compiler Version %FAKE_CL_VERSION% for x64
if not "%FAKE_VCVARS_EXIT%"=="0" exit /b %FAKE_VCVARS_EXIT%
set "FAKE_VCVARS_ACTIVATED=1"
set "VSCMD_ARG_HOST_ARCH=%FAKE_VCVARS_HOST%"
set "VSCMD_ARG_TGT_ARCH=%FAKE_VCVARS_TARGET%"
set "VCToolsVersion=%FAKE_VCVARS_TOOLSET%"
exit /b 0
'
    Write-Ascii (Join-Path $toolset "cl.cmd") '@echo off
echo pinned-cl^|%*>>"%FAKE_RELEASE_WITNESS%"
echo pinned-cl-path^|"%~f0">>"%FAKE_RELEASE_WITNESS%"
if not "%FAKE_VCVARS_ACTIVATED%"=="1" exit /b 91
if not "%VSCMD_ARG_HOST_ARCH%"=="x64" exit /b 92
if not "%VSCMD_ARG_TGT_ARCH%"=="x64" exit /b 93
if not "%VCToolsVersion%"=="14.44.35207" exit /b 94
if "%FAKE_CL_MODE%"=="launch-failure" (
  echo The system cannot execute the specified program. 1>&2
  exit /b 9009
)
if "%FAKE_CL_MODE%"=="multiple-banner" (
  echo Microsoft ^(R^) C/C++ Optimizing Compiler Version %FAKE_CL_VERSION% for x64 1>&2
  echo Microsoft ^(R^) C/C++ Optimizing Compiler Version %FAKE_CL_VERSION% for x64 1>&2
) else if "%FAKE_CL_MODE%"=="malformed-banner" (
  echo Microsoft ^(R^) C/C++ Optimizing Compiler Version malformed for x64 1>&2
) else if not "%FAKE_CL_MODE%"=="missing-banner" (
  echo Microsoft ^(R^) C/C++ Optimizing Compiler Version %FAKE_CL_VERSION% for x64 1>&2
)
if not "%FAKE_CL_MODE%"=="missing-d8003" echo cl : Command line error D8003 : missing source filename 1>&2
exit /b %FAKE_CL_EXIT%
'
    Write-Ascii $VswhereOutput "$Root`r`n"
    return $toolset
}

try {
    New-Item -ItemType Directory -Path $FakeBin, $FakeProfile, $FakeKits, $FakeVs -Force | Out-Null
    $signtoolPath = Join-Path $FakeKits "bin\10.0.26100.0\x64\signtool.exe"
    New-Item -ItemType Directory -Path (Split-Path -Parent $signtoolPath) -Force | Out-Null
    Copy-Item -LiteralPath $env:ComSpec -Destination $signtoolPath
    Write-Ascii (Join-Path $FakeProfile ".dotnet\tools\vpk.exe") "fake vpk"
    New-Item -ItemType Directory -Path (Join-Path $FakeKits "Lib\10.0.26100.0") -Force | Out-Null
    $toolset = Set-FakeVsLayout $FakeVs

    Write-Ascii (Join-Path $FakeBin "rustc.cmd") '@echo off
echo rustc^|%*>>"%FAKE_RELEASE_WITNESS%"
echo rustc fake
echo host: %FAKE_RUST_HOST%
echo release: %FAKE_RUST_RELEASE%
'
    Write-Ascii (Join-Path $FakeBin "cargo.cmd") '@echo off
echo cargo^|%*>>"%FAKE_RELEASE_WITNESS%"
echo cargo %FAKE_CARGO_VERSION% (fake)
'
    Write-Ascii (Join-Path $FakeBin "cargo-deny.cmd") '@echo off
echo cargo-deny^|%*>>"%FAKE_RELEASE_WITNESS%"
echo cargo-deny %FAKE_DENY_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "dotnet.cmd") '@echo off
echo dotnet^|%*>>"%FAKE_RELEASE_WITNESS%"
if "%1"=="tool" (
  echo Package Id Version Commands
  echo --------------------------------
  if "%FAKE_VPK_ROW_MODE%"=="duplicate" (
    echo %FAKE_VPK_ID% %FAKE_VPK_VERSION% %FAKE_VPK_COMMAND%
    echo %FAKE_VPK_ID% %FAKE_VPK_VERSION% %FAKE_VPK_COMMAND%
    exit /b 0
  )
  if "%FAKE_VPK_ROW_MODE%"=="malformed" (
    echo %FAKE_VPK_ID% malformed %FAKE_VPK_COMMAND%
    exit /b 0
  )
  echo %FAKE_VPK_ID% %FAKE_VPK_VERSION% %FAKE_VPK_COMMAND%
  exit /b 0
)
echo %FAKE_DOTNET_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "node.cmd") '@echo off
echo node^|%*>>"%FAKE_RELEASE_WITNESS%"
echo v%FAKE_NODE_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "npm.cmd") '@echo off
echo npm^|%*>>"%FAKE_RELEASE_WITNESS%"
if not "%FAKE_NPM_EXIT%"=="0" exit /b %FAKE_NPM_EXIT%
echo %FAKE_NPM_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "npm") '#!/bin/sh
exit 99
'
    Write-Ascii (Join-Path $FakeBin "vswhere.cmd") '@echo off
echo vswhere^|%*>>"%FAKE_RELEASE_WITNESS%"
type "%~dp0vswhere-output.txt"
'
    Write-Ascii (Join-Path $FakeBin "smctl.cmd") '@echo off
echo smctl^|%*>>"%FAKE_RELEASE_WITNESS%"
echo smctl version %FAKE_SMCTL_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "signtool.cmd") '@echo off
echo AMBIENT-SIGNTOOL>>"%FAKE_RELEASE_WITNESS%"
exit /b 98
'
    foreach ($tool in @("curl", "gh", "wrangler", "scp")) {
        Write-Ascii (Join-Path $FakeBin "$tool.cmd") "@echo off`r`necho NETWORK-$tool>>`"%FAKE_RELEASE_WITNESS%`"`r`nexit /b 97`r`n"
    }
    Write-Ascii (Join-Path $FakeBin "cl.cmd") "@echo off`r`necho AMBIENT-CL>>`"%FAKE_RELEASE_WITNESS%`"`r`nexit /b 98`r`n"
    Write-Ascii (Join-Path $FakeBin "path-injection.cmd") "@echo off`r`necho PATH-INJECTION>>`"%FAKE_RELEASE_WITNESS%`"`r`nexit /b 98`r`n"
    $env:PATH = "$FakeBin;$env:PATH"

    $env:SOLSTONE_RELEASE_TOOLS_CONTRACT = $ContractPath
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN = $FakeBin
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH = $FakeBin
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_USERPROFILE = $FakeProfile
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_WINDOWS_KITS = $FakeKits
    $env:FAKE_RELEASE_WITNESS = $Witness
    $env:FAKE_RUST_RELEASE = "1.96.0"
    $env:FAKE_RUST_HOST = "x86_64-pc-windows-msvc"
    $env:FAKE_CARGO_VERSION = "1.96.0"
    $env:FAKE_DENY_VERSION = "0.20.2"
    $env:FAKE_DOTNET_VERSION = "8.0.422"
    $env:FAKE_VPK_ID = "vpk"
    $env:FAKE_VPK_VERSION = "1.2.0"
    $env:FAKE_VPK_COMMAND = "vpk"
    $env:FAKE_VPK_ROW_MODE = "normal"
    $env:FAKE_NODE_VERSION = "24.16.0"
    $env:FAKE_NPM_VERSION = "11.13.0"
    $env:FAKE_NPM_EXIT = "0"
    $env:FAKE_CL_VERSION = "19.44.35228"
    $env:FAKE_CL_MODE = "normal"
    $env:FAKE_CL_EXIT = "2"
    $env:FAKE_VCVARS_EXIT = "0"
    $env:FAKE_VCVARS_HOST = "x64"
    $env:FAKE_VCVARS_TARGET = "x64"
    $env:FAKE_VCVARS_TOOLSET = "14.44.35207"
    $env:FAKE_VCVARS_OUTPUT_MODE = "normal"
    $env:FAKE_SMCTL_VERSION = "1.64.2"
    $env:VSCMD_ARG_HOST_ARCH = "wrong-inherited-host"
    $env:VSCMD_ARG_TGT_ARCH = "wrong-inherited-target"
    $env:VCToolsVersion = "wrong-inherited-toolset"

    Fresh-Contract | Out-Null
    $smctlFake = Join-Path $FakeBin "smctl.cmd"
    Move-Item $smctlFake "$smctlFake.saved"
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "missing smctl fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: smctl.path expected one executable, actual unavailable. Install DigiCert smctl 1.64.2 on the Windows build box.")) "missing smctl exact expected/actual/repair diagnostic"
    Move-Item "$smctlFake.saved" $smctlFake
    $env:SOLSTONE_SIGN = $null
    Fresh-Contract | Out-Null

    Reset-Witness
    $beforeOwnedFiles = Snapshot-OwnedFiles
    $result = Run-Preflight
    if ($result.status -ne 0) { [Console]::Error.WriteLine($result.stderr) }
    Assert-True ($result.status -eq 0) "unsigned preflight succeeds"
    Assert-True ($result.stderr -eq "") "unsigned stderr empty"
    Assert-True (($result.stdout -split '\r?\n').Count -eq 1) "selection is one JSON line"
    $selection = $result.stdout | ConvertFrom-Json
    Assert-True ($selection.schema -eq "solstone.release-tool-selection.v1") "selection schema"
    Assert-True ($selection.mode -eq "unsigned") "unsigned mode"
    Assert-True ($selection.tools.cargo.path -eq (Join-Path $FakeBin "cargo.cmd")) "selected cargo path"
    Assert-True ((Test-Path -LiteralPath (Join-Path $FakeBin "npm.cmd") -PathType Leaf) -and (Test-Path -LiteralPath (Join-Path $FakeBin "npm") -PathType Leaf)) "canonical npm companions present"
    Assert-True ($selection.tools.npm.path -eq (Join-Path $FakeBin "npm.cmd")) "selected callable npm.cmd path"
    Assert-True ($selection.tools.vpk.version -eq "1.2.0") "selected vpk version"
    Assert-True ($selection.tools.'msvc-cl'.compilerVersion -eq "19.44.35228") "compiler banner selected"
    Assert-True ($selection.tools.'msvc-cl'.toolsetVersion -eq "14.44.35207") "toolset directory selected"
    Assert-True ($selection.tools.'msvc-cl'.vcvarsVersionArg -eq "-vcvars_ver=14.44.35207") "pinned vcvars activation selected"
    Assert-True ($null -eq $selection.tools.smctl) "unsigned omits smctl"
    Assert-True ($null -eq $selection.tools.signtool) "unsigned omits signtool"
    Assert-True ((Snapshot-OwnedFiles) -eq $beforeOwnedFiles) "unsigned preflight mutates no owned files"
    $unsignedWitness = Get-Content $Witness
    Assert-True (@($unsignedWitness | Where-Object { $_ -eq "npm|--version" }).Count -eq 1) "only npm.cmd is invoked"
    Assert-True (@($unsignedWitness | Where-Object { $_ -eq "pinned-cl|/Bv" }).Count -eq 1) "exact pinned compiler is invoked"
    Assert-True (-not (($unsignedWitness | Out-String).Contains("AMBIENT-CL"))) "ambient compiler is not invoked"
    Assert-True ($env:VSCMD_ARG_HOST_ARCH -eq "wrong-inherited-host") "MSVC child leaves parent host identity unchanged"
    Assert-True ($env:VSCMD_ARG_TGT_ARCH -eq "wrong-inherited-target") "MSVC child leaves parent target identity unchanged"
    Assert-True ($env:VCToolsVersion -eq "wrong-inherited-toolset") "MSVC child leaves parent toolset identity unchanged"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("NETWORK-"))) "unsigned preflight performs no network command"

    Reset-Witness
    $beforeOwnedFiles = Snapshot-OwnedFiles
    $result = Run-Preflight -Sign
    Assert-True ($result.status -eq 0) "signed preflight succeeds"
    $selection = $result.stdout | ConvertFrom-Json
    Assert-True ($selection.mode -eq "signed") "signed mode"
    Assert-True ($selection.tools.smctl.version -eq "1.64.2") "signed selects smctl"
    Assert-True ($selection.tools.signtool.path -eq $signtoolPath) "signed selects exact SignTool"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("AMBIENT-SIGNTOOL"))) "ambient SignTool rejected"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("healthcheck"))) "preflight performs no credential healthcheck"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("NETWORK-"))) "signed preflight performs no network command"
    Assert-True ((Snapshot-OwnedFiles) -eq $beforeOwnedFiles) "signed preflight mutates no owned files"

    $env:SOLSTONE_SIGN = "1"
    $result = Run-Preflight
    Assert-True (($result.stdout | ConvertFrom-Json).mode -eq "signed") "SOLSTONE_SIGN selects signed mode"
    $env:SOLSTONE_SIGN = $null

    foreach ($case in @(
        ,@("FAKE_RUST_RELEASE", "9.9.9", "ERROR: release tool mismatch: rustc.release expected 1.96.0, actual 9.9.9. Run 'make rust-toolchain' on the Windows build box.")
        ,@("FAKE_RUST_HOST", "wrong-host", "ERROR: release tool mismatch: rustc.host expected x86_64-pc-windows-msvc, actual wrong-host. Run 'make rust-toolchain' on the Windows build box.")
        ,@("FAKE_CARGO_VERSION", "9.9.9", "ERROR: release tool mismatch: cargo.version expected 1.96.0, actual 9.9.9. Run 'make rust-toolchain' on the Windows build box.")
        ,@("FAKE_DENY_VERSION", "9.9.9", "ERROR: release tool mismatch: cargo-deny.version expected 0.20.2, actual 9.9.9. Run 'make provision-cargo-deny'.")
        ,@("FAKE_DOTNET_VERSION", "9.9.9", "ERROR: release tool mismatch: dotnet.version expected 8.0.422, actual 9.9.9. Install the pinned .NET SDK on the Windows build box.")
        ,@("FAKE_VPK_VERSION", "9.9.9", "ERROR: release tool mismatch: vpk.version expected 1.2.0, actual 9.9.9. Install the pinned Velopack global tool on the Windows build box.")
        ,@("FAKE_NODE_VERSION", "9.9.9", "ERROR: release tool mismatch: node.version expected 24.16.0, actual 9.9.9. Install Node.js 24.16.0 on the Windows build box.")
        ,@("FAKE_NPM_VERSION", "9.9.9", "ERROR: release tool mismatch: npm.version expected 11.13.0, actual 9.9.9. Install npm 11.13.0 with the pinned Node.js toolchain.")
        ,@("FAKE_CL_VERSION", "9.9.9", "ERROR: release tool mismatch: msvc-cl.compilerVersion expected 19.44.35228, actual 9.9.9. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")
    )) {
        $old = [Environment]::GetEnvironmentVariable($case[0])
        [Environment]::SetEnvironmentVariable($case[0], $case[1])
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[0]) skew fails"
        Assert-True ($result.stderr.Contains($case[2])) "$($case[0]) exact expected/actual/repair diagnostic"
        [Environment]::SetEnvironmentVariable($case[0], $old)
    }

    $npmFake = Join-Path $FakeBin "npm.cmd"
    Move-Item $npmFake "$npmFake.saved"
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "extensionless-only npm fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: npm.path expected one reachable npm.cmd co-located with selected node, actual unavailable. Install npm 11.13.0 with the pinned Node.js toolchain.")) "extensionless-only npm exact diagnostic"
    Assert-True (Test-Path -LiteralPath (Join-Path $FakeBin "npm") -PathType Leaf) "extensionless npm remains present"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("npm|"))) "extensionless npm is not invoked"
    Move-Item "$npmFake.saved" $npmFake

    $secondNpmBin = Join-Path $Temp "second-npm"
    New-Item -ItemType Directory -Path $secondNpmBin -Force | Out-Null
    Copy-Item -LiteralPath $npmFake -Destination (Join-Path $secondNpmBin "npm.cmd")
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH = "$FakeBin;$secondNpmBin"
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "two distinct npm.cmd candidates fail"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: npm.path expected one reachable npm.cmd co-located with selected node, actual multiple:")) "two distinct npm.cmd exact diagnostic"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("npm|"))) "ambiguous npm candidates are not invoked"

    $otherNpmBin = Join-Path $Temp "other-npm"
    New-Item -ItemType Directory -Path $otherNpmBin -Force | Out-Null
    $otherNpm = Join-Path $otherNpmBin "npm.cmd"
    Copy-Item -LiteralPath $npmFake -Destination $otherNpm
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH = $otherNpmBin
    Reset-Witness
    $result = Run-Preflight
    $expectedNpm = Join-Path $FakeBin "npm.cmd"
    Assert-True ($result.status -ne 0) "npm.cmd outside selected node directory fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: npm.path expected $expectedNpm, actual $otherNpm. Install npm 11.13.0 with the pinned Node.js toolchain.")) "npm.cmd co-location exact diagnostic"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("npm|"))) "non-co-located npm.cmd is not invoked"
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH = $FakeBin

    foreach ($case in @(
        ,@("", "missing npm version", "actual unavailable")
        ,@("malformed", "malformed npm version", "actual unavailable")
    )) {
        $env:FAKE_NPM_VERSION = $case[0]
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[1]) fails"
        Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: npm.version expected 11.13.0, $($case[2]). Install npm 11.13.0 with the pinned Node.js toolchain.")) "$($case[1]) exact diagnostic"
    }
    $env:FAKE_NPM_VERSION = "11.13.0"

    $env:FAKE_NPM_EXIT = "23"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "npm invocation failure fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: npm.invocation expected exit 0, actual exit 23. Install npm 11.13.0 with the pinned Node.js toolchain.")) "npm invocation failure exact diagnostic"
    $env:FAKE_NPM_EXIT = "0"

    $env:FAKE_VCVARS_EXIT = "7"
    $env:FAKE_VCVARS_OUTPUT_MODE = "compiler-like"
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "vcvars nonzero fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.activationExit expected 0, actual 7. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "vcvars nonzero exact diagnostic"
    $activationWitness = Get-Content $Witness | Out-String
    Assert-True ($activationWitness.Contains("vcvarsall|x64 -vcvars_ver=14.44.35207")) "vcvars receives exact pinned activation args"
    Assert-True (-not $activationWitness.Contains("pinned-cl|")) "failed vcvars prevents compiler launch"
    Assert-True (-not $result.stderr.Contains("msvc-cl.compilerVersion")) "vcvars output cannot satisfy or fault compiler channel"
    $env:FAKE_VCVARS_EXIT = "0"
    $env:FAKE_VCVARS_OUTPUT_MODE = "normal"

    foreach ($case in @(
        ,@("FAKE_VCVARS_HOST", "wrong", "msvc-cl.activatedHost", "x64", "wrong")
        ,@("FAKE_VCVARS_HOST", "", "msvc-cl.activatedHost", "x64", "unavailable")
        ,@("FAKE_VCVARS_TARGET", "wrong", "msvc-cl.activatedTarget", "x64", "wrong")
        ,@("FAKE_VCVARS_TARGET", "", "msvc-cl.activatedTarget", "x64", "unavailable")
        ,@("FAKE_VCVARS_TOOLSET", "wrong", "msvc-cl.activatedToolsetVersion", "14.44.35207", "wrong")
        ,@("FAKE_VCVARS_TOOLSET", "", "msvc-cl.activatedToolsetVersion", "14.44.35207", "unavailable")
    )) {
        $old = [Environment]::GetEnvironmentVariable($case[0])
        [Environment]::SetEnvironmentVariable($case[0], $case[1])
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[2]) $($case[4]) fails"
        $identityDiagnostic = "ERROR: release tool mismatch: $($case[2]) expected $($case[3]), actual $($case[4]). Install the pinned Visual Studio Build Tools MSVC x64 toolset."
        if (-not $result.stderr.Contains($identityDiagnostic)) { [Console]::Error.WriteLine($result.stderr) }
        Assert-True ($result.stderr.Contains($identityDiagnostic)) "$($case[2]) $($case[4]) exact diagnostic"
        [Environment]::SetEnvironmentVariable($case[0], $old)
    }

    foreach ($case in @(
        ,@("missing-banner", "msvc-cl.compilerVersion expected 19.44.35228, actual unavailable")
        ,@("multiple-banner", "msvc-cl.compilerVersion expected 19.44.35228, actual multiple: 2 banners")
        ,@("malformed-banner", "msvc-cl.compilerVersion expected 19.44.35228, actual malformed")
        ,@("missing-d8003", "msvc-cl.compilerDiagnostic expected D8003, actual unavailable")
        ,@("launch-failure", "msvc-cl.compilerLaunch expected exact pinned compiler launched, actual exit 9009 without compiler banner")
    )) {
        $env:FAKE_CL_MODE = $case[0]
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[0]) compiler evidence fails"
        Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: $($case[1]). Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "$($case[0]) compiler evidence exact diagnostic"
    }
    $env:FAKE_CL_MODE = "normal"

    foreach ($compilerExit in @("0", "1", "3")) {
        $env:FAKE_CL_EXIT = $compilerExit
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "compiler exit $compilerExit fails"
        Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.compilerExit expected 2, actual $compilerExit. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "compiler exit $compilerExit exact diagnostic"
    }
    $env:FAKE_CL_EXIT = "2"

    Assert-AcceptedVsPath (Join-Path $Temp "vs install (x64)") "spaced parenthesized"
    Assert-AcceptedVsPath (Join-Path $Temp "vs & path-injection & install") "ampersand"

    foreach ($unsafeCharacter in @("^", "%", "!")) {
        $unsafeVs = Join-Path $Temp "vs $unsafeCharacter path-injection"
        Set-FakeVsLayout $unsafeVs | Out-Null
        Reset-Witness
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "MSVC $unsafeCharacter path rejected"
        $unsafeVcvars = Join-Path $unsafeVs "VC\Auxiliary\Build\vcvarsall.bat"
        $unsafeCl = Join-Path $unsafeVs "VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\cl.cmd"
        $expectedPathText = "absolute path free of cmd metacharacters (^ % !)"
        $repairText = "Install the pinned Visual Studio Build Tools MSVC x64 toolset."
        Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.vcvarsallPath expected $expectedPathText, actual $unsafeVcvars. $repairText")) "MSVC $unsafeCharacter vcvars exact rejection diagnostic"
        Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.path expected $expectedPathText, actual $unsafeCl. $repairText")) "MSVC $unsafeCharacter compiler exact rejection diagnostic"
        $unsafeWitness = Get-Content $Witness | Out-String
        Assert-True (-not $unsafeWitness.Contains("vcvarsall|")) "MSVC $unsafeCharacter rejection launches no vcvars child"
        Assert-True (-not $unsafeWitness.Contains("pinned-cl|")) "MSVC $unsafeCharacter rejection launches no pinned compiler"
        Assert-True (-not $unsafeWitness.Contains("AMBIENT-CL")) "MSVC $unsafeCharacter rejection launches no ambient compiler"
        Assert-True (-not $unsafeWitness.Contains("PATH-INJECTION")) "MSVC $unsafeCharacter rejection performs no injected action"
    }
    $toolset = Set-FakeVsLayout $FakeVs

    $env:FAKE_CARGO_VERSION = "malformed"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "malformed cargo version fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: cargo.version expected 1.96.0, actual unavailable. Run 'make rust-toolchain' on the Windows build box.")) "malformed cargo exact expected/actual/repair diagnostic"
    $env:FAKE_CARGO_VERSION = "1.96.0"

    $env:FAKE_SMCTL_VERSION = "9.9.9"
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "smctl skew fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: smctl.version expected 1.64.2, actual 9.9.9. Install DigiCert smctl 1.64.2 on the Windows build box.")) "smctl exact expected/actual/repair diagnostic"
    $env:FAKE_SMCTL_VERSION = "1.64.2"

    foreach ($case in @(
        ,@("FAKE_VPK_ID", "wrong", "ERROR: release tool mismatch: vpk.globalToolRow expected one vpk row, actual unavailable. Install the pinned Velopack global tool on the Windows build box.")
        ,@("FAKE_VPK_COMMAND", "wrong", "ERROR: release tool mismatch: vpk.command expected vpk, actual wrong. Install the pinned Velopack global tool on the Windows build box.")
    )) {
        $old = [Environment]::GetEnvironmentVariable($case[0])
        [Environment]::SetEnvironmentVariable($case[0], $case[1])
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[0]) mismatch fails"
        Assert-True ($result.stderr.Contains($case[2])) "$($case[0]) mismatch diagnostic"
        [Environment]::SetEnvironmentVariable($case[0], $old)
    }

    foreach ($case in @(
        ,@("duplicate", "ERROR: release tool mismatch: vpk.globalToolRow expected one vpk row, actual 2 rows. Install the pinned Velopack global tool on the Windows build box.")
        ,@("malformed", "ERROR: release tool mismatch: vpk.version expected 1.2.0, actual malformed. Install the pinned Velopack global tool on the Windows build box.")
    )) {
        $env:FAKE_VPK_ROW_MODE = $case[0]
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[0]) vpk row fails"
        Assert-True ($result.stderr.Contains($case[1])) "$($case[0]) vpk row exact expected/actual/repair diagnostic"
    }
    $env:FAKE_VPK_ROW_MODE = "normal"

    foreach ($case in @(
        ,@("rustc", "Run 'make rust-toolchain' on the Windows build box.")
        ,@("cargo", "Run 'make rust-toolchain' on the Windows build box.")
        ,@("cargo-deny", "Run 'make provision-cargo-deny'.")
        ,@("dotnet", "Install the pinned .NET SDK on the Windows build box.")
        ,@("node", "Install Node.js 24.16.0 on the Windows build box.")
        ,@("npm", "Install npm 11.13.0 with the pinned Node.js toolchain.")
    )) {
        $tool = $case[0]
        $path = Join-Path $FakeBin "$tool.cmd"
        $saved = "$path.saved"
        Move-Item $path $saved
        Reset-Witness
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$tool missing fails"
        $missingDiagnostic = if ($tool -eq "npm") {
            "ERROR: release tool mismatch: npm.path expected one reachable npm.cmd co-located with selected node, actual unavailable. $($case[1])"
        } else {
            "ERROR: release tool mismatch: $tool.path expected one executable, actual unavailable. $($case[1])"
        }
        Assert-True ($result.stderr.Contains($missingDiagnostic)) "$tool missing exact expected/actual/repair diagnostic"
        if ($tool -eq "node") {
            Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: npm.path expected one reachable npm.cmd co-located with selected node, actual selected node unavailable. Install npm 11.13.0 with the pinned Node.js toolchain.")) "missing node prevents npm selection"
            Assert-True (-not ((Get-Content $Witness | Out-String).Contains("npm|"))) "missing node prevents npm invocation"
        }
        Move-Item $saved $path
    }

    $vswhereFake = Join-Path $FakeBin "vswhere.cmd"
    Move-Item $vswhereFake "$vswhereFake.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing vswhere fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.vswhere expected exact executable, actual unavailable. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "missing vswhere exact expected/actual/repair diagnostic"
    Move-Item "$vswhereFake.saved" $vswhereFake

    $clFake = Join-Path $toolset "cl.cmd"
    Move-Item $clFake "$clFake.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing cl fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.path expected $clFake, actual unavailable. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "missing cl exact expected/actual/repair diagnostic"
    Move-Item "$clFake.saved" $clFake

    $vcvarsFake = Join-Path $FakeVs "VC\Auxiliary\Build\vcvarsall.bat"
    Move-Item $vcvarsFake "$vcvarsFake.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing vcvarsall fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.vcvarsallPath expected $vcvarsFake, actual unavailable. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "missing vcvarsall exact expected/actual/repair diagnostic"
    Move-Item "$vcvarsFake.saved" $vcvarsFake

    Copy-Item (Join-Path $FakeBin "cargo.cmd") (Join-Path $FakeBin "cargo.exe")
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "duplicate cargo executable fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: cargo.path expected one executable, actual multiple:")) "duplicate cargo expected/actual diagnostic"
    Assert-True ($result.stderr.Contains("Run 'make rust-toolchain' on the Windows build box.")) "duplicate cargo repair"
    Remove-Item (Join-Path $FakeBin "cargo.exe")

    $toolsetRoot = Join-Path $FakeVs "VC\Tools\MSVC\14.44.35207"
    Move-Item $toolsetRoot "$toolsetRoot.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing MSVC toolset fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: msvc-cl.toolsetVersion expected 14.44.35207, actual unavailable. Install the pinned Visual Studio Build Tools MSVC x64 toolset.")) "missing MSVC toolset exact expected/actual/repair diagnostic"
    Move-Item "$toolsetRoot.saved" $toolsetRoot

    $sdkRoot = Join-Path $FakeKits "Lib\10.0.26100.0"
    Move-Item $sdkRoot "$sdkRoot.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing Windows SDK fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: windows-sdk.version expected 10.0.26100.0, actual unavailable. Install Windows SDK 10.0.26100.0.")) "missing Windows SDK exact expected/actual/repair diagnostic"
    Move-Item "$sdkRoot.saved" $sdkRoot

    $contract = Fresh-Contract
    $contract.tools.powershell.expected.majorMinor = "9.9"
    Save-Contract $contract
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "PowerShell skew fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: powershell.version expected 9.9, actual 5.1. Run the release rail under Windows PowerShell 5.1.")) "PowerShell exact expected/actual/repair diagnostic"

    $contract = Fresh-Contract
    $contract.tools.signtool.expected.productVersion = "0.0.0.0"
    Save-Contract $contract
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "SignTool ProductVersion skew fails"
    $actualSigntoolProductVersion = ([Diagnostics.FileVersionInfo]::GetVersionInfo($signtoolPath)).ProductVersion
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: signtool.productVersion expected 0.0.0.0, actual $actualSigntoolProductVersion. Install the pinned x64 Windows SDK SignTool; ambient PATH copies are not accepted.")) "SignTool ProductVersion exact expected/actual/repair diagnostic"

    $contract = Fresh-Contract
    $contract.tools.signtool.expected.originalFilename = "WRONG.EXE"
    Save-Contract $contract
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "SignTool OriginalFilename skew fails"
    $actualSigntoolOriginalFilename = ([Diagnostics.FileVersionInfo]::GetVersionInfo($signtoolPath)).OriginalFilename
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: signtool.originalFilename expected WRONG.EXE, actual $actualSigntoolOriginalFilename. Install the pinned x64 Windows SDK SignTool; ambient PATH copies are not accepted.")) "SignTool OriginalFilename exact expected/actual/repair diagnostic"

    Fresh-Contract | Out-Null
    Move-Item $signtoolPath "$signtoolPath.saved"
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "missing exact SignTool fails"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: signtool.path expected $signtoolPath, actual unavailable. Install the pinned x64 Windows SDK SignTool; ambient PATH copies are not accepted.")) "missing exact SignTool expected/actual/repair diagnostic"
    Move-Item "$signtoolPath.saved" $signtoolPath

    $contract = Fresh-Contract
    $contract.tools.dotnet.expected.version = $null
    $nullPinJson = $contract | ConvertTo-Json -Depth 12
    $contract = Fresh-Contract
    $contract.schema = "wrong.schema"
    $invalidSchemaJson = $contract | ConvertTo-Json -Depth 12
    Fresh-Contract | Out-Null
    $validContractJson = Get-Content -LiteralPath $ContractPath -Raw -Encoding UTF8
    $duplicateSchemaJson = $validContractJson -replace '"schema"\s*:\s*"solstone.release-toolchain.v1"', '"schema": "solstone.release-toolchain.v1", "schema": "duplicate"'
    foreach ($case in @(
        [pscustomobject]@{ name = "null pin"; json = $nullPinJson; error = "ERROR: release toolchain contract invalid: contract.tools.dotnet.expected.version must not be null." },
        [pscustomobject]@{ name = "invalid schema"; json = $invalidSchemaJson; error = "ERROR: release toolchain contract invalid: schema must be solstone.release-toolchain.v1." },
        [pscustomobject]@{ name = "duplicate schema"; json = $duplicateSchemaJson; error = "ERROR: release toolchain contract invalid: schema must appear exactly once." }
    )) {
        [IO.File]::WriteAllText($ContractPath, $case.json, [Text.Encoding]::UTF8)
        Reset-Witness
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case.name) contract fails"
        Assert-True ($result.stderr -eq $case.error) "$($case.name) exact contract diagnostic"
        Assert-True ([string]::IsNullOrEmpty((Get-Content $Witness -Raw))) "$($case.name) invokes no tools"
    }

    $contract = Fresh-Contract
    $contract.groups.unsigned += "cargo"
    Save-Contract $contract
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "duplicate group entry fails"
    Assert-True ([string]::IsNullOrEmpty((Get-Content $Witness -Raw))) "duplicate contract invokes no tools"

    $contract = Fresh-Contract
    $contract.tools | Add-Member -NotePropertyName unexpected -NotePropertyValue $contract.tools.cargo
    Save-Contract $contract
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "unknown tool fails"
    Assert-True ([string]::IsNullOrEmpty((Get-Content $Witness -Raw))) "unknown tool invokes no tools"

    Fresh-Contract | Out-Null
    Remove-Item -LiteralPath (Join-Path $FakeProfile ".dotnet\tools\vpk.exe")
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing vpk path fails"
    $expectedVpkPath = Join-Path $FakeProfile ".dotnet\tools\vpk.exe"
    Assert-True ($result.stderr.Contains("ERROR: release tool mismatch: vpk.path expected $expectedVpkPath, actual unavailable. Install the pinned Velopack global tool on the Windows build box.")) "missing vpk path exact expected/actual/repair diagnostic"

    Write-Host "preflight-release-tools.test.ps1: $Assertions assertions passed"
} finally {
    $env:SOLSTONE_SIGN = $OldSign
    $env:PATH = $OldPath
    $env:VSCMD_ARG_HOST_ARCH = $OldMsvcHost
    $env:VSCMD_ARG_TGT_ARCH = $OldMsvcTarget
    $env:VCToolsVersion = $OldMsvcToolset
    foreach ($name in @(
        "SOLSTONE_RELEASE_TOOLS_CONTRACT", "SOLSTONE_RELEASE_TOOLS_FAKE_BIN",
        "SOLSTONE_RELEASE_TOOLS_FAKE_NPM_PATH", "SOLSTONE_RELEASE_TOOLS_FAKE_USERPROFILE", "SOLSTONE_RELEASE_TOOLS_FAKE_WINDOWS_KITS",
        "FAKE_RELEASE_WITNESS", "FAKE_RUST_RELEASE", "FAKE_RUST_HOST", "FAKE_CARGO_VERSION", "FAKE_DENY_VERSION",
        "FAKE_DOTNET_VERSION", "FAKE_VPK_ID", "FAKE_VPK_VERSION", "FAKE_VPK_COMMAND", "FAKE_VPK_ROW_MODE",
        "FAKE_NODE_VERSION", "FAKE_NPM_VERSION", "FAKE_NPM_EXIT", "FAKE_CL_VERSION", "FAKE_CL_MODE", "FAKE_CL_EXIT",
        "FAKE_VCVARS_EXIT", "FAKE_VCVARS_HOST", "FAKE_VCVARS_TARGET", "FAKE_VCVARS_TOOLSET", "FAKE_VCVARS_OUTPUT_MODE",
        "FAKE_SMCTL_VERSION"
    )) { [Environment]::SetEnvironmentVariable($name, $null) }
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
}
