"""Command-line entry point."""

from __future__ import annotations

import argparse
import sys
import traceback
from pathlib import Path

from narrate import NarrateError, __version__
from narrate.build import BuildOptions, build, default_out
from narrate.draft import PACE_TARGET, draft
from narrate.manifest import Manifest
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
    """Build the top-level parser with the `build` and `draft` subcommands."""
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
