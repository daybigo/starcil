[CmdletBinding()]
param(
    [int]$CommandTimeoutMs = 15000
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$script:RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..\..")).Path
$script:Starcil = Join-Path $script:RepoRoot "target\debug\starcil.exe"
$script:TranscriptPath = Join-Path $PSScriptRoot "campaign-output.txt"
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:Failures = 0
$script:Skips = 0
$script:Passes = 0
$script:CheckDetail = "ok"
$script:Sessions = New-Object 'System.Collections.Generic.List[string]'
$script:ServerProcesses = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
$script:KnownChildPids = New-Object 'System.Collections.Generic.HashSet[int]'

[System.IO.File]::WriteAllText($script:TranscriptPath, "", $script:Utf8NoBom)

function Write-CampaignLine {
    param([Parameter(Mandatory = $true)][string]$Line)

    $singleLine = ($Line -replace "[\r\n]+", " " -replace "\s+", " ").Trim()
    Write-Host $singleLine
    [System.IO.File]::AppendAllText(
        $script:TranscriptPath,
        $singleLine + [Environment]::NewLine,
        $script:Utf8NoBom
    )
}

function Write-Check {
    param(
        [Parameter(Mandatory = $true)][string]$Area,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][ValidateSet("PASS", "FAIL", "SKIP")][string]$Status,
        [Parameter(Mandatory = $true)][string]$Detail
    )

    switch ($Status) {
        "PASS" { $script:Passes++ }
        "FAIL" { $script:Failures++ }
        "SKIP" { $script:Skips++ }
    }
    Write-CampaignLine "CHECK $Area/$Name $Status $Detail"
}

function Set-CheckDetail {
    param([Parameter(Mandatory = $true)][string]$Detail)
    $script:CheckDetail = $Detail
}

function Get-ExceptionDetail {
    param([Parameter(Mandatory = $true)]$ErrorRecord)

    $message = if ($ErrorRecord.Exception) { $ErrorRecord.Exception.Message } else { [string]$ErrorRecord }
    return ($message -replace "[\r\n]+", " " -replace "\s+", " ").Trim()
}

function Invoke-CampaignCheck {
    param(
        [Parameter(Mandatory = $true)][string]$Area,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Action
    )

    $script:CheckDetail = "ok"
    try {
        $value = & $Action
        Write-Check $Area $Name "PASS" $script:CheckDetail
        return $value
    }
    catch {
        Write-Check $Area $Name "FAIL" (Get-ExceptionDetail $_)
        return $null
    }
}

