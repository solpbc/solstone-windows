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
$FakeVs = Join-Path $Temp "vs"
$ContractPath = Join-Path $Temp "contract.json"
$Witness = Join-Path $Temp "witness.txt"
$Assertions = 0
$OldSign = $env:SOLSTONE_SIGN

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

try {
    New-Item -ItemType Directory -Path $FakeBin, $FakeProfile, $FakeKits, $FakeVs -Force | Out-Null
    $signtoolPath = Join-Path $FakeKits "bin\10.0.26100.0\x64\signtool.exe"
    New-Item -ItemType Directory -Path (Split-Path -Parent $signtoolPath) -Force | Out-Null
    Copy-Item -LiteralPath $env:ComSpec -Destination $signtoolPath
    Write-Ascii (Join-Path $FakeProfile ".dotnet\tools\vpk.exe") "fake vpk"
    New-Item -ItemType Directory -Path (Join-Path $FakeKits "Lib\10.0.26100.0") -Force | Out-Null
    $toolset = Join-Path $FakeVs "VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64"
    New-Item -ItemType Directory -Path $toolset -Force | Out-Null
    Write-Ascii (Join-Path $FakeVs "VC\Auxiliary\Build\vcvarsall.bat") "@echo off`r`nexit /b 0`r`n"

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
echo %FAKE_NPM_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "vswhere.cmd") '@echo off
echo vswhere^|%*>>"%FAKE_RELEASE_WITNESS%"
echo %FAKE_VS_INSTALL%
'
    Write-Ascii (Join-Path $toolset "cl.cmd") '@echo off
echo cl^|%*>>"%FAKE_RELEASE_WITNESS%"
echo Microsoft (R) C/C++ Optimizing Compiler Version %FAKE_CL_VERSION% for x64
'
    Write-Ascii (Join-Path $FakeBin "smctl.cmd") '@echo off
echo smctl^|%*>>"%FAKE_RELEASE_WITNESS%"
echo smctl version %FAKE_SMCTL_VERSION%
'
    Write-Ascii (Join-Path $FakeBin "signtool.cmd") '@echo off
