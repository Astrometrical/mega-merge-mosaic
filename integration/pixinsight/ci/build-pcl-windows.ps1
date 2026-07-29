# Build PCL-pxi.lib from the pinned open-source PCL on Windows (msbuild/MSVC).
# The upstream commit omits src/pcl/windows/vc17/PCL.vcxproj, so we drop the
# repo-pinned, version-matched project into the fetched tree before building.
#   build-pcl-windows.ps1 -Out <prefix-dir> [-Work <clone-dir>]
[CmdletBinding()]
param(
  [Parameter(Mandatory=$true)][string]$Out,
  [string]$Work
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

# Read the pin (shell env file: KEY="value" lines).
$pin = @{}
Get-Content (Join-Path $here 'pcl-pin.env') | ForEach-Object {
  if ($_ -match '^\s*([A-Z_]+)="?([^"]*)"?\s*$') { $pin[$Matches[1]] = $Matches[2] }
}
$sha = $pin['PCL_SHA']; $repo = $pin['PCL_REPO_URL']
if (-not $sha) { throw 'PCL_SHA not found in pcl-pin.env' }

if (-not $Work) { $Work = Join-Path $env:RUNNER_TEMP ("pcl-" + $sha.Substring(0,8)) }
New-Item -ItemType Directory -Force -Path $Work, "$Out/lib", "$Out/include" | Out-Null

Push-Location $Work
if (-not (Test-Path .git)) { git init -q; git remote add origin $repo }
git fetch -q --depth 1 origin $sha
git checkout -q FETCH_HEAD
$head = (git rev-parse HEAD).Trim()
if ($head -ne $sha) { throw "PCL SHA mismatch: got $head want $sha" }

# Drop the pinned core project into the (upstream-absent) windows build dir.
New-Item -ItemType Directory -Force -Path 'src/pcl/windows/vc17' | Out-Null
Copy-Item (Join-Path $here 'win/PCL.vcxproj') 'src/pcl/windows/vc17/PCL.vcxproj' -Force

# Guard against source-list drift: every ClCompile in the pinned project must
# exist in the fetched tree, and no new src/pcl/*.cpp should be missing from it.
$proj = [xml](Get-Content 'src/pcl/windows/vc17/PCL.vcxproj')
$ns = @{ m = 'http://schemas.microsoft.com/developer/msbuild/2003' }
$listed = Select-Xml -Xml $proj -Namespace $ns -XPath '//m:ClCompile/@Include' |
  ForEach-Object { Split-Path $_.Node.Value -Leaf } | Sort-Object -Unique
$actual = Get-ChildItem 'src/pcl' -Filter *.cpp | ForEach-Object { $_.Name } | Sort-Object -Unique
$missing = $listed | Where-Object { $_ -notin $actual }
$extra   = $actual | Where-Object { $_ -notin $listed }
if ($missing) { throw "PCL.vcxproj lists sources absent from the pinned tree: $($missing -join ', ')" }
if ($extra)   { throw "pinned PCL tree has sources not in PCL.vcxproj (pin bump?): $($extra -join ', ')" }

$env:PCLDIR = $Work
$env:PCLSRCDIR = Join-Path $Work 'src'
$env:PCLINCDIR = Join-Path $Work 'include'
$env:PCLLIBDIR64 = Join-Path $Work 'lib\windows\x64'
$env:PCLBINDIR64 = Join-Path $Work 'bin'
New-Item -ItemType Directory -Force -Path $env:PCLLIBDIR64, $env:PCLBINDIR64 | Out-Null

msbuild 'src\pcl\windows\vc17\PCL.vcxproj' /t:Build /m `
  /p:Configuration=Release /p:Platform=x64
if ($LASTEXITCODE -ne 0) { throw "msbuild failed ($LASTEXITCODE)" }

$lib = Get-ChildItem $Work -Recurse -Filter 'PCL-pxi.lib' | Select-Object -First 1
if (-not $lib) { throw 'PCL-pxi.lib not produced' }
Copy-Item $lib.FullName "$Out/lib/PCL-pxi.lib" -Force
Copy-Item "$Work/include/*" "$Out/include/" -Recurse -Force
Pop-Location
Write-Host "PCL built: $Out/lib/PCL-pxi.lib, headers in $Out/include"
