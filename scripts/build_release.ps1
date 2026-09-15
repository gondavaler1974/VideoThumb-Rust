$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# ------------------------------------------------------------
# Locate the FFmpeg SDK
#
# Search order:
# 1. FFMPEG_ROOT environment variable
# 2. Local .cargo/config.toml (optional, gitignored override)
# 3. This project's own ffmpeg-sdk directory
# 4. If no SDK is found, download it into this project's
#    own ffmpeg-sdk directory
#
# The project is fully self-contained and does not depend
# on any other VideoThumb repository or installation.
# ------------------------------------------------------------

function Test-FfmpegRoot {
	param(
		[string]$Path
	)

	if (-not $Path) {
		return $false
	}

	return Test-Path (
		Join-Path $Path 'include\libavcodec\avcodec.h'
	)
}

function Find-ProjectFfmpegRoot {
	$sdkDir = Join-Path $root 'ffmpeg-sdk'

	if (-not (Test-Path $sdkDir)) {
		return $null
	}

	return Get-ChildItem $sdkDir -Directory |
	Where-Object {
		Test-FfmpegRoot $_.FullName
	} |
	Select-Object -First 1
}

$ffroot = $null

# ------------------------------------------------------------
# 1. FFMPEG_ROOT environment variable
# ------------------------------------------------------------

if (Test-FfmpegRoot $env:FFMPEG_ROOT) {
	$ffroot = Get-Item $env:FFMPEG_ROOT
}

# ------------------------------------------------------------
# 2. Local .cargo/config.toml
#
# Example:
#
# [env]
# FFMPEG_ROOT = "C:/path/to/ffmpeg-sdk"
#
# This file is intentionally excluded from version control.
# ------------------------------------------------------------

if (-not $ffroot) {
	$cargoConfig = Join-Path $root '.cargo\config.toml'

	if (Test-Path $cargoConfig) {
		$match = Select-String `
			-Path $cargoConfig `
			-Pattern '^\s*FFMPEG_ROOT\s*=\s*"([^"]+)"\s*$' |
		Select-Object -First 1

		if ($match) {
			$configuredRoot = $match.Matches[0].Groups[1].Value

			if (Test-FfmpegRoot $configuredRoot) {
				$ffroot = Get-Item $configuredRoot
			}
		}
	}
}

# ------------------------------------------------------------
# 3. This project's own ffmpeg-sdk directory
# ------------------------------------------------------------

if (-not $ffroot) {
	$ffroot = Find-ProjectFfmpegRoot
}

# ------------------------------------------------------------
# 4. Download the FFmpeg SDK if none was found
# ------------------------------------------------------------

if (-not $ffroot) {
	Write-Host ""
	Write-Host "FFmpeg SDK not found." -ForegroundColor Yellow
	Write-Host "Downloading FFmpeg SDK into this project..." -ForegroundColor Yellow
	Write-Host ""

	& (Join-Path $PSScriptRoot 'get_ffmpeg_sdk.ps1')

	$ffroot = Find-ProjectFfmpegRoot
}

if (-not $ffroot) {
	throw 'FFmpeg SDK could not be located or downloaded.'
}

Write-Host ""
Write-Host "FFmpeg SDK:" -ForegroundColor Cyan
Write-Host $ffroot.FullName
Write-Host ""

# ------------------------------------------------------------
# Build the release version
# ------------------------------------------------------------

& cargo build --release

if ($LASTEXITCODE -ne 0) {
	throw "cargo build --release failed with exit code $LASTEXITCODE"
}

# ------------------------------------------------------------
# Recreate the dist directory
# ------------------------------------------------------------

$dist = Join-Path $root 'dist'

if (Test-Path $dist) {
	Remove-Item $dist -Recurse -Force
}

New-Item -ItemType Directory -Path $dist | Out-Null

# ------------------------------------------------------------
# Copy the Rust DLL as a Total Commander WLX64 plugin
# ------------------------------------------------------------

$dll = Join-Path $root 'target\release\videothumb_rs.dll'

if (-not (Test-Path $dll)) {
	throw "Rust DLL not found: $dll"
}

Copy-Item `
	$dll `
(Join-Path $dist 'VideoThumb.wlx64')

# ------------------------------------------------------------
# Copy VideoThumb.ini
# ------------------------------------------------------------

$ini = Join-Path $root 'VideoThumb.ini'

if (-not (Test-Path $ini)) {
	throw "VideoThumb.ini not found: $ini"
}

Copy-Item `
	$ini `
(Join-Path $dist 'VideoThumb.ini')

# ------------------------------------------------------------
# Copy documentation and license files
# ------------------------------------------------------------

$readme = Join-Path $root 'README.md'
$projectLicense = Join-Path $root 'LICENSE'
$thirdPartyLicenses = Join-Path $root 'THIRD-PARTY-LICENSES.txt'
$ffmpegLicense = Join-Path $ffroot.FullName 'LICENSE.txt'

if (-not (Test-Path $readme)) {
	throw "README.md not found: $readme"
}

if (-not (Test-Path $projectLicense)) {
	throw "Project license not found: $projectLicense"
}

if (-not (Test-Path $thirdPartyLicenses)) {
	throw "Third-party license file not found: $thirdPartyLicenses"
}

if (-not (Test-Path $ffmpegLicense)) {
	throw "FFmpeg license not found: $ffmpegLicense"
}

Copy-Item `
	$readme `
(Join-Path $dist 'README.md')

Copy-Item `
	$projectLicense `
(Join-Path $dist 'VideoThumb-LICENSE.txt')

Copy-Item `
	$thirdPartyLicenses `
(Join-Path $dist 'THIRD-PARTY-LICENSES.txt')

Copy-Item `
	$ffmpegLicense `
(Join-Path $dist 'FFmpeg-LICENSE.txt')

# ------------------------------------------------------------
# Copy the required FFmpeg runtime DLLs
# ------------------------------------------------------------

$ffmpegBin = Join-Path $ffroot.FullName 'bin'

if (-not (Test-Path $ffmpegBin)) {
	throw "FFmpeg bin directory not found: $ffmpegBin"
}

'avcodec', 'avformat', 'avutil', 'swscale', 'swresample' |
ForEach-Object {

	$pattern = "$($_)-*.dll"

	$files = Get-ChildItem `
		$ffmpegBin `
		-Filter $pattern `
		-File

	if (-not $files) {
		throw "Required FFmpeg DLL not found: $pattern"
	}

	$files | ForEach-Object {
		Copy-Item `
			$_.FullName `
			$dist
	}
}

# ------------------------------------------------------------
# Done
# ------------------------------------------------------------

Write-Host ""
Write-Host "Release ready:" -ForegroundColor Green
Write-Host $dist
Write-Host ""

Get-ChildItem $dist |
Select-Object Name, Length