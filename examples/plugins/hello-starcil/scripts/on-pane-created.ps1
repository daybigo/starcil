[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$requiredVariables = @(
    "STARCIL_PLUGIN_ID",
    "STARCIL_PLUGIN_ROOT",
    "STARCIL_PLUGIN_STATE_DIR",
    "STARCIL_PLUGIN_CONTEXT_JSON",
    "STARCIL_PLUGIN_EVENT",
    "STARCIL_PLUGIN_EVENT_JSON"
)

$environment = [ordered]@{}
Get-ChildItem Env: |
    Where-Object { $_.Name -like "STARCIL_*" } |
    Sort-Object Name |
    ForEach-Object { $environment[$_.Name] = [string]$_.Value }

foreach ($name in $requiredVariables) {
    if (-not $environment.Contains($name) -or [string]::IsNullOrWhiteSpace($environment[$name])) {
        throw "Starcil did not inject required environment variable $name"
    }
}

$stateDirectory = [string]$environment["STARCIL_PLUGIN_STATE_DIR"]
[System.IO.Directory]::CreateDirectory($stateDirectory) | Out-Null
$logPath = Join-Path $stateDirectory "pane-created.log"
$entry = [ordered]@{
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
    process_id = $PID
    working_directory = (Get-Location).Path
    event_json = [string]$environment["STARCIL_PLUGIN_EVENT_JSON"]
    environment = $environment
}
$line = ($entry | ConvertTo-Json -Depth 8 -Compress) + [Environment]::NewLine
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::AppendAllText($logPath, $line, $utf8WithoutBom)
