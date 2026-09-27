#!/usr/bin/env python3
"""Convert raw Orion XISF panels to FITS solved by nova.astrometry.net.

For each input XISF: build a 16-bit mono luminance FITS (bottom-up, no
solution), upload it to nova (private, tweak_order 3, scale/position hints
from the XISF header), poll, download the wcs.fits header, and write an RGB
float32 bottom-up FITS of the ORIGINAL planes with nova's WCS + SIP cards.

Bottom-up means the stored rows are numpy flipud of the XISF's top-down
rows and there is NO ROWORDER card, so mmm's reader flips them back and
reflects the WCS — the path a real astrometry.net-solved FITS takes.

Re-runnable: panels whose OUT_DIR/PANEL-N.fits exists are skipped, and
submission/job ids are kept in OUT_DIR/nova_jobs.json so an interrupted run
resumes polling instead of re-uploading. A failed solve is retried once with
scale_err 50 and no position hint. A failed WCS download/write is retried on
the next poll, up to MAX_DOWNLOAD_TRIES per run, without stopping the other
panels; a rerun retries it again.

Usage: python3 scripts/nova_solve_panels.py OUT_DIR PANEL.xisf [...]
Requires: numpy, astropy, requests; the key in ~/.config/astrometry/apikey
(never printed or written anywhere by this script).
"""
import json
import os
import re
import sys
import time

import numpy as np
import requests
from astropy.io import fits

API = "https://nova.astrometry.net/api"
WCS_URL = "https://nova.astrometry.net/wcs_file/{}"
SKIP_CARDS = {"SIMPLE", "BITPIX", "EXTEND", "END", "COMMENT", "HISTORY", ""}
MAX_DOWNLOAD_TRIES = 5  # per panel per run, for the solved WCS download + FITS write
COPY_KEYWORDS = ("OBJECT", "EXPTIME", "INSTRUME", "TELESCOP", "FOCALLEN", "XPIXSZ", "YPIXSZ", "DATE-OBS", "FILTER")


def read_xisf(path):
    """Return (planes (C, H, W) float32 top-down, {FITS keyword: raw value})."""
    with open(path, "rb") as fh:
        head = fh.read(16)
        assert head[:8] == b"XISF0100", f"{path}: not a monolithic XISF"
        hlen = int.from_bytes(head[8:12], "little")
        xml = fh.read(hlen).decode("utf-8", "replace")
    # Parse the <Image> element's attributes independently of their order.
    image = re.search(r"<Image\s([^>]*)>", xml)
    assert image, f"{path}: no <Image> element"
    attrs = dict(re.findall(r'(\w+)="([^"]*)"', image.group(1)))
    w, h, ch = (int(x) for x in attrs["geometry"].split(":"))
    assert attrs.get("sampleFormat") == "Float32", f"{path}: sampleFormat {attrs.get('sampleFormat')}"
    loc = attrs["location"].split(":")
    assert loc[0] == "attachment", f"{path}: location {attrs['location']}"
    off, size = int(loc[1]), int(loc[2])
    assert size == w * h * ch * 4, f"{path}: attachment size {size} != {w}x{h}x{ch} f32"
    data = np.memmap(path, dtype="<f4", mode="r", offset=off, shape=(ch, h, w))
    kw = dict(re.findall(r'<FITSKeyword name="([^"]+)" value="([^"]*)"', xml))
    return np.array(data, dtype=np.float32), kw


def keyword_value(raw):
    """XISF FITSKeyword value text → Python value (quoted strings unquoted)."""
    raw = raw.strip()
    if raw.startswith("'"):
        return raw.strip("'").replace("''", "'").rstrip()
    for conv in (int, float):
        try:
            return conv(raw)
        except ValueError:
            pass
    return raw


def load_state(path):
    if os.path.exists(path):
        with open(path) as fh:
            return json.load(fh)
    return {}


def save_state(path, state):
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(state, fh, indent=2)
    os.replace(tmp, path)


def login():
    with open(os.path.expanduser("~/.config/astrometry/apikey")) as fh:
        key = fh.read().strip()
    r = requests.post(f"{API}/login", data={"request-json": json.dumps({"apikey": key})}, timeout=60).json()
    del key
    if r.get("status") != "success":
        sys.exit("nova login failed (status %r)" % r.get("status"))
    return r["session"]


def upload(session, up_path, kw, retry):
    args = {
        "publicly_visible": "n",
        "allow_modifications": "d",
        "allow_commercial_use": "n",
        "tweak_order": 3,
        "scale_units": "arcsecperpix",
        "scale_type": "ev",
        "scale_est": 1.6,
        "scale_err": 50 if retry else 25,
    }
    if not retry and "RA" in kw and "DEC" in kw:
        args.update(center_ra=float(keyword_value(kw["RA"])), center_dec=float(keyword_value(kw["DEC"])), radius=5.0)
    with open(up_path, "rb") as fh:
        r = requests.post(
            f"{API}/upload",
            data={"request-json": json.dumps({"session": session, **args})},
            files={"file": (os.path.basename(up_path), fh, "application/octet-stream")},
            timeout=900,
        ).json()
    if r.get("status") != "success":
        raise RuntimeError(f"upload of {up_path} failed: {r.get('status')} {r.get('errormessage')}")
    return r["subid"]


