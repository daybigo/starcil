[CmdletBinding()]
param(
    [string]$StarcilBinary,
    [string]$PluginPath,
    [ValidateRange(1, 60)]
    [int]$TimeoutSeconds = 8
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw "verify-plugin-e2e.ps1 requires Windows"
}

if ([string]::IsNullOrWhiteSpace($StarcilBinary)) {
    $StarcilBinary = Join-Path $PSScriptRoot "..\target\debug\starcil.exe"
}
if ([string]::IsNullOrWhiteSpace($PluginPath)) {
    $PluginPath = Join-Path $PSScriptRoot "..\examples\plugins\hello-starcil"
}
$StarcilBinary = (Resolve-Path -LiteralPath $StarcilBinary).Path
$PluginPath = (Resolve-Path -LiteralPath $PluginPath).Path
$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
$session = "d5-$PID-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) "starcil-plugin-e2e-$([Guid]::NewGuid().ToString('N'))"
$serverStdout = Join-Path $temporaryRoot "server.stdout.log"
$serverStderr = Join-Path $temporaryRoot "server.stderr.log"
$pluginId = "examples.hello-starcil"
$actionId = "$pluginId.capture-environment"
$failures = [System.Collections.Generic.List[string]]::new()
$serverProcess = $null
$linked = $false
$unlinked = $false
$popupOpened = $false
$socketPath = $null
$environmentChanged = $false
$savedEnvironment = @{}

function Invoke-Starcil {
    param([Parameter(Mandatory)][string[]]$ArgumentList)

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $outputLines = @(& $StarcilBinary @ArgumentList 2>&1 | ForEach-Object { $_.ToString() })
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    [pscustomobject]@{
        ExitCode = $exitCode
        Output = ($outputLines -join [Environment]::NewLine).Trim()
    }
}

function Write-Invocation {
    param(
        [Parameter(Mandatory)][string[]]$ArgumentList,
        [Parameter(Mandatory)]$Result
    )

    Write-Output "> starcil $($ArgumentList -join ' ')"
    if (-not [string]::IsNullOrWhiteSpace($Result.Output)) {
        Write-Output $Result.Output
    }
    Write-Output "EXIT: $($Result.ExitCode)"
}

function Require-Success {
    param(
        [Parameter(Mandatory)][string]$Label,
        [Parameter(Mandatory)]$Result
    )

    if ($Result.ExitCode -ne 0) {
        throw "$Label failed with exit $($Result.ExitCode): $($Result.Output)"
    }
}

function Add-ContractFailure {
    param([Parameter(Mandatory)][string]$Message)

    $failures.Add($Message)
    Write-Output "CONTRACT_FAIL: $Message"
}

function Wait-ForPath {
    param([Parameter(Mandatory)][string]$LiteralPath)

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (Test-Path -LiteralPath $LiteralPath -PathType Leaf) {
            return $true
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Invoke-RawRequest {
    param(
        [Parameter(Mandatory)][string]$PipePath,
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)]$Params
    )

    $requestId = "d5-$($Method.Replace('.', '-'))-$([Guid]::NewGuid().ToString('N'))"
    $request = [ordered]@{ id = $requestId; method = $Method; params = $Params }
    $stream = $null
    $reader = $null
    $writer = $null
    try {
        $prefix = "\\.\pipe\"
        if (-not $PipePath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "invalid Starcil named pipe path $PipePath"
        }
        $pipeName = $PipePath.Substring($prefix.Length)
        $stream = [System.IO.Pipes.NamedPipeClientStream]::new(
            ".",
            $pipeName,
            [System.IO.Pipes.PipeDirection]::InOut,
            [System.IO.Pipes.PipeOptions]::None
        )
        $stream.Connect($TimeoutSeconds * 1000)
        $utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
        $reader = [System.IO.StreamReader]::new($stream, $utf8WithoutBom, $false, 1024, $true)
        $writer = [System.IO.StreamWriter]::new($stream, $utf8WithoutBom, 1024, $true)
        $writer.AutoFlush = $true
        $writer.WriteLine(($request | ConvertTo-Json -Depth 8 -Compress))
        while ($true) {
            $line = $reader.ReadLine()
            if ($null -eq $line) {
                throw "Starcil closed the pipe before replying to $Method"
            }
            $response = $line | ConvertFrom-Json
            if ($response.id -eq $requestId) {
                return [pscustomobject]@{ Raw = $line; Value = $response }
            }
        }
    }
    finally {
        if ($null -ne $writer) { $writer.Dispose() }
        if ($null -ne $reader) { $reader.Dispose() }
        if ($null -ne $stream) { $stream.Dispose() }
    }
}

