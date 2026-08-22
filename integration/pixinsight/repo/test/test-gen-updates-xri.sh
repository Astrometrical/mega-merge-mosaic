#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GEN="$HERE/../gen-updates-xri.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

for p in "linux x64" "windows x64" "macos x64"; do
  set -- $p; os="$1"; arch="$2"
  printf 'fileName=%s-%s-module.tar.gz\nsha1=%040d\nos=%s\narch=%s\n' \
    "$os" "$arch" 1 "$os" "$arch" > "$TMP/$os-$arch.meta"
done

bash "$GEN" 20260728 "1.9.0:1.9.9" "MergeMosaic module" "$TMP/updates.xri" \
  "$TMP/linux-x64.meta" "$TMP/windows-x64.meta" "$TMP/macos-x64.meta"

XRI="$TMP/updates.xri"
[ -f "$XRI" ] || { echo "FAIL: updates.xri missing"; exit 1; }

# Well-formed XML (stdlib, no xmllint needed).
python3 - "$XRI" <<'PY'
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
assert root.tag.endswith('xri'), root.tag
xml = open(sys.argv[1]).read()
for os_attr in ('os="linux"','os="windows"','os="macosx"'):
    assert os_attr in xml, os_attr
assert xml.count('type="module"') == 3, "expected 3 module packages"
assert 'releaseDate="20260728"' in xml
assert 'arch="x64"' in xml
assert 'version="1.9.0:1.9.9"' in xml
print("xri structure OK")
PY
echo "PASS: gen-updates-xri"
