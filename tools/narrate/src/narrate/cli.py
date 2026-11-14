"""Command-line entry point."""

from __future__ import annotations

import argparse
import sys
import traceback
import webbrowser
from pathlib import Path

from narrate import NarrateError, __version__
from narrate.build import BuildOptions, build, default_out, ensure_ignored
from narrate.cache import cache_root
from narrate.draft import PACE_TARGET, draft
from narrate.manifest import Manifest
from narrate.mux import load_timeline, mux, script_cues
from narrate.profile import load_profile
from narrate.review import ReviewApp, serve
from narrate.script import load_script
from narrate.trylab import TryApp, candidates_for, check_carriers, prepare, workdir_for
from narrate.trylab import serve as try_serve


def _positive_float(text: str) -> float:
    try:
        value = float(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a number: {text!r}") from None
    if not 0 < value < float("inf"):
        raise argparse.ArgumentTypeError(f"must be a positive number: {text!r}")
    return value


def build_parser() -> argparse.ArgumentParser:
    """Build the top-level parser with the `build`, `draft` and `review` subcommands."""
    parser = argparse.ArgumentParser(prog="narrate", description="Script to voice-over audio.")
    parser.add_argument("--version", action="version", version=f"narrate {__version__}")
    sub = parser.add_subparsers(dest="command", metavar="COMMAND")
    b = sub.add_parser("build", help="generate, check and write the clips and manifest")
    b.add_argument("script", type=Path)
    b.add_argument("--out", type=Path, help="output directory (default: <script dir>/out/<stem>)")
    b.add_argument("--voices", type=Path, help="voice library directory")
    b.add_argument("--cache", type=Path, help="take cache directory")
    b.add_argument("--allow-flagged", action="store_true", help="exit 0 even if lines are flagged")
    d = sub.add_parser("draft", help="write a timing-only manifest without audio")
    d.add_argument("script", type=Path)
    d.add_argument("--out", type=Path, help="output directory (default: <script dir>/out/<stem>)")
    d.add_argument(
        "--pace", type=_positive_float, default=PACE_TARGET, help="syllables per second (default %(default)s)"
    )
    r = sub.add_parser("review", help="listen to new lines in the browser and approve or reject them")
    r.add_argument("script", type=Path)
    r.add_argument("--out", type=Path, help="output directory (default: <script dir>/out/<stem>)")
    r.add_argument("--voices", type=Path, help="voice library directory")
    r.add_argument("--cache", type=Path, help="take cache directory")
    r.add_argument("--approved", type=Path, help="approved-take store directory")
    r.add_argument("--port", type=int, default=0, help="port to listen on (default: any free port)")
    r.add_argument("--no-browser", action="store_true", help="do not open the page in a browser")
    r.add_argument("--no-build", action="store_true", help="review the existing build without building first")
    t = sub.add_parser("try", help="hear a word in several spellings and write the closest to a pronunciation list")
    t.add_argument("word")
    t.add_argument("--voice", required=True, help="voice profile name")
    t.add_argument("--spell", action="append", default=[], help="a respelling to try (repeatable)")
    t.add_argument("--in", dest="carriers", action="append", default=[], help="carrier sentence with {} (repeatable)")
    t.add_argument("--style", help="reference style (default: the voice's default)")
    t.add_argument("--to", type=Path, help="pronunciation list to write (default: the voice's first list)")
    t.add_argument("--port", type=int, default=0, help="port to listen on (default: any free port)")
    t.add_argument("--no-browser", action="store_true", help="do not open the page in a browser")
    m = sub.add_parser("mux", help="mix approved narration clips onto a silent video")
    m.add_argument("--timeline", type=Path, help="timeline file (video, manifest and cues)")
    m.add_argument("--video", type=Path, help="silent video (with --script)")
    m.add_argument("--script", type=Path, help="script whose lines carry `at` times (with --video)")
    m.add_argument("--manifest", type=Path, help="manifest (default: the script's out directory)")
    m.add_argument("-o", "--output", type=Path, required=True, help="output video")
    m.add_argument("--allow-overlap", action="store_true", help="mux even if clips overlap the next cue")
    m.add_argument("--allow-unapproved", action="store_true", help="mux lines that are not approved")
    return parser


def _run_mux(args: argparse.Namespace) -> int:
    if args.timeline is not None:
        if args.video or args.script or args.manifest:
            raise NarrateError("mux: --timeline cannot be combined with --video/--script/--manifest")
        video, manifest, cues = load_timeline(args.timeline)
    else:
        if args.video is None or args.script is None:
            raise NarrateError("mux: give --timeline, or both --video and --script")
        script = load_script(args.script)
        video, cues = args.video, script_cues(script)
        manifest = args.manifest or default_out(script) / "narration.json"
    mux(
        video,
        manifest,
        cues,
        args.output,
        allow_overlap=args.allow_overlap,
        allow_unapproved=args.allow_unapproved,
    )
    print(args.output)
    return 0


def _num(value, fmt: str) -> str:
    return "-" if value is None else format(value, fmt)


def _print_summary(manifest: Manifest, path: Path) -> None:
    for ln in manifest.lines:
        check = ln.check or {}
        take = check.get("take")
        row = (
            ln.id,
            ln.status,
            "-" if take is None else str(take),
            _num(check.get("cer"), ".3f"),
            _num(check.get("pace"), ".2f"),
            _num(ln.duration, ".2f"),
        )
        print("  ".join(row))
        if ln.status == "flagged":
            print(f"  heard: {check.get('heard')!r}  flags: {', '.join(ln.flags)}")
    print(path)


def _run_try(args: argparse.Namespace) -> int:
    candidates = candidates_for(args.word, args.spell)
    carriers = check_carriers(args.carriers)
    profile = load_profile(args.voice)
    to = args.to
    if to is None:
        if not profile.pronunciations:
            raise NarrateError(f"voice {args.voice!r} has no pronunciation list; pass --to LIST")
        to = profile.pronunciations[0]
    workdir = workdir_for(cache_root(None), args.word, candidates, carriers, args.voice, args.style)
    script = prepare(args.word, candidates, carriers, args.voice, args.style, workdir)
    out = workdir / "out"
    ensure_ignored(out)
    build(script, options=BuildOptions(out))
    try_serve(TryApp(script, args.word, candidates, to, out), args.port, None if args.no_browser else webbrowser.open)
    return 0


def _run(args: argparse.Namespace) -> int:
    if args.command == "try":
        return _run_try(args)
    if args.command == "mux":
        return _run_mux(args)
    script = load_script(args.script)
    out = args.out if args.out is not None else default_out(script)
    if args.command == "review":
        options = BuildOptions(out, args.voices, args.cache, args.approved)
        if not args.no_build:
            build(args.script, options=options)
        serve(ReviewApp(args.script, options), args.port, None if args.no_browser else webbrowser.open)
        return 0
    if args.command == "draft":
        manifest = draft(args.script, out=out, pace_target=args.pace)
    else:
        manifest = build(args.script, options=BuildOptions(out, args.voices, args.cache))
    _print_summary(manifest, out / "narration.json")
    return 1 if manifest.flagged() and not getattr(args, "allow_flagged", False) else 0


def main(argv: list[str] | None = None) -> int:
    """Run the CLI and return the process exit code."""
    parser = build_parser()
    try:
        args = parser.parse_args(argv)
    except SystemExit as exc:  # argparse exits for --version/--help/usage errors
        code = exc.code
        return code if isinstance(code, int) else (0 if code is None else 2)
    if args.command is None:
        parser.print_usage()
        return 2
    try:
        return _run(args)
    except NarrateError as exc:
        print(f"narrate: {exc}", file=sys.stderr)
        return 2
    except Exception:  # a bug, never "flagged"
        traceback.print_exc()
        print("narrate: internal error (traceback above)", file=sys.stderr)
        return 3
