#!/usr/bin/env bash
# Build libPCL-pxi.a from the pinned open-source PCL on macOS (clang),
# out-of-tree, no PixInsight. Mirrors build-pcl.sh. Usage:
#   build-pcl-macos.sh --out <prefix-dir> [--work <clone-dir>] [--arch arm64|x64]
# arm64 (default) serves PixInsight 1.9.4+ native cores; x64 serves Intel
# Macs and every 1.9.0-1.9.3 macOS core (pre-1.9.4 PixInsight is x64-only on
# macOS, running under Rosetta on Apple Silicon).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$HERE/pcl-pin.env"

OUT="" ; WORK="" ; ARCH="arm64"
while [ $# -gt 0 ]; do
  case "$1" in
    --out)  OUT="$2"; shift 2 ;;
    --work) WORK="$2"; shift 2 ;;
    --arch) ARCH="$2"; shift 2 ;;
    --help) echo "usage: build-pcl-macos.sh --out <dir> [--work <dir>] [--arch arm64|x64]"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUT" ] || { echo "--out is required" >&2; exit 2; }
case "$ARCH" in arm64|x64) ;; *) echo "--arch must be arm64 or x64" >&2; exit 2 ;; esac
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$OUT/lib" "$OUT/include" "$WORK"

cd "$WORK"
if [ ! -d .git ]; then
  git init -q
  git remote add origin "$PCL_REPO_URL"
fi
git fetch -q --depth 1 origin "$PCL_SHA"
git checkout -q FETCH_HEAD
HEAD_SHA="$(git rev-parse HEAD)"
[ "$HEAD_SHA" = "$PCL_SHA" ] || { echo "PCL SHA mismatch: got $HEAD_SHA want $PCL_SHA" >&2; exit 1; }

# Soft version check (SHA is the hard gate) — same accessor grep as build-pcl.sh.
VER_CPP="src/pcl/Version.cpp"
if [ -f "$VER_CPP" ]; then
  check_ver_fn() { grep -A2 "^int Version::$1()" "$VER_CPP" | grep -qE "return[[:space:]]+$2[[:space:]]*;"; }
  check_ver_fn Major   "$PCL_VER_MAJOR"   || echo "warning: Version.cpp major != $PCL_VER_MAJOR" >&2
  check_ver_fn Minor   "$PCL_VER_MINOR"   || echo "warning: Version.cpp minor != $PCL_VER_MINOR" >&2
  check_ver_fn Release "$PCL_VER_RELEASE" || echo "warning: Version.cpp release != $PCL_VER_RELEASE" >&2
fi

# HARD gate on the API version compiled into the module (mirrors build-pcl.sh).
API_HDR="include/pcl/api/APIInterface.h"
grep -qE "^#define[[:space:]]+PCL_API_Version[[:space:]]+0x${PCL_API_VERSION_HEX}[[:space:]]*$" "$API_HDR" || {
  echo "ERROR: PCL_API_Version at this pin != 0x${PCL_API_VERSION_HEX}:" >&2
  grep 'define PCL_API_Version' "$API_HDR" >&2 || echo "  (no PCL_API_Version found in $API_HDR)" >&2
  echo "Raising it drops older cores; update PCL_API_VERSION_HEX in pcl-pin.env only deliberately." >&2
  exit 1
}

# Pre-1.9.4 PCL trees ship x64-only macOS makefiles (upstream added arm64
# variants in PCL 2.10.3). The x64->arm64 delta in upstream's generated
# makefiles is purely mechanical — verified by reproducing all seven of PCL
# 2.10.4's real arm64 makefiles byte-identically (mod timestamp) from their
# x64 siblings: '-arch x86_64' -> '-arch arm64', drop '-msse4.2', and
# 'x64' -> 'arm64' in paths. Generate any missing arm64 makefile from the
# pin's own x64 one, so source lists always match the pinned tree exactly.
# (x64 builds always use the tree's own makefile-x64 — present at every pin.)
THIRDPARTY_LIBS=(cminpack lcms lz4 RFC6234 zlib zstd)
gen_arm64_makefile() {
  local dir="$1"
  [ -f "$dir/makefile-arm64" ] && return 0
  [ -f "$dir/makefile-x64" ] || { echo "expected $dir/makefile-x64 in PCL tree" >&2; exit 1; }
  sed -e 's/-arch x86_64/-arch arm64/g' -e 's/ -msse4\.2//g' -e 's/x64/arm64/g' \
    "$dir/makefile-x64" > "$dir/makefile-arm64"
  mkdir -p "$dir/arm64/Release"
}
if [ "$ARCH" = arm64 ]; then
  gen_arm64_makefile src/pcl/macosx/g++
  for lib in "${THIRDPARTY_LIBS[@]}"; do
    gen_arm64_makefile "src/3rdparty/$lib/macosx/g++"
  done
