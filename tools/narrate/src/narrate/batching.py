"""VRAM-safe batching of synthesis jobs."""

from __future__ import annotations

import hashlib
from collections.abc import Sequence
from dataclasses import dataclass

from narrate.profile import Reference

MAX_ROWS = 6
BUDGET_NUMERATOR = 56_000
BUDGET_REFERENCE_PAD = 13.4


@dataclass(frozen=True)
class Job:
    """One take of one line to synthesise."""

    key: str
    take: int
    spoken: str
    reference: Reference


def budget(reference_seconds: float) -> int:
    """Characters of `rows * longest` that fit in VRAM for a reference this long."""
    return int(BUDGET_NUMERATOR / (reference_seconds + BUDGET_REFERENCE_PAD))


def plan_batches(jobs: Sequence[Job]) -> list[list[Job]]:
    """Group jobs by reference and split each group into VRAM-safe batches."""
    groups: dict[str, list[Job]] = {}
    for job in jobs:
        groups.setdefault(job.reference.sha256, []).append(job)
    batches: list[list[Job]] = []
    for group in groups.values():
        group = sorted(group, key=lambda j: len(j.spoken))
        limit = budget(group[0].reference.seconds)
        current: list[Job] = []
        for job in group:
            rows = len(current)
            if current and not (rows < MAX_ROWS and (rows + 1) * len(job.spoken) <= limit):
                batches.append(current)
                current = []
            current.append(job)
        batches.append(current)
    return batches


def batch_seed(job: Job) -> int:
    """A deterministic seed that differs per line and take."""
    return int(hashlib.sha256(f"{job.key}:{job.take}".encode()).hexdigest()[:8], 16)
