#!/usr/bin/env bash
# Build the PCL module (.dylib) + worker on macOS, stage the unsigned payload.
# Usage: build-module-macos.sh --pcl <prefix> --stage <dir>
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"

PCL="" ; STAGE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pcl)   PCL="$2"; shift 2 ;;
    --stage) STAGE="$2"; shift 2 ;;
    --help)  echo "usage: build-module-macos.sh --pcl <prefix> --stage <dir>"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$PCL" ] && [ -n "$STAGE" ] || { echo "--pcl and --stage are required" >&2; exit 2; }

MODULE_DIR="$REPO_ROOT/integration/pixinsight/module"
make -C "$MODULE_DIR" clean

build_log="$(mktemp)"
trap 'rm -f "$build_log"' EXIT
make -C "$MODULE_DIR" PCLINCDIR="$PCL/include" PCLLIBDIR="$PCL/lib" 2>&1 | tee "$build_log"

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

( cd "$REPO_ROOT" && cargo build --release -p mmm-ipc-worker )
WORKER="$REPO_ROOT/target/release/mmm-ipc-worker"
[ -f "$WORKER" ] || { echo "mmm-ipc-worker not built" >&2; exit 1; }

mkdir -p "$STAGE/bin"
cp -f "$DYLIB" "$STAGE/bin/mmm-pxm.dylib"
cp -f "$WORKER" "$STAGE/bin/mmm-ipc-worker"
echo "staged unsigned payload in $STAGE/bin: $(ls "$STAGE/bin")"
