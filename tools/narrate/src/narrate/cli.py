"""Command-line entry point."""

from __future__ import annotations

import argparse
import sys
import traceback
import webbrowser
from pathlib import Path

from narrate import NarrateError, __version__
from narrate.build import BuildOptions, build, default_out
from narrate.draft import PACE_TARGET, draft
from narrate.manifest import Manifest
from narrate.review import ReviewApp, serve
from narrate.script import load_script


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
    return parser


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


def _run(args: argparse.Namespace) -> int:
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
