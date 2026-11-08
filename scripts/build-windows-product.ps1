[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string] $Configuration = 'release',
    [string] $TargetDirectory,
    [switch] $DeployDebugBundle
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$useExplicitTargetDirectory = -not [string]::IsNullOrWhiteSpace($TargetDirectory)
if ($DeployDebugBundle) {
    if ($Configuration -ne 'debug') { throw '-DeployDebugBundle 只能与 -Configuration debug 一起使用。' }
    if (-not $useExplicitTargetDirectory) {
        $config = Get-Content -LiteralPath (Join-Path $repo '.cargo/config.toml') -Raw
        $targetMatches = [regex]::Matches($config, '(?m)^[ \t]*target-dir[ \t]*=[ \t]*"([^"]+)"[ \t]*$')
        if ($targetMatches.Count -ne 1) { throw '.cargo/config.toml 中必须且只能有一个 target-dir。' }
        $TargetDirectory = $targetMatches[0].Groups[1].Value
        $useExplicitTargetDirectory = $true
    }
}
if ($useExplicitTargetDirectory -and -not [IO.Path]::IsPathRooted($TargetDirectory)) {
    $TargetDirectory = Join-Path $repo $TargetDirectory
}
if ($useExplicitTargetDirectory) { $TargetDirectory = [IO.Path]::GetFullPath($TargetDirectory) }

