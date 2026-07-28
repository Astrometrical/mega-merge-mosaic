#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SP="$HERE/../sign-and-publish.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/linux/bin" "$TMP/windows/bin" "$TMP/macos/bin"
: > "$TMP/linux/bin/mmm-pxm.so"
: > "$TMP/windows/bin/mmm-pxm.dll"
: > "$TMP/macos/bin/mmm-pxm.dylib"
: > "$TMP/updates.xri"

# (a) No CPD identity -> dormant, exit 0.
out="$(unset MMM_CPD_XSSK; bash "$SP" --stage-root "$TMP" --xri "$TMP/updates.xri")"
echo "$out" | grep -q "DORMANT" || { echo "FAIL: not dormant without CPD key"; exit 1; }

# (b) Dry-run with a fake key -> prints sign commands for all three binaries + xri, executes nothing.
out="$(MMM_CPD_XSSK=/fake.xssk MMM_CPD_XSSK_PASSWORD=x bash "$SP" --dry-run \
        --stage-root "$TMP" --xri "$TMP/updates.xri")"
for b in mmm-pxm.so mmm-pxm.dll mmm-pxm.dylib; do
  echo "$out" | grep -q -- "--sign-module-file=.*$b" || { echo "FAIL: no sign cmd for $b"; exit 1; }
done
echo "$out" | grep -qi "codesign\|--sign-xml\|updates.xri" || { echo "FAIL: no xri sign cmd"; exit 1; }
# No signatures were actually produced.
[ -z "$(find "$TMP" -name '*.xsgn')" ] || { echo "FAIL: dry-run produced a signature"; exit 1; }
echo "PASS: sign-and-publish"