echo AMBIENT-SIGNTOOL>>"%FAKE_RELEASE_WITNESS%"
exit /b 98
'

    $env:SOLSTONE_RELEASE_TOOLS_CONTRACT = $ContractPath
    $env:SOLSTONE_RELEASE_TOOLS_FAKE_BIN = $FakeBin
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
    $env:FAKE_NODE_VERSION = "24.16.0"
    $env:FAKE_NPM_VERSION = "11.13.0"
    $env:FAKE_VS_INSTALL = $FakeVs
    $env:FAKE_CL_VERSION = "19.44.35228"
    $env:FAKE_SMCTL_VERSION = "1.64.2"

    $smctlFake = Join-Path $FakeBin "smctl.cmd"
    Move-Item $smctlFake "$smctlFake.saved"
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "missing smctl fails"
    Assert-True ($result.stderr.Contains("smctl.path expected one executable, actual unavailable")) "missing smctl diagnostic"
    Move-Item "$smctlFake.saved" $smctlFake
    $env:SOLSTONE_SIGN = $null
    Fresh-Contract | Out-Null

    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -eq 0) "unsigned preflight succeeds"
    Assert-True ($result.stderr -eq "") "unsigned stderr empty"
    Assert-True (($result.stdout -split '\r?\n').Count -eq 1) "selection is one JSON line"
    $selection = $result.stdout | ConvertFrom-Json
    Assert-True ($selection.schema -eq "solstone.release-tool-selection.v1") "selection schema"
    Assert-True ($selection.mode -eq "unsigned") "unsigned mode"
    Assert-True ($selection.tools.cargo.path -eq (Join-Path $FakeBin "cargo.cmd")) "selected cargo path"
    Assert-True ($selection.tools.vpk.version -eq "1.2.0") "selected vpk version"
    Assert-True ($selection.tools.'msvc-cl'.compilerVersion -eq "19.44.35228") "compiler banner selected"
    Assert-True ($selection.tools.'msvc-cl'.toolsetVersion -eq "14.44.35207") "toolset directory selected"
    Assert-True ($null -eq $selection.tools.smctl) "unsigned omits smctl"
    Assert-True ($null -eq $selection.tools.signtool) "unsigned omits signtool"

    Reset-Witness
    $result = Run-Preflight -Sign
    Assert-True ($result.status -eq 0) "signed preflight succeeds"
    $selection = $result.stdout | ConvertFrom-Json
    Assert-True ($selection.mode -eq "signed") "signed mode"
    Assert-True ($selection.tools.smctl.version -eq "1.64.2") "signed selects smctl"
    Assert-True ($selection.tools.signtool.path -eq $signtoolPath) "signed selects exact SignTool"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("AMBIENT-SIGNTOOL"))) "ambient SignTool rejected"
    Assert-True (-not ((Get-Content $Witness | Out-String).Contains("healthcheck"))) "preflight performs no credential healthcheck"

    $env:SOLSTONE_SIGN = "1"
    $result = Run-Preflight
    Assert-True (($result.stdout | ConvertFrom-Json).mode -eq "signed") "SOLSTONE_SIGN selects signed mode"
    $env:SOLSTONE_SIGN = $null

    foreach ($case in @(
        ,@("FAKE_RUST_RELEASE", "9.9.9", "rustc.release expected 1.96.0, actual 9.9.9"),
        ,@("FAKE_RUST_HOST", "wrong-host", "rustc.host expected x86_64-pc-windows-msvc, actual wrong-host"),
        ,@("FAKE_CARGO_VERSION", "9.9.9", "cargo.version expected 1.96.0, actual 9.9.9"),
        ,@("FAKE_DENY_VERSION", "9.9.9", "cargo-deny.version expected 0.20.2, actual 9.9.9"),
        ,@("FAKE_DOTNET_VERSION", "9.9.9", "dotnet.version expected 8.0.422, actual 9.9.9"),
        ,@("FAKE_VPK_VERSION", "9.9.9", "vpk.version expected 1.2.0, actual 9.9.9"),
        ,@("FAKE_NODE_VERSION", "9.9.9", "node.version expected 24.16.0, actual 9.9.9"),
        ,@("FAKE_NPM_VERSION", "9.9.9", "npm.version expected 11.13.0, actual 9.9.9"),
        ,@("FAKE_CL_VERSION", "9.9.9", "msvc-cl.compilerVersion expected 19.44.35228, actual 9.9.9")
    )) {
        $old = [Environment]::GetEnvironmentVariable($case[0])
        [Environment]::SetEnvironmentVariable($case[0], $case[1])
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[0]) skew fails"
        Assert-True ($result.stderr.Contains($case[2])) "$($case[0]) expected/actual diagnostic"
        [Environment]::SetEnvironmentVariable($case[0], $old)
    }

    $env:FAKE_SMCTL_VERSION = "9.9.9"
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "smctl skew fails"
    Assert-True ($result.stderr.Contains("smctl.version expected 1.64.2, actual 9.9.9")) "smctl skew diagnostic"
    $env:FAKE_SMCTL_VERSION = "1.64.2"

    foreach ($case in @(
        ,@("FAKE_VPK_ID", "wrong", "vpk.globalToolRow"),
        ,@("FAKE_VPK_COMMAND", "wrong", "vpk.command expected vpk, actual wrong")
    )) {
        $old = [Environment]::GetEnvironmentVariable($case[0])
        [Environment]::SetEnvironmentVariable($case[0], $case[1])
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$($case[0]) mismatch fails"
        Assert-True ($result.stderr.Contains($case[2])) "$($case[0]) mismatch diagnostic"
        [Environment]::SetEnvironmentVariable($case[0], $old)
    }

    foreach ($tool in @("rustc", "cargo", "cargo-deny", "dotnet", "node", "npm")) {
        $path = Join-Path $FakeBin "$tool.cmd"
        $saved = "$path.saved"
        Move-Item $path $saved
        $result = Run-Preflight
        Assert-True ($result.status -ne 0) "$tool missing fails"
        Assert-True ($result.stderr.Contains("$tool.path expected one executable, actual unavailable")) "$tool missing diagnostic"
        Move-Item $saved $path
    }

    $vswhereFake = Join-Path $FakeBin "vswhere.cmd"
    Move-Item $vswhereFake "$vswhereFake.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing vswhere fails"
    Assert-True ($result.stderr.Contains("msvc-cl.vswhere expected exact executable, actual unavailable")) "missing vswhere diagnostic"
    Move-Item "$vswhereFake.saved" $vswhereFake

    Copy-Item (Join-Path $FakeBin "cargo.cmd") (Join-Path $FakeBin "cargo.exe")
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "duplicate cargo executable fails"
    Assert-True ($result.stderr.Contains("cargo.path expected one executable, actual multiple:")) "duplicate cargo diagnostic"
    Remove-Item (Join-Path $FakeBin "cargo.exe")

    $toolsetRoot = Join-Path $FakeVs "VC\Tools\MSVC\14.44.35207"
    Move-Item $toolsetRoot "$toolsetRoot.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing MSVC toolset fails"
    Assert-True ($result.stderr.Contains("msvc-cl.toolsetVersion expected 14.44.35207, actual unavailable")) "missing MSVC toolset diagnostic"
    Move-Item "$toolsetRoot.saved" $toolsetRoot

    $sdkRoot = Join-Path $FakeKits "Lib\10.0.26100.0"
    Move-Item $sdkRoot "$sdkRoot.saved"
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing Windows SDK fails"
    Assert-True ($result.stderr.Contains("windows-sdk.version expected 10.0.26100.0, actual unavailable")) "missing Windows SDK diagnostic"
    Move-Item "$sdkRoot.saved" $sdkRoot

    $contract = Fresh-Contract
    $contract.tools.powershell.expected.majorMinor = "9.9"
    Save-Contract $contract
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "PowerShell skew fails"
    Assert-True ($result.stderr.Contains("powershell.version expected 9.9, actual 5.1")) "PowerShell major/minor diagnostic"

    $contract = Fresh-Contract
    $contract.tools.signtool.expected.productVersion = "0.0.0.0"
    Save-Contract $contract
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "SignTool ProductVersion skew fails"
    Assert-True ($result.stderr.Contains("signtool.productVersion expected 0.0.0.0")) "SignTool ProductVersion diagnostic"

    $contract = Fresh-Contract
    $contract.tools.signtool.expected.originalFilename = "WRONG.EXE"
    Save-Contract $contract
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "SignTool OriginalFilename skew fails"
    Assert-True ($result.stderr.Contains("signtool.originalFilename expected WRONG.EXE")) "SignTool OriginalFilename diagnostic"

    Fresh-Contract | Out-Null
    Move-Item $signtoolPath "$signtoolPath.saved"
    $result = Run-Preflight -Sign
    Assert-True ($result.status -ne 0) "missing exact SignTool fails"
    Assert-True ($result.stderr.Contains("signtool.path expected $signtoolPath, actual unavailable")) "missing exact SignTool diagnostic"
    Move-Item "$signtoolPath.saved" $signtoolPath

    $contract = Fresh-Contract
    $contract.tools.dotnet.expected.version = $null
    Save-Contract $contract
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "null contract pin fails"
    Assert-True ($result.stderr.StartsWith("ERROR: release toolchain contract invalid:")) "null contract diagnostic"
    Assert-True ((Get-Content $Witness -Raw) -eq "") "invalid contract invokes no tools"

    $contract = Fresh-Contract
    $contract.groups.unsigned += "cargo"
    Save-Contract $contract
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "duplicate group entry fails"
    Assert-True ((Get-Content $Witness -Raw) -eq "") "duplicate contract invokes no tools"

    $contract = Fresh-Contract
    $contract.tools | Add-Member -NotePropertyName unexpected -NotePropertyValue $contract.tools.cargo
    Save-Contract $contract
    Reset-Witness
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "unknown tool fails"
    Assert-True ((Get-Content $Witness -Raw) -eq "") "unknown tool invokes no tools"

    Fresh-Contract | Out-Null
    Remove-Item -LiteralPath (Join-Path $FakeProfile ".dotnet\tools\vpk.exe")
    $result = Run-Preflight
    Assert-True ($result.status -ne 0) "missing vpk path fails"
    Assert-True ($result.stderr.Contains("vpk.path expected")) "missing vpk path diagnostic"

    Write-Host "preflight-release-tools.test.ps1: $Assertions assertions passed"
} finally {
    $env:SOLSTONE_SIGN = $OldSign
    foreach ($name in @(
        "SOLSTONE_RELEASE_TOOLS_CONTRACT", "SOLSTONE_RELEASE_TOOLS_FAKE_BIN",
        "SOLSTONE_RELEASE_TOOLS_FAKE_USERPROFILE", "SOLSTONE_RELEASE_TOOLS_FAKE_WINDOWS_KITS",
        "FAKE_RELEASE_WITNESS", "FAKE_RUST_RELEASE", "FAKE_RUST_HOST", "FAKE_CARGO_VERSION", "FAKE_DENY_VERSION",
        "FAKE_DOTNET_VERSION", "FAKE_VPK_ID", "FAKE_VPK_VERSION", "FAKE_VPK_COMMAND",
        "FAKE_NODE_VERSION", "FAKE_NPM_VERSION", "FAKE_VS_INSTALL", "FAKE_CL_VERSION",
        "FAKE_SMCTL_VERSION"
    )) { [Environment]::SetEnvironmentVariable($name, $null) }
    if (Test-Path $Temp) { Remove-Item -Recurse -Force $Temp }
}