function Assert-Campaign {
    param(
        [Parameter(Mandatory = $true)][bool]$Condition,
        [Parameter(Mandatory = $true)][string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Quote-NativeArgument {
    param([AllowEmptyString()][string]$Argument)

    if ($null -eq $Argument -or $Argument.Length -eq 0) {
        return '""'
    }
    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }

    $builder = New-Object System.Text.StringBuilder
    [void]$builder.Append('"')
    $slashes = 0
    foreach ($character in $Argument.ToCharArray()) {
        if ([int]$character -eq 92) {
            $slashes++
            continue
        }
        if ([int]$character -eq 34) {
            for ($index = 0; $index -lt (($slashes * 2) + 1); $index++) {
                [void]$builder.Append([char]92)
            }
            [void]$builder.Append('"')
            $slashes = 0
            continue
        }
        for ($index = 0; $index -lt $slashes; $index++) {
            [void]$builder.Append([char]92)
        }
        $slashes = 0
        [void]$builder.Append($character)
    }
    for ($index = 0; $index -lt ($slashes * 2); $index++) {
        [void]$builder.Append([char]92)
    }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Join-NativeArguments {
    param([string[]]$Arguments)
    return (($Arguments | ForEach-Object { Quote-NativeArgument ([string]$_) }) -join " ")
}

function Merge-ChildEnvironment {
    param([hashtable]$Override)

    $merged = @{}
    foreach ($entry in $script:BaseEnvironment.GetEnumerator()) {
        $merged[$entry.Key] = [string]$entry.Value
    }
    if ($null -ne $Override) {
        foreach ($entry in $Override.GetEnumerator()) {
            $merged[$entry.Key] = [string]$entry.Value
        }
    }
    return $merged
}

function Start-NativeProcess {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$Arguments = @(),
        [hashtable]$Environment = @{},
        [string]$WorkingDirectory = $script:RepoRoot,
        [switch]$Redirect
    )

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = Join-NativeArguments $Arguments
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = [bool]$Redirect
    $startInfo.RedirectStandardError = [bool]$Redirect
    if ($Redirect) {
        $startInfo.StandardOutputEncoding = $script:Utf8NoBom
        $startInfo.StandardErrorEncoding = $script:Utf8NoBom
    }
    $childEnvironment = Merge-ChildEnvironment $Environment
    foreach ($entry in $childEnvironment.GetEnumerator()) {
        $startInfo.EnvironmentVariables[$entry.Key] = [string]$entry.Value
    }

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw "failed to start $FilePath"
    }

    $stdoutTask = $null
    $stderrTask = $null
    if ($Redirect) {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
    }

    return [pscustomobject]@{
        Process = $process
        StdoutTask = $stdoutTask
        StderrTask = $stderrTask
        Command = "$FilePath $($startInfo.Arguments)"
        Redirect = [bool]$Redirect
    }
}

function Complete-NativeProcess {
    param(
        [Parameter(Mandatory = $true)]$Launch,
        [int]$TimeoutMs = $CommandTimeoutMs
    )

    $process = $Launch.Process
    $timedOut = -not $process.WaitForExit($TimeoutMs)
    if ($timedOut) {
        try { Stop-Process -Id $process.Id -Force -ErrorAction Stop } catch {}
        try { $process.WaitForExit() } catch {}
    }
    else {
        $process.WaitForExit()
    }

    $stdout = ""
    $stderr = ""
    if ($Launch.Redirect) {
        $stdout = $Launch.StdoutTask.Result
        $stderr = $Launch.StderrTask.Result
    }
    $exitCode = if ($timedOut) { -1 } else { $process.ExitCode }

    return [pscustomobject]@{
        ExitCode = $exitCode
        StdOut = $stdout
        StdErr = $stderr
        TimedOut = $timedOut
        Command = $Launch.Command
        ProcessId = $process.Id
    }
}

function Invoke-Starcil {
    param(
        [string[]]$Arguments = @(),
        [hashtable]$Environment = @{},
        [int]$TimeoutMs = $CommandTimeoutMs,
        [string]$WorkingDirectory = $script:RepoRoot
    )

    $launch = Start-NativeProcess -FilePath $script:Starcil -Arguments $Arguments `
        -Environment $Environment -WorkingDirectory $WorkingDirectory -Redirect
    return Complete-NativeProcess -Launch $launch -TimeoutMs $TimeoutMs
}

function Get-LastNonEmptyLine {
    param([AllowEmptyString()][string]$Text)

    $lines = @($Text -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($lines.Count -eq 0) { return "" }
    return $lines[$lines.Count - 1].Trim()
}

function Get-SuccessResult {
    param([Parameter(Mandatory = $true)]$Invocation)

    if ($Invocation.TimedOut) {
        throw "command timed out: $($Invocation.Command)"
    }
    if ($Invocation.ExitCode -ne 0) {
        $detail = (Get-LastNonEmptyLine $Invocation.StdErr)
        if ([string]::IsNullOrWhiteSpace($detail)) { $detail = (Get-LastNonEmptyLine $Invocation.StdOut) }
        throw "exit $($Invocation.ExitCode): $detail"
    }
    $text = $Invocation.StdOut.Trim()
    if ([string]::IsNullOrWhiteSpace($text)) {
        throw "successful command returned empty stdout"
    }
    try {
        $envelope = $text | ConvertFrom-Json
    }
    catch {
        throw "stdout was not JSON: $(Get-LastNonEmptyLine $text)"
    }
    $errorProperty = $envelope.PSObject.Properties["error"]
    if ($null -ne $errorProperty -and $null -ne $errorProperty.Value) {
        throw "server error $($errorProperty.Value.code): $($errorProperty.Value.message)"
    }
    $resultProperty = $envelope.PSObject.Properties["result"]
    if ($null -eq $resultProperty -or $null -eq $resultProperty.Value) {
        throw "JSON response omitted result"
    }
    return $resultProperty.Value
}

function Get-ErrorEnvelope {
    param([Parameter(Mandatory = $true)]$Invocation)

    Assert-Campaign (-not $Invocation.TimedOut) "error command timed out"
    Assert-Campaign ($Invocation.ExitCode -ne 0) "command unexpectedly exited 0"
    $line = Get-LastNonEmptyLine $Invocation.StdErr
    Assert-Campaign (-not [string]::IsNullOrWhiteSpace($line)) "error command returned empty stderr"
    try { return ($line | ConvertFrom-Json) } catch { throw "stderr was not JSON: $line" }
}

function Register-Session {
    param([Parameter(Mandatory = $true)][string]$Session)
    if (-not $script:Sessions.Contains($Session)) { $script:Sessions.Add($Session) }
}

function Start-StarcilServer {
    param(
        [Parameter(Mandatory = $true)][string]$Session,
        [string]$WorkingDirectory = $script:RepoRoot
    )

    Register-Session $Session
    $launch = Start-NativeProcess -FilePath $script:Starcil `
        -Arguments @("--session", $Session, "server") `
        -WorkingDirectory $WorkingDirectory
    $script:ServerProcesses.Add($launch.Process)
    return $launch.Process
}

function Wait-ForServer {
    param(
        [Parameter(Mandatory = $true)][string]$Session,
        [int]$TimeoutMs = 8000
    )

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    do {
        $probe = Invoke-Starcil -Arguments @("--session", $Session, "status") -TimeoutMs 2000
        if (-not $probe.TimedOut -and $probe.ExitCode -eq 0) {
            try {
                $pong = Get-SuccessResult $probe
                if ($pong.type -eq "pong") { return $pong }
            }
            catch {}
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    return $null
}

function Get-StarcilPipeName {
    param([Parameter(Mandatory = $true)][string]$Session)

    try {
        $candidate = Get-ChildItem -Path "\\.\pipe\" -ErrorAction Stop |
            Where-Object { $_.Name -like "starcil-*-$Session" } |
            Select-Object -First 1
        if ($null -ne $candidate) { return $candidate.Name }
    }
    catch {}

    $domain = [Environment]::GetEnvironmentVariable("USERDOMAIN")
    $user = [Environment]::GetEnvironmentVariable("USERNAME")
    if ([string]::IsNullOrEmpty($user)) { $user = [Environment]::GetEnvironmentVariable("USER") }
    if ([string]::IsNullOrEmpty($user)) { $user = "unknown-user" }
    $identity = "$domain\$user"
    $hash = [System.Numerics.BigInteger]::Parse("14695981039346656037")
    $prime = [System.Numerics.BigInteger]::Parse("1099511628211")
    $mask = [System.Numerics.BigInteger]::Parse("18446744073709551615")
    foreach ($byte in [System.Text.Encoding]::UTF8.GetBytes($identity)) {
        $hash = (($hash -bxor [System.Numerics.BigInteger]$byte) * $prime) -band $mask
    }
    $hex = $hash.ToString("x")
    if ($hex.Length -gt 16) { $hex = $hex.Substring($hex.Length - 16) }
    $hex = $hex.PadLeft(16, '0')
    return "starcil-$hex-$Session"
}

function Invoke-RawRequest {
    param(
        [Parameter(Mandatory = $true)][string]$Session,
        [Parameter(Mandatory = $true)][string]$Method,
        [Parameter(Mandatory = $true)]$Params,
        [int]$TimeoutMs = 10000
    )

    $pipeName = Get-StarcilPipeName $Session
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(
        ".",
        $pipeName,
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::None
    )
    $reader = $null
    $writer = $null
    try {
        $pipe.Connect($TimeoutMs)
        $reader = New-Object System.IO.StreamReader($pipe, $script:Utf8NoBom, $false, 4096, $true)
        $writer = New-Object System.IO.StreamWriter($pipe, $script:Utf8NoBom, 4096, $true)
        $writer.AutoFlush = $true
        $id = "campaign:$($Method.Replace('.', ':')):$([Guid]::NewGuid().ToString('N'))"
        $request = [ordered]@{ id = $id; method = $Method; params = $Params }
        $writer.WriteLine(($request | ConvertTo-Json -Compress -Depth 100))

        $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
        while ([DateTime]::UtcNow -lt $deadline) {
            $remaining = [Math]::Max(1, [int]($deadline - [DateTime]::UtcNow).TotalMilliseconds)
            $readTask = $reader.ReadLineAsync()
            if (-not $readTask.Wait($remaining)) { throw "timed out reading NDJSON response for $Method" }
            $line = $readTask.Result
            if ($null -eq $line) { throw "server closed the NDJSON connection for $Method" }
            $incoming = $line | ConvertFrom-Json
            $incomingId = $incoming.PSObject.Properties["id"]
            if ($null -eq $incomingId -or $incomingId.Value -ne $id) { continue }
            $incomingError = $incoming.PSObject.Properties["error"]
            if ($null -ne $incomingError -and $null -ne $incomingError.Value) {
                throw "server error $($incomingError.Value.code): $($incomingError.Value.message)"
            }
            $incomingResult = $incoming.PSObject.Properties["result"]
            Assert-Campaign ($null -ne $incomingResult -and $null -ne $incomingResult.Value) "raw response omitted result"
            return $incomingResult.Value
        }
        throw "timed out waiting for matching NDJSON response for $Method"
    }
    finally {
        if ($null -ne $writer) { $writer.Dispose() }
        if ($null -ne $reader) { $reader.Dispose() }
        $pipe.Dispose()
    }
}

function Wait-ForFileContent {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string[]]$Needles,
        [int]$TimeoutMs = 8000
    )

    $deadline = [DateTime]::UtcNow.AddMilliseconds($TimeoutMs)
    do {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            try {
                $content = [System.IO.File]::ReadAllText($Path)
                $allPresent = $true
                foreach ($needle in $Needles) {
                    if (-not $content.Contains($needle)) { $allPresent = $false; break }
                }
                if ($allPresent) { return $content }
            }
            catch {}
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    return $null
}

function Get-ProcessTreeIds {
    param([Parameter(Mandatory = $true)][int]$RootId)

    $all = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    $queue = New-Object 'System.Collections.Generic.Queue[int]'
    $seen = New-Object 'System.Collections.Generic.HashSet[int]'
    $queue.Enqueue($RootId)
    while ($queue.Count -gt 0) {
        $parent = $queue.Dequeue()
        foreach ($child in $all | Where-Object { [int]$_.ParentProcessId -eq $parent }) {
            $childId = [int]$child.ProcessId
            if ($seen.Add($childId)) { $queue.Enqueue($childId) }
        }
    }
    return @($seen)
}

function Stop-TrackedProcessTree {
    param([Parameter(Mandatory = $true)][int]$RootId)

    $descendants = @(Get-ProcessTreeIds $RootId)
    [array]::Reverse($descendants)
    foreach ($processId in $descendants) {
        [void]$script:KnownChildPids.Add([int]$processId)
        try { Stop-Process -Id $processId -Force -ErrorAction Stop } catch {}
    }
    try { Stop-Process -Id $RootId -Force -ErrorAction Stop } catch {}
}

function Get-CampaignServerProcesses {
    $resolvedBinary = [System.IO.Path]::GetFullPath($script:Starcil)
    $all = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    return @($all | Where-Object {
        $exeMatches = $_.ExecutablePath -and (
            [System.IO.Path]::GetFullPath([string]$_.ExecutablePath) -eq $resolvedBinary
        )
        if (-not $exeMatches) { return $false }
        $commandLine = [string]$_.CommandLine
        foreach ($session in $script:Sessions) {
            if ($commandLine.Contains($session) -and $commandLine.Contains("server")) { return $true }
        }
        return $false
    })
}

function Invoke-SessionJson {
    param(
        [Parameter(Mandatory = $true)][string]$Session,
        [Parameter(Mandatory = $true)][string[]]$CommandArguments,
        [int]$TimeoutMs = $CommandTimeoutMs,
        [hashtable]$Environment = @{},
        [string]$WorkingDirectory = $script:RepoRoot
    )

    $allArguments = @("--session", $Session) + $CommandArguments
    $invocation = Invoke-Starcil -Arguments $allArguments -Environment $Environment `
        -TimeoutMs $TimeoutMs -WorkingDirectory $WorkingDirectory
    return Get-SuccessResult $invocation
}

function Invoke-SessionText {
    param(
        [Parameter(Mandatory = $true)][string]$Session,
        [Parameter(Mandatory = $true)][string[]]$CommandArguments,
        [int]$TimeoutMs = $CommandTimeoutMs,
        [hashtable]$Environment = @{},
        [string]$WorkingDirectory = $script:RepoRoot
    )

    $allArguments = @("--session", $Session) + $CommandArguments
    $invocation = Invoke-Starcil -Arguments $allArguments -Environment $Environment `
        -TimeoutMs $TimeoutMs -WorkingDirectory $WorkingDirectory
    if ($invocation.TimedOut) { throw "command timed out: $($invocation.Command)" }
    if ($invocation.ExitCode -ne 0) {
        throw "exit $($invocation.ExitCode): $(Get-LastNonEmptyLine $invocation.StdErr)"
    }
    return $invocation.StdOut
}

$suffix = ([Guid]::NewGuid().ToString("N")).Substring(0, 8)
$script:TempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "starcil-b4-$suffix"
$tempHome = Join-Path $script:TempRoot "home"
$tempAppData = Join-Path $script:TempRoot "appdata\roaming"
$tempLocalAppData = Join-Path $script:TempRoot "appdata\local"
$mainConfigPath = Join-Path $script:TempRoot "config-main.toml"
$restoreCwd = Join-Path $script:TempRoot "restore-cwd"
$restoreCwdTwo = Join-Path $script:TempRoot "restore-cwd-two"
$worktreeRoot = Join-Path $script:TempRoot "worktree-repo"
$worktreeCheckout = Join-Path $script:TempRoot "worktree-checkout"
$pluginRoot = Join-Path $script:TempRoot "plugin-fixture"

foreach ($directory in @(
    $script:TempRoot,
    $tempHome,
    $tempAppData,
    $tempLocalAppData,
    $restoreCwd,
    $restoreCwdTwo,
    $pluginRoot
)) {
    [void](New-Item -ItemType Directory -Path $directory -Force)
}

$script:BaseEnvironment = @{
    "APPDATA" = $tempAppData
    "LOCALAPPDATA" = $tempLocalAppData
    "STARCIL_CONFIG_PATH" = $mainConfigPath
    "STARCIL_SESSION" = ""
    "STARCIL_SOCKET_PATH" = ""
}

$worktreesConfigPath = (Join-Path $script:TempRoot "managed-worktrees").Replace('\', '/')
$mainConfig = @"
onboarding = false

[terminal]
default_shell = "cmd.exe"
shell_mode = "non_login"

[ui.toast]
delivery = "off"

[worktrees]
directory = "$worktreesConfigPath"

[update]
channel = "stable"
version_check = false
manifest_check = false
"@
[System.IO.File]::WriteAllText($mainConfigPath, $mainConfig, $script:Utf8NoBom)

$mainSession = "b4main-$suffix"
$restoreSession = "b4restore-$suffix"
$pluginId = "b4.plugin.$suffix"
$agentName = "b4agent"
$agentRenamed = "b4renamed"

Write-CampaignLine "Starcil B4 live E2E campaign run=$suffix"

function Start-MainSessionViaAutostart {
    Register-Session $mainSession
    $client = Start-NativeProcess -FilePath $script:Starcil `
        -Arguments @("--session", $mainSession) `
        -WorkingDirectory $script:RepoRoot
    $pong = Wait-ForServer -Session $mainSession -TimeoutMs 8000
    if (-not $client.Process.HasExited) {
        try { Stop-Process -Id $client.Process.Id -Force -ErrorAction Stop } catch {}
    }
    $clientResult = Complete-NativeProcess -Launch $client -TimeoutMs 3000
    if ($null -eq $pong) {
        throw "bare client did not autostart a reachable server; client exit=$($clientResult.ExitCode)"
    }
    Set-CheckDetail "bare named-session client autostarted protocol $($pong.protocol_major).$($pong.protocol_minor)"
    return $pong
}

function Ensure-MainSessionFallback {
    $existing = Wait-ForServer -Session $mainSession -TimeoutMs 1000
    if ($null -ne $existing) { return $existing }
    [void](Start-StarcilServer -Session $mainSession -WorkingDirectory $script:RepoRoot)
    return Wait-ForServer -Session $mainSession -TimeoutMs 8000
}

function Run-QuickStartChecks {
    $autostart = Invoke-CampaignCheck "quick-start" "server-autostart" {
        Start-MainSessionViaAutostart
    }
    if ($null -eq $autostart) {
        $autostart = Ensure-MainSessionFallback
    }

    $status = Invoke-CampaignCheck "quick-start" "status" {
        $result = Invoke-SessionJson -Session $mainSession -CommandArguments @("status")
        Assert-Campaign ($result.type -eq "pong") "status response type was '$($result.type)'"
        Assert-Campaign ($result.session -eq $mainSession) "status returned session '$($result.session)'"
        Set-CheckDetail "pong session=$($result.session) version=$($result.version)"
        return $result
    }

    if ($null -eq $status) { return $null }

    $context = Invoke-CampaignCheck "quick-start" "pane-run-read" {
        $paneList = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "list")
        Assert-Campaign ($paneList.type -eq "pane_list") "pane list response type was '$($paneList.type)'"
        Assert-Campaign (@($paneList.panes).Count -ge 1) "initial session had no pane"
        $pane = @($paneList.panes)[0]
        $token = "STARCIL_B4_CMD_$suffix"
        $run = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "run", $pane.pane_id, "echo $token")
        Assert-Campaign ($run.type -eq "ok") "pane run response type was '$($run.type)'"
        $matched = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "pane", "wait-output", $pane.pane_id, "--match", $token,
            "--source", "recent", "--timeout", "10000"
        ) -TimeoutMs 12000
        Assert-Campaign ($matched.type -eq "pane_output_matched") "wait-output did not match cmd output"
        $read = Invoke-SessionText -Session $mainSession -CommandArguments @(
            "pane", "read", $pane.pane_id, "--source", "recent", "--lines", "40"
        )
        Assert-Campaign ($read.Contains($token)) "pane read omitted '$token'"
        Set-CheckDetail "real cmd pane $($pane.pane_id) echoed and read '$token'"
        return [pscustomobject]@{
            WorkspaceId = [string]$pane.workspace_id
            TabId = [string]$pane.tab_id
            PaneId = [string]$pane.pane_id
        }
    }
    return $context
}

function Run-CrudAndLayoutChecks {
    param([Parameter(Mandatory = $true)]$Initial)

    $workspaceContext = Invoke-CampaignCheck "concepts" "workspace-create-list-get" {
        $created = @()
        foreach ($label in @("b4-alpha", "b4-beta", "b4-gamma")) {
            $result = Invoke-SessionJson -Session $mainSession -CommandArguments @(
                "workspace", "create", "--cwd", $script:TempRoot,
                "--label", $label, "--no-focus"
            )
            Assert-Campaign ($result.type -eq "workspace_created") "workspace create returned '$($result.type)'"
            $created += $result
        }
        $listed = Invoke-SessionJson -Session $mainSession -CommandArguments @("workspace", "list")
        $ids = @($listed.workspaces | ForEach-Object { [string]$_.workspace_id })
        foreach ($item in $created) {
            Assert-Campaign ($ids -contains [string]$item.workspace.workspace_id) "workspace list omitted $($item.workspace.workspace_id)"
        }
        $get = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "workspace", "get", [string]$created[0].workspace.workspace_id
        )
        Assert-Campaign ($get.workspace.label -eq "b4-alpha") "workspace get returned label '$($get.workspace.label)'"
        Set-CheckDetail "created/listed/got $($created.Count) workspaces"
        return [pscustomobject]@{
            Alpha = $created[0]
            Beta = $created[1]
            Gamma = $created[2]
        }
    }

    if ($null -eq $workspaceContext) {
        Write-Check "concepts" "workspace-rename-focus" "SKIP" "workspace creation prerequisite failed"
        Write-Check "concepts" "workspace-reorders" "SKIP" "workspace creation prerequisite failed"
        Write-Check "concepts" "workspace-close" "SKIP" "workspace creation prerequisite failed"
        return $null
    }

    $alphaId = [string]$workspaceContext.Alpha.workspace.workspace_id
    $betaId = [string]$workspaceContext.Beta.workspace.workspace_id
    $gammaId = [string]$workspaceContext.Gamma.workspace.workspace_id

    [void](Invoke-CampaignCheck "concepts" "workspace-rename-focus" {
        $renamed = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "workspace", "rename", $alphaId, "b4-alpha-renamed"
        )
        Assert-Campaign ($renamed.workspace.label -eq "b4-alpha-renamed") "workspace rename was not visible"
        $focused = Invoke-SessionJson -Session $mainSession -CommandArguments @("workspace", "focus", $alphaId)
        Assert-Campaign ([bool]$focused.workspace.focused) "workspace focus response was not focused"
        Set-CheckDetail "renamed and focused $alphaId"
    })

    [void](Invoke-CampaignCheck "concepts" "workspace-reorders" {
        $move = Invoke-RawRequest -Session $mainSession -Method "workspace.move" -Params @{
            workspace_id = $gammaId
            insert_index = 0
        }
        Assert-Campaign ($move.type -eq "workspace_moved") "workspace.move returned '$($move.type)'"
        $firstMovedWorkspace = [string](@($move.workspaces)[0])
        Assert-Campaign ($firstMovedWorkspace -eq $gammaId) "workspace.move did not place gamma first"
        $block = Invoke-RawRequest -Session $mainSession -Method "workspace.move_block" -Params @{
            workspace_ids = @($alphaId, $betaId)
            before_workspace_id = [string]$Initial.WorkspaceId
        }
        Assert-Campaign ($block.type -eq "workspace_reordered") "workspace.move_block returned '$($block.type)'"
        $orderedIds = @($block.workspaces | ForEach-Object { [string]$_ })
        $alphaIndex = [array]::IndexOf($orderedIds, $alphaId)
        $betaIndex = [array]::IndexOf($orderedIds, $betaId)
        $anchorIndex = [array]::IndexOf($orderedIds, [string]$Initial.WorkspaceId)
        Assert-Campaign ($alphaIndex -ge 0 -and $betaIndex -eq ($alphaIndex + 1) -and $anchorIndex -gt $betaIndex) `
            "workspace.move_block order was not authoritative"
        Set-CheckDetail "workspace.move and workspace.move_block preserved requested order"
    })

    $tabContext = Invoke-CampaignCheck "concepts" "tab-create-list-get" {
        $rootTab = [string]$workspaceContext.Alpha.tab.tab_id
        $rootPane = [string]$workspaceContext.Alpha.root_pane.pane_id
        $created = @()
        foreach ($label in @("b4-tab-two", "b4-tab-three", "b4-tab-close")) {
            $result = Invoke-SessionJson -Session $mainSession -CommandArguments @(
                "tab", "create", "--workspace", $alphaId, "--cwd", $script:TempRoot,
                "--label", $label, "--no-focus"
            )
            Assert-Campaign ($result.type -eq "tab_created") "tab create returned '$($result.type)'"
            $created += $result
        }
        $listed = Invoke-SessionJson -Session $mainSession -CommandArguments @("tab", "list", "--workspace", $alphaId)
        $ids = @($listed.tabs | ForEach-Object { [string]$_.tab_id })
        foreach ($item in $created) {
            Assert-Campaign ($ids -contains [string]$item.tab.tab_id) "tab list omitted $($item.tab.tab_id)"
        }
        $get = Invoke-SessionJson -Session $mainSession -CommandArguments @("tab", "get", [string]$created[0].tab.tab_id)
        Assert-Campaign ($get.tab.label -eq "b4-tab-two") "tab get returned label '$($get.tab.label)'"
        Set-CheckDetail "created/listed/got tabs in $alphaId"
        return [pscustomobject]@{
            RootTab = $rootTab
            RootPane = $rootPane
            Two = $created[0]
            Three = $created[1]
            Close = $created[2]
        }
    }

    if ($null -eq $tabContext) {
        Write-Check "concepts" "tab-rename-focus-move-close" "SKIP" "tab creation prerequisite failed"
        Write-Check "concepts" "pane-crud" "SKIP" "tab creation prerequisite failed"
        Write-Check "concepts" "split-zoom-swap-move" "SKIP" "tab creation prerequisite failed"
        Write-Check "concepts" "neighbor-edges-layout" "SKIP" "tab creation prerequisite failed"
        Write-Check "concepts" "layout-export-apply-ratio" "SKIP" "tab creation prerequisite failed"
        return [pscustomobject]@{ AlphaId = $alphaId; GammaId = $gammaId }
    }

    $tabTwoId = [string]$tabContext.Two.tab.tab_id
    $tabTwoRoot = [string]$tabContext.Two.root_pane.pane_id
    $tabThreeId = [string]$tabContext.Three.tab.tab_id
    $tabThreeRoot = [string]$tabContext.Three.root_pane.pane_id
    $tabCloseId = [string]$tabContext.Close.tab.tab_id

    [void](Invoke-CampaignCheck "concepts" "tab-rename-focus-move-close" {
        $renamed = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "tab", "rename", $tabTwoId, "b4-tab-two-renamed"
        )
        Assert-Campaign ($renamed.tab.label -eq "b4-tab-two-renamed") "tab rename was not visible"
        $focused = Invoke-SessionJson -Session $mainSession -CommandArguments @("tab", "focus", $tabTwoId)
        Assert-Campaign ([bool]$focused.tab.focused) "tab focus response was not focused"
        $moved = Invoke-RawRequest -Session $mainSession -Method "tab.move" -Params @{
            tab_id = $tabCloseId
            insert_index = 0
        }
        Assert-Campaign ($moved.type -eq "tab_moved") "tab.move returned '$($moved.type)'"
        $firstMovedTab = [string](@($moved.tabs)[0])
        Assert-Campaign ($firstMovedTab -eq $tabCloseId) "tab.move did not place the tab first"
        $closed = Invoke-SessionJson -Session $mainSession -CommandArguments @("tab", "close", $tabCloseId)
        Assert-Campaign ($closed.type -eq "tab_closed") "tab close returned '$($closed.type)'"
        Set-CheckDetail "renamed/focused/reordered/closed tabs in $alphaId"
    })

    $paneContext = Invoke-CampaignCheck "concepts" "pane-crud" {
        $split = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "pane", "split", $tabTwoRoot, "--direction", "right", "--ratio", "0.55",
            "--cwd", $script:TempRoot, "--focus"
        )
        Assert-Campaign ($split.type -eq "pane_info") "pane split returned '$($split.type)'"
        $newPane = [string]$split.pane.pane_id
        $listed = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "list", "--workspace", $alphaId)
        Assert-Campaign (@($listed.panes | Where-Object { $_.pane_id -eq $newPane }).Count -eq 1) "pane list omitted $newPane"
        $get = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "get", $newPane)
        Assert-Campaign ($get.pane.pane_id -eq $newPane) "pane get returned a different pane"
        $renamed = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "pane", "rename", $newPane, "b4-pane-right"
        )
        Assert-Campaign ($renamed.pane.label -eq "b4-pane-right") "pane rename was not visible"
        $focusedLeft = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "pane", "focus", "--direction", "left", "--pane", $newPane
        )
        Assert-Campaign ($focusedLeft.pane.pane_id -eq $tabTwoRoot) "directional focus did not reach root pane"
        Set-CheckDetail "split/list/get/rename/focus exercised on $newPane"
        return [pscustomobject]@{ NewPane = $newPane }
    }

    if ($null -eq $paneContext) {
        Write-Check "concepts" "split-zoom-swap-move" "SKIP" "pane CRUD prerequisite failed"
        Write-Check "concepts" "neighbor-edges-layout" "SKIP" "pane CRUD prerequisite failed"
        Write-Check "concepts" "layout-export-apply-ratio" "SKIP" "pane CRUD prerequisite failed"
    }
    else {
        $newPane = [string]$paneContext.NewPane
        [void](Invoke-CampaignCheck "concepts" "neighbor-edges-layout" {
            $neighbor = Invoke-SessionJson -Session $mainSession -CommandArguments @(
                "pane", "neighbor", "--direction", "left", "--pane", $newPane
            )
            Assert-Campaign ($neighbor.type -eq "pane_neighbor") "pane neighbor returned '$($neighbor.type)'"
            Assert-Campaign ([string]$neighbor.neighbor -eq $tabTwoRoot) "left neighbor was '$($neighbor.neighbor)'"
            $edges = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "edges", "--pane", $tabTwoRoot)
            Assert-Campaign ($edges.type -eq "pane_edges" -and $null -ne $edges.edges) "pane edges omitted edge data"
            $layout = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "layout", "--pane", $newPane)
            Assert-Campaign ($layout.type -eq "pane_layout") "pane layout returned '$($layout.type)'"
            Assert-Campaign (@($layout.layout.panes).Count -eq 2) "pane layout did not contain two panes"
            Set-CheckDetail "neighbor, edges and two-pane layout returned authoritative snapshots"
        })

        [void](Invoke-CampaignCheck "concepts" "layout-export-apply-ratio" {
            $exported = Invoke-RawRequest -Session $mainSession -Method "layout.export" -Params @{ tab_id = $tabTwoId }
            Assert-Campaign ($exported.type -eq "layout_export" -and $exported.root.type -eq "split") `
                "layout.export did not return a split root"
            $ratio = Invoke-RawRequest -Session $mainSession -Method "layout.set_split_ratio" -Params @{
                tab_id = $tabTwoId
                path = @()
                ratio = 0.63
            }
            Assert-Campaign ($ratio.type -eq "layout_split_ratio_set") "set_split_ratio returned '$($ratio.type)'"
            $actualRatio = [double]$ratio.layout.root.ratio
            Assert-Campaign ([Math]::Abs($actualRatio - 0.63) -lt 0.01) "split ratio was $actualRatio instead of 0.63"
            $applied = Invoke-RawRequest -Session $mainSession -Method "layout.apply" -Params @{
                workspace_id = $alphaId
                tab_label = "b4-applied-layout"
                focus = $false
                root = @{
                    type = "split"
                    direction = "down"
                    ratio = 0.4
                    first = @{ type = "pane"; label = "b4-applied-top"; cwd = $script:TempRoot }
                    second = @{ type = "pane"; label = "b4-applied-bottom"; cwd = $script:TempRoot }
                }
            }
            Assert-Campaign ($applied.type -eq "layout_applied") "layout.apply returned '$($applied.type)'"
            Assert-Campaign (@($applied.layout.panes).Count -eq 2) "layout.apply response omitted two created panes"
            $appliedExport = Invoke-RawRequest -Session $mainSession -Method "layout.export" -Params @{
                tab_id = [string]$applied.tab.tab_id
            }
            Assert-Campaign ($appliedExport.root.type -eq "split") "export after layout.apply omitted split tree"
            Set-CheckDetail "exported split, set root ratio to $actualRatio, applied a fresh split tab"
        })

        [void](Invoke-CampaignCheck "concepts" "split-zoom-swap-move" {
            $zoomOn = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "zoom", $newPane, "--on")
            Assert-Campaign ([bool]$zoomOn.zoomed) "pane zoom --on did not zoom"
            $zoomOff = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "zoom", $newPane, "--off")
            Assert-Campaign (-not [bool]$zoomOff.zoomed) "pane zoom --off did not unzoom"
            $swapped = Invoke-SessionJson -Session $mainSession -CommandArguments @(
                "pane", "swap", "--source-pane", $tabTwoRoot, "--target-pane", $newPane
            )
            Assert-Campaign ($swapped.type -eq "pane_swap" -and [bool]$swapped.changed) "explicit pane swap did not change layout"
            $resized = Invoke-SessionJson -Session $mainSession -CommandArguments @(
                "pane", "resize", "--direction", "left", "--amount", "0.05", "--pane", $newPane
            )
            Assert-Campaign ($resized.type -eq "pane_resize") "pane resize returned '$($resized.type)'"
            $moved = Invoke-SessionJson -Session $mainSession -CommandArguments @(
                "pane", "move", $newPane, "--tab", $tabThreeId, "--split", "right",
                "--target-pane", $tabThreeRoot, "--ratio", "0.5", "--focus"
            )
            Assert-Campaign ($moved.type -eq "pane_move" -and [bool]$moved.changed) "pane move did not change destination"
            $movedPane = [string]$moved.pane.pane_id
            $closed = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "close", $movedPane)
            Assert-Campaign ($closed.type -eq "pane_closed") "pane close returned '$($closed.type)'"
            Set-CheckDetail "split/zoom/swap/resize/move/close completed; moved pane=$movedPane"
        })
    }

    [void](Invoke-CampaignCheck "concepts" "workspace-close" {
        $closed = Invoke-SessionJson -Session $mainSession -CommandArguments @("workspace", "close", $gammaId)
        Assert-Campaign ($closed.type -eq "workspace_closed") "workspace close returned '$($closed.type)'"
        $listed = Invoke-SessionJson -Session $mainSession -CommandArguments @("workspace", "list")
        Assert-Campaign (@($listed.workspaces | Where-Object { $_.workspace_id -eq $gammaId }).Count -eq 0) `
            "closed workspace remained listed"
        Set-CheckDetail "closed $gammaId and verified absence"
    })

    return [pscustomobject]@{
        AlphaId = $alphaId
        BetaId = $betaId
        AgentPane = [string]$Initial.PaneId
    }
}

