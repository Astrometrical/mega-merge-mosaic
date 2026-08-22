#!/usr/bin/env bash
# Build the PCL module (.dylib) + worker on macOS, stage the unsigned payload.
# Usage: build-module-macos.sh --pcl <prefix> --stage <dir> [--arch arm64|x64]
# The runner's native arch must match --arch (the CI matrix pairs arm64 with
# macos-latest and x64 with macos-15-intel); the worker is built natively and
# both binaries are arch-asserted below before staging.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

PCL="" ; STAGE="" ; ARCH="arm64"
while [ $# -gt 0 ]; do
  case "$1" in
    --pcl)   PCL="$2"; shift 2 ;;
    --stage) STAGE="$2"; shift 2 ;;
    --arch)  ARCH="$2"; shift 2 ;;
    --help)  echo "usage: build-module-macos.sh --pcl <prefix> --stage <dir> [--arch arm64|x64]"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$PCL" ] && [ -n "$STAGE" ] || { echo "--pcl and --stage are required" >&2; exit 2; }
case "$ARCH" in arm64|x64) ;; *) echo "--arch must be arm64 or x64" >&2; exit 2 ;; esac
# lipo reports the x64 slice as "x86_64".
LIPO_ARCH="$ARCH"; [ "$ARCH" = x64 ] && LIPO_ARCH="x86_64"

MODULE_DIR="$REPO_ROOT/integration/pixinsight/module"
make -C "$MODULE_DIR" clean

build_log="$(mktemp)"
trap 'rm -f "$build_log"' EXIT
make -C "$MODULE_DIR" PCLINCDIR="$PCL/include" PCLLIBDIR="$PCL/lib" \
  MMM_MACOS_ARCH="$ARCH" 2>&1 | tee "$build_log"

if grep -q 'warning:' "$build_log"; then
  echo "ERROR: warnings in module build" >&2
  grep 'warning:' "$build_log" >&2
  exit 1
fi

DYLIB="$MODULE_DIR/mmm-pxm.dylib"
[ -f "$DYLIB" ] || { echo "mmm-pxm.dylib not built" >&2; exit 1; }
# Host objects must be linked in: any undefined mmm:: symbol means they weren't.
if nm -u "$DYLIB" | grep -E ' _?_ZN3mmm' ; then
  echo "ERROR: undefined mmm:: symbols in mmm-pxm.dylib (host objects not linked)" >&2
  exit 1
fi
# A wrong-arch dylib would install fine and then silently fail to load in an
# arch-mismatched core; assert the slice matches the requested arch.
GOT="$(lipo -archs "$DYLIB")"
[ "$GOT" = "$LIPO_ARCH" ] || { echo "mmm-pxm.dylib arch '$GOT' != expected '$LIPO_ARCH'" >&2; exit 1; }

( cd "$REPO_ROOT" && cargo build --release -p mmm-ipc-worker )
WORKER="$REPO_ROOT/target/release/mmm-ipc-worker"
[ -f "$WORKER" ] || { echo "mmm-ipc-worker not built" >&2; exit 1; }
GOT="$(lipo -archs "$WORKER")"
[ "$GOT" = "$LIPO_ARCH" ] || { echo "mmm-ipc-worker arch '$GOT' != expected '$LIPO_ARCH' (runner arch must match --arch)" >&2; exit 1; }

mkdir -p "$STAGE/bin"
cp -f "$DYLIB" "$STAGE/bin/mmm-pxm.dylib"
cp -f "$WORKER" "$STAGE/bin/mmm-ipc-worker"

# Update tarballs overlay the PixInsight install root, so the documentation must
# land in the archive as doc/tools/MegaMergeMosaic/... — stage it alongside bin/.
DOC_SRC="$REPO_ROOT/integration/pixinsight/doc/tools/MegaMergeMosaic"
[ -f "$DOC_SRC/MegaMergeMosaic.html" ] || { echo "module documentation missing at $DOC_SRC" >&2; exit 1; }
mkdir -p "$STAGE/doc/tools/MegaMergeMosaic"
cp -Rf "$DOC_SRC/." "$STAGE/doc/tools/MegaMergeMosaic/"

echo "staged unsigned payload in $STAGE/bin: $(ls "$STAGE/bin")"
echo "staged documentation in $STAGE/doc/tools/MegaMergeMosaic: $(ls "$STAGE/doc/tools/MegaMergeMosaic")"
