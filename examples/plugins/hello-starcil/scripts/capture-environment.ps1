[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$requiredVariables = @(
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
$artifactPath = Join-Path $stateDirectory "action-environment.json"
$payload = [ordered]@{
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
    process_id = $PID
    working_directory = (Get-Location).Path
    environment = $environment
}
$json = $payload | ConvertTo-Json -Depth 8
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($artifactPath, $json, $utf8WithoutBom)