function Report-AgentState {
    param(
        [Parameter(Mandatory = $true)][string]$PaneId,
        [Parameter(Mandatory = $true)][ValidateSet("idle", "working", "blocked", "unknown")][string]$State,
        [Parameter(Mandatory = $true)][int]$Sequence
    )

    return Invoke-SessionJson -Session $mainSession -CommandArguments @(
        "pane", "report-agent", $PaneId,
        "--source", "b4:campaign",
        "--agent", $agentName,
        "--state", $State,
        "--seq", [string]$Sequence
    )
}

function Run-AgentChecks {
    param([Parameter(Mandatory = $true)][string]$PaneId)

    $reported = Invoke-CampaignCheck "agents" "report-lifecycle" {
        $result = Report-AgentState -PaneId $PaneId -State "idle" -Sequence 1
        Assert-Campaign ($result.type -eq "agent_reported" -and [bool]$result.accepted) `
            "idle report was not accepted"
        Set-CheckDetail "reported idle authority b4:campaign on cmd pane $PaneId"
        return $true
    }
    if ($null -eq $reported) {
        foreach ($name in @("list-get-read", "rename-focus-explain", "wait-until", "stall-error", "prompt-roundtrip")) {
            Write-Check "agents" $name "SKIP" "reported-agent prerequisite failed"
        }
        return
    }

    [void](Invoke-CampaignCheck "agents" "list-get-read" {
        $listed = Invoke-SessionJson -Session $mainSession -CommandArguments @("agent", "list")
        Assert-Campaign ($listed.type -eq "agent_list") "agent list returned '$($listed.type)'"
        Assert-Campaign (@($listed.agents | Where-Object { $_.pane_id -eq $PaneId -and $_.agent -eq $agentName }).Count -eq 1) `
            "agent list omitted reported agent"
        $get = Invoke-SessionJson -Session $mainSession -CommandArguments @("agent", "get", $PaneId)
        Assert-Campaign ($get.type -eq "agent_info" -and $get.agent.agent_status -eq "idle") `
            "agent get did not expose idle state"
        [void](Invoke-SessionText -Session $mainSession -CommandArguments @(
            "agent", "read", $PaneId, "--source", "detection", "--lines", "16", "--format", "text"
        ))
        Set-CheckDetail "agent list/get/read succeeded for $PaneId"
    })

    [void](Invoke-CampaignCheck "agents" "rename-focus-explain" {
        $renamed = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "agent", "rename", $PaneId, $agentRenamed
        )
        Assert-Campaign ($renamed.agent.name -eq $agentRenamed) "agent rename returned '$($renamed.agent.name)'"
        $focused = Invoke-SessionJson -Session $mainSession -CommandArguments @("agent", "focus", $PaneId)
        Assert-Campaign ([bool]$focused.agent.focused) "agent focus did not focus pane"
        $explained = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "agent", "explain", $PaneId, "--json", "--verbose"
        )
        Assert-Campaign ($explained.type -eq "agent_explain" -and $explained.pane_id -eq $PaneId) `
            "agent explain returned an unexpected target"
        Set-CheckDetail "renamed to $agentRenamed, focused and explained"
    })

    [void](Invoke-CampaignCheck "agents" "wait-until" {
        $waitLaunch = Start-NativeProcess -FilePath $script:Starcil -Arguments @(
            "--session", $mainSession, "agent", "wait", $PaneId,
            "--until", "working", "--timeout", "8000"
        ) -WorkingDirectory $script:RepoRoot -Redirect
        Start-Sleep -Milliseconds 350
        $working = Report-AgentState -PaneId $PaneId -State "working" -Sequence 2
        Assert-Campaign ([bool]$working.accepted) "working report was not accepted"
        $waitInvocation = Complete-NativeProcess -Launch $waitLaunch -TimeoutMs 10000
        $wait = Get-SuccessResult $waitInvocation
        Assert-Campaign ($wait.type -eq "agent_wait" -and $wait.outcome -eq "reached" -and $wait.state -eq "working") `
            "agent wait did not reach working"
        Set-CheckDetail "wait reached working after reported transition in $($wait.elapsed_ms)ms"
    })

    [void](Invoke-CampaignCheck "agents" "stall-error" {
        $idle = Report-AgentState -PaneId $PaneId -State "idle" -Sequence 3
        Assert-Campaign ([bool]$idle.accepted) "idle reset report was not accepted"
        $stalledInvocation = Invoke-Starcil -Arguments @(
            "--session", $mainSession, "agent", "wait", $PaneId,
            "--until", "blocked", "--timeout", "7000"
        ) -TimeoutMs 9000
        $errorEnvelope = Get-ErrorEnvelope $stalledInvocation
        Assert-Campaign ($errorEnvelope.error.code -eq "agent_prompt_stalled") `
            "expected agent_prompt_stalled, got '$($errorEnvelope.error.code)'"
        Assert-Campaign ($errorEnvelope.error.message -match "no lifecycle change observed") `
            "stall error message was not diagnostic"
        Set-CheckDetail "stable idle state produced agent_prompt_stalled after the 5s guard"
    })

    [void](Invoke-CampaignCheck "agents" "prompt-roundtrip" {
        $idle = Report-AgentState -PaneId $PaneId -State "idle" -Sequence 4
        Assert-Campaign ([bool]$idle.accepted) "pre-prompt idle report was not accepted"
        $token = "STARCIL_B4_PROMPT_$suffix"
        $promptLaunch = Start-NativeProcess -FilePath $script:Starcil -Arguments @(
            "--session", $mainSession, "agent", "prompt", $PaneId,
            "echo $token", "--wait", "--until", "idle", "--until", "done", "--timeout", "10000"
        ) -WorkingDirectory $script:RepoRoot -Redirect
        $matched = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "pane", "wait-output", $PaneId, "--match", $token,
            "--source", "recent", "--timeout", "8000"
        ) -TimeoutMs 10000
        Assert-Campaign ($matched.type -eq "pane_output_matched") "prompt text did not execute in cmd pane"
        $working = Report-AgentState -PaneId $PaneId -State "working" -Sequence 5
        Assert-Campaign ([bool]$working.accepted) "prompt working report was not accepted"
        Start-Sleep -Milliseconds 250
        $settled = Report-AgentState -PaneId $PaneId -State "idle" -Sequence 6
        Assert-Campaign ([bool]$settled.accepted) "prompt idle report was not accepted"
        $promptInvocation = Complete-NativeProcess -Launch $promptLaunch -TimeoutMs 12000
        $prompt = Get-SuccessResult $promptInvocation
        Assert-Campaign ($prompt.type -eq "agent_wait" -and @("idle", "done") -contains [string]$prompt.state) `
            "prompt --wait did not settle at idle/done"
        $read = Invoke-SessionText -Session $mainSession -CommandArguments @(
            "pane", "read", $PaneId, "--source", "recent", "--lines", "40"
        )
        Assert-Campaign ($read.Contains($token)) "prompt output was not readable"
        Set-CheckDetail "prompt executed '$token' and reported working-to-$($prompt.state) lifecycle"
    })
}

