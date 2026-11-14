"""Command-line entry point."""

from __future__ import annotations

import argparse

from narrate import __version__


def build_parser() -> argparse.ArgumentParser:
    """Build the top-level parser; later tasks add subcommands."""
    parser = argparse.ArgumentParser(prog="narrate", description="Script to voice-over audio.")
    parser.add_argument("--version", action="version", version=f"narrate {__version__}")
    parser.add_subparsers(dest="command", metavar="COMMAND")
    return parser


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
    return 0
