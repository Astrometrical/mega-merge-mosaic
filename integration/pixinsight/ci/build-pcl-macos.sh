#!/usr/bin/env bash
# Build libPCL-pxi.a from the pinned open-source PCL on macOS (clang, arm64),
# out-of-tree, no PixInsight. Mirrors build-pcl.sh. Usage:
#   build-pcl-macos.sh --out <prefix-dir> [--work <clone-dir>]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$HERE/pcl-pin.env"

OUT="" ; WORK=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out)  OUT="$2"; shift 2 ;;
    --work) WORK="$2"; shift 2 ;;
    --help) echo "usage: build-pcl-macos.sh --out <dir> [--work <dir>]"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OUT" ] || { echo "--out is required" >&2; exit 2; }
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

# The upstream macosx makefile hardcodes an Xcode isysroot; retarget it to the
# runner's installed SDK for robustness. arm64 is the macos-latest arch.
MK="src/pcl/macosx/g++/makefile-arm64"
[ -f "$MK" ] || { echo "expected $MK in PCL tree" >&2; exit 1; }
SDK="$(xcrun --show-sdk-path)"
# Replace any hardcoded '-isysroot <path>' with the discovered SDK path.
/usr/bin/sed -i '' -E "s#-isysroot [^[:space:]]+#-isysroot ${SDK//#/\\#}#g" "$MK"

export PCLDIR="$WORK"
export PCLSRCDIR="$WORK/src"
export PCLINCDIR="$WORK/include"
export PCLLIBDIR64="$WORK/lib/macosx/x64"
export PCLBINDIR64="$WORK/bin"
mkdir -p "$PCLLIBDIR64" "$PCLBINDIR64"
( cd src/pcl/macosx/g++ && make -f makefile-arm64 -j"$(sysctl -n hw.ncpu)" )

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
THIRDPARTY_LIBS=(cminpack lcms lz4 RFC6234 zlib zstd)

# These makefiles hardcode the same Xcode isysroot as the core one; retarget
# all of them to the runner's installed SDK.
for mk in "$WORK"/src/3rdparty/*/macosx/g++/makefile-arm64; do
  [ -f "$mk" ] || continue
  /usr/bin/sed -i '' -E "s#-isysroot [^[:space:]]+#-isysroot ${SDK//#/\\#}#g" "$mk"
done

DRIVER="src/3rdparty/macosx/make-3rdparty-arm64.sh"
if [ -f "$DRIVER" ]; then
  bash "$DRIVER"
else
  echo "warning: $DRIVER not found; building 3rdparty libs individually" >&2
  for lib in "${THIRDPARTY_LIBS[@]}"; do
    MK3="src/3rdparty/$lib/macosx/g++/makefile-arm64"
    [ -f "$MK3" ] || { echo "expected $MK3 in PCL tree" >&2; exit 1; }
    ( cd "$(dirname "$MK3")" && make -f makefile-arm64 -j"$(sysctl -n hw.ncpu)" )
  done
fi

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
