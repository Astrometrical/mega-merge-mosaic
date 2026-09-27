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
   0 where both are nonzero: the raw RMS relative difference
   sqrt(mean((a-b)^2)) / sqrt(mean(b^2)), the trimmed one (worst 1 % of
   points by |a-b| dropped), the median per-point |a-b|/b and the
   99th-percentile |a-b|. When the two outputs come from different
   astrometric solutions (e.g. nova's order-3 SIP vs PixInsight's spline),
   bright-star wings shifted 1-2 px dominate the raw RMS without indicating
   any pipeline defect, so only the trimmed RMS is graded.
3. Star field (not graded): median run-to-run offset of ~thousands of
   unsaturated stars — a constant offset would reveal a reflection or
   half-pixel error.

mmm writes its output TOP-DOWN with ROWORDER = 'TOP-DOWN' and WCS cards that
refer to the stored row index, so astropy's 0-based pixel coordinates index
the numpy data array directly (data[c, y, x]).

PASS iff all graded star offsets < 4 px in both outputs, no NaN in either
output, and the trimmed RMS relative difference < 5 %.

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


def centroid(img, x, y):
    """Background-subtracted 7x7 centroid around integer (x, y)."""
    s = img[y - 3:y + 4, x - 3:x + 4].astype(np.float64) - np.median(img[y - 8:y + 9, x - 8:x + 9])
    s[s < 0] = 0
    if s.sum() <= 0:
        return float(x), float(y)
    yy, xx = np.mgrid[-3:4, -3:4]
    return x + float((s * xx).sum() / s.sum()), y + float((s * yy).sum() / s.sum())


def star_field(test, test_wcs, ref, ref_wcs):
    """Not graded: run-to-run offset of many unsaturated stars.

    The brightest pixel of each 64 px block of the reference luminance (peak
    0.02-0.5, >= 0.01 above the local median, block fully covered) is
    centroided, mapped into the test output, re-centroided there and mapped
    back; prints the median offset vector and |offset| percentiles.
    """
    ref_l, test_l = ref.mean(axis=0), test.mean(axis=0)
    h, w = ref_l.shape
    th, tw = test_l.shape
    pts = []
    for by in range(8, h - 8 - LATTICE, LATTICE):
        for bx in range(8, w - 8 - LATTICE, LATTICE):
            blk = ref_l[by:by + LATTICE, bx:bx + LATTICE]
            if (blk == 0).any():
                continue
            y, x = np.unravel_index(int(np.argmax(blk)), blk.shape)
            x, y = x + bx, y + by
            peak = ref_l[y, x]
            if not 0.02 < peak < 0.5 or peak - np.median(ref_l[y - 8:y + 9, x - 8:x + 9]) < 0.01:
                continue
            pts.append(centroid(ref_l, x, y))
    if not pts:
        print("star field: no stars found")
        return
    pts = np.array(pts)
    tp = test_wcs.all_world2pix(ref_wcs.all_pix2world(pts, 0), 0)
    found, back = [], []
    for (rx, ry), (tx, ty) in zip(pts, tp):
        ix, iy = int(round(tx)), int(round(ty))
        if not (12 < ix < tw - 13 and 12 < iy < th - 13):
            continue
        sub = test_l[iy - 4:iy + 5, ix - 4:ix + 5]
        sy, sx = np.unravel_index(int(np.argmax(sub)), sub.shape)
        found.append((rx, ry))
        back.append(centroid(test_l, ix - 4 + sx, iy - 4 + sy))
    found, back = np.array(found), np.array(back)
    mapped = ref_wcs.all_world2pix(test_wcs.all_pix2world(back, 0), 0)
    d = mapped - found
    mag = np.hypot(d[:, 0], d[:, 1])
    med = np.median(d, axis=0)
    print(f"star field (not graded): {len(d)} stars, median offset dx {med[0]:+.2f} dy {med[1]:+.2f} px,"
          f" |offset| median {np.median(mag):.2f} p90 {np.percentile(mag, 90):.2f} px")


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
    raw_rms_rel = float(np.sqrt(np.mean(diff ** 2)) / np.sqrt(np.mean(b ** 2)))
    p99 = float(np.percentile(np.abs(diff), 99))
    med_ratio = float(np.median(a / b))
    order = np.argsort(-np.abs(diff))  # worst first
    trim = max(1, len(diff) // 100)
    keep = np.ones(len(diff), dtype=bool)
    keep[order[:trim]] = False
    trim_rms_rel = float(np.sqrt(np.mean(diff[keep] ** 2)) / np.sqrt(np.mean(b[keep] ** 2)))
    rel = np.abs(diff) / b
    share = np.cumsum(diff[order] ** 2) / np.sum(diff ** 2)
    n90 = int(np.searchsorted(share, 0.9) + 1)
    print(f"lattice: {LATTICE} px, {len(gx)} points, {both.sum()} in the footprint intersection (both nonzero)")
    print(f"channel 0: reference mean {b.mean():.6g}, test mean {a.mean():.6g}, median ratio test/ref {med_ratio:.4f}")
    print(f"channel 0: raw RMS relative difference {raw_rms_rel * 100:.3f} %  (not graded: dominated by"
          f" bright-star wings — {n90} of {len(diff)} points carry {share[n90 - 1] * 100:.1f} % of the squared"
          f" difference)")
    print(f"channel 0: trimmed RMS relative difference {trim_rms_rel * 100:.3f} %  (worst 1 % = {trim} points"
          f" by |a-b| dropped)")
    print(f"channel 0: per-point |a-b|/b median {np.median(rel) * 100:.2f} %, p90 {np.percentile(rel, 90) * 100:.2f} %;"
          f" 99th-percentile |a-b| {p99:.4g}")
    for i in order[:5]:
        print(f"  largest: reference pixel ({gx[both][i]:.0f}, {gy[both][i]:.0f})  reference {b[i]:.4f}  test {a[i]:.4f}")

    star_field(test, test_wcs, ref, ref_wcs)

    passed = (worst_test < MAX_STAR_OFFSET and worst_ref < MAX_STAR_OFFSET and nan_test == 0 and nan_ref == 0
              and trim_rms_rel < MAX_RMS_REL)
    print(f"{'PASS' if passed else 'FAIL'}: worst graded star offset test {worst_test:.2f} px, reference"
          f" {worst_ref:.2f} px (< {MAX_STAR_OFFSET}); NaN {nan_test}/{nan_ref}; trimmed RMS relative difference"
          f" {trim_rms_rel * 100:.3f} % (< {MAX_RMS_REL * 100:.0f} %)")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
