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
    -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
    -*) die "unknown option $a" ;;
    *) pos+=("$a") ;;
  esac
done
[ "${#pos[@]}" -ge 1 ] && [ "${#pos[@]}" -le 2 ] || die "usage: tools/tour.sh SESSION_DIR [OUT_DIR] [--draft] [--allow-unapproved] [--allow-overlap] [--fresh-copy]"

[ -d "${pos[0]}" ] || die "no such session dir: ${pos[0]}"
session="$(cd "${pos[0]}" && pwd)"
out_arg="${pos[1]:-crates/mmm-app/tours/out/ngc7000-video}"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"

uv_bin="$(command -v uv || true)"
if [ -z "$uv_bin" ]; then
  uv_bin="$HOME/AppData/Roaming/Python/Python313/Scripts/uv.exe"
  [ -x "$uv_bin" ] || die "uv not found on PATH or at $uv_bin"
fi
narrate() { "$uv_bin" run --directory tools/narrate narrate "$@"; }

script="$root/crates/mmm-app/tours/ngc7000.narration.toml"
manifest="$root/crates/mmm-app/tours/out/ngc7000/narration.json"

# 1. Narration.
is_draft=$draft
if [ "$draft" -eq 0 ]; then
  rc=0
  narrate build "$script" --allow-flagged || rc=$?
  if [ "$rc" -eq 2 ]; then
    echo "tour: narrate build failed (exit 2): falling back to a draft." >&2
    echo "tour: for a real voice run: uv sync --extra qwen --directory tools/narrate" >&2
    is_draft=1
  elif [ "$rc" -ne 0 ]; then
    echo "tour: narrate build failed (exit $rc)" >&2
    exit "$rc"
  fi
fi
if [ "$is_draft" -eq 1 ]; then
  echo "tour: using a draft narration (timing only, no audio)"
  narrate draft "$script"
fi
[ -f "$manifest" ] || die "narration manifest not found: $manifest"

# 2. Record.
if [ "$fresh" -eq 1 ]; then
  rm -f "$session/session.lock"
fi
if [ -z "${FFMPEG:-}" ] && ! command -v ffmpeg >/dev/null 2>&1; then
  echo "tour: warning: ffmpeg is not on PATH and FFMPEG is unset" >&2
fi
WGPU_BACKEND=vulkan TOUR_SESSION="$session" TOUR_NARRATION="$manifest" TOUR_OUT="$out" \
  cargo test -p mmm-app --config 'profile.dev.package.mmm-core.opt-level=3' \
  ngc7000 -- --ignored --nocapture

# 3. Mux.
if [ "$is_draft" -eq 1 ]; then
  echo "tour: draft narration, so no mux. Silent video and captions are in $out"
  exit 0
fi
narrate mux --timeline "$out/timeline.json" -o "$out/tour.mp4" ${mux_flags[@]+"${mux_flags[@]}"}
echo "tour: video written to $out/tour.mp4"
