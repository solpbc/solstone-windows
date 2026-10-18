# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'smoke-fresh-gate.ps1')
function Require([bool]$ok, [string]$why) { if (!$ok) { throw $why } }
function Refuses([scriptblock]$action) {
    $failed = $false
    try { $null = & $action } catch { $failed = $true }
    Require $failed 'expected refusal'
}
$a = [pscustomobject]@{Kind='String';Data='app.exe --token AbC'}
$b = [pscustomobject]@{Kind='String';Data='app.exe --token abc'}
Require (Test-SmokeRunValueEqual $a $a) 'identical Run value'
Require (!(Test-SmokeRunValueEqual $a $b)) 'case-sensitive Run argument changed'
Require (!(Test-SmokeRunValueEqual $a $null)) 'Run value removed'
Require (Test-SmokeRunValueEqual $null $null) 'absent Run values'
Require (!(Test-SmokeRunValueEqual $a ([pscustomobject]@{Kind='ExpandString';Data=$a.Data}))) 'Run kind changed'
Require (Test-SmokeRunValueEqual ([pscustomobject]@{Kind='Binary';Data=[byte[]](1,2)}) ([pscustomobject]@{Kind='Binary';Data=[byte[]](1,2)})) 'binary equality'
Require (!(Test-SmokeRunValueEqual ([pscustomobject]@{Kind='MultiString';Data=[string[]]('A','B')}) ([pscustomobject]@{Kind='MultiString';Data=[string[]]('A','b')}))) 'multi-string case changed'
$apps = @([pscustomobject]@{ExecutablePath='C:\proof\app.exe';ProcessId=42})
$ports = @([pscustomobject]@{LocalPort=47247;OwningProcess=42})
Require ((Get-SmokeHealthOwner 'C:\proof\app.exe' 47247 $apps $ports) -eq 42) 'matching listener owner'
Refuses { Get-SmokeHealthOwner 'C:\other\app.exe' 47247 $apps $ports }
Refuses { Get-SmokeHealthOwner 'C:\proof\app.exe' 47247 $apps @() }
Refuses { Get-SmokeHealthOwner 'C:\proof\app.exe' 47247 @($apps[0],$apps[0]) $ports }
Refuses { Get-SmokeHealthOwner 'C:\proof\app.exe' 47247 $apps @([pscustomobject]@{LocalPort=47247;OwningProcess=43}) }
$root = Join-Path ([IO.Path]::GetTempPath()) ('smoke-fresh-'+[guid]::NewGuid().ToString('N'))
try {
    Assert-SmokeNoMedia $root
    $null = New-Item -ItemType Directory -Path $root
    Assert-SmokeNoMedia $root
    foreach ($name in @('1.incomplete','quarantine','1')) {
        $child = Join-Path $root $name
        $null = New-Item -ItemType Directory -Path $child
        Refuses { Assert-SmokeNoMedia $root }
        Remove-Item -LiteralPath $child
    }
} finally { if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force } }

if (Select-SmokeFreshState -Setup ([pscustomobject]@{sequence_enabled=$false}) -RequestedFresh $false) { throw 'dark candidate selected fresh proof' }
if (-not (Select-SmokeFreshState -Setup ([pscustomobject]@{sequence_enabled=$true}) -RequestedFresh $false)) { throw 'activated candidate selected legacy proof' }
foreach ($setup in @($null, [pscustomobject]@{}, [pscustomobject]@{sequence_enabled='false'})) {
    $refused = $false
    try { Select-SmokeFreshState -Setup $setup -RequestedFresh $false } catch { $refused=$true }
    if (-not $refused) { throw 'unknown activation was accepted' }
}
$refused = $false
try { Select-SmokeFreshState -Setup ([pscustomobject]@{sequence_enabled=$false}) -RequestedFresh $true } catch { $refused=$true }
if (-not $refused) { throw 'dark candidate satisfied explicit fresh proof' }

Write-Host 'smoke-fresh-gate.test.ps1: passed'