function Run-MetadataNotificationAndApiChecks {
    param(
        [Parameter(Mandatory = $true)][string]$PaneId,
        [Parameter(Mandatory = $true)][string]$WorkspaceId
    )

    [void](Invoke-CampaignCheck "metadata" "pane-workspace-tokens" {
        $paneReport = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "pane", "report-metadata", $PaneId, "--source", "b4:metadata",
            "--token", "campaign=pane-$suffix", "--seq", "1"
        )
        Assert-Campaign ($paneReport.type -eq "metadata_reported" -and [bool]$paneReport.applied) `
            "pane metadata report was not applied"
        $pane = Invoke-SessionJson -Session $mainSession -CommandArguments @("pane", "get", $PaneId)
        Assert-Campaign ($pane.pane.tokens.campaign -eq "pane-$suffix") "pane get omitted campaign token"

        $workspaceReport = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "workspace", "report-metadata", $WorkspaceId, "--source", "b4:metadata",
            "--token", "campaign=workspace-$suffix", "--seq", "1"
        )
        Assert-Campaign ($workspaceReport.type -eq "metadata_reported" -and [bool]$workspaceReport.applied) `
            "workspace metadata report was not applied"
        $workspace = Invoke-SessionJson -Session $mainSession -CommandArguments @("workspace", "get", $WorkspaceId)
        Assert-Campaign ($workspace.workspace.tokens.campaign -eq "workspace-$suffix") `
            "workspace get omitted campaign token"
        Set-CheckDetail "pane and workspace tokens are visible in get responses"
    })

    [void](Invoke-CampaignCheck "notification" "show-headless" {
        $result = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "notification", "show", "B4 campaign", "--body", "headless delivery probe",
            "--position", "top-right", "--sound", "none"
        )
        Assert-Campaign ($result.type -eq "notification_show" -and -not [bool]$result.shown) `
            "headless notification unexpectedly reported shown"
        Assert-Campaign (@("disabled", "no_foreground_client") -contains [string]$result.reason) `
            "unexpected notification reason '$($result.reason)'"
        Set-CheckDetail "headless notification returned reason=$($result.reason)"
    })

    [void](Invoke-CampaignCheck "api" "snapshot" {
        $snapshot = Invoke-SessionJson -Session $mainSession -CommandArguments @("api", "snapshot")
        Assert-Campaign ($snapshot.type -eq "session_snapshot") "api snapshot returned '$($snapshot.type)'"
        Assert-Campaign ($snapshot.session -eq $mainSession) "snapshot returned session '$($snapshot.session)'"
        Assert-Campaign (@($snapshot.workspaces).Count -ge 1 -and @($snapshot.tabs).Count -ge 1 -and @($snapshot.panes).Count -ge 1) `
            "snapshot omitted model records"
        Set-CheckDetail "snapshot parsed with $(@($snapshot.workspaces).Count) workspaces and $(@($snapshot.panes).Count) panes"
    })

    [void](Invoke-CampaignCheck "api" "schema-json" {
        $invocation = Invoke-Starcil -Arguments @("api", "schema", "--json") -TimeoutMs 10000
        Assert-Campaign (-not $invocation.TimedOut -and $invocation.ExitCode -eq 0) `
            "api schema command failed: $(Get-LastNonEmptyLine $invocation.StdErr)"
        try { $schema = $invocation.StdOut | ConvertFrom-Json } catch { throw "api schema stdout did not parse as JSON" }
        $schemaUri = $schema.PSObject.Properties['$schema']
        Assert-Campaign ($null -ne $schemaUri -and [string]$schemaUri.Value -match "2020-12") `
            "schema did not declare Draft 2020-12"
        Assert-Campaign (@($schema.oneOf).Count -ge 4) "schema root did not expose request/response/event variants"
        Set-CheckDetail "Draft 2020-12 JSON parsed with $(@($schema.oneOf).Count) root variants"
    })

    $integrationReason = "integration RPC executes in the long-lived server, so CLI-only USERPROFILE/HOME overrides are not forwarded; skipped to avoid touching the real home"
    Write-Check "integrations" "status-temp-home" "SKIP" $integrationReason
    Write-Check "integrations" "install-temp-home" "SKIP" $integrationReason
    Write-Check "integrations" "uninstall-temp-home" "SKIP" $integrationReason
}

