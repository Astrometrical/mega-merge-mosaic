#!/usr/bin/env python3
"""Generate crates/mmm-core/tests/fixtures/sip_oracle.json with astropy.

Each case: a FITS header (as [name, value, comment] cards, values in FITS
text form), the image size, ~48 forward samples (i, j, ra, dec) from
astropy's all_pix2world (origin 1 = FITS pixel coordinates) and ~48 inverse
samples (ra, dec, i, j) from all_world2pix at tight tolerance. Cases cover
SIP order 2 and 3, with and without AP/BP, bottom-up (no ROWORDER) and
TOP-DOWN. Run from the repo root; needs astropy >= 5 and numpy.
"""
import json, warnings
import numpy as np
from astropy.io import fits
from astropy.wcs import WCS

warnings.simplefilter("ignore")
OUT = "crates/mmm-core/tests/fixtures/sip_oracle.json"
rng = np.random.default_rng(20260926)


def fit_inverse(a, b, order, crpix, w, h):
    """Least-squares AP/BP of the same order over a grid of (U, V)."""
    us = np.linspace(-crpix[0], w - crpix[0], 25)
    vs = np.linspace(-crpix[1], h - crpix[1], 25)
    U0, V0 = np.meshgrid(us, vs)
    u, v = U0.ravel(), V0.ravel()
    f = sum(a[p][q] * u**p * v**q for p in range(order + 1) for q in range(order + 1 - p))
    g = sum(b[p][q] * u**p * v**q for p in range(order + 1) for q in range(order + 1 - p))
    U, V = u + f, v + g
    terms = [(p, q) for p in range(order + 1) for q in range(order + 1 - p)]
    M = np.stack([U**p * V**q for p, q in terms], axis=1)
    ap = np.linalg.lstsq(M, u - U, rcond=None)[0]
    bp = np.linalg.lstsq(M, v - V, rcond=None)[0]
    return terms, ap, bp


