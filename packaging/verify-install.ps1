[CmdletBinding()]
param(
    [string]$BinaryPath
)

$ErrorActionPreference = "Stop"

if (-not $BinaryPath) {
    $BinaryPath = Join-Path $PSScriptRoot "..\target\debug\starcil.exe"
}

function Assert-InstallCondition {
    param(
        [bool]$Condition,
        [string]$Message
    )
    if (-not $Condition) {
        throw "VERIFY FAILED: $Message"
    }
}

function Normalize-PathEntry {
    param([string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) { return "" }
    $trimCharacters = [char[]]@('\', '/')
    return $Value.Trim().Trim('"').TrimEnd($trimCharacters)
}

function Get-PathEntryCount {
    param(
        [AllowNull()][string]$PathValue,
        [string]$ExpectedEntry
    )
    $normalizedExpected = Normalize-PathEntry $ExpectedEntry
    return @($PathValue -split ";" | Where-Object {
        $_ -and (Normalize-PathEntry $_) -ieq $normalizedExpected
    }).Count
}

function Restore-ProcessEnvironment {
    param(
        [string]$Name,
        [AllowNull()][string]$Value
    )
    if ($null -eq $Value) {
        Remove-Item -LiteralPath "Env:$Name" -ErrorAction SilentlyContinue
    }
    else {
        Set-Item -LiteralPath "Env:$Name" -Value $Value
    }
}

$binary = Get-Item -LiteralPath $BinaryPath -ErrorAction Stop
if ($binary.PSIsContainer) {
    throw "BinaryPath must point to target/debug/starcil.exe"
}

$installScript = Join-Path $PSScriptRoot "install.ps1"
$uninstallScript = Join-Path $PSScriptRoot "uninstall.ps1"
if (-not (Test-Path -LiteralPath $installScript -PathType Leaf)) { throw "Missing $installScript" }
if (-not (Test-Path -LiteralPath $uninstallScript -PathType Leaf)) { throw "Missing $uninstallScript" }

$originalLocalAppData = $env:LOCALAPPDATA
$originalAppData = $env:APPDATA
$originalProcessPath = $env:Path
$originalUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
$temporaryBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$temporaryRoot = [IO.Path]::GetFullPath(
    (Join-Path $temporaryBase ("starcil-verify-install-" + [guid]::NewGuid().ToString("N")))
)
if (-not $temporaryRoot.StartsWith($temporaryBase, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing unsafe verification root: $temporaryRoot"
}

$verificationPassed = $false
$pathRestored = $false
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null

try {
    $env:LOCALAPPDATA = Join-Path $temporaryRoot "local-app-data"
    $env:APPDATA = Join-Path $temporaryRoot "roaming-app-data"
    New-Item -ItemType Directory -Path $env:LOCALAPPDATA, $env:APPDATA -Force | Out-Null

    $configSentinel = Join-Path $env:APPDATA "starcil\config.toml"
    $dataSentinel = Join-Path $env:LOCALAPPDATA "starcil\keep-data.txt"
    New-Item -ItemType Directory -Path (Split-Path $configSentinel), (Split-Path $dataSentinel) -Force | Out-Null
    Set-Content -LiteralPath $configSentinel -Value "theme = 'verify-sentinel'" -Encoding Ascii
    Set-Content -LiteralPath $dataSentinel -Value "keep this data" -Encoding Ascii

    $releaseDirectory = Join-Path $temporaryRoot "fake-release"
    $zipStaging = Join-Path $temporaryRoot "zip-staging"
    New-Item -ItemType Directory -Path $releaseDirectory, $zipStaging -Force | Out-Null
    $stagedBinary = Join-Path $zipStaging "starcil.exe"
    Copy-Item -LiteralPath $binary.FullName -Destination $stagedBinary
    $assetName = "starcil-x86_64-pc-windows-gnu.zip"
    $archivePath = Join-Path $releaseDirectory $assetName
    Compress-Archive -LiteralPath $stagedBinary -DestinationPath $archivePath -CompressionLevel Optimal
    $archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath (Join-Path $releaseDirectory "SHA256SUMS") `
        -Value "$archiveHash  $assetName" -Encoding Ascii

    Write-Host "LOCAL_RELEASE: $releaseDirectory"
    & $installScript -LocalSource $releaseDirectory

    $installDirectory = Join-Path $env:LOCALAPPDATA "starcil\bin"
    $installedBinary = Join-Path $installDirectory "starcil.exe"
    Assert-InstallCondition (Test-Path -LiteralPath $installedBinary -PathType Leaf) "installed binary is missing"
    $sourceHash = (Get-FileHash -LiteralPath $binary.FullName -Algorithm SHA256).Hash
    $installedHash = (Get-FileHash -LiteralPath $installedBinary -Algorithm SHA256).Hash
    Assert-InstallCondition ($sourceHash -eq $installedHash) "installed binary differs from the local release"

    $versionOutput = @(& $installedBinary --version 2>&1)
    Assert-InstallCondition ($LASTEXITCODE -eq 0) "installed starcil --version failed"
    $versionLine = $versionOutput | ForEach-Object { $_.ToString().Trim() } |
        Where-Object { $_ -match '^starcil\s+\S+$' } | Select-Object -First 1
    Assert-InstallCondition ($null -ne $versionLine) "installed starcil --version output is invalid"
    Write-Host "VERSION_OK: $versionLine"

    $userPathAfterFirst = [Environment]::GetEnvironmentVariable("Path", "User")
    $processPathAfterFirst = $env:Path
    Assert-InstallCondition ((Get-PathEntryCount $userPathAfterFirst $installDirectory) -eq 1) `
        "first install did not add exactly one user PATH entry"
    Assert-InstallCondition ((Get-PathEntryCount $processPathAfterFirst $installDirectory) -eq 1) `
        "first install did not add exactly one process PATH entry"

    & $installScript -LocalSource $releaseDirectory
    $userPathAfterSecond = [Environment]::GetEnvironmentVariable("Path", "User")
    $processPathAfterSecond = $env:Path
    Assert-InstallCondition ($userPathAfterSecond -ceq $userPathAfterFirst) `
        "second install changed the user PATH"
    Assert-InstallCondition ($processPathAfterSecond -ceq $processPathAfterFirst) `
        "second install changed the process PATH"
    Assert-InstallCondition ((Get-PathEntryCount $userPathAfterSecond $installDirectory) -eq 1) `
        "second install duplicated the user PATH entry"
    Write-Host "PATH_IDEMPOTENT: PASS"

    & $uninstallScript
    Assert-InstallCondition (-not (Test-Path -LiteralPath $installedBinary)) "uninstall left starcil.exe behind"
    Assert-InstallCondition (Test-Path -LiteralPath $configSentinel -PathType Leaf) `
        "uninstall removed user configuration"
    Assert-InstallCondition (Test-Path -LiteralPath $dataSentinel -PathType Leaf) `
        "uninstall removed user data"
    $userPathAfterUninstall = [Environment]::GetEnvironmentVariable("Path", "User")
    Assert-InstallCondition ((Get-PathEntryCount $userPathAfterUninstall $installDirectory) -eq 0) `
        "uninstall left the user PATH entry behind"
    Assert-InstallCondition ((Get-PathEntryCount $env:Path $installDirectory) -eq 0) `
        "uninstall left the process PATH entry behind"
    Write-Host "UNINSTALL_BINARY_GONE: PASS"
    Write-Host "CONFIG_AND_DATA_KEPT: PASS"
    $verificationPassed = $true
}
finally {
    [Environment]::SetEnvironmentVariable("Path", $originalUserPath, "User")
    $env:Path = $originalProcessPath
    Restore-ProcessEnvironment "LOCALAPPDATA" $originalLocalAppData
    Restore-ProcessEnvironment "APPDATA" $originalAppData

    $restoredUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathRestored = if ($null -eq $originalUserPath) {
        $null -eq $restoredUserPath
    }
    else {
        $restoredUserPath -ceq $originalUserPath
    }

    if (Test-Path -LiteralPath $temporaryRoot) {
        $resolvedCleanup = [IO.Path]::GetFullPath($temporaryRoot)
        if (-not $resolvedCleanup.StartsWith($temporaryBase, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing unsafe verification cleanup: $resolvedCleanup"
        }
        Remove-Item -LiteralPath $resolvedCleanup -Recurse -Force
    }
    if (-not $pathRestored) {
        throw "VERIFY FAILED: user PATH was not restored"
    }
}

if ($verificationPassed) {
    Write-Host "PATH_RESTORED: PASS"
    Write-Host "PASS: local install and uninstall verification"
}