function Invoke-GitCommand {
    param(
        [Parameter(Mandatory = $true)][string[]]$CommandArguments,
        [int]$TimeoutMs = 15000
    )

    $invocation = Complete-NativeProcess -Launch (
        Start-NativeProcess -FilePath "git.exe" -Arguments $CommandArguments `
            -WorkingDirectory $script:TempRoot -Redirect
    ) -TimeoutMs $TimeoutMs
    if ($invocation.TimedOut) { throw "git command timed out" }
    if ($invocation.ExitCode -ne 0) {
        throw "git exit $($invocation.ExitCode): $(Get-LastNonEmptyLine $invocation.StdErr)"
    }
    return $invocation.StdOut
}

function Run-WorktreeChecks {
    $repositoryReady = Invoke-CampaignCheck "worktrees" "temp-repository" {
        [void](New-Item -ItemType Directory -Path $worktreeRoot -Force)
        [void](Invoke-GitCommand @("init", $worktreeRoot))
        [void](Invoke-GitCommand @("-C", $worktreeRoot, "config", "user.email", "b4@starcil.invalid"))
        [void](Invoke-GitCommand @("-C", $worktreeRoot, "config", "user.name", "Starcil B4"))
        [System.IO.File]::WriteAllText(
            (Join-Path $worktreeRoot "tracked.txt"),
            "B4 $suffix" + [Environment]::NewLine,
            $script:Utf8NoBom
        )
        [void](Invoke-GitCommand @("-C", $worktreeRoot, "add", "tracked.txt"))
        [void](Invoke-GitCommand @("-C", $worktreeRoot, "commit", "-m", "B4 fixture"))
        $head = (Invoke-GitCommand @("-C", $worktreeRoot, "rev-parse", "HEAD")).Trim()
        Assert-Campaign ($head -match "^[0-9a-f]{40,64}$") "temp repository did not produce a commit"
        Set-CheckDetail "initialized isolated git repository at $worktreeRoot"
        return $true
    }
    if ($null -eq $repositoryReady) {
        Write-Check "worktrees" "create-list-open" "SKIP" "temp git repository prerequisite failed"
        Write-Check "worktrees" "remove" "SKIP" "temp git repository prerequisite failed"
        return
    }

    $sourceWorkspace = Invoke-CampaignCheck "worktrees" "workspace-root" {
        $created = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "workspace", "create", "--cwd", $worktreeRoot,
            "--label", "b4-worktree-root", "--no-focus"
        )
        Assert-Campaign ($created.type -eq "workspace_created") "worktree root workspace was not created"
        Set-CheckDetail "workspace $($created.workspace.workspace_id) points at temp repository"
        return $created
    }
    if ($null -eq $sourceWorkspace) {
        Write-Check "worktrees" "create-list-open" "SKIP" "worktree root workspace prerequisite failed"
        Write-Check "worktrees" "remove" "SKIP" "worktree root workspace prerequisite failed"
        return
    }

    $worktreeContext = Invoke-CampaignCheck "worktrees" "create-list-open" {
        $sourceId = [string]$sourceWorkspace.workspace.workspace_id
        $branch = "b4/feature-$suffix"
        $created = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "worktree", "create", "--workspace", $sourceId,
            "--branch", $branch, "--base", "HEAD", "--path", $worktreeCheckout,
            "--label", "b4-worktree-child", "--no-focus", "--json"
        ) -TimeoutMs 20000
        Assert-Campaign ($created.type -eq "worktree_created") "worktree create returned '$($created.type)'"
        Assert-Campaign (Test-Path -LiteralPath $worktreeCheckout -PathType Container) `
            "worktree checkout directory was not created"
        $childWorkspace = [string]$created.workspace.workspace_id

        $listed = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "worktree", "list", "--workspace", $sourceId, "--json"
        )
        Assert-Campaign ($listed.type -eq "worktree_list") "worktree list returned '$($listed.type)'"
        Assert-Campaign (@($listed.worktrees | Where-Object { $_.branch -eq $branch }).Count -eq 1) `
            "worktree list omitted branch $branch"

        $opened = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "worktree", "open", "--workspace", $sourceId,
            "--path", $worktreeCheckout, "--no-focus", "--json"
        )
        Assert-Campaign ($opened.type -eq "worktree_opened" -and [bool]$opened.already_open) `
            "worktree open did not recognize the existing workspace"
        Set-CheckDetail "created/listed/opened branch $branch via workspace $childWorkspace"
        return [pscustomobject]@{ ChildWorkspace = $childWorkspace; Branch = $branch }
    }
    if ($null -eq $worktreeContext) {
        Write-Check "worktrees" "remove" "SKIP" "worktree create/list/open prerequisite failed"
        return
    }

    [void](Invoke-CampaignCheck "worktrees" "remove" {
        $removed = Invoke-SessionJson -Session $mainSession -CommandArguments @(
            "worktree", "remove", "--workspace", [string]$worktreeContext.ChildWorkspace, "--force", "--json"
        ) -TimeoutMs 20000
        Assert-Campaign ($removed.type -eq "worktree_removed") "worktree remove returned '$($removed.type)'"
        Assert-Campaign (-not (Test-Path -LiteralPath $worktreeCheckout)) "worktree checkout remained after remove"
        Set-CheckDetail "removed branch $($worktreeContext.Branch) and checkout directory"
    })
}

