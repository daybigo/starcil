[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

if (-not $env:LOCALAPPDATA) {
    throw "LOCALAPPDATA is required to locate the Starcil installation"
}

$installDirectory = Join-Path $env:LOCALAPPDATA "starcil\bin"
$binaryPath = Join-Path $installDirectory "starcil.exe"
$trimCharacters = [char[]]@('\', '/')
$normalizedInstallDirectory = $installDirectory.Trim().Trim('"').TrimEnd($trimCharacters)

if (Test-Path -LiteralPath $binaryPath -PathType Leaf) {
    Remove-Item -LiteralPath $binaryPath -Force
    Write-Host "Removed Starcil binary: $binaryPath"
}
elseif (Test-Path -LiteralPath $binaryPath) {
    throw "Refusing to remove non-file install target: $binaryPath"
}
else {
    Write-Host "Starcil binary was not present: $binaryPath"
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($null -ne $userPath) {
    $keptUserEntries = @($userPath -split ";" | Where-Object {
        $_ -and $_.Trim().Trim('"').TrimEnd($trimCharacters) -ine $normalizedInstallDirectory
    })
    $newUserPath = [string]::Join(";", $keptUserEntries)
    if ($newUserPath -cne $userPath) {
        [Environment]::SetEnvironmentVariable(
            "Path",
            $(if ($newUserPath) { $newUserPath } else { $null }),
            "User"
        )
        Write-Host "Removed Starcil bin from the user PATH"
    }
}

$keptProcessEntries = @($env:Path -split ";" | Where-Object {
    $_ -and $_.Trim().Trim('"').TrimEnd($trimCharacters) -ine $normalizedInstallDirectory
})
$env:Path = [string]::Join(";", $keptProcessEntries)

$configPath = if ($env:APPDATA) { Join-Path $env:APPDATA "starcil" } else { "<APPDATA>\starcil" }
$dataPath = Join-Path $env:LOCALAPPDATA "starcil"
Write-Host "Kept user configuration: $configPath"
Write-Host "Kept user data: $dataPath"
