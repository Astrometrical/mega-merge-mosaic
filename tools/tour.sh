#!/usr/bin/env bash
# Record the NGC 7000 tour: narration, headless recording, then mux.
#
# Usage: tools/tour.sh SESSION_DIR [OUT_DIR] [--draft] [--allow-unapproved]
#                      [--allow-overlap] [--fresh-copy] [--print-paths]
#
#   --draft             timing-only narration (no voice); records, does not mux
#   --allow-unapproved  pass through to `narrate mux`
#   --allow-overlap     pass through to `narrate mux`
#   --fresh-copy        delete session.lock from SESSION_DIR first (use on a
#                       copy of the template session, never the original)
#   --print-paths       print the resolved paths and exit (a dry run)
#
# Relative paths are taken from the caller's working directory. OUT_DIR
# defaults to crates/mmm-app/tours/out/ngc7000-video.
#
# Environment:
#   NARRATE_SCRIPT  the narration script (default
#                   crates/mmm-app/tours/ngc7000.narration.toml); its manifest
#                   is read from <script dir>/out/<stem>/narration.json, where
#                   narrate writes it
#   UV              the uv executable (default: uv on PATH, else the
#                   per-user Python 3.13 install)
#   FFMPEG          the ffmpeg executable (default: ffmpeg on PATH)
#   WGPU_BACKEND    the wgpu backend (default vulkan)
#
# A real voice needs the qwen extra: uv sync --extra qwen --directory tools/narrate
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

die() { echo "tour: $*" >&2; exit 2; }

draft=0 fresh=0 print_paths=0
mux_flags=()
pos=()
for a in "$@"; do
  case "$a" in
    --draft) draft=1 ;;
    --fresh-copy) fresh=1 ;;
    --print-paths) print_paths=1 ;;
    --allow-unapproved|--allow-overlap) mux_flags+=("$a") ;;
    -h|--help) sed -n '2,/^set -/p' "${BASH_SOURCE[0]}" | sed '$d'; exit 0 ;;
    -*) die "unknown option $a" ;;
    *) pos+=("$a") ;;
  esac
done
[ "${#pos[@]}" -ge 1 ] && [ "${#pos[@]}" -le 2 ] || die "usage: tools/tour.sh SESSION_DIR [OUT_DIR] [--draft] [--allow-unapproved] [--allow-overlap] [--fresh-copy] [--print-paths]"

# `$1` as an absolute path, resolved against the current working directory,
# in the form cargo and uv expect (C:/... with forward slashes under Git
# Bash). The last fallback needs `$1`'s parent to exist.
abspath() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -am "$1"
  elif realpath -m / >/dev/null 2>&1; then
    realpath -m "$1"
  elif [ -d "$1" ]; then
    (cd "$1" && pwd)
  else
    echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
  fi
}

# Resolve every user-supplied path while still in the caller's directory.
[ -d "${pos[0]}" ] || die "no such session dir: ${pos[0]}"
session="$(abspath "${pos[0]}")"
out_arg="${pos[1]:-$root/crates/mmm-app/tours/out/ngc7000-video}"
mkdir -p "$out_arg"
out="$(abspath "$out_arg")"

script_arg="${NARRATE_SCRIPT:-$root/crates/mmm-app/tours/ngc7000.narration.toml}"
[ -f "$script_arg" ] || die "no such narration script: $script_arg"
script="$(abspath "$script_arg")"
# narrate's default_out: <script dir>/out/<stem>, the stem being the file
# name up to its first dot.
script_name="$(basename "$script")"
manifest="$(dirname "$script")/out/${script_name%%.*}/narration.json"

if [ -n "${UV:-}" ]; then
  uv_bin="$UV"
else
  uv_bin="$(command -v uv || true)"
  [ -n "$uv_bin" ] || uv_bin="$HOME/AppData/Roaming/Python/Python313/Scripts/uv.exe"
fi
command -v "$uv_bin" >/dev/null 2>&1 || die "uv not found: $uv_bin (put uv on PATH or set UV)"
case "$uv_bin" in */*) uv_bin="$(abspath "$uv_bin")" ;; esac

if [ "$print_paths" -eq 1 ]; then
  printf 'session=%s\nout=%s\nscript=%s\nmanifest=%s\nuv=%s\n' \
    "$session" "$out" "$script" "$manifest" "$uv_bin"
  exit 0
fi

cd "$root"

if [ "$fresh" -eq 1 ]; then
  rm -f "$session/session.lock"
fi

narrate() { "$uv_bin" run --directory tools/narrate narrate "$@"; }

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
WGPU_BACKEND="${WGPU_BACKEND:-vulkan}" TOUR_SESSION="$session" TOUR_NARRATION="$manifest" \
  TOUR_OUT="$out" \
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