function Run-PluginChecks {
    $manifestPath = Join-Path $pluginRoot "starcil-plugin.toml"
    $manifest = @"
id = "$pluginId"
name = "Starcil B4 Fixture"
version = "1.0.0"
min_starcil_version = "0.1.0"
description = "B4 live E2E fixture"
platforms = ["windows"]

[[actions]]
id = "probe"
title = "Run B4 probe"
contexts = ["workspace", "pane"]
command = ["cmd.exe", "/d", "/s", "/c", "echo B4_PLUGIN_$suffix 1>&2"]
"@
    [System.IO.File]::WriteAllText($manifestPath, $manifest, $script:Utf8NoBom)

    $linked = Invoke-CampaignCheck "plugins" "link-list" {
        $link = Invoke-Starcil -Arguments @(
            "--session", $mainSession, "plugin", "link", $pluginRoot
        )
        Assert-Campaign (-not $link.TimedOut -and $link.ExitCode -eq 0) `
            "plugin link failed: $(Get-LastNonEmptyLine $link.StdErr)"
        Assert-Campaign ($link.StdOut.Contains($pluginId)) "plugin link output omitted $pluginId"

        $list = Invoke-Starcil -Arguments @(
            "--session", $mainSession, "plugin", "list", "--plugin", $pluginId, "--json"
        )
        Assert-Campaign (-not $list.TimedOut -and $list.ExitCode -eq 0) `
            "plugin list failed: $(Get-LastNonEmptyLine $list.StdErr)"
        try { $listJson = $list.StdOut | ConvertFrom-Json } catch { throw "plugin list --json did not parse" }
        Assert-Campaign ($listJson.type -eq "plugin_list" -and @($listJson.plugins).Count -eq 1) `
            "plugin list did not return exactly the linked fixture"
        Assert-Campaign ($listJson.plugins[0].plugin_id -eq $pluginId -and [bool]$listJson.plugins[0].enabled) `
            "linked plugin identity/state was incorrect"
        Set-CheckDetail "linked and listed minimal manifest $pluginId"
        return $true
    }

    if ($null -eq $linked) {
        Write-Check "plugins" "action-invoke" "SKIP" "plugin link prerequisite failed"
        Write-Check "plugins" "log-list" "SKIP" "plugin link prerequisite failed"
        return
    }

    [void](Invoke-CampaignCheck "plugins" "action-invoke" {
        $actions = Invoke-Starcil -Arguments @(
            "--session", $mainSession, "plugin", "action", "list", "--plugin", $pluginId
        )
        Assert-Campaign (-not $actions.TimedOut -and $actions.ExitCode -eq 0) `
            "plugin action list failed: $(Get-LastNonEmptyLine $actions.StdErr)"
        $qualifiedAction = "$pluginId.probe"
        Assert-Campaign ($actions.StdOut.Contains($qualifiedAction)) "action list omitted $qualifiedAction"

        $invoke = Invoke-Starcil -Arguments @(
            "--session", $mainSession, "plugin", "action", "invoke", $qualifiedAction
        )
        Assert-Campaign (-not $invoke.TimedOut -and $invoke.ExitCode -eq 0) `
            "plugin action invoke failed: $(Get-LastNonEmptyLine $invoke.StdErr)"
        try { $invokeJson = $invoke.StdOut | ConvertFrom-Json } catch { throw "plugin action invoke stdout did not parse" }
        Assert-Campaign ($invokeJson.type -eq "plugin_action_invoked") `
            "plugin action invoke returned '$($invokeJson.type)'"
        Set-CheckDetail "listed and invoked $qualifiedAction"
    })

    [void](Invoke-CampaignCheck "plugins" "log-list" {
        $deadline = [DateTime]::UtcNow.AddSeconds(6)
        $logText = ""
        do {
            $logs = Invoke-Starcil -Arguments @(
                "--session", $mainSession, "plugin", "log", "list",
                "--plugin", $pluginId, "--limit", "10"
            )
            Assert-Campaign (-not $logs.TimedOut -and $logs.ExitCode -eq 0) `
                "plugin log list failed: $(Get-LastNonEmptyLine $logs.StdErr)"
            $logText = $logs.StdOut
            if ($logText.Contains($pluginId) -and $logText.Contains('"state":"exited"')) { break }
            Start-Sleep -Milliseconds 150
        } while ([DateTime]::UtcNow -lt $deadline)
        Assert-Campaign ($logText.Contains($pluginId)) "plugin log list omitted the action invocation"
        Assert-Campaign ($logText.Contains('"state":"exited"')) "plugin action did not reach exited state"
        Assert-Campaign ($logText.Contains("B4_PLUGIN_$suffix")) "plugin stderr tail omitted fixture token"
        Set-CheckDetail "log list captured exited action and stderr token"
    })

    try {
        [void](Invoke-Starcil -Arguments @("--session", $mainSession, "plugin", "unlink", $pluginId) -TimeoutMs 5000)
    }
    catch {}
}

