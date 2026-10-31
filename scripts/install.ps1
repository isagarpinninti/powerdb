# Install PowerDB on Windows. Usage: .\install.ps1 -Repo owner/name [-Version v0.1.0]
param([Parameter(Mandatory)][string]$Repo, [string]$Version = "latest", [string]$Dest = "$env:LOCALAPPDATA\PowerDB")
$ErrorActionPreference = "Stop"
$t = "x86_64-pc-windows-msvc"
$base = if ($Version -eq "latest") { "https://github.com/$Repo/releases/latest/download" } else { "https://github.com/$Repo/releases/download/$Version" }
$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("powerdb-" + [guid]::NewGuid()))
try {
  Invoke-WebRequest "$base/powerdb-$t.zip" -OutFile "$tmp\pkg.zip"
  Invoke-WebRequest "$base/SHA256SUMS" -OutFile "$tmp\SHA256SUMS"
  $want = ((Select-String -Path "$tmp\SHA256SUMS" -Pattern "powerdb-$t.zip").Line -split '\s+')[0]
  $got = (Get-FileHash "$tmp\pkg.zip" -Algorithm SHA256).Hash.ToLower()
  if (-not $want -or $want -ne $got) { throw "checksum mismatch; refusing to install" }
  New-Item -ItemType Directory -Force $Dest | Out-Null
  Expand-Archive "$tmp\pkg.zip" -DestinationPath $Dest -Force
  & "$Dest\powerdb.exe" --version
  Write-Host "Installed to $Dest. Add it to PATH, then run: powerdb init"
} finally { Remove-Item -Recurse -Force $tmp }
