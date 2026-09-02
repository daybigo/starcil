$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")

& (Join-Path $repo "build.ps1") run -p starcil-host --example live_host
if ($LASTEXITCODE -ne 0) {
    throw "live RealHost probe failed with exit code $LASTEXITCODE"
}