function Run-PersistenceChecks {
    Register-Session $restoreSession
    $restoreServer = $null
    $setup = Invoke-CampaignCheck "session-state" "prepare-durable-tree" {
        $restoreServer = Start-StarcilServer -Session $restoreSession -WorkingDirectory $restoreCwd
        $pong = Wait-ForServer -Session $restoreSession -TimeoutMs 8000
        Assert-Campaign ($null -ne $pong) "restore test server did not become reachable"

        $snapshot = Invoke-SessionJson -Session $restoreSession -CommandArguments @("api", "snapshot")
        $workspaceOne = [string]$snapshot.focused_workspace_id
        $tabOne = [string]$snapshot.focused_tab_id
        $paneOne = [string]$snapshot.focused_pane_id
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "workspace", "rename", $workspaceOne, "restore-ws-one"
        ))
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "tab", "rename", $tabOne, "restore-tab-one"
        ))
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "pane", "rename", $paneOne, "restore-pane-one"
        ))
        $split = Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "pane", "split", $paneOne, "--direction", "right", "--ratio", "0.61",
            "--cwd", $restoreCwd, "--focus"
        )
        $paneTwo = [string]$split.pane.pane_id
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "pane", "rename", $paneTwo, "restore-pane-two"
        ))

        $workspaceTwoResult = Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "workspace", "create", "--cwd", $restoreCwdTwo,
            "--label", "restore-ws-two", "--focus"
        )
        $workspaceTwo = [string]$workspaceTwoResult.workspace.workspace_id
        $tabTwo = [string]$workspaceTwoResult.tab.tab_id
        $paneThree = [string]$workspaceTwoResult.root_pane.pane_id
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "tab", "rename", $tabTwo, "restore-tab-two"
        ))
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
            "pane", "rename", $paneThree, "restore-pane-three"
        ))
        [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @("workspace", "focus", $workspaceTwo))

        $before = Invoke-SessionJson -Session $restoreSession -CommandArguments @("api", "snapshot")
        Assert-Campaign (@($before.workspaces).Count -eq 2 -and @($before.tabs).Count -eq 2 -and @($before.panes).Count -eq 3) `
            "durable fixture did not have the expected 2/2/3 model"
        Set-CheckDetail "prepared 2 workspaces, 2 tabs, 3 panes and a split tree"
        return [pscustomobject]@{
            Server = $restoreServer
            Before = $before
        }
    }

    if ($null -eq $setup) {
        foreach ($name in @("state-file", "hard-kill-restart", "model-labels-tree", "shells-relaunched-at-cwd")) {
            Write-Check "session-state" $name "SKIP" "durable tree prerequisite failed"
        }
        return $null
    }

    $statePath = Join-Path (Join-Path (Join-Path $tempLocalAppData "starcil\runtime") $restoreSession) `
        "state-$restoreSession.json"
    $stateReady = Invoke-CampaignCheck "session-state" "state-file" {
        $content = Wait-ForFileContent -Path $statePath -Needles @(
            '"schema_version": 1',
            'restore-ws-one',
            'restore-tab-two',
            'restore-pane-three'
        ) -TimeoutMs 10000
        Assert-Campaign ($null -ne $content) "state file did not contain the durable tree after debounce: $statePath"
        try { $state = $content | ConvertFrom-Json } catch { throw "state file was not valid JSON" }
        Assert-Campaign ([int]$state.schema_version -eq 1 -and $state.session -eq $restoreSession) `
            "state document schema/session mismatch"
        Set-CheckDetail "schema v1 state exists at $statePath"
        return $true
    }

    if ($null -eq $stateReady) {
        foreach ($name in @("hard-kill-restart", "model-labels-tree", "shells-relaunched-at-cwd")) {
            Write-Check "session-state" $name "SKIP" "state file prerequisite failed"
        }
        return $setup
    }

    $restartContext = Invoke-CampaignCheck "session-state" "hard-kill-restart" {
        $oldServer = $setup.Server
        $oldServerId = [int]$oldServer.Id
        $oldDescendants = @(Get-ProcessTreeIds $oldServerId)
        foreach ($childId in $oldDescendants) { [void]$script:KnownChildPids.Add([int]$childId) }
        Stop-Process -Id $oldServerId -Force -ErrorAction Stop
        Assert-Campaign ($oldServer.WaitForExit(5000)) "hard-killed server PID $oldServerId did not exit"
        Start-Sleep -Milliseconds 400
        foreach ($childId in $oldDescendants) {
            if (Get-Process -Id $childId -ErrorAction SilentlyContinue) {
                try { Stop-Process -Id $childId -Force -ErrorAction Stop } catch {}
            }
        }

        $newServer = Start-StarcilServer -Session $restoreSession -WorkingDirectory $script:RepoRoot
        $pong = Wait-ForServer -Session $restoreSession -TimeoutMs 10000
        Assert-Campaign ($null -ne $pong) "same session did not restart after hard kill"
        Assert-Campaign ([int]$newServer.Id -ne $oldServerId) "restart reused the hard-killed server PID"
        $after = Invoke-SessionJson -Session $restoreSession -CommandArguments @("api", "snapshot")
        Set-CheckDetail "Stop-Process killed PID $oldServerId; restarted same session as PID $($newServer.Id)"
        return [pscustomobject]@{ Server = $newServer; After = $after; Before = $setup.Before }
    }

    if ($null -eq $restartContext) {
        Write-Check "session-state" "model-labels-tree" "SKIP" "restart prerequisite failed"
        Write-Check "session-state" "shells-relaunched-at-cwd" "SKIP" "restart prerequisite failed"
        return $setup
    }

    [void](Invoke-CampaignCheck "session-state" "model-labels-tree" {
        $before = $restartContext.Before
        $after = $restartContext.After
        Assert-Campaign (@($after.workspaces).Count -eq 2 -and @($after.tabs).Count -eq 2 -and @($after.panes).Count -eq 3) `
            "restored model counts were not 2 workspaces / 2 tabs / 3 panes"
        $workspaceLabels = @($after.workspaces | ForEach-Object { [string]$_.label })
        $tabLabels = @($after.tabs | ForEach-Object { [string]$_.label })
        $paneLabels = @($after.panes | ForEach-Object { [string]$_.label })
        foreach ($label in @("restore-ws-one", "restore-ws-two")) {
            Assert-Campaign ($workspaceLabels -contains $label) "restored workspaces omitted '$label'"
        }
        foreach ($label in @("restore-tab-one", "restore-tab-two")) {
            Assert-Campaign ($tabLabels -contains $label) "restored tabs omitted '$label'"
        }
        foreach ($label in @("restore-pane-one", "restore-pane-two", "restore-pane-three")) {
            Assert-Campaign ($paneLabels -contains $label) "restored panes omitted '$label'"
        }
        $splitLayouts = @($after.layouts | Where-Object { @($_.panes).Count -eq 2 })
        Assert-Campaign ($splitLayouts.Count -eq 1) "restored layouts omitted the two-pane layout"
        $portable = Invoke-RawRequest -Session $restoreSession -Method "layout.export" -Params @{
            tab_id = [string]$splitLayouts[0].tab_id
        }
        Assert-Campaign ($portable.root.type -eq "split") "restored portable layout omitted the split tree"
        Assert-Campaign ([Math]::Abs(([double]$portable.root.ratio) - 0.61) -lt 0.01) `
            "restored split ratio was '$($portable.root.ratio)'"

        foreach ($label in @("restore-pane-one", "restore-pane-two", "restore-pane-three")) {
            $oldPane = @($before.panes | Where-Object { $_.label -eq $label })[0]
            $newPane = @($after.panes | Where-Object { $_.label -eq $label })[0]
            Assert-Campaign ($oldPane.terminal_id -ne $newPane.terminal_id) `
                "pane '$label' retained old terminal id instead of relaunching"
        }
        Assert-Campaign (Test-Path -LiteralPath $statePath -PathType Leaf) "state file disappeared after restart"
        Set-CheckDetail "counts, labels, split tree and fresh terminal ids restored"
    })

    [void](Invoke-CampaignCheck "session-state" "shells-relaunched-at-cwd" {
        $after = Invoke-SessionJson -Session $restoreSession -CommandArguments @("api", "snapshot")
        $expectations = @{
            "restore-pane-one" = $restoreCwd
            "restore-pane-two" = $restoreCwd
            "restore-pane-three" = $restoreCwdTwo
        }
        foreach ($entry in $expectations.GetEnumerator()) {
            $pane = @($after.panes | Where-Object { $_.label -eq $entry.Key })[0]
            Assert-Campaign ($null -ne $pane) "restored pane '$($entry.Key)' was missing"
            Assert-Campaign ([string]$pane.cwd -eq [string]$entry.Value) `
                "pane '$($entry.Key)' metadata cwd was '$($pane.cwd)'"
            $token = "B4CWD_$($entry.Key)_$suffix"
            [void](Invoke-SessionJson -Session $restoreSession -CommandArguments @(
                "pane", "run", [string]$pane.pane_id, "echo ${token}:%CD%"
            ))
            $matched = Invoke-SessionJson -Session $restoreSession -CommandArguments @(
                "pane", "wait-output", [string]$pane.pane_id,
                "--match", $token, "--source", "recent", "--timeout", "8000"
            ) -TimeoutMs 10000
            Assert-Campaign ($matched.type -eq "pane_output_matched") `
                "relaunched cmd for '$($entry.Key)' did not execute cwd probe"
            $read = Invoke-SessionText -Session $restoreSession -CommandArguments @(
                "pane", "read", [string]$pane.pane_id, "--source", "recent-unwrapped", "--lines", "40"
            )
            if ($read.IndexOf([string]$entry.Value, [System.StringComparison]::OrdinalIgnoreCase) -lt 0) {
                $observed = ($read -replace "[\r\n]+", " | " -replace "\s+", " ").Trim()
                if ($observed.Length -gt 500) { $observed = $observed.Substring($observed.Length - 500) }
                throw "relaunched cmd for '$($entry.Key)' expected cwd '$($entry.Value)'; observed '$observed'"
            }
        }
        Set-CheckDetail "all three relaunched cmd shells reported their persisted cwd"
    })

    return $restartContext
}

function Run-ConfigAndChannelChecks {
    $configPath = Join-Path $script:TempRoot "config-commands.toml"
    $configText = @"
onboarding = false

[update]
channel = "stable"
version_check = false

[keys]
prefix = "ctrl+a"
help = "prefix+?"

[[keys.command]]
key = "prefix+alt+g"
type = "shell"
command = "echo b4"
description = "B4 fixture"
"@
    [System.IO.File]::WriteAllText($configPath, $configText, $script:Utf8NoBom)
    $configEnvironment = @{ "STARCIL_CONFIG_PATH" = $configPath }

    [void](Invoke-CampaignCheck "config" "check-temp" {
        $check = Invoke-Starcil -Arguments @("config", "check") -Environment $configEnvironment
        Assert-Campaign (-not $check.TimedOut -and $check.ExitCode -eq 0) `
            "config check failed: $(Get-LastNonEmptyLine $check.StdOut) $(Get-LastNonEmptyLine $check.StdErr)"
        Assert-Campaign ($check.StdOut.Contains("Configuration is valid.")) "config check omitted valid verdict"
        Assert-Campaign ($check.StdOut.Contains($configPath)) "config check did not use temp path"
        Set-CheckDetail "validated isolated config $configPath"
    })

    [void](Invoke-CampaignCheck "config" "reset-keys-temp" {
        $reset = Invoke-Starcil -Arguments @("config", "reset-keys") -Environment $configEnvironment
        Assert-Campaign (-not $reset.TimedOut -and $reset.ExitCode -eq 0) `
            "config reset-keys failed: $(Get-LastNonEmptyLine $reset.StdErr)"
        $backup = "$configPath.bak"
        Assert-Campaign (Test-Path -LiteralPath $backup -PathType Leaf) "reset-keys did not create backup"
        $after = [System.IO.File]::ReadAllText($configPath)
        Assert-Campaign (-not $after.Contains("[keys]")) "reset-keys retained [keys]"
        Assert-Campaign (-not $after.Contains("[[keys.command]]")) "reset-keys retained custom commands"
        Assert-Campaign ($after.Contains('channel = "stable"') -and $after.Contains("onboarding = false")) `
            "reset-keys removed unrelated config"
        Set-CheckDetail "backup created; key tree removed; unrelated settings preserved"
    })

    [void](Invoke-CampaignCheck "channel" "show-set-temp" {
        $showStable = Invoke-Starcil -Arguments @("channel", "show") -Environment $configEnvironment
        Assert-Campaign ($showStable.ExitCode -eq 0 -and $showStable.StdOut.Trim() -eq "stable") `
            "channel show did not return stable"
        $set = Invoke-Starcil -Arguments @("channel", "set", "preview") -Environment $configEnvironment
        Assert-Campaign ($set.ExitCode -eq 0 -and $set.StdOut.Contains("Update channel set to preview.")) `
            "channel set preview failed"
        $showPreview = Invoke-Starcil -Arguments @("channel", "show") -Environment $configEnvironment
        Assert-Campaign ($showPreview.ExitCode -eq 0 -and $showPreview.StdOut.Trim() -eq "preview") `
            "channel show did not return preview after set"
        $after = [System.IO.File]::ReadAllText($configPath)
        Assert-Campaign ($after.Contains("onboarding = false")) "channel set removed unrelated config"
        Set-CheckDetail "show stable, set preview, show preview on isolated config"
    })

    return $configPath
}