def make_case(name, order, with_inverse, top_down, w, h, seed):
    r = np.random.default_rng(seed)
    hdr = fits.Header()
    hdr["CTYPE1"], hdr["CTYPE2"] = "RA---TAN-SIP", "DEC--TAN-SIP"
    hdr["CRVAL1"], hdr["CRVAL2"] = 83.8 + r.uniform(-1, 1), -5.4 + r.uniform(-1, 1)
    hdr["CRPIX1"], hdr["CRPIX2"] = w / 2 + r.uniform(-30, 30), h / 2 + r.uniform(-30, 30)
    scale = 4.4e-4
    th = np.deg2rad(r.uniform(-20, 20))
    hdr["CD1_1"], hdr["CD1_2"] = -scale * np.cos(th), scale * np.sin(th)
    hdr["CD2_1"], hdr["CD2_2"] = scale * np.sin(th), scale * np.cos(th)
    hdr["EQUINOX"] = 2000.0
    if top_down:
        hdr["ROWORDER"] = "TOP-DOWN"
    a = np.zeros((order + 1, order + 1))
    b = np.zeros((order + 1, order + 1))
    # Term magnitude is calibrated so that mag * R**(p+q) (R = half the
    # longer canvas side) is ~constant across canvas sizes: a few px at
    # p+q=2, up to ~16 px at p+q=3. Without the R-scaling, a fixed mag
    # (independent of canvas size) blows the order-3 term up to hundreds
    # of pixels on the 4880x3235 case (u, v ~ +-2440 cubed), which fails
    # SipSolution::validate's 0.05 deg corner-consistency check.
    half_extent = max(w, h) / 2.0
    for p in range(order + 1):
        for q in range(order + 1 - p):
            if p + q >= 2:
                mag = 10.0 ** (-(2 * (p + q) + 1.5)) * (800.0 / half_extent) ** (p + q)
                a[p][q] = r.uniform(-1, 1) * mag
                b[p][q] = r.uniform(-1, 1) * mag
    hdr["A_ORDER"], hdr["B_ORDER"] = order, order
    for p in range(order + 1):
        for q in range(order + 1 - p):
            if a[p][q] != 0:
                hdr[f"A_{p}_{q}"] = float(a[p][q])
            if b[p][q] != 0:
                hdr[f"B_{p}_{q}"] = float(b[p][q])
    if with_inverse:
        terms, ap, bp = fit_inverse(a, b, order, (hdr["CRPIX1"], hdr["CRPIX2"]), w, h)
        hdr["AP_ORDER"], hdr["BP_ORDER"] = order, order
        for (p, q), x, y in zip(terms, ap, bp):
            hdr[f"AP_{p}_{q}"], hdr[f"BP_{p}_{q}"] = float(x), float(y)
    wcs = WCS(hdr)

    # Sanity: astropy must actually be applying the SIP distortion, not
    # silently falling back to the plain linear TAN map. Compare the
    # corners against the pure-linear WCS (CTYPE stripped of -SIP, no A/B
    # cards) and require a > 1e-6 deg difference somewhere.
    lin_hdr = hdr.copy()
    lin_hdr["CTYPE1"], lin_hdr["CTYPE2"] = "RA---TAN", "DEC--TAN"
    for key in list(lin_hdr.keys()):
        if key[:2] in ("A_", "B_") or key[:3] in ("AP_", "BP_"):
            del lin_hdr[key]
    lin_wcs = WCS(lin_hdr)
    ci = np.array([1.0, w, 1.0, w])
    cj = np.array([1.0, 1.0, h, h])
    ra_sip, dec_sip = wcs.all_pix2world(ci, cj, 1)
    ra_lin, dec_lin = lin_wcs.all_pix2world(ci, cj, 1)
    dev = np.hypot((ra_sip - ra_lin) * np.cos(np.deg2rad(dec_sip)), dec_sip - dec_lin)
    assert dev.max() > 1e-6, f"{name}: SIP corner deviation {dev.max():.2e} deg is too small \
to prove astropy applied the distortion"

    i = np.concatenate([[1, w, 1, w, hdr["CRPIX1"]], rng.uniform(1, w, 43)])
    j = np.concatenate([[1, 1, h, h, hdr["CRPIX2"]], rng.uniform(1, h, 43)])
    ra, dec = wcs.all_pix2world(i, j, 1)
    # Jitter (ra, dec) to get a genuinely different inverse target, but keep
    # the target pixel inside the solved domain: the four fixed corner
    # points above sit exactly on the domain boundary, and the full jitter
    # (~0.01 deg =~ 20-25 px at this plate scale) can push their inverse
    # target tens of px past the edge, into the grid's border-extrapolation
    # region where WcsModel::sky_to_pixel's Catmull-Rom grid is expected to
    # be much less accurate than in the interior (see Grid2D::node). Shrink
    # the jitter per-point until the inverse lands back in [1, w] x [1, h].
    base_dra = rng.uniform(-0.01, 0.01, ra.size)
    base_ddec = rng.uniform(-0.01, 0.01, dec.size)
    ra2, dec2, i2, j2 = ra.copy(), dec.copy(), i.copy(), j.copy()
    for k in range(ra.size):
        for scale in (1.0, 0.5, 0.25, 0.1, 0.05, 0.02, 0.0):
            cra, cdec = ra[k] + base_dra[k] * scale, dec[k] + base_ddec[k] * scale
            ci2, cj2 = wcs.all_world2pix(
                np.array([cra]), np.array([cdec]), 1, tolerance=1e-10, maxiter=100
            )
            if 1.0 <= ci2[0] <= w and 1.0 <= cj2[0] <= h:
                ra2[k], dec2[k], i2[k], j2[k] = cra, cdec, ci2[0], cj2[0]
                break
    cards = [[c.keyword, _card_value(c), c.comment] for c in hdr.cards]
    return {
        "name": name, "width": w, "height": h, "cards": cards,
        "forward": np.stack([i, j, ra, dec], axis=1).tolist(),
        "inverse": np.stack([ra2, dec2, i2, j2], axis=1).tolist(),
    }


def _card_value(c):
    """The value as it appears in the 80-char card text (FITS text form)."""
    img = c.image
    body = img[10:] if img[8:10] == "= " else ""
    if body.lstrip().startswith("'"):
        s = body.lstrip()
        k = 1
        while k < len(s):
            if s[k] == "'":
                if k + 1 < len(s) and s[k + 1] == "'":
                    k += 2
                    continue
                break
            k += 1
        return s[: k + 1].rstrip()
    return body.split("/")[0].strip()


cases = [
    make_case("order2_bottomup_noinv", 2, False, False, 1600, 1200, 1),
    make_case("order2_bottomup_inv", 2, True, False, 1600, 1200, 2),
    make_case("order3_bottomup_inv", 3, True, False, 4880, 3235, 3),
    make_case("order2_topdown_inv", 2, True, True, 1000, 700, 4),
]
with open(OUT, "w") as fh:
    json.dump(cases, fh, indent=1)
print(f"wrote {OUT}: {len(cases)} cases")
