#!/usr/bin/env bash
# Emit a schema-valid PixInsight updates.xri from one or more package .meta files.
# Usage: gen-updates-xri.sh <releaseDate YYYYMMDD> <versionRange> <title> <out> <meta...>
set -euo pipefail
DATE="${1:?releaseDate}"; VER="${2:?versionRange}"; TITLE="${3:?title}"; OUT="${4:?out}"
shift 4
[ $# -ge 1 ] || { echo "at least one .meta required" >&2; exit 2; }

os_to_xri() { case "$1" in macos) echo macosx ;; *) echo "$1" ;; esac; }

MID="${DATE}-mmm"
{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<xri version="1.0" xmlns="http://www.pixinsight.com/xri">'
  echo "   <description><p>${TITLE}.</p></description>"
  echo "   <metadata id=\"${MID}\" releaseDate=\"${DATE}\">"
  echo "      <title>${TITLE}</title>"
  echo "      <description><p>${TITLE}. MosaicMerge process for PixInsight.</p></description>"
  echo "   </metadata>"
  for META in "$@"; do
    # shellcheck disable=SC1090
    fileName=""; sha1=""; os=""; arch=""
    while IFS='=' read -r k v; do
      case "$k" in fileName) fileName="$v";; sha1) sha1="$v";; os) os="$v";; arch) arch="$v";; esac
    done < "$META"
    xos="$(os_to_xri "$os")"
    echo "   <platform os=\"${xos}\" arch=\"${arch}\" version=\"${VER}\">"
    echo "      <package fileName=\"${fileName}\" sha1=\"${sha1}\" type=\"module\" releaseDate=\"${DATE}\" metadata=\"${MID}\"/>"
    echo "   </platform>"
  done
  echo '</xri>'
} > "$OUT"
echo "wrote $OUT"
