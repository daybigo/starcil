param(
    [string]$Marker = (Join-Path $env:TEMP "starcil-detach-probe.txt")
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..\..")
$resolvedMarker = [System.IO.Path]::GetFullPath($Marker)
Remove-Item -LiteralPath $resolvedMarker -ErrorAction SilentlyContinue

& (Join-Path $repo "build.ps1") run -p starcil-platform --example detach_probe -- $resolvedMarker
if ($LASTEXITCODE -ne 0) {
    throw "detach_probe parent failed with exit code $LASTEXITCODE"
}

$deadline = [DateTime]::UtcNow.AddSeconds(10)
while (-not (Test-Path -LiteralPath $resolvedMarker)) {
    if ([DateTime]::UtcNow -ge $deadline) {
        throw "detached child did not create $resolvedMarker"
    }
    Start-Sleep -Milliseconds 100
}

$content = Get-Content -LiteralPath $resolvedMarker -Raw
if ($content -ne "detached-child-completed") {
    throw "unexpected marker content: $content"
}

Write-Output "DETACH_OK marker=$resolvedMarker"
Remove-Item -LiteralPath $resolvedMarker -ErrorAction SilentlyContinue
