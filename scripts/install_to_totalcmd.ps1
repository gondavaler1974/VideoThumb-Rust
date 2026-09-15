param(
	[string]$TotalCommanderDir = 'C:\totalcmdx64',
	[string]$PluginName = 'VideoThumb'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$dist = Join-Path $root 'dist'
$target = Join-Path $TotalCommanderDir "Plugins\wlx\$PluginName"
if (-not (Test-Path (Join-Path $dist 'VideoThumb.wlx64'))) {
	throw 'Build dist first with scripts/build_release.ps1.'
}
New-Item -ItemType Directory -Path $target -Force | Out-Null
Copy-Item (Join-Path $dist '*') $target -Force
Write-Host "Installed to $target" -ForegroundColor Green
