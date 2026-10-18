# Require the packaged helper's own capability response, not its name or timestamp.
function Get-WindowsHookCapabilityResponse {
    param([Parameter(Mandatory = $true)][string] $HelperPath)
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = [IO.Path]::GetFullPath($HelperPath)
    $start.Arguments = '--capabilities'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($key in @($start.Environment.Keys)) {
        if ($key -match '^(PEBREL|NEBULA)_') { [void] $start.Environment.Remove($key) }
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    try {
        $started = $process.Start()
        if (-not $started) { throw "Hook helper capability probe could not start: $HelperPath" }
        $process.StandardInput.Close()
        # Leave output in the bounded OS pipe until exit. A noisy helper reaches the timeout.
        if (-not $process.WaitForExit(2000)) {
            throw "Hook helper capability probe timed out: $HelperPath"
        }
        $buffer = [char[]]::new(65)
        $read = $process.StandardOutput.ReadBlockAsync($buffer, 0, $buffer.Length)
        if (-not $read.Wait(1000)) {
            throw "Hook helper capability output did not close: $HelperPath"
        }
        $count = $read.GetAwaiter().GetResult()
        [PSCustomObject]@{
            ExitCode = $process.ExitCode
            Output = [string]::new($buffer, 0, $count)
        }
    } finally {
        if ($started -and -not $process.HasExited) {
            try { $process.Kill($true); [void] $process.WaitForExit(1000) } catch {}
        }
        $process.Dispose()
    }
}

function Assert-WindowsHookCapabilities {
    param([Parameter(Mandatory = $true)][string] $HelperPath)
    $result = Get-WindowsHookCapabilityResponse -HelperPath $HelperPath
    $expected = "pebrel-hook-capabilities/1 native-hook/1 wsl-hook/1`n"
    $output = $result.Output.Replace("`r`n", "`n")
    if ($result.ExitCode -ne 0 -or $result.Output.Length -gt 64 -or $output -cne $expected) {
        throw "Hook helper capability contract is missing or incompatible: $HelperPath"
    }
}