function Assert-SafeTemporaryRoot {
    param([Parameter(Mandatory)][string]$LiteralPath)

    $tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $candidate = [System.IO.Path]::GetFullPath($LiteralPath).TrimEnd('\') + '\'
    $leaf = Split-Path -Leaf $LiteralPath
    if (-not $candidate.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase) -or
        -not $leaf.StartsWith("starcil-plugin-e2e-", [StringComparison]::Ordinal)) {
        throw "refusing to remove unsafe temporary root $LiteralPath"
    }
}

try {
    [System.IO.Directory]::CreateDirectory($temporaryRoot) | Out-Null
    [System.IO.Directory]::CreateDirectory((Join-Path $temporaryRoot "roaming")) | Out-Null
    [System.IO.Directory]::CreateDirectory((Join-Path $temporaryRoot "local")) | Out-Null

    foreach ($name in @("APPDATA", "LOCALAPPDATA", "STARCIL_CONFIG_PATH", "STARCIL_SESSION", "STARCIL_SOCKET_PATH")) {
        $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
    }
    [Environment]::SetEnvironmentVariable("APPDATA", (Join-Path $temporaryRoot "roaming"), "Process")
    [Environment]::SetEnvironmentVariable("LOCALAPPDATA", (Join-Path $temporaryRoot "local"), "Process")
    [Environment]::SetEnvironmentVariable("STARCIL_CONFIG_PATH", $null, "Process")
    [Environment]::SetEnvironmentVariable("STARCIL_SESSION", $null, "Process")
    [Environment]::SetEnvironmentVariable("STARCIL_SOCKET_PATH", $null, "Process")
    $environmentChanged = $true

    Write-Output "SESSION: $session"
    Write-Output "PLUGIN: $PluginPath"
    Write-Output "ISOLATED_ROOT: $temporaryRoot"

    $serverStart = @{
        FilePath = $StarcilBinary
        ArgumentList = @("--session", $session, "server")
        WorkingDirectory = $repositoryRoot
        RedirectStandardOutput = $serverStdout
        RedirectStandardError = $serverStderr
        WindowStyle = "Hidden"
        PassThru = $true
    }
    $serverProcess = Start-Process @serverStart

    $status = $null
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if ($serverProcess.HasExited) {
            $detail = if (Test-Path -LiteralPath $serverStderr) { Get-Content -LiteralPath $serverStderr -Raw } else { "" }
            throw "server exited before becoming reachable: $detail"
        }
        $status = Invoke-Starcil -ArgumentList @("--session", $session, "status", "server")
        if ($status.ExitCode -eq 0) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    Require-Success -Label "server readiness" -Result $status
    Write-Invocation -ArgumentList @("--session", $session, "status", "server") -Result $status

    $linkArgs = @("--session", $session, "plugin", "link", $PluginPath)
    $link = Invoke-Starcil -ArgumentList $linkArgs
    Write-Invocation -ArgumentList $linkArgs -Result $link
    Require-Success -Label "plugin link" -Result $link
    $linked = $true

    $listArgs = @("--session", $session, "plugin", "list", "--plugin", $pluginId, "--json")
    $list = Invoke-Starcil -ArgumentList $listArgs
    Write-Invocation -ArgumentList $listArgs -Result $list
    Require-Success -Label "plugin list" -Result $list
    $listJson = $list.Output | ConvertFrom-Json
    $plugin = @($listJson.plugins) | Where-Object { $_.plugin_id -eq $pluginId } | Select-Object -First 1
    if ($null -eq $plugin -or -not $plugin.enabled) {
        throw "plugin list did not return enabled plugin $pluginId"
    }
    $stateDirectory = [string]$plugin.state_dir

    $actionArgs = @("--session", $session, "plugin", "action", "invoke", $actionId)
    $action = Invoke-Starcil -ArgumentList $actionArgs
    Write-Invocation -ArgumentList $actionArgs -Result $action
    Require-Success -Label "plugin action invoke" -Result $action
    $actionJson = $action.Output | ConvertFrom-Json
    if ($actionJson.type -ne "plugin_action_invoked") {
        throw "unexpected action response: $($action.Output)"
    }

    $artifactPath = Join-Path $stateDirectory "action-environment.json"
    if (-not (Wait-ForPath -LiteralPath $artifactPath)) {
        throw "action artifact was not created at $artifactPath"
    }
    $artifact = Get-Content -LiteralPath $artifactPath -Raw | ConvertFrom-Json
    $requiredArtifactVariables = @(
        "STARCIL_ENV",
        "STARCIL_SOCKET_PATH",
        "STARCIL_BIN_PATH",
        "STARCIL_PLUGIN_ID",
        "STARCIL_PLUGIN_ROOT",
        "STARCIL_PLUGIN_CONFIG_DIR",
        "STARCIL_PLUGIN_STATE_DIR",
        "STARCIL_PLUGIN_CONTEXT_JSON",
        "STARCIL_PLUGIN_ACTION_ID"
    )
    foreach ($name in $requiredArtifactVariables) {
        $property = $artifact.environment.PSObject.Properties[$name]
        if ($null -eq $property -or [string]::IsNullOrWhiteSpace([string]$property.Value)) {
            throw "artifact omitted injected variable $name"
        }
    }
    if ($artifact.environment.STARCIL_PLUGIN_ID -ne $pluginId -or
        $artifact.environment.STARCIL_PLUGIN_ACTION_ID -ne $actionId -or
        $artifact.environment.STARCIL_ENV -ne "1") {
        throw "artifact contains incorrect plugin identity/action environment"
    }
    $context = $artifact.environment.STARCIL_PLUGIN_CONTEXT_JSON | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace([string]$context.workspace_id) -or
        [string]::IsNullOrWhiteSpace([string]$context.tab_id) -or
        [string]::IsNullOrWhiteSpace([string]$context.pane_id)) {
        throw "action context did not contain the live workspace, tab, and pane ids"
    }
    Write-Output "ARTIFACT_OK: $artifactPath"
    Write-Output "INJECTED_ENV_OK: $($requiredArtifactVariables -join ', ')"
    $socketPath = [string]$artifact.environment.STARCIL_SOCKET_PATH

    $popupArgs = @("--session", $session, "plugin", "pane", "open", "--plugin", $pluginId, "--entrypoint", "hello-popup")
    $popup = Invoke-Starcil -ArgumentList $popupArgs
    Write-Invocation -ArgumentList $popupArgs -Result $popup
    Require-Success -Label "plugin popup open" -Result $popup
    $popupOpened = $true

    $popupClose = Invoke-RawRequest -PipePath $socketPath -Method "popup.close" -Params ([ordered]@{})
    Write-Output "> raw popup.close"
    Write-Output $popupClose.Raw
    $popupError = $popupClose.Value.PSObject.Properties["error"]
    $popupResult = $popupClose.Value.PSObject.Properties["result"]
    if ($null -ne $popupError -or $null -eq $popupResult -or $popupResult.Value.type -ne "ok") {
        throw "popup.close failed: $($popupClose.Raw)"
    }
    $popupOpened = $false

    $splitArgs = @("--session", $session, "pane", "split", "--direction", "right", "--no-focus")
    $split = Invoke-Starcil -ArgumentList $splitArgs
    Write-Invocation -ArgumentList $splitArgs -Result $split
    Require-Success -Label "pane split" -Result $split
    $splitJson = $split.Output | ConvertFrom-Json
    $createdPaneId = [string]$splitJson.result.pane.pane_id
    if ([string]::IsNullOrWhiteSpace($createdPaneId)) {
        throw "pane split response omitted result.pane.pane_id"
    }

    $eventLogPath = Join-Path $stateDirectory "pane-created.log"
    if (Wait-ForPath -LiteralPath $eventLogPath) {
        $eventLine = @(Get-Content -LiteralPath $eventLogPath | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })[-1]
        $eventEntry = $eventLine | ConvertFrom-Json
        $eventPayload = $eventEntry.event_json | ConvertFrom-Json
        if ($eventEntry.environment.STARCIL_PLUGIN_EVENT -ne "pane.created" -or
            $eventPayload.pane_id -ne $createdPaneId) {
            Add-ContractFailure "pane.created hook wrote an unexpected event payload"
        }
        else {
            Write-Output "EVENT_HOOK_OK: $eventLogPath pane_id=$createdPaneId"
        }
    }
    else {
        Add-ContractFailure "pane.created was emitted by pane split, but the plugin hook did not create $eventLogPath"
    }

    $logArgs = @("--session", $session, "plugin", "log", "list", "--plugin", $pluginId, "--limit", "20")
    $logs = $null
    $parsedLogs = @()
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $logs = Invoke-Starcil -ArgumentList $logArgs
        if ($logs.ExitCode -eq 0) {
            $parsedLogs = @($logs.Output -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
            $actionLog = $parsedLogs | Where-Object { $_.kind -eq "action" -and $_.state -eq "exited" } | Select-Object -First 1
            if ($null -ne $actionLog) { break }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    Write-Invocation -ArgumentList $logArgs -Result $logs
    Require-Success -Label "plugin log list" -Result $logs
    $actionLog = $parsedLogs | Where-Object { $_.kind -eq "action" -and $_.state -eq "exited" -and $_.exit_code -eq 0 } | Select-Object -First 1
    if ($null -eq $actionLog) {
        Add-ContractFailure "plugin log list did not show the successful action run"
    }
    $eventLog = $parsedLogs | Where-Object { $_.kind -eq "event" -and $_.state -in @("running", "exited") } | Select-Object -First 1
    if ($null -eq $eventLog) {
        Add-ContractFailure "plugin log list did not show the pane.created event hook run"
    }

    $disableArgs = @("--session", $session, "plugin", "disable", $pluginId)
    $disable = Invoke-Starcil -ArgumentList $disableArgs
    Write-Invocation -ArgumentList $disableArgs -Result $disable
    Require-Success -Label "plugin disable" -Result $disable

    $disabledInvoke = Invoke-Starcil -ArgumentList $actionArgs
    Write-Invocation -ArgumentList $actionArgs -Result $disabledInvoke
    if ($disabledInvoke.ExitCode -eq 0 -or $disabledInvoke.Output -notmatch "plugin_disabled") {
        Add-ContractFailure "disabled action invoke did not return plugin_disabled"
    }
    else {
        Write-Output "PLUGIN_DISABLED_OK"
    }

    $unlinkArgs = @("--session", $session, "plugin", "unlink", $pluginId)
    $unlink = Invoke-Starcil -ArgumentList $unlinkArgs
    Write-Invocation -ArgumentList $unlinkArgs -Result $unlink
    Require-Success -Label "plugin unlink" -Result $unlink
    $unlinked = $true

    $afterUnlink = Invoke-Starcil -ArgumentList $listArgs
    Write-Invocation -ArgumentList $listArgs -Result $afterUnlink
    Require-Success -Label "plugin list after unlink" -Result $afterUnlink
    $afterUnlinkJson = $afterUnlink.Output | ConvertFrom-Json
    if (@($afterUnlinkJson.plugins | Where-Object { $_.plugin_id -eq $pluginId }).Count -ne 0) {
        Add-ContractFailure "plugin remained registered after unlink"
    }
}
catch {
    Add-ContractFailure "fatal E2E step: $($_.Exception.Message)"
}
finally {
    if ($popupOpened -and $null -ne $serverProcess -and -not $serverProcess.HasExited) {
        try {
            if (-not [string]::IsNullOrWhiteSpace($socketPath)) {
                $null = Invoke-RawRequest -PipePath $socketPath -Method "popup.close" -Params ([ordered]@{})
            }
        }
        catch {
            Add-ContractFailure "cleanup could not close popup: $($_.Exception.Message)"
        }
    }

    if ($linked -and -not $unlinked -and $null -ne $serverProcess -and -not $serverProcess.HasExited) {
        try {
            $cleanupUnlink = Invoke-Starcil -ArgumentList @("--session", $session, "plugin", "unlink", $pluginId)
            if ($cleanupUnlink.ExitCode -ne 0) {
                Add-ContractFailure "cleanup could not unlink plugin: $($cleanupUnlink.Output)"
            }
        }
        catch {
            Add-ContractFailure "cleanup unlink raised an error: $($_.Exception.Message)"
        }
    }

    if ($null -ne $serverProcess -and -not $serverProcess.HasExited) {
        try {
            $stopArgs = @("--session", $session, "server", "stop")
            $stop = Invoke-Starcil -ArgumentList $stopArgs
            Write-Invocation -ArgumentList $stopArgs -Result $stop
        }
        catch {
            Add-ContractFailure "server stop raised an error: $($_.Exception.Message)"
        }

        try {
            $stopDeadline = [DateTime]::UtcNow.AddSeconds(5)
            do {
                $serverProcess.Refresh()
                if ($serverProcess.HasExited) { break }
                Start-Sleep -Milliseconds 100
            } while ([DateTime]::UtcNow -lt $stopDeadline)
            if (-not $serverProcess.HasExited) {
                Stop-Process -Id $serverProcess.Id -Force
                Wait-Process -Id $serverProcess.Id -Timeout 5 -ErrorAction SilentlyContinue
                Write-Output "CLEANUP_FORCED_EXACT_PID: $($serverProcess.Id)"
            }
        }
        catch {
            Add-ContractFailure "server process cleanup failed for PID $($serverProcess.Id): $($_.Exception.Message)"
        }
    }

    if ($environmentChanged) {
        try {
            $deleteArgs = @("session", "delete", $session, "--json")
            $delete = Invoke-Starcil -ArgumentList $deleteArgs
            Write-Invocation -ArgumentList $deleteArgs -Result $delete
            if ($delete.ExitCode -ne 0) {
                Add-ContractFailure "session cleanup failed: $($delete.Output)"
            }
        }
        catch {
            Add-ContractFailure "session cleanup raised an error: $($_.Exception.Message)"
        }
        finally {
            foreach ($name in $savedEnvironment.Keys) {
                [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], "Process")
            }
        }
    }

    if (Test-Path -LiteralPath $serverStderr -PathType Leaf) {
        try {
            $serverErrorText = [string](Get-Content -LiteralPath $serverStderr -Raw)
            if (-not [string]::IsNullOrWhiteSpace($serverErrorText)) {
                Write-Output "SERVER_STDERR:"
                Write-Output $serverErrorText.Trim()
            }
        }
        catch {
            Add-ContractFailure "could not read server stderr: $($_.Exception.Message)"
        }
    }

    if (Test-Path -LiteralPath $temporaryRoot) {
        try {
            Assert-SafeTemporaryRoot -LiteralPath $temporaryRoot
            Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
            Write-Output "CLEANUP_OK: process, session, and isolated files removed"
        }
        catch {
            Add-ContractFailure "isolated file cleanup failed: $($_.Exception.Message)"
        }
    }
}

if ($failures.Count -gt 0) {
    Write-Output "E2E_RESULT: FAIL ($($failures.Count) contract failure(s))"
    foreach ($failure in $failures) {
        Write-Output "- $failure"
    }
    [Console]::Out.Flush()
    throw "plugin E2E contract failed"
}

Write-Output "E2E_RESULT: PASS"
