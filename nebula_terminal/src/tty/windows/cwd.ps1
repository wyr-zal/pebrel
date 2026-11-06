# Report successful location changes before a long-running command can block
# the next prompt. Only FileSystem locations are terminal working directories.
function global:Send-PebrelWorkingDirectory {
    param($Location)

    if ($Location.Provider.Name -ne 'FileSystem') { return }
    $path = $Location.ProviderPath.Replace('\', '/')
    if (-not $path.StartsWith('/')) { $path = '/' + $path }
    # Encode each component so spaces, Unicode, %, # and control bytes cannot
    # change the OSC framing. Keep both leading slashes for a UNC directory.
    $uriPath = ($path.Split('/') | ForEach-Object { [Uri]::EscapeDataString($_) }) -join '/'
    [Console]::Write("$([char]27)]7;file://localhost$uriPath$([char]7)")
}

# Windows PowerShell 5.1 lacks this hook and keeps prompt-based reporting.
$locationCommands = $ExecutionContext.SessionState.InvokeCommand
if ($locationCommands.PSObject.Properties['LocationChangedAction'] -and
    (-not $global:PebrelLocationChangedAction -or
        -not [object]::ReferenceEquals($locationCommands.LocationChangedAction, $global:PebrelLocationChangedAction))) {
    $previousLocationAction = $locationCommands.LocationChangedAction
    $locationAction = {
        param($sender, $eventArgs)

        try {
            if ($previousLocationAction) { $previousLocationAction.Invoke($sender, $eventArgs) }
        } finally {
            # An existing callback may change location again. Read the final
            # actual location so the older outer event cannot overwrite it.
            try { Send-PebrelWorkingDirectory $ExecutionContext.SessionState.Path.CurrentLocation } catch {}
        }
    }.GetNewClosure()
    $global:PebrelLocationChangedAction = [System.EventHandler[System.Management.Automation.LocationChangedEventArgs]]$locationAction
    $locationCommands.LocationChangedAction = $global:PebrelLocationChangedAction
}
