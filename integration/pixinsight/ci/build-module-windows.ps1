# Build mmm-pxm.dll + mmm-ipc-worker.exe on Windows, stage the unsigned payload.
# Mirrors the gates of build-module-macos.sh: warning-free build, the module DLL
# is produced, the worker builds, and both land in <Stage>/bin.
#   build-module-windows.ps1 -Pcl <prefix> -Stage <dir>
# <Pcl> is the prefix from build-pcl-windows.ps1 (has lib/PCL-pxi.lib + the six
# lib/*-pxi.lib and include/). The module links fully with no PixInsight present
# (see module/CMakeLists.txt).
[CmdletBinding()]
param(
  [Parameter(Mandatory=$true)][string]$Pcl,
  [Parameter(Mandatory=$true)][string]$Stage
)
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$moduleDir = Join-Path $repo 'integration/pixinsight/module'
$build = Join-Path $moduleDir 'win-build'
$Pcl = (Resolve-Path $Pcl).Path

# Configure (x64 + toolset v143, both pinned to match the hard-pinned x64/v143
# PCL-pxi.lib so the module's ABI can't drift if the runner image advances) and
# build Release.
cmake -S $moduleDir -B $build -A x64 -T v143 -DPCL_PREFIX="$Pcl"
if ($LASTEXITCODE -ne 0) { throw "cmake configure failed ($LASTEXITCODE)" }

$buildLog = cmake --build $build --config Release 2>&1 | Tee-Object -Variable _bl
if ($LASTEXITCODE -ne 0) {
  $buildLog | ForEach-Object { $_ }
  throw "module build failed ($LASTEXITCODE)"
}

# Warning-free gate (MSVC warnings are "... : warning Cxxxx: ...").
$warnings = $buildLog | Select-String -Pattern 'warning C'
if ($warnings) {
  $warnings | ForEach-Object { Write-Host $_ }
  throw "ERROR: MSVC warnings in module build"
}

$dll = Get-ChildItem $build -Recurse -Filter 'mmm-pxm.dll' | Select-Object -First 1
if (-not $dll) { throw 'mmm-pxm.dll not built' }

Push-Location $repo
try {
  cargo build --release --target x86_64-pc-windows-msvc -p mmm-ipc-worker
  if ($LASTEXITCODE -ne 0) { throw "cargo build (worker) failed ($LASTEXITCODE)" }
} finally {
  Pop-Location
}
$worker = Join-Path $repo 'target/x86_64-pc-windows-msvc/release/mmm-ipc-worker.exe'
if (-not (Test-Path $worker)) { throw 'mmm-ipc-worker.exe not built' }

New-Item -ItemType Directory -Force -Path (Join-Path $Stage 'bin') | Out-Null
Copy-Item $dll.FullName (Join-Path $Stage 'bin/mmm-pxm.dll') -Force
Copy-Item $worker (Join-Path $Stage 'bin/mmm-ipc-worker.exe') -Force
Write-Host "staged unsigned payload in $Stage/bin: $((Get-ChildItem (Join-Path $Stage 'bin')).Name -join ', ')"
