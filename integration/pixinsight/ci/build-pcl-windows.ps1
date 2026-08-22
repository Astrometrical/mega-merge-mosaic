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
if (-not $sha)  { throw 'PCL_SHA not found in pcl-pin.env' }
if (-not $repo) { throw 'PCL_REPO_URL not found in pcl-pin.env' }

if (-not $Work) { $Work = Join-Path $env:RUNNER_TEMP ("pcl-" + $sha.Substring(0,8)) }
New-Item -ItemType Directory -Force -Path $Work, "$Out/lib", "$Out/include" | Out-Null

# Six vendored 3rdparty static libs the PCL modules also link against; built
# the same way PCL's own driver does (see below). Names match the *-pxi.lib
# produced by each src/3rdparty/<lib>/windows/vc17/<lib>.vcxproj.
$thirdparty = @('cminpack','lcms','lz4','RFC6234','zlib','zstd')

Push-Location $Work
try {
  if (-not (Test-Path .git)) { git init -q; git remote add origin $repo }
  git fetch -q --depth 1 origin $sha
  if ($LASTEXITCODE -ne 0) { throw "git fetch failed ($LASTEXITCODE)" }
  git checkout -q FETCH_HEAD
  if ($LASTEXITCODE -ne 0) { throw "git checkout failed ($LASTEXITCODE)" }
  $head = (git rev-parse HEAD).Trim()
  if ($head -ne $sha) { throw "PCL SHA mismatch: got $head want $sha" }

  # Soft version check (SHA is the hard gate) -- mirrors build-pcl-macos.sh.
  $verCpp = Join-Path $Work 'src/pcl/Version.cpp'
  if (Test-Path $verCpp) {
    $vtxt = Get-Content $verCpp -Raw
    foreach ($v in @(@('Major',$pin['PCL_VER_MAJOR']), @('Minor',$pin['PCL_VER_MINOR']), @('Release',$pin['PCL_VER_RELEASE']))) {
      $fn = $v[0]; $want = $v[1]
      if ($want -and ($vtxt -notmatch "int\s+Version::$fn\s*\(\s*\)\s*\{?\s*return\s+$want\s*;")) {
        Write-Warning "Version.cpp $fn != $want (SHA is the hard gate; continuing)"
      }
    }
  }

  # HARD gate on the API version compiled into the module (mirrors build-pcl.sh):
  # PCL_API_Version becomes the module's declared API requirement, and cores
  # older than it refuse to load the module.
  $apiWant = $pin['PCL_API_VERSION_HEX']
  if (-not $apiWant) { throw 'PCL_API_VERSION_HEX not found in pcl-pin.env' }
  $apiHdr = Get-Content 'include/pcl/api/APIInterface.h' -Raw
  if ($apiHdr -notmatch "(?m)^#define\s+PCL_API_Version\s+0x$apiWant\s*$") {
    $got = ($apiHdr | Select-String 'define PCL_API_Version.*').Matches.Value
    throw "PCL_API_Version at this pin != 0x$apiWant (got: $got). Raising it drops older cores; update PCL_API_VERSION_HEX in pcl-pin.env only deliberately."
  }

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
  if ($LASTEXITCODE -ne 0) { throw "msbuild (PCL) failed ($LASTEXITCODE)" }

  $lib = Get-ChildItem $Work -Recurse -Filter 'PCL-pxi.lib' | Select-Object -First 1
  if (-not $lib) { throw 'PCL-pxi.lib not produced' }
  Copy-Item $lib.FullName "$Out/lib/PCL-pxi.lib" -Force

  # Core PCL-pxi.lib alone is not link-complete: the modules (ColorManagement,
  # PixelMath, XISF, ...) also link six vendored 3rdparty static libs (lcms/lz4/
  # zstd/zlib/RFC6234/cminpack) pulled in transitively through PCL. Build them
  # via PCL's own driver, which cd's into each src\3rdparty\<lib>\windows\vc17
  # and runs msbuild <lib>.vcxproj (Release|x64). Those vcxproj write their
  # *-pxi.lib into $(PCLLIBDIR64) (already exported above).
  $driver = Join-Path $Work 'src\3rdparty\windows\make-3rdparty.bat'
  if (-not (Test-Path $driver)) { throw "3rdparty driver not found: $driver" }
  cmd /c "`"$driver`""
  # The driver has no per-lib error check (its exit code is only the last
  # msbuild's), so the hard gate is asserting every archive exists below.
  foreach ($name in $thirdparty) {
    $a = Get-ChildItem $Work -Recurse -Filter "$name-pxi.lib" | Select-Object -First 1
    if (-not $a) { throw "$name-pxi.lib not produced" }
    Copy-Item $a.FullName "$Out/lib/$name-pxi.lib" -Force
  }

  Copy-Item "$Work/include/*" "$Out/include/" -Recurse -Force
} finally {
  Pop-Location
}

# Assert the full 7-archive closure landed in $Out/lib before declaring success.
foreach ($name in @('PCL') + $thirdparty) {
  if (-not (Test-Path "$Out/lib/$name-pxi.lib")) { throw "missing $name-pxi.lib in $Out/lib" }
}
Write-Host "PCL + 3rdparty built: $((Get-ChildItem "$Out/lib" -Filter '*-pxi.lib').Name -join ', '); headers in $Out/include"
