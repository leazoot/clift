<#
.SYNOPSIS
Installs a released clift binary on Windows.

    irm https://raw.githubusercontent.com/leazoot/clift/main/install.ps1 | iex

.DESCRIPTION
Finds the release, downloads the archive and the SHA256SUMS file published next
to it, refuses to continue unless the archive's digest matches, and only then
copies the binaries into place. Nothing is written outside a private temporary
directory until the digest has been checked. No administrator rights are used.

Parameters are read from the environment when the script is piped to iex, so
the one-liner can still be steered:

    $env:CLIFT_VERSION     = "0.1.0"              release to install (default: latest)
    $env:CLIFT_INSTALL_DIR = "D:\tools\clift"     default: %LOCALAPPDATA%\Programs\clift
    $env:CLIFT_WITH_RELAYD = "1"                  also install clift-relayd
    $env:CLIFT_NO_PATH     = "1"                  do not touch the user PATH
    $env:CLIFT_NO_SETUP    = "1"                  install only; do not start `clift setup`

Run as a file instead and the same things are parameters:

    .\install.ps1 -Version 0.1.0 -InstallDir D:\tools\clift -WithRelayd -NoPath -NoSetup

After installing, `clift setup` asks a few questions on the console (only when
there is one) so that the first paste works.
#>
[CmdletBinding()]
param(
    [string]$Version = $env:CLIFT_VERSION,
    [string]$InstallDir = $env:CLIFT_INSTALL_DIR,
    [switch]$WithRelayd = ($env:CLIFT_WITH_RELAYD -eq '1'),
    [switch]$NoPath = ($env:CLIFT_NO_PATH -eq '1'),
    [switch]$NoSetup = ($env:CLIFT_NO_SETUP -eq '1')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'leazoot/clift'
$DownloadBase = if ($env:CLIFT_DOWNLOAD_BASE) { $env:CLIFT_DOWNLOAD_BASE } else { "https://github.com/$Repo/releases" }
$Target = 'x86_64-pc-windows-msvc'

# Windows PowerShell 5.1 still defaults to older TLS; GitHub requires 1.2.
if ($PSVersionTable.PSVersion.Major -lt 6) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}

$onWindows = ($PSVersionTable.PSVersion.Major -lt 6) -or $IsWindows
if (-not $onWindows) {
    throw 'install.ps1 is the Windows installer; on macOS or Linux run install.sh'
}

if (-not $InstallDir) {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\clift'
}

switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { }
    'ARM64' { Write-Warning 'There is no native Windows-on-ARM build yet; installing the x86_64 build, which Windows 11 runs under emulation.' }
    default { throw "no prebuilt Windows binary for $($env:PROCESSOR_ARCHITECTURE); see https://github.com/$Repo/releases" }
}

if ($DownloadBase -notlike 'https://*') {
    Write-Warning 'CLIFT_DOWNLOAD_BASE is not https; the digest check still applies, but the SHA256SUMS file comes from the same place'
}

# The latest release is found by following the redirect behind
# /releases/latest and reading where it landed, which needs no token. The
# redirect is followed rather than capped: Windows PowerShell 5.1 does not
# honour `-MaximumRedirection 0` the way PowerShell 7 does (on a real Windows
# 10 it followed the redirect anyway and reported no Location header), while
# every version records the final address on the response it returns -- in
# two different places, so both are read. If that still gives nothing, the
# GitHub API is asked once; it is rate-limited for anonymous callers, so it is
# the fallback and not the first choice.
function Resolve-Latest {
    $url = "$DownloadBase/latest"
    $landed = $null
    $problem = $null
    try {
        $response = Invoke-WebRequest -Uri $url -UseBasicParsing -ErrorAction Stop
        $base = $response.BaseResponse
        if ($base.ResponseUri) {
            $landed = $base.ResponseUri.AbsoluteUri
        } elseif ($base.RequestMessage -and $base.RequestMessage.RequestUri) {
            $landed = $base.RequestMessage.RequestUri.AbsoluteUri
        }
    } catch {
        $problem = $_.Exception.Message
    }
    if ("$landed" -match '/tag/v([^/]+)$') { return $Matches[1] }

    if ($DownloadBase -eq "https://github.com/$Repo/releases") {
        try {
            $latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing -ErrorAction Stop
            if ("$($latest.tag_name)" -match '^v([^/]+)$') { return $Matches[1] }
        } catch {
            if (-not $problem) { $problem = $_.Exception.Message }
        }
    }

    $detail = "landed on '$landed'"
    if ($problem) { $detail = "$detail; $problem" }
    throw "could not work out the latest release from $url ($detail); set CLIFT_VERSION to install a specific release"
}

if (-not $Version) { $Version = Resolve-Latest }
$Version = $Version -replace '^v', ''

$Archive = "clift-$Version-$Target.zip"
$ReleaseUrl = "$DownloadBase/download/v$Version"

