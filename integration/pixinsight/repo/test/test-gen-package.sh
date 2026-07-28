#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$HERE/../gen-package.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Fixture staging tree: the install-root overlay.
mkdir -p "$TMP/stage/bin"
echo "fake-module" > "$TMP/stage/bin/mmm-pxm.so"
echo "fake-worker" > "$TMP/stage/bin/mmm-ipc-worker"

bash "$GEN" linux x64 "$TMP/stage" "$TMP/out"

TARBALL="$TMP/out/linux-x64-module.tar.gz"
META="$TMP/out/linux-x64.meta"
[ -f "$TARBALL" ] || { echo "FAIL: tarball missing"; exit 1; }
[ -f "$META" ]    || { echo "FAIL: meta missing"; exit 1; }

# Layout: overlay paths present.
tar tzf "$TARBALL" | grep -qx "bin/mmm-pxm.so"    || { echo "FAIL: bin/mmm-pxm.so not in tar"; exit 1; }
tar tzf "$TARBALL" | grep -qx "bin/mmm-ipc-worker" || { echo "FAIL: bin/mmm-ipc-worker not in tar"; exit 1; }

# sha1 matches sha1sum, lowercase hex.
WANT="$(sha1sum "$TARBALL" | cut -d' ' -f1)"
GOT="$(grep '^sha1=' "$META" | cut -d= -f2)"
[ "$GOT" = "$WANT" ] || { echo "FAIL: sha1 $GOT != $WANT"; exit 1; }
echo "$GOT" | grep -qE '^[0-9a-f]{40}$' || { echo "FAIL: sha1 not lowercase hex"; exit 1; }
grep -qx "fileName=linux-x64-module.tar.gz" "$META" || { echo "FAIL: fileName wrong"; exit 1; }
grep -qx "os=linux" "$META" || { echo "FAIL: os wrong"; exit 1; }
grep -qx "arch=x64" "$META" || { echo "FAIL: arch wrong"; exit 1; }
echo "PASS: gen-package"
