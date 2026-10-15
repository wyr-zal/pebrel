[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release',
    [string] $TargetDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$useExplicitTargetDirectory = -not [string]::IsNullOrWhiteSpace($TargetDirectory)
if ($useExplicitTargetDirectory -and -not [IO.Path]::IsPathRooted($TargetDirectory)) {
    $TargetDirectory = Join-Path $repo $TargetDirectory
}
$previousTargetDirectory = $env:CARGO_TARGET_DIR
Push-Location $repo
try {
    if ($useExplicitTargetDirectory) {
        $env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($TargetDirectory)
    }
    # Keep the feature graph identical for the initial build, ZIP and installer.
    # The acceptance lab is tested by CI but does not supply a packaged binary.
    $arguments = @(
        'build', '--locked', '--timings',
        '-p', 'nebula', '--bin', 'pebrel',
        '-p', 'nebula_hook', '--bin', 'pebrel-hook',
        '--features', 'nebula/gpui-shell'
    )
    if ($Configuration -eq 'release') { $arguments += '--release' }
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo Windows product build failed with exit code $LASTEXITCODE"
    }
} finally {
    if ($useExplicitTargetDirectory) {
        $env:CARGO_TARGET_DIR = $previousTargetDirectory
    }
    Pop-Location
}
