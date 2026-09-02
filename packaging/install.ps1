[CmdletBinding()]
param(
    [string]$BaseUrl = $env:STARCIL_INSTALL_BASE_URL,
    [string]$LocalSource = $env:STARCIL_INSTALL_LOCAL_SOURCE
)

$RepoSlug = if ($env:STARCIL_UPDATE_REPO) { $env:STARCIL_UPDATE_REPO } else { "daybigo/starcil" }
$AssetName = "starcil-x86_64-pc-windows-gnu.zip"
$ChecksumAssetName = "SHA256SUMS"

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

if ($BaseUrl -and $LocalSource) {
    throw "Use either -BaseUrl or -LocalSource, not both"
}

$headers = @{
    Accept = "application/vnd.github+json"
    "User-Agent" = "starcil-installer"
}
$releaseTag = $null
$archiveUrl = $null
$checksumUrl = $null
$localSourcePath = $null

if ($LocalSource) {
    $localSourceItem = Get-Item -LiteralPath $LocalSource -ErrorAction Stop
    if (-not $localSourceItem.PSIsContainer) {
        throw "LocalSource must be a directory: $LocalSource"
    }
    $localSourcePath = $localSourceItem.FullName
}
elseif ($BaseUrl) {
    $base = $BaseUrl.TrimEnd("/")
    $archiveUrl = "$base/$AssetName"
    $checksumUrl = "$base/$ChecksumAssetName"
}
else {
    # The latest stable release first (GitHub excludes prereleases from
    # `releases/latest`); the newest preview only when no stable exists yet.
    $release = $null
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$RepoSlug/releases/latest" -Headers $headers
    }
    catch {
        $release = $null
    }
    if (-not $release) {
        $releasesUrl = "https://api.github.com/repos/$RepoSlug/releases?per_page=30"
        $releases = Invoke-RestMethod -Uri $releasesUrl -Headers $headers
        $release = $releases |
            Where-Object { -not $_.draft -and $_.prerelease } |
            Sort-Object -Property published_at -Descending |
            Select-Object -First 1
    }

    if (-not $release) {
        throw "No release is available for Windows in $RepoSlug"
    }

    $archiveAsset = $release.assets |
        Where-Object { $_.name -eq $AssetName } |
        Select-Object -First 1
    $checksumAsset = $release.assets |
        Where-Object { $_.name -eq $ChecksumAssetName } |
        Select-Object -First 1
    if (-not $archiveAsset) { throw "Release $($release.tag_name) is missing $AssetName" }
    if (-not $checksumAsset) { throw "Release $($release.tag_name) is missing $ChecksumAssetName" }
    $releaseTag = $release.tag_name
    $archiveUrl = $archiveAsset.browser_download_url
    $checksumUrl = $checksumAsset.browser_download_url
}

$temporaryBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$temporaryDirectory = [IO.Path]::GetFullPath(
    (Join-Path $temporaryBase ("starcil-install-" + [guid]::NewGuid().ToString("N")))
)
if (-not $temporaryDirectory.StartsWith($temporaryBase, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing unsafe installer temporary path: $temporaryDirectory"
}
New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null

try {
    $archivePath = Join-Path $temporaryDirectory $AssetName
    $checksumPath = Join-Path $temporaryDirectory $ChecksumAssetName
    if ($localSourcePath) {
        $localArchive = Join-Path $localSourcePath $AssetName
        $localChecksums = Join-Path $localSourcePath $ChecksumAssetName
        if (-not (Test-Path -LiteralPath $localArchive -PathType Leaf)) {
            throw "Local release is missing $AssetName"
        }
        if (-not (Test-Path -LiteralPath $localChecksums -PathType Leaf)) {
            throw "Local release is missing $ChecksumAssetName"
        }
        Copy-Item -LiteralPath $localArchive -Destination $archivePath
        Copy-Item -LiteralPath $localChecksums -Destination $checksumPath
    }
    else {
        Invoke-WebRequest -Uri $archiveUrl -Headers $headers -OutFile $archivePath
        Invoke-WebRequest -Uri $checksumUrl -Headers $headers -OutFile $checksumPath
    }

    $escapedAssetName = [regex]::Escape($AssetName)
    $checksumLine = Get-Content -LiteralPath $checksumPath |
        Where-Object { $_ -match "^(?<hash>[A-Fa-f0-9]{64})\s+\*?$escapedAssetName$" } |
        Select-Object -First 1
    if (-not $checksumLine) { throw "$ChecksumAssetName has no entry for $AssetName" }
    $checksumLine -match "^(?<hash>[A-Fa-f0-9]{64})" | Out-Null
    $expectedHash = $Matches.hash.ToLowerInvariant()
    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw "SHA-256 mismatch for $AssetName"
    }

    $expandedPath = Join-Path $temporaryDirectory "expanded"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $expandedPath
    $binary = Get-ChildItem -LiteralPath $expandedPath -Filter "starcil.exe" -File -Recurse |
        Select-Object -First 1
    if (-not $binary) { throw "$AssetName does not contain starcil.exe" }

    $version = if ($releaseTag) { $releaseTag.TrimStart("v") } else { $null }
    if (-not $version) {
        $versionOutput = @(& $binary.FullName --version 2>&1)
        if ($LASTEXITCODE -ne 0) {
            throw "The verified local release binary failed --version"
        }
        $versionLine = $versionOutput |
            ForEach-Object { $_.ToString().Trim() } |
            Where-Object { $_ -match '^starcil\s+(?<version>\S+)$' } |
            Select-Object -First 1
        if (-not $versionLine) {
            throw "Could not determine the Starcil version from the verified binary"
        }
        $versionLine -match '^starcil\s+(?<version>\S+)$' | Out-Null
        $version = $Matches.version
    }

    $installDirectory = Join-Path $env:LOCALAPPDATA "starcil\bin"
    New-Item -ItemType Directory -Path $installDirectory -Force | Out-Null
    $target = Join-Path $installDirectory "starcil.exe"
    if (Test-Path -LiteralPath $target -PathType Leaf) {
        # A running Starcil keeps its executable locked against overwrites,
        # but Windows allows renaming it: park the old binary so the running
        # server and TUI finish on it, and the new one takes the name.
        $parked = "$target.old"
        if (Test-Path -LiteralPath $parked) {
            try {
                Remove-Item -LiteralPath $parked -Force -ErrorAction Stop
            }
            catch {
                $parked = "$target.old-" + [DateTime]::UtcNow.ToString("yyyyMMddHHmmss")
            }
        }
        Move-Item -LiteralPath $target -Destination $parked -Force
    }
    Copy-Item -LiteralPath $binary.FullName -Destination $target -Force

    $trimCharacters = [char[]]@('\', '/')
    $normalizedInstallDirectory = $installDirectory.Trim().Trim('"').TrimEnd($trimCharacters)
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathEntries = @($userPath -split ";" | Where-Object { $_ })
    $alreadyPresent = $pathEntries | Where-Object {
        $_.Trim().Trim('"').TrimEnd($trimCharacters) -ieq $normalizedInstallDirectory
    }
    if (-not $alreadyPresent) {
        $newUserPath = if ($userPath) { "$userPath;$installDirectory" } else { $installDirectory }
        [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
    }
    $processEntry = @($env:Path -split ";" | Where-Object { $_ }) | Where-Object {
        $_.Trim().Trim('"').TrimEnd($trimCharacters) -ieq $normalizedInstallDirectory
    }
    if (-not $processEntry) {
        $env:Path = if ($env:Path) { "$installDirectory;$env:Path" } else { $installDirectory }
    }

    Write-Host ('starcil {0} installed {1} run `starcil`' -f $version, ([char]0x2014))
}
finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        $resolvedCleanup = [IO.Path]::GetFullPath($temporaryDirectory)
        if (-not $resolvedCleanup.StartsWith($temporaryBase, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing unsafe installer cleanup path: $resolvedCleanup"
        }
        Remove-Item -LiteralPath $resolvedCleanup -Recurse -Force
    }
}
