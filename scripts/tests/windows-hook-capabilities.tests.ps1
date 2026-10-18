$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot '../windows-hook-capabilities.ps1')

$script:probeResult = $null
function Get-WindowsHookCapabilityResponse {
    param([string] $HelperPath)
    if ($HelperPath -ne 'fixture/pebrel-hook.exe') { throw 'Unexpected helper selected' }
    return $script:probeResult
}

$accepted = "pebrel-hook-capabilities/1 native-hook/1 wsl-hook/1`n"
foreach ($output in @($accepted, $accepted.Replace("`n", "`r`n"))) {
    $script:probeResult = [PSCustomObject]@{ ExitCode = 0; Output = $output }
    Assert-WindowsHookCapabilities -HelperPath 'fixture/pebrel-hook.exe'
}

foreach ($case in @(
    @{ ExitCode = 0; Output = ''; Label = 'Empty exit-zero response' },
    @{ ExitCode = 0; Output = "pebrel-hook-capabilities/1 native-hook/1`n"; Label = 'Native-only helper' },
    @{ ExitCode = 0; Output = $accepted.Replace('capabilities/1', 'capabilities/2'); Label = 'Unknown protocol' },
    @{ ExitCode = 0; Output = $accepted + "extra`n"; Label = 'Additional output' },
    @{ ExitCode = 0; Output = ('x' * 65); Label = 'Oversized response' },
    @{ ExitCode = 7; Output = $accepted; Label = 'Failed process with correct text' }
)) {
    $script:probeResult = [PSCustomObject]@{ ExitCode = $case.ExitCode; Output = $case.Output }
    $rejected = $false
    try { Assert-WindowsHookCapabilities -HelperPath 'fixture/pebrel-hook.exe' } catch {
        $rejected = $_.Exception.Message -match 'capability'
    }
    if (-not $rejected) { throw "$($case.Label) was accepted as a compatible package helper" }
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
foreach ($name in @('package-release.ps1', 'build-installer.ps1')) {
    $source = Get-Content -LiteralPath (Join-Path $repo "scripts/$name") -Raw -Encoding UTF8
    if ($source -notmatch 'windows-hook-capabilities.ps1' -or
        $source -notmatch 'Assert-WindowsHookCapabilities') {
        throw "$name does not enforce the helper capability contract"
    }
}

Write-Output 'windows-hook-capabilities.tests.ps1: PASS (positive and negative helper contracts)'
