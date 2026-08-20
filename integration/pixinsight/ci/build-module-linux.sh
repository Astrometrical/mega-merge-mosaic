#!/usr/bin/env bash
# Build the PCL module + worker on Linux and stage the unsigned package payload.
# Usage: build-module-linux.sh --pcl <prefix> --stage <dir>
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

PCL="" ; STAGE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pcl)   PCL="$2"; shift 2 ;;
    --stage) STAGE="$2"; shift 2 ;;
    --help)  echo "usage: build-module-linux.sh --pcl <prefix> --stage <dir>"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$PCL" ] && [ -n "$STAGE" ] || { echo "--pcl and --stage are required" >&2; exit 2; }

MODULE_DIR="$REPO_ROOT/integration/pixinsight/module"
make -C "$MODULE_DIR" clean

build_log="$(mktemp)"
trap 'rm -f "$build_log"' EXIT
make -C "$MODULE_DIR" PCLINCDIR="$PCL/include" PCLLIBDIR="$PCL/lib" 2>&1 | tee "$build_log"

# The module build must be warning-free under -Wall (binding constraint, not convention).
if grep -q 'warning:' "$build_log"; then
  echo "ERROR: -Wall warnings in module build" >&2
  grep 'warning:' "$build_log" >&2
  exit 1
fi

SO="$MODULE_DIR/mmm-pxm.so"
[ -f "$SO" ] || { echo "mmm-pxm.so not built" >&2; exit 1; }
# The host objects must be linked in: any undefined mmm:: symbol means they weren't.
if nm -D -u "$SO" | grep -E '_ZN3mmm|mmm::' ; then
  echo "ERROR: undefined mmm:: symbols in mmm-pxm.so (host objects not linked)" >&2
  exit 1
fi

( cd "$REPO_ROOT" && cargo build --release -p mmm-ipc-worker )
WORKER="$REPO_ROOT/target/release/mmm-ipc-worker"
[ -f "$WORKER" ] || { echo "mmm-ipc-worker not built" >&2; exit 1; }

mkdir -p "$STAGE/bin"
cp -f "$SO" "$STAGE/bin/mmm-pxm.so"
cp -f "$WORKER" "$STAGE/bin/mmm-ipc-worker"

# Update tarballs overlay the PixInsight install root, so the documentation must
# land in the archive as doc/tools/MegaMergeMosaic/... — stage it alongside bin/.
DOC_SRC="$REPO_ROOT/integration/pixinsight/doc/tools/MegaMergeMosaic"
[ -f "$DOC_SRC/MegaMergeMosaic.html" ] || { echo "module documentation missing at $DOC_SRC" >&2; exit 1; }
mkdir -p "$STAGE/doc/tools/MegaMergeMosaic"
cp -Rf "$DOC_SRC/." "$STAGE/doc/tools/MegaMergeMosaic/"

echo "staged unsigned payload in $STAGE/bin: $(ls "$STAGE/bin")"
echo "staged documentation in $STAGE/doc/tools/MegaMergeMosaic: $(ls "$STAGE/doc/tools/MegaMergeMosaic")"