if ($DeployDebugBundle) {
    $expectedTarget = [IO.Path]::GetFullPath('F:/ProjectsWork/pebrel-debug-daily-20261013/cargo-target')
    if (-not [string]::Equals($TargetDirectory, $expectedTarget, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Cargo 输出目录不是约定的测试目录，已停止：$TargetDirectory"
    }
    $bundle = Split-Path -Parent $TargetDirectory
    $targetExe = Join-Path $bundle 'pebrel.exe'
    if (-not (Test-Path -LiteralPath (Join-Path $bundle 'runtime') -PathType Container)) {
        throw "测试 bundle 缺少 runtime 目录：$bundle"
    }
    function Assert-PebrelBundleClosed {
        param([string] $ExecutablePath)
        $expectedPath = [IO.Path]::GetFullPath($ExecutablePath)
        $running = @(Get-CimInstance -ClassName Win32_Process -Filter "Name = 'pebrel.exe'" -ErrorAction Stop)
        foreach ($process in $running) {
            if ([string]::IsNullOrWhiteSpace($process.ExecutablePath)) {
                throw "无法确认 Pebrel 进程 $($process.ProcessId) 的程序路径；测试 bundle 未改动。"
            }
            if ([string]::Equals([IO.Path]::GetFullPath($process.ExecutablePath), $expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
                throw "目标测试 bundle 正被 Pebrel 窗口使用（PID $($process.ProcessId)）。请关闭该窗口后重跑；bundle 未改动。"
            }
        }
    }
    Assert-PebrelBundleClosed -ExecutablePath $targetExe
    $artifacts = @(
        [pscustomobject]@{ Source = 'pebrel.exe'; Destination = 'pebrel.exe' },
        [pscustomobject]@{ Source = 'pebrel.pdb'; Destination = 'pebrel.pdb' },
        [pscustomobject]@{ Source = 'pebrel-hook.exe'; Destination = 'runtime/pebrel-hook.exe' },
        [pscustomobject]@{ Source = 'pebrel_hook.pdb'; Destination = 'runtime/pebrel_hook.pdb' }
    )
    foreach ($artifact in $artifacts) {
        $destinationPath = Join-Path $bundle $artifact.Destination
        if (-not (Test-Path -LiteralPath $destinationPath -PathType Leaf)) { throw "测试 bundle 缺少应有文件：$destinationPath" }
    }
    $backupRoot = 'F:/12704/temp-backups'
    if (-not (Test-Path -LiteralPath $backupRoot -PathType Container)) { throw "备份目录不存在：$backupRoot" }
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
        '-p', 'nebula', '--bin', 'pebrel', '--bin', 'pebrel-cli',
        '-p', 'nebula_hook', '--bin', 'pebrel-hook',
        '--features', 'nebula/gpui-shell'
    )
    if ($Configuration -eq 'release') { $arguments += '--release' }
    & cargo @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo Windows product build failed with exit code $LASTEXITCODE"
    }
    if ($DeployDebugBundle) {
        $buildOutput = Join-Path $TargetDirectory 'debug'
        foreach ($artifact in $artifacts) {
            $sourcePath = Join-Path $buildOutput $artifact.Source
            if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) { throw "构建产物缺失：$sourcePath" }
        }
        Assert-PebrelBundleClosed -ExecutablePath $targetExe

        $date = Get-Date -Format 'yyyyMMdd'
        $task = 'pebrel-debug-bundle-' + (Get-Date -Format 'HHmmssfff')
        $backupDateDirectory = Join-Path $backupRoot $date
        New-Item -ItemType Directory -Path $backupDateDirectory -Force | Out-Null
        $backupBundle = Join-Path $backupDateDirectory (Join-Path $task 'bundle')
        New-Item -ItemType Directory -Path $backupBundle | Out-Null
        foreach ($artifact in $artifacts) {
            $destinationPath = Join-Path $bundle $artifact.Destination
            $backupPath = Join-Path $backupBundle $artifact.Destination
            New-Item -ItemType Directory -Path (Split-Path -Parent $backupPath) -Force | Out-Null
            Copy-Item -LiteralPath $destinationPath -Destination $backupPath
            $oldHash = (Get-FileHash -LiteralPath $destinationPath -Algorithm SHA256).Hash
            $backupHash = (Get-FileHash -LiteralPath $backupPath -Algorithm SHA256).Hash
            if ($oldHash -ne $backupHash) { throw "旧文件备份校验失败：$($artifact.Destination)；尚未改动 bundle。" }
        }

        $deployStarted = $false
        try {
            foreach ($artifact in $artifacts) {
                $deployStarted = $true
                Copy-Item -LiteralPath (Join-Path $buildOutput $artifact.Source) `
                    -Destination (Join-Path $bundle $artifact.Destination) -Force
            }
            & icacls.exe $targetExe /setintegritylevel M | Out-Null
            if ($LASTEXITCODE -ne 0) { throw '无法将 pebrel.exe 设置为 Medium Integrity。' }
            foreach ($artifact in $artifacts) {
                $sourceHash = (Get-FileHash -LiteralPath (Join-Path $buildOutput $artifact.Source) -Algorithm SHA256).Hash
                $deployedHash = (Get-FileHash -LiteralPath (Join-Path $bundle $artifact.Destination) -Algorithm SHA256).Hash
                if ($sourceHash -ne $deployedHash) { throw "文件 SHA256 不一致：$($artifact.Destination)" }
            }
        } catch {
            $deployError = $_
            if ($deployStarted) {
                try {
                    foreach ($artifact in $artifacts) {
                        Copy-Item -LiteralPath (Join-Path $backupBundle $artifact.Destination) `
                            -Destination (Join-Path $bundle $artifact.Destination) -Force
                    }
                    & icacls.exe $targetExe /setintegritylevel M | Out-Null
                    if ($LASTEXITCODE -ne 0) { throw '回滚后无法恢复 pebrel.exe 的 Medium Integrity。' }
                } catch {
                    throw "交付失败：$($deployError.Exception.Message)；回滚也失败：$($_.Exception.Message)。旧文件备份在：$backupBundle"
                }
            }
            throw $deployError.Exception
        }

        Write-Output "Debug bundle 交付并校验完成。旧文件备份：$backupBundle"
        foreach ($artifact in $artifacts) {
            $hash = (Get-FileHash -LiteralPath (Join-Path $bundle $artifact.Destination) -Algorithm SHA256).Hash
            Write-Output "$($artifact.Destination) SHA256 $hash"
        }
    }
} finally {
    if ($useExplicitTargetDirectory) {
        $env:CARGO_TARGET_DIR = $previousTargetDirectory
    }
    Pop-Location
}