fi
MK="src/pcl/macosx/g++/makefile-$ARCH"
[ -f "$MK" ] || { echo "expected $MK in PCL tree" >&2; exit 1; }

# The upstream macosx makefiles hardcode an Xcode isysroot; retarget it to the
# runner's installed SDK for robustness.
SDK="$(xcrun --show-sdk-path)"
# Replace any hardcoded '-isysroot <path>' with the discovered SDK path.
/usr/bin/sed -i '' -E "s#-isysroot [^[:space:]]+#-isysroot ${SDK//#/\\#}#g" "$MK"

export PCLDIR="$WORK"
export PCLSRCDIR="$WORK/src"
export PCLINCDIR="$WORK/include"
export PCLLIBDIR64="$WORK/lib/macosx/$ARCH"
export PCLBINDIR64="$WORK/bin"
mkdir -p "$PCLLIBDIR64" "$PCLBINDIR64"
( cd src/pcl/macosx/g++ && make -f "makefile-$ARCH" -j"$(sysctl -n hw.ncpu)" )

LIB="$(find "$WORK/src/pcl" -name libPCL-pxi.a -print -quit)"
[ -n "$LIB" ] || { echo "libPCL-pxi.a not produced" >&2; exit 1; }
cp -f "$LIB" "$OUT/lib/libPCL-pxi.a"

# Core libPCL-pxi.a alone is not link-complete: PixInsight's own modules
# (ColorManagement, PixelMath, XISF, ...) are built against six vendored
# 3rdparty static libs too (cminpack, lcms, lz4, RFC6234, zlib, zstd) --
# without them the module link is missing lcms (cmsOpenProfileFromFile, ...)
# and other symbols pulled in transitively through PCL. Build them the same
# way PCL's own driver does: src/3rdparty/macosx/make-3rdparty-arm64.sh cd's
# into each src/3rdparty/<lib>/macosx/g++ and runs `make -f makefile-arm64`.
# Each of those makefiles (confirmed by reading them at the pinned SHA) has
# an OBJ_DIR under its own source tree AND a "post-build" step that
# `cp -p`s the finished lib*-pxi.a into $PCLLIBDIR64 -- same pattern as the
# core makefile above -- so exporting PCLLIBDIR64 (already done) is enough
# for every archive to land in one place; we still `find` across the WORK
# tree per lib (mirroring how libPCL-pxi.a itself is located above) rather
# than assuming the post-build copy always fires.
# These makefiles hardcode the same Xcode isysroot as the core one; retarget
# all of them to the runner's installed SDK.
for mk in "$WORK"/src/3rdparty/*/macosx/g++/"makefile-$ARCH"; do
  [ -f "$mk" ] || continue
  /usr/bin/sed -i '' -E "s#-isysroot [^[:space:]]+#-isysroot ${SDK//#/\\#}#g" "$mk"
done

# Per-lib makes, arch-uniform. Upstream's make-3rdparty-*.sh drivers do exactly
# this (cd into each lib's macosx/g++ and make), but the driver's name/presence
# varies by pin (pre-1.9.4: unsuffixed x64-only make-3rdparty.sh; 2.10.3+:
# per-arch variants), so drive the same makes ourselves.
for lib in "${THIRDPARTY_LIBS[@]}"; do
  MK3="src/3rdparty/$lib/macosx/g++/makefile-$ARCH"
  [ -f "$MK3" ] || { echo "expected $MK3 in PCL tree" >&2; exit 1; }
  ( cd "$(dirname "$MK3")" && make -f "makefile-$ARCH" -j"$(sysctl -n hw.ncpu)" )
done

for lib in "${THIRDPARTY_LIBS[@]}"; do
  A="$(find "$WORK/src/3rdparty" -name "lib${lib}-pxi.a" -print -quit)"
  [ -n "$A" ] || { echo "lib${lib}-pxi.a not produced" >&2; exit 1; }
  cp -f "$A" "$OUT/lib/lib${lib}-pxi.a"
done

# Assert the full 7-archive closure landed in $OUT/lib before declaring success.
for name in PCL "${THIRDPARTY_LIBS[@]}"; do
  [ -f "$OUT/lib/lib${name}-pxi.a" ] || { echo "missing lib${name}-pxi.a in $OUT/lib" >&2; exit 1; }
done

cp -a "$WORK/include/." "$OUT/include/"
echo "PCL + 3rdparty built: $(ls "$OUT/lib" | tr '\n' ' '), headers in $OUT/include"