$WorkDir = Join-Path ([IO.Path]::GetTempPath()) ("clift-install-" + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $WorkDir | Out-Null

# Puts $Source at $Destination. Windows refuses to overwrite a program that is
# running, and the hotkey helper is one: it stays up between logins by design.
# A running program may still be renamed, so the old file is moved aside and
# the new one put in its place; the helper keeps running the old copy until it
# is restarted, which `clift setup` and `clift hotkey --install` both do.
# Returns whether that happened. The moved-aside copy is removed on the next
# run, once nothing uses it any more.
function Install-Binary([string]$Source, [string]$Destination) {
    $previous = "$Destination.previous"
    Remove-Item -Path $previous -Force -ErrorAction SilentlyContinue
    try {
        Copy-Item -Path $Source -Destination $Destination -Force -ErrorAction Stop
        return $false
    } catch [System.IO.IOException] {
        if (-not (Test-Path $Destination)) { throw }
        Move-Item -Path $Destination -Destination $previous -Force -ErrorAction Stop
        Copy-Item -Path $Source -Destination $Destination -Force -ErrorAction Stop
        return $true
    }
}

try {
    Write-Host "Downloading clift $Version for $Target..."
    $archivePath = Join-Path $WorkDir $Archive
    $sumsPath = Join-Path $WorkDir 'SHA256SUMS'
    Invoke-WebRequest -Uri "$ReleaseUrl/$Archive" -OutFile $archivePath -UseBasicParsing
    try {
        Invoke-WebRequest -Uri "$ReleaseUrl/SHA256SUMS" -OutFile $sumsPath -UseBasicParsing
    } catch {
        throw "could not download $ReleaseUrl/SHA256SUMS; refusing to install an unverified archive ($($_.Exception.Message))"
    }

    # A line is "<hex>  <name>" or, from a tool in binary mode, "<hex> *<name>".
    $expected = $null
    foreach ($line in Get-Content $sumsPath) {
        if ($line -match '^([0-9a-fA-F]{64})\s+\*?(.+?)\s*$' -and $Matches[2] -eq $Archive) {
            $expected = $Matches[1].ToLowerInvariant()
            break
        }
    }
    if (-not $expected) { throw "SHA256SUMS does not list $Archive; refusing to install" }

    $actual = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "digest mismatch for $Archive`n  expected: $expected`n  actual:   $actual`nNothing was installed."
    }

    Expand-Archive -Path $archivePath -DestinationPath $WorkDir -Force
    $unpacked = Join-Path $WorkDir "clift-$Version-$Target"
    if (-not (Test-Path (Join-Path $unpacked 'clift.exe'))) { throw 'archive did not contain clift.exe' }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $replacedRunning = Install-Binary (Join-Path $unpacked 'clift.exe') (Join-Path $InstallDir 'clift.exe')
    $installed = 'clift'
    if ($WithRelayd) {
        if (-not (Test-Path (Join-Path $unpacked 'clift-relayd.exe'))) { throw 'archive did not contain clift-relayd.exe' }
        $replacedRunning = (Install-Binary (Join-Path $unpacked 'clift-relayd.exe') (Join-Path $InstallDir 'clift-relayd.exe')) -or $replacedRunning
        $installed = 'clift and clift-relayd'
    }

    $reported = & (Join-Path $InstallDir 'clift.exe') --version 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "$InstallDir\clift.exe was installed but does not run on this system:`n  $reported"
    }

    Write-Host "Installed $installed $Version ($Target) to $InstallDir"
    Write-Host "  verified: sha256 $actual"
    if ($replacedRunning) {
        Write-Host "  the hotkey helper that is running still uses the previous version; clift setup or clift hotkey --install restarts it with this one"
    }

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onPath = ($userPath -split ';') -contains $InstallDir -or ($env:Path -split ';') -contains $InstallDir
    if (-not $onPath) {
        if ($NoPath) {
            Write-Host ''
            Write-Host "$InstallDir is not on your PATH (left alone because -NoPath was given)."
        } else {
            $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
            $env:Path = "$env:Path;$InstallDir"
            Write-Host "  added $InstallDir to your user PATH; new terminals will see it"
        }
    }

    Write-Host ''
    # The questions need a console. Redirected (CI, a captured run) nothing
    # waits: the command is named and the script ends.
    $console = -not ([Console]::IsInputRedirected -or [Console]::IsOutputRedirected -or [Console]::IsErrorRedirected)
    if ($NoSetup -or -not $console) {
        Write-Host 'Next: clift setup'
    } else {
        try {
            & (Join-Path $InstallDir 'clift.exe') setup
        } catch {
            Write-Host "Set up later with: clift setup ($($_.Exception.Message))"
        }
        if ($LASTEXITCODE -ne 0) { Write-Host 'Set up later with: clift setup' }
    }
} finally {
    Remove-Item -Path $WorkDir -Recurse -Force -ErrorAction SilentlyContinue
}
