#!/usr/bin/env bash
# RELOCATING (2026-07-30): moving to the private "Astrometrical" website repo.
# Kept here as the tested starting point. See
# docs/superpowers/specs/2026-07-30-distribution-relocation-and-repo-trust-findings.md
#
# Findings that change this script when re-homed (do NOT edit it in place here):
#   - A LOCAL signing identity is accepted for the maintainer's own machines, so
#     signing is NOT gated on CPD; drop the MMM_CPD_XSSK dormant gate for a single
#     key input (MMM_XSSK / MMM_XSSK_PASSWORD) defaulting to the local key.
#   - `--sign-xml-file` is confirmed real (the "caveat" is retired).
#   - ORDERING BUG: sign modules FIRST, then package (so .xsgn is inside the
#     tar.gz and the updates.xri sha1 matches). This script currently signs a
#     stage tree whose tarball was already built upstream.
#
# DORMANT repository signing + publish driver (historical framing; see above).
#
# Module signing requires the PixInsight binary (there is no standalone signer)
# AND a globally trusted CPD identity. Until a CPD .xssk is provided via
# MMM_CPD_XSSK, this script does nothing but report that it is dormant. One
# PixInsight install signs all platforms' module binaries (confirmed: signing is
# a hash-and-sign over file bytes), so this drives every mmm-pxm.* + the .xri in
# one pass on a single signer host.
#
# Usage: sign-and-publish.sh [--dry-run] --stage-root <dir> --xri <file> [--out <dir>]
# Env:   MMM_CPD_XSSK, MMM_CPD_XSSK_PASSWORD  (the CPD identity; unset => dormant)
#        PIXINSIGHT (path to the PixInsight binary; default: PixInsight)
set -euo pipefail

DRY=0; STAGE_ROOT=""; XRI=""; OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --stage-root) STAGE_ROOT="$2"; shift 2 ;;
    --xri) XRI="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$STAGE_ROOT" ] && [ -n "$XRI" ] || { echo "--stage-root and --xri are required" >&2; exit 2; }

if [ -z "${MMM_CPD_XSSK:-}" ]; then
  echo "DORMANT: repository signing/publishing is gated on a CPD identity."
  echo "         Set MMM_CPD_XSSK (and MMM_CPD_XSSK_PASSWORD) once the Pleiades"
  echo "         Certified-Developer certificate is in hand. Nothing signed/published."
  exit 0
fi

PIXINSIGHT="${PIXINSIGHT:-PixInsight}"
run() { if [ "$DRY" -eq 1 ]; then echo "[dry-run] $*"; else "$@"; fi; }

# 1) Sign every module binary (any platform) from this one host.
find "$STAGE_ROOT" -type f \( -name 'mmm-pxm.so' -o -name 'mmm-pxm.dll' -o -name 'mmm-pxm.dylib' \) \
| while read -r BIN; do
  run "$PIXINSIGHT" --automation-mode -n \
    "--sign-module-file=$BIN" \
    "--xssk-file=$MMM_CPD_XSSK" "--xssk-password=${MMM_CPD_XSSK_PASSWORD:-}" \
    --force-exit
done

# 2) Sign the repository index (CodeSign / --sign-xml-file).
run "$PIXINSIGHT" --automation-mode -n \
  "--sign-xml-file=$XRI" \
  "--xssk-file=$MMM_CPD_XSSK" "--xssk-password=${MMM_CPD_XSSK_PASSWORD:-}" \
  --force-exit

# 3) Publish (GitHub Pages). Left as an explicit manual/documented step until the
#    hosting URL is confirmed; see repo/README.md.
if [ -n "$OUT" ]; then
  run cp -r "$STAGE_ROOT" "$XRI" "$OUT/"
fi
if [ "$DRY" -eq 1 ]; then echo "signing complete (dry-run)"; else echo "signing complete"; fi
