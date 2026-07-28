#!/usr/bin/env bash
# Assemble one platform's update package (tar.gz overlay) + its .meta.
# Usage: gen-package.sh <os> <arch> <staging-dir> <out-dir>
set -euo pipefail
OS="${1:?os}"; ARCH="${2:?arch}"; STAGE="${3:?staging-dir}"; OUT="${4:?out-dir}"
[ -d "$STAGE" ] || { echo "staging dir not found: $STAGE" >&2; exit 2; }
mkdir -p "$OUT"
FILE="${OS}-${ARCH}-module.tar.gz"
TARBALL="$OUT/$FILE"

# Tar the staging tree's contents so archive paths are install-root-relative
# (e.g. bin/mmm-pxm.so), deterministically (sorted, no owner/time noise).
# --sort/--transform/--owner/--mtime are GNU-tar-only; the repo generator runs
# only on the ubuntu CI job, so this is safe.
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='UTC 2020-01-01' \
    --transform='s,^\./,,' -C "$STAGE" -czf "$TARBALL" .

SHA1="$(sha1sum "$TARBALL" | cut -d' ' -f1)"
{
  echo "fileName=$FILE"
  echo "sha1=$SHA1"
  echo "os=$OS"
  echo "arch=$ARCH"
} > "$OUT/${OS}-${ARCH}.meta"
echo "packaged $TARBALL sha1=$SHA1"
