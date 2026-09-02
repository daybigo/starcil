$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")

& (Join-Path $repo "build.ps1") run -p starcil-terminal --example live_pwsh
if ($LASTEXITCODE -ne 0) {
    throw "live PowerShell probe failed with exit code $LASTEXITCODE"
}
