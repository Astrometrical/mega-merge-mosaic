#!/usr/bin/env python3
"""Compare two mmm mosaic outputs of the same field (e.g. FITS-run vs XISF-run).

1. Star check: for each output, predict the pixel of bright stars from the
   output's WCS and find the brightest luminance pixel within a 25 px box
   (±12 px) of the prediction; print the offset. A saturated core (several
   pixels sharing the clipped maximum) is eroded until one more step would
   empty it and the centroid of the remaining core is used. theta1 Ori C is
   measured but NOT graded: its clipped plateau merges theta1 Ori A, B and D
   (all 5-16" north of C), which drags any peak estimate ~4 px north-west
   in either output; theta2 Ori A (isolated, 2.5' SE) is graded instead.
   Also printed: each graded star's peak in the test output mapped through
   both WCS into the reference output, minus the reference's own peak
   (the run-to-run astrometric difference, independent of catalogue error).
2. Photometric check: over the sky-footprint intersection, map a 64 px
   lattice of the REFERENCE (second) output's pixels through both WCS
   objects, bilinearly sample the first output there, and report on channel
   0 where both are nonzero: the RMS relative difference
   sqrt(mean((a-b)^2)) / sqrt(mean(b^2)) and the 99th-percentile |a-b|.

mmm writes its output TOP-DOWN with ROWORDER = 'TOP-DOWN' and WCS cards that
refer to the stored row index, so astropy's 0-based pixel coordinates index
the numpy data array directly (data[c, y, x]).

PASS iff all graded star offsets < 4 px in both outputs, no NaN in either output,
and the RMS relative difference < 5 %.

Usage: python3 scripts/compare_mosaics.py TEST.fits REFERENCE.fits
Requires: numpy, astropy.
"""
import sys
import warnings

import numpy as np
from astropy.io import fits
from astropy.wcs import WCS

warnings.simplefilter("ignore")  # mmm passes through non-standard XISF-derived cards

STARS = {  # name: (RA, Dec, graded)
    "theta1 Ori C": (83.81858, -5.38970, False),
    "theta2 Ori A": (83.84542, -5.41606, True),
    "iota Ori": (83.85826, -5.90990, True),
    # The task brief gave (83.78462, -4.83860), ~140 px west of any bright
    # star; 42 Ori (c Ori, HR 1887) is at 05h35m23.2s -04d50m18s (J2000).
    "42 Ori": (83.84650, -4.83836, True),
}
HALF_BOX = 12  # 25 px box
LATTICE = 64
MAX_STAR_OFFSET = 4.0
MAX_RMS_REL = 0.05


def load(path):
    with fits.open(path, memmap=True) as hdul:
        hdr = hdul[0].header.copy()
        data = np.asarray(hdul[0].data, dtype=np.float32)
    rowo = str(hdr.get("ROWORDER", "BOTTOM-UP")).strip().upper()
    assert rowo == "TOP-DOWN", f"{path}: expected ROWORDER TOP-DOWN, got {rowo}"
    return data, WCS(hdr, naxis=2)


def star_offsets(name, data, wcs):
    """Print each star's predicted vs measured pixel; return (worst graded offset, {star: peak})."""
    _, h, w = data.shape
    worst, peaks = 0.0, {}
    print(f"{name}: {w}x{h}")
    for star, (ra, dec, graded) in STARS.items():
        px, py = wcs.all_world2pix([[ra, dec]], 0)[0]
        cx, cy = int(round(px)), int(round(py))
        x0, x1 = max(cx - HALF_BOX, 0), min(cx + HALF_BOX + 1, w)
        y0, y1 = max(cy - HALF_BOX, 0), min(cy + HALF_BOX + 1, h)
        if x0 >= x1 or y0 >= y1:
            print(f"  {star:13s} predicted ({px:9.2f}, {py:9.2f}) OUTSIDE the image")
            if graded:
                worst = float("inf")
            continue
        lum = data[:, y0:y1, x0:x1].mean(axis=0)
        peak = lum.max()
        ys, xs = np.nonzero(plateau_core(lum == peak))
        bx, by = x0 + xs.mean(), y0 + ys.mean()
        peaks[star] = (bx, by)
        d = float(np.hypot(bx - px, by - py))
        if graded:
            worst = max(worst, d)
        print(f"  {star:13s} predicted ({px:9.2f}, {py:9.2f})  peak ({bx:8.1f}, {by:8.1f})"
              f"  lum {peak:.4f} (core {len(xs)} px)  offset dx {bx - px:+6.2f} dy {by - py:+6.2f}"
              f"  |d| {d:5.2f} px{'' if graded else '  (not graded)'}")
    return worst, peaks


def plateau_core(mask):
    """Erode a boolean plateau (4-neighbour) until the next step would empty it."""
    while True:
        m = np.pad(mask, 1)
        eroded = mask & m[:-2, 1:-1] & m[2:, 1:-1] & m[1:-1, :-2] & m[1:-1, 2:]
        if not eroded.any():
            return mask
        mask = eroded