function Run-SessionLifecycleChecks {
    [void](Invoke-CampaignCheck "sessions" "list" {
        $list = Invoke-Starcil -Arguments @("session", "list", "--json")
        Assert-Campaign (-not $list.TimedOut -and $list.ExitCode -eq 0) `
            "session list failed: $(Get-LastNonEmptyLine $list.StdErr)"
        try { $json = $list.StdOut | ConvertFrom-Json } catch { throw "session list --json did not parse" }
        $main = @($json.sessions | Where-Object { $_.name -eq $mainSession })
        $restore = @($json.sessions | Where-Object { $_.name -eq $restoreSession })
        Assert-Campaign ($main.Count -eq 1 -and [bool]$main[0].running) "main session was not listed running"
        Assert-Campaign ($restore.Count -eq 1 -and [bool]$restore[0].running) "restore session was not listed running"
        Set-CheckDetail "listed both throwaway sessions as running"
    })

    [void](Invoke-CampaignCheck "sessions" "stop" {
        $issues = New-Object 'System.Collections.Generic.List[string]'
        foreach ($session in @($mainSession, $restoreSession)) {
            $stopped = Invoke-Starcil -Arguments @("session", "stop", $session, "--json") -TimeoutMs 8000
            if ($stopped.TimedOut) {
                $issues.Add("$session timed out")
                continue
            }
            if ($stopped.ExitCode -ne 0) {
                $issues.Add("$session exit $($stopped.ExitCode): $(Get-LastNonEmptyLine $stopped.StdErr)")
                continue
            }
            try {
                $result = Get-SuccessResult $stopped
                if ($result.type -ne "ok") { $issues.Add("$session returned type '$($result.type)'") }
            }
            catch { $issues.Add("$session response: $(Get-ExceptionDetail $_)") }
        }
        Start-Sleep -Milliseconds 600
        foreach ($session in @($mainSession, $restoreSession)) {
            $probe = Wait-ForServer -Session $session -TimeoutMs 700
            if ($null -ne $probe) { $issues.Add("$session remained reachable") }
        }
        Assert-Campaign ($issues.Count -eq 0) $(if ($issues.Count -gt 0) { $issues -join "; " } else { "no issues" })
        Set-CheckDetail "stopped both throwaway session servers"
    })

    [void](Invoke-CampaignCheck "sessions" "delete" {
        foreach ($session in @($mainSession, $restoreSession)) {
            $deleted = Invoke-Starcil -Arguments @("session", "delete", $session, "--json") -TimeoutMs 8000
            Assert-Campaign (-not $deleted.TimedOut -and $deleted.ExitCode -eq 0) `
                "session delete for $session failed: $(Get-LastNonEmptyLine $deleted.StdErr)"
            try { $result = $deleted.StdOut | ConvertFrom-Json } catch { throw "session delete for $session did not return JSON" }
            Assert-Campaign ($result.type -eq "session_deleted" -and $result.session -eq $session) `
                "session delete returned an unexpected payload for $session"
        }
        $list = Invoke-Starcil -Arguments @("session", "list", "--json")
        Assert-Campaign ($list.ExitCode -eq 0) "post-delete session list failed"
        $json = $list.StdOut | ConvertFrom-Json
        Assert-Campaign (@($json.sessions | Where-Object { $_.name -in @($mainSession, $restoreSession) }).Count -eq 0) `
            "deleted sessions remained discoverable"
        Set-CheckDetail "deleted throwaway session data/runtime directories"
    })
}

function Run-OfflineUpdateCheck {
    param([Parameter(Mandatory = $true)][string]$ConfigPath)

    [void](Invoke-CampaignCheck "update" "offline-clean" {
        $environment = @{
            "STARCIL_CONFIG_PATH" = $ConfigPath
            "STARCIL_UPDATE_REPO" = "starcil-b4-$suffix/does-not-exist"
            "HTTP_PROXY" = "http://127.0.0.1:9"
            "HTTPS_PROXY" = "http://127.0.0.1:9"
            "ALL_PROXY" = "http://127.0.0.1:9"
            "NO_PROXY" = ""
        }
        $update = Invoke-Starcil -Arguments @("update") -Environment $environment -TimeoutMs 12000
        Assert-Campaign (-not $update.TimedOut) "offline update command timed out"
        Assert-Campaign ($update.ExitCode -eq 0) "offline update exited $($update.ExitCode): $(Get-LastNonEmptyLine $update.StdErr)"
        Assert-Campaign ($update.StdErr.Trim().Length -eq 0) "offline update emitted stderr: $(Get-LastNonEmptyLine $update.StdErr)"
        $expected = "Starcil is already up to date, or the update service is currently offline."
        Assert-Campaign ($update.StdOut.Trim() -eq $expected) "offline update message was '$($update.StdOut.Trim())'"
        Set-CheckDetail "offline update exited 0 with the clean no-update message"
    })
}

function Invoke-CampaignCleanup {
    $cleanupErrors = New-Object 'System.Collections.Generic.List[string]'

    foreach ($session in @($script:Sessions | Select-Object -Unique)) {
        try {
            [void](Invoke-Starcil -Arguments @("session", "stop", $session, "--json") -TimeoutMs 2500)
        }
        catch {}
    }
    Start-Sleep -Milliseconds 400

    foreach ($process in $script:ServerProcesses) {
        try {
            if (-not $process.HasExited) { Stop-TrackedProcessTree -RootId ([int]$process.Id) }
        }
        catch { $cleanupErrors.Add("tracked PID $($process.Id): $(Get-ExceptionDetail $_)") }
    }

    foreach ($server in @(Get-CampaignServerProcesses)) {
        try { Stop-TrackedProcessTree -RootId ([int]$server.ProcessId) }
        catch { $cleanupErrors.Add("discovered PID $($server.ProcessId): $(Get-ExceptionDetail $_)") }
    }
    Start-Sleep -Milliseconds 400

    $remainingServers = @(Get-CampaignServerProcesses)
    if ($remainingServers.Count -gt 0) {
        $cleanupErrors.Add("remaining Starcil server PIDs: $(@($remainingServers.ProcessId) -join ',')")
    }
    foreach ($childId in $script:KnownChildPids) {
        if (Get-Process -Id $childId -ErrorAction SilentlyContinue) {
            try { Stop-Process -Id $childId -Force -ErrorAction Stop } catch {}
            if (Get-Process -Id $childId -ErrorAction SilentlyContinue) {
                $cleanupErrors.Add("remaining child PID $childId")
            }
        }
    }

    try {
        $resolvedTemp = [System.IO.Path]::GetFullPath($script:TempRoot)
        $tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        $leaf = Split-Path -Leaf $resolvedTemp
        if (-not $resolvedTemp.StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase) -or
            -not $leaf.StartsWith("starcil-b4-", [System.StringComparison]::Ordinal)) {
            throw "refusing to remove unvalidated temp path $resolvedTemp"
        }
        if (Test-Path -LiteralPath $resolvedTemp) {
            Remove-Item -LiteralPath $resolvedTemp -Recurse -Force -ErrorAction Stop
        }
    }
    catch { $cleanupErrors.Add("temp cleanup: $(Get-ExceptionDetail $_)") }

    if ($cleanupErrors.Count -eq 0) {
        Write-Check "cleanup" "processes-sessions-temp" "PASS" "no campaign server/child process remains; temp root removed"
    }
    else {
        Write-Check "cleanup" "processes-sessions-temp" "FAIL" ($cleanupErrors -join "; ")
    }
}

$fatalError = $null
try {
    Assert-Campaign (Test-Path -LiteralPath $script:Starcil -PathType Leaf) `
        "target/debug/starcil.exe is missing; run .\build.ps1 build -p starcil first"

    $initial = Run-QuickStartChecks
    if ($null -eq $initial) {
        throw "quick-start did not produce a usable live session"
    }
    $crud = Run-CrudAndLayoutChecks -Initial $initial
    if ($null -ne $crud) {
        Run-AgentChecks -PaneId ([string]$crud.AgentPane)
        Run-MetadataNotificationAndApiChecks -PaneId ([string]$crud.AgentPane) -WorkspaceId ([string]$crud.AlphaId)
    }
    else {
        Write-Check "agents" "campaign" "SKIP" "CRUD context unavailable"
        Write-Check "metadata" "campaign" "SKIP" "CRUD context unavailable"
        Write-Check "notification" "campaign" "SKIP" "CRUD context unavailable"
        Write-Check "api" "campaign" "SKIP" "CRUD context unavailable"
        $integrationReason = "integration RPC isolation was not attempted because the main session was unavailable"
        Write-Check "integrations" "status-temp-home" "SKIP" $integrationReason
        Write-Check "integrations" "install-temp-home" "SKIP" $integrationReason
        Write-Check "integrations" "uninstall-temp-home" "SKIP" $integrationReason
    }
    Run-WorktreeChecks
    Run-PluginChecks
    [void](Run-PersistenceChecks)
    $configPath = Run-ConfigAndChannelChecks
    Run-SessionLifecycleChecks
    Run-OfflineUpdateCheck -ConfigPath $configPath
}
catch {
    $fatalError = Get-ExceptionDetail $_
    Write-Check "campaign" "unhandled" "FAIL" $fatalError
}
finally {
    Invoke-CampaignCleanup
    Write-CampaignLine "SUMMARY PASS=$($script:Passes) FAIL=$($script:Failures) SKIP=$($script:Skips)"
}

if ($script:Failures -gt 0) { exit 1 }
exit 0
