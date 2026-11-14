#!/usr/bin/env bash
# Record the NGC 7000 tour: narration, headless recording, then mux.
#
# Usage: tools/tour.sh SESSION_DIR [OUT_DIR] [--draft] [--allow-unapproved]
#                      [--allow-overlap] [--fresh-copy]
#
#   --draft             timing-only narration (no voice); records, does not mux
#   --allow-unapproved  pass through to `narrate mux`
#   --allow-overlap     pass through to `narrate mux`
#   --fresh-copy        delete session.lock from SESSION_DIR first (use on a
#                       copy of the template session, never the original)
#
# A real voice needs the qwen extra: uv sync --extra qwen --directory tools/narrate
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

die() { echo "tour: $*" >&2; exit 2; }

draft=0 fresh=0
mux_flags=()
pos=()
for a in "$@"; do
  case "$a" in
    --draft) draft=1 ;;
    --fresh-copy) fresh=1 ;;
    --allow-unapproved|--allow-overlap) mux_flags+=("$a") ;;
    -h|--help) sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed '$d'; exit 0 ;;
    -*) die "unknown option $a" ;;
    *) pos+=("$a") ;;
  esac
done
[ "${#pos[@]}" -ge 1 ] && [ "${#pos[@]}" -le 2 ] || die "usage: tools/tour.sh SESSION_DIR [OUT_DIR] [--draft] [--allow-unapproved] [--allow-overlap] [--fresh-copy]"

# An absolute path in the form cargo and uv expect (Windows form with forward
# slashes under Git Bash; plain pwd elsewhere).
abspath() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m "$1"
  else
    (cd "$1" && { pwd -W 2>/dev/null || pwd; })
  fi
}

[ -d "${pos[0]}" ] || die "no such session dir: ${pos[0]}"
session="$(abspath "${pos[0]}")"
out_arg="${pos[1]:-$root/crates/mmm-app/tours/out/ngc7000-video}"
mkdir -p "$out_arg"
out="$(abspath "$out_arg")"
if [ "$fresh" -eq 1 ]; then
  rm -f "$session/session.lock"
fi

uv_bin="$(command -v uv || true)"
if [ -z "$uv_bin" ]; then
  uv_bin="$HOME/AppData/Roaming/Python/Python313/Scripts/uv.exe"
  [ -x "$uv_bin" ] || die "uv not found on PATH or at $uv_bin"
fi
narrate() { "$uv_bin" run --directory tools/narrate narrate "$@"; }

script="${NARRATE_SCRIPT:-$(abspath "$root/crates/mmm-app/tours")/ngc7000.narration.toml}"
manifest="$(abspath "$root/crates/mmm-app/tours")/out/ngc7000/narration.json"
draft_notice="DRAFT: no narration audio; run \`uv sync --extra qwen\` (or set up the voice profile) and re-run to mux"

# 1. Narration. Fall back to a draft only when the voice isn't set up; any
# other build failure (script typo, bad lexicon, ...) is the user's to fix.
is_draft=$draft
if [ "$draft" -eq 0 ]; then
  errf="$(mktemp)"
  rc=0
  narrate build "$script" --allow-flagged 2> >(tee "$errf" >&2) || rc=$?
  wait  # let the tee finish so the capture is complete
  if [ "$rc" -eq 2 ] && grep -qE "needs the qwen extra|voice profile .* not found" "$errf"; then
    echo "tour: the voice isn't set up (see above): falling back to a draft." >&2
    is_draft=1
  elif [ "$rc" -ne 0 ]; then
    rm -f "$errf"
    echo "tour: narrate build failed (see above)" >&2
    exit "$rc"
  fi
  rm -f "$errf"
fi
if [ "$is_draft" -eq 1 ]; then
  echo "tour: using a draft narration (timing only, no audio)" >&2
  narrate draft "$script"
fi
[ -f "$manifest" ] || die "narration manifest not found: $manifest"

# 2. Record.
if [ -z "${FFMPEG:-}" ] && ! command -v ffmpeg >/dev/null 2>&1; then
  echo "tour: warning: ffmpeg is not on PATH and FFMPEG is unset" >&2
fi
WGPU_BACKEND=vulkan TOUR_SESSION="$session" TOUR_NARRATION="$manifest" TOUR_OUT="$out" \
  cargo test -p mmm-app --config 'profile.dev.package.mmm-core.opt-level=3' \
  ngc7000 -- --ignored --nocapture

# 3. Mux.
if [ "$is_draft" -eq 1 ]; then
  echo "tour: draft narration, so no mux. Silent video and captions are in $out"
  echo "tour: $draft_notice" >&2
  exit 0
fi
narrate mux --timeline "$out/timeline.json" -o "$out/tour.mp4" ${mux_flags[@]+"${mux_flags[@]}"}
echo "tour: video written to $out/tour.mp4"
