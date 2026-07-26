---
name: Bug report
about: Report something that is broken or produces wrong output
title: ""
labels: bug
assignees: ""
---

**mmm version**
The version string from the Astrometrical banner `mmm` prints on every run.

**OS**
Linux / Windows / macOS (and version).

**Input type**
Registered full-canvas XISF (MosaicByCoordinates output) or raw plate-solved
XISF panels? OSC/RGB or mono?

**Command run**
The exact `mmm` command line (analyze / report / blend and flags).

**Expected vs actual**
What you expected to happen, and what actually happened. Paste any error output.

**Diagnostics**
Big test files can't be attached to an issue. What helps instead:

- an excerpt of `mmm report` output for the session, and/or
- a small `--roi` crop that reproduces the problem, and/or
- the seam-map PNG (`mmm report --seam-png seam_map.png`).