def bilinear(img, x, y):
    """Sample img[y, x] bilinearly; returns (values, all-four-neighbours-nonzero mask)."""
    h, w = img.shape
    x0 = np.floor(x).astype(int)
    y0 = np.floor(y).astype(int)
    inside = (x0 >= 0) & (y0 >= 0) & (x0 + 1 < w) & (y0 + 1 < h)
    x0, y0, fx, fy = x0[inside], y0[inside], (x - np.floor(x))[inside], (y - np.floor(y))[inside]
    v00, v10 = img[y0, x0], img[y0, x0 + 1]
    v01, v11 = img[y0 + 1, x0], img[y0 + 1, x0 + 1]
    vals = np.zeros(x.shape, dtype=np.float64)
    ok = np.zeros(x.shape, dtype=bool)
    vals[inside] = (v00 * (1 - fx) * (1 - fy) + v10 * fx * (1 - fy) + v01 * (1 - fx) * fy + v11 * fx * fy)
    ok[inside] = (v00 != 0) & (v10 != 0) & (v01 != 0) & (v11 != 0)
    return vals, ok


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    test_path, ref_path = sys.argv[1], sys.argv[2]
    test, test_wcs = load(test_path)
    ref, ref_wcs = load(ref_path)

    nan_test, nan_ref = int(np.isnan(test).sum()), int(np.isnan(ref).sum())
    print(f"NaN count: test {nan_test}, reference {nan_ref}")

    worst_test, peaks_test = star_offsets(f"test      {test_path}", test, test_wcs)
    worst_ref, peaks_ref = star_offsets(f"reference {ref_path}", ref, ref_wcs)
    print("run-to-run: test peak mapped into the reference grid minus the reference peak")
    for star in STARS:
        if star in peaks_test and star in peaks_ref:
            world = test_wcs.all_pix2world([peaks_test[star]], 0)
            mx, my = ref_wcs.all_world2pix(world, 0)[0]
            rx, ry = peaks_ref[star]
            print(f"  {star:13s} dx {mx - rx:+6.2f} dy {my - ry:+6.2f}  |d| {np.hypot(mx - rx, my - ry):5.2f} px")

    _, h, w = ref.shape
    gy, gx = np.mgrid[LATTICE // 2:h:LATTICE, LATTICE // 2:w:LATTICE]
    gx, gy = gx.ravel().astype(float), gy.ravel().astype(float)
    world = ref_wcs.all_pix2world(np.column_stack([gx, gy]), 0)
    tp = test_wcs.all_world2pix(world, 0)
    a, ok = bilinear(test[0], tp[:, 0], tp[:, 1])
    b = ref[0][gy.astype(int), gx.astype(int)].astype(np.float64)
    both = ok & (b != 0)
    a, b = a[both], b[both]
    diff = a - b
    rms_rel = float(np.sqrt(np.mean(diff ** 2)) / np.sqrt(np.mean(b ** 2)))
    p99 = float(np.percentile(np.abs(diff), 99))
    med_ratio = float(np.median(a / b))
    print(f"lattice: {LATTICE} px, {len(gx)} points, {both.sum()} in the footprint intersection (both nonzero)")
    print(f"channel 0: reference mean {b.mean():.6g}, test mean {a.mean():.6g}, median ratio test/ref {med_ratio:.4f}")
    print(f"channel 0: RMS relative difference {rms_rel * 100:.3f} %  (RMS abs {np.sqrt(np.mean(diff ** 2)):.3g})")
    print(f"channel 0: 99th-percentile |difference| {p99:.4g}  (mean |difference| {np.mean(np.abs(diff)):.3g})")
    # Diagnostics (not graded): how concentrated the squared difference is.
    # A few lattice points landing on bright-star wings dominate the RMS
    # when the two runs' astrometry differs by a pixel or two there.
    order = np.argsort(-diff ** 2)
    top = max(1, len(diff) // 100)
    share = float(np.sum(diff[order[:top]] ** 2) / np.sum(diff ** 2))
    keep = np.ones(len(diff), dtype=bool)
    keep[order[:top]] = False
    rms_rel_trim = float(np.sqrt(np.mean(diff[keep] ** 2)) / np.sqrt(np.mean(b[keep] ** 2)))
    rel = np.abs(diff) / b
    print(f"diagnostic: worst 1 % of points ({top}) carry {share * 100:.1f} % of the squared difference;"
          f" RMS relative difference without them {rms_rel_trim * 100:.3f} %")
    print(f"diagnostic: per-point |a-b|/b median {np.median(rel) * 100:.2f} %, p90 {np.percentile(rel, 90) * 100:.2f} %,"
          f" p99 {np.percentile(rel, 99) * 100:.2f} %")
    for i in order[:5]:
        print(f"  largest: reference pixel ({gx[both][i]:.0f}, {gy[both][i]:.0f})  reference {b[i]:.4f}  test {a[i]:.4f}")

    passed = (worst_test < MAX_STAR_OFFSET and worst_ref < MAX_STAR_OFFSET and nan_test == 0 and nan_ref == 0
              and rms_rel < MAX_RMS_REL)
    print(f"{'PASS' if passed else 'FAIL'}: worst star offset test {worst_test:.2f} px, reference {worst_ref:.2f} px"
          f" (< {MAX_STAR_OFFSET}); NaN {nan_test}/{nan_ref}; RMS relative difference {rms_rel * 100:.3f} %"
          f" (< {MAX_RMS_REL * 100:.0f} %)")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
