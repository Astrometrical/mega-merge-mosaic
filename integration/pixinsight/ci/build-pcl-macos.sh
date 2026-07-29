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
cp -a "$WORK/include/." "$OUT/include/"
echo "PCL built: $OUT/lib/libPCL-pxi.a, headers in $OUT/include"
