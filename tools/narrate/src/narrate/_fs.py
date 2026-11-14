"""Filesystem helpers that tolerate Windows' short-lived sharing violations."""

from __future__ import annotations

import hashlib
import os
import time
from pathlib import Path


def replace_with_retry(src: Path, dst: Path, attempts: int = 20, delay: float = 0.025) -> None:
    """`os.replace`, retried on PermissionError (Windows refuses while another handle is open)."""
    for i in range(attempts):
        try:
            os.replace(src, dst)
            return
        except PermissionError:
            if i == attempts - 1:
                raise
            time.sleep(delay)


def read_text_retry(path: Path, attempts: int = 20, delay: float = 0.025) -> str:
    """`Path.read_text` (UTF-8), retried on PermissionError."""
    for i in range(attempts):
        try:
            return Path(path).read_text(encoding="utf-8")
        except PermissionError:
            if i == attempts - 1:
                raise
            time.sleep(delay)
    raise AssertionError("unreachable")


def sha256_file(path: Path) -> str:
    """The sha256 of a file's bytes, as lowercase hex."""
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def ensure_ignored(folder: Path) -> None:
    """Create `folder` with a `.gitignore` of `*` (kept if present) so voice audio is never committed."""
    folder.mkdir(parents=True, exist_ok=True)
    ignore = folder / ".gitignore"
    if not ignore.exists():
        ignore.write_text("*\n", encoding="utf-8")