def write_solved(out_path, planes, kw, wcs_path):
    hdr = fits.getheader(wcs_path)
    # (C, H, W) top-down → rows bottom-up; no ROWORDER card (FITS default).
    out = fits.PrimaryHDU(np.ascontiguousarray(planes[:, ::-1, :]))
    for card in hdr.cards:
        k = card.keyword
        if k in SKIP_CARDS or k.startswith("NAXIS"):
            continue
        out.header.append(fits.Card(k, card.value, card.comment), end=True)
    for k in COPY_KEYWORDS:
        if k in kw and k not in out.header:
            out.header[k] = keyword_value(kw[k])
    out.writeto(out_path + ".tmp", overwrite=True)
    os.replace(out_path + ".tmp", out_path)
    return hdr


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    out_dir, inputs = sys.argv[1], sys.argv[2:]
    os.makedirs(out_dir, exist_ok=True)
    state_path = os.path.join(out_dir, "nova_jobs.json")
    state = load_state(state_path)
    session = login()

    panels = {}
    for path in inputs:
        name = re.search(r"PANEL-(\d+)", os.path.basename(path)).group(1)
        out_path = os.path.join(out_dir, f"PANEL-{name}.fits")
        if os.path.exists(out_path):
            print(f"panel {name}: {out_path} exists, skipping", flush=True)
            continue
        panels[name] = {"path": path, "out": out_path}
        st = state.setdefault(name, {"source": os.path.basename(path), "attempts": []})
        if st["attempts"] and st["attempts"][-1].get("status") not in ("failure",):
            st["attempts"][-1]["download_errors"] = 0  # a rerun retries a failed WCS download
            if st["attempts"][-1].get("status") == "download_failed":
                st["attempts"][-1]["status"] = None
            print(f"panel {name}: resuming submission {st['attempts'][-1]['subid']}", flush=True)
            continue
        planes, kw = read_xisf(path)
        lum16 = np.clip(planes.mean(axis=0) * 65535.0, 0, 65535).astype(np.uint16)
        up = os.path.join(out_dir, f"upload_{name}.fits")
        fits.PrimaryHDU(np.flipud(lum16)).writeto(up, overwrite=True)  # bottom-up storage
        del planes, lum16
        subid = upload(session, up, kw, retry=False)
        st["attempts"].append({"subid": subid, "job": None, "status": None, "retry": False})
        save_state(state_path, state)
        print(f"panel {name}: submitted {subid}", flush=True)
        time.sleep(5)  # stagger uploads

    pending = set(panels)
    while pending:
        time.sleep(15)
        for name in sorted(pending):
            st = state[name]
            att = st["attempts"][-1]
            try:
                if att["job"] is None:
                    s = requests.get(f"{API}/submissions/{att['subid']}", timeout=60).json()
                    if s.get("jobs") and s["jobs"][0]:
                        att["job"] = s["jobs"][0]
                        save_state(state_path, state)
                        print(f"panel {name}: job {att['job']}", flush=True)
                    continue
                status = requests.get(f"{API}/jobs/{att['job']}", timeout=60).json().get("status")
            except (requests.RequestException, ValueError) as e:
                print(f"panel {name}: poll error {e!r}, will retry", flush=True)
                continue
            if status == "success":
                # Download + write can fail transiently (HTTP error, truncated
                # or corrupt wcs.fits); keep the panel pending so the next poll
                # retries, up to MAX_DOWNLOAD_TRIES, instead of killing the batch.
                try:
                    wcs = requests.get(WCS_URL.format(att["job"]), timeout=120)
                    wcs.raise_for_status()
                    wcs_path = os.path.join(out_dir, f"wcs_{name}.fits")
                    with open(wcs_path, "wb") as fh:
                        fh.write(wcs.content)
                    planes, kw = read_xisf(panels[name]["path"])
                    hdr = write_solved(panels[name]["out"], planes, kw, wcs_path)
                    del planes
                except (requests.RequestException, ValueError, OSError, KeyError) as e:
                    att["download_errors"] = att.get("download_errors", 0) + 1
                    if att["download_errors"] >= MAX_DOWNLOAD_TRIES:
                        att["status"] = "download_failed"
                        pending.discard(name)
                        print(f"panel {name}: solved (job {att['job']}) but WCS download/write failed"
                              f" {att['download_errors']} times ({e!r}); giving up — rerun to retry", flush=True)
                    else:
                        print(f"panel {name}: WCS download/write failed ({e!r}), will retry"
                              f" ({att['download_errors']}/{MAX_DOWNLOAD_TRIES})", flush=True)
                    save_state(state_path, state)
                    continue
                att["status"] = "success"
                save_state(state_path, state)
                print(f"panel {name}: solved (job {att['job']}), CTYPE1={hdr['CTYPE1']} A_ORDER={hdr.get('A_ORDER')}",
                      flush=True)
                pending.discard(name)
            elif status == "failure":
                att["status"] = "failure"
                save_state(state_path, state)
                if att["retry"]:
                    print(f"panel {name}: nova failed to solve (job {att['job']}) after retry", flush=True)
                    pending.discard(name)
                else:
                    print(f"panel {name}: nova failed (job {att['job']}); retrying with scale_err 50, no position",
                          flush=True)
                    _, kw = read_xisf(panels[name]["path"])
                    subid = upload(session, os.path.join(out_dir, f"upload_{name}.fits"), kw, retry=True)
                    st["attempts"].append({"subid": subid, "job": None, "status": None, "retry": True})
                    save_state(state_path, state)
                    print(f"panel {name}: resubmitted {subid}", flush=True)


if __name__ == "__main__":
    main()
