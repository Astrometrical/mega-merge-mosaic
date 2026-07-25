//! Mosaic reference frame selection and spline-aware panel reprojection
//! (phase 5): turn *unaligned* solved panels into cropped, canvas-placed
//! caches the existing session pipeline can consume via
//! [`crate::panel_reader::PanelReader`].
//!
//! ## Frame convention
//!
//! [`MosaicFrame`] is a TAN (gnomonic) frame, north-up, expressed in the same
//! internal top-down convention as every [`LinearWcs`] this crate handles:
//! rows are stored top-down and the linear matrix applies to top-down
//! PixInsight image coordinates. Its matrix is `diag(-scale, +scale)` in that
//! frame — byte-for-byte the convention of a real MosaicByCoordinates canvas
//! (see `astrometry.rs` module docs: the Orion canvas stores exactly
//! `[[-s, 0], [0, s]]` applied to top-down coordinates, with the reference
//! image coordinate at the canvas center `(w/2, h/2)`). Displayed top-down
//! the canvas is therefore south-up; the bottom-up reflection to standard
//! FITS cards is [`crate::astrometry::wcs_cards`]'s job, unchanged.
//!
//! ## Content placement (the half-pixel law)
//!
//! MosaicByCoordinates-registered content obeys the placement law measured in
//! `astrometry.rs`: a sky position's content sits at the ARRAY INDEX equal to
//! its span-convention solution coordinate — displaced (−0.5, −0.5) px from
//! the strict span reading. [`reproject_panel`] reproduces that law so that a
//! raw-panel session aligns with a registered-panel session of the same data:
//! output array pixel `(i, j)` is assigned the sky position of frame solution
//! *coordinate* `(i, j)` (not `(i + 0.5, j + 0.5)`), while the raw source —
//! which was never resampled and therefore honors the span convention — is
//! sampled at array position `solution coordinate − 0.5`. Reprojecting with a
//! frame identical to the source solution consequently shifts content by
//! exactly (−0.5, −0.5) px: the same displacement MosaicByCoordinates itself
//! applies.
//!
//! ## Sampling
//!
//! Lanczos-3, evaluated per output pixel with the exact mapping (canvas →
//! sky via the analytic inverse TAN of the frame, sky → source pixel via
//! [`WcsModel::sky_to_pixel`], grids included — no per-row approximation).
//! The separable 6×6 kernel is weight-normalized per axis (constant fields
//! reproduce exactly). An output pixel is **zero — the no-data sentinel —
//! unless the full 6×6 support lies inside the source frame and every tapped
//! sample is finite**; there is no partial-support renormalization at the
//! rim, so reprojected panels get a hard ~3 px inset instead of interpolation
//! garbage. Strongly different input/frame scales make Lanczos slightly
//! non-flux-conserving (acceptable; matches MosaicByCoordinates behaviour).
//!
//! Caches are written as raw planar f32 little-endian with the bbox's
//! dimensions (`aligned.bin`, mmap-able; see
//! [`crate::panel_reader::PanelStorage::CroppedCache`]).

use std::io::Write;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::astrometry::{LinearWcs, WcsModel};
use crate::formats::xisf::XisfPanel;
use crate::{Error, Result};

/// Margin added around the union footprint when choosing the canvas.
pub const FRAME_MARGIN_PX: u64 = 16;

/// Boundary samples per panel edge when tracing footprints (the mapping is
/// smooth; corners plus a few intermediate points bound it tightly).
const EDGE_SAMPLES: usize = 16;

/// Padding around a panel's forward-mapped footprint bbox, in output pixels:
/// covers interpolation spread and curvature between boundary samples.
const BBOX_PAD: i64 = 3;

/// The mosaic reference frame: TAN, north-up, `CD = diag(-scale, +scale)` in
/// the internal top-down convention (standard-frame semantics after the
/// writer's bottom-up reflection), reference point at the canvas center.
/// Serializable so solved-input sessions persist it in `session.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MosaicFrame {
    /// Reference sky coordinates [RA, Dec] at the canvas center, degrees.
    pub crval: [f64; 2],
    /// Pixel scale in degrees per pixel.
    pub scale_deg: f64,
    /// Canvas width in pixels.
    pub width: u64,
    /// Canvas height in pixels.
    pub height: u64,
}

impl MosaicFrame {
    /// The frame's linear solution ([`LinearWcs`], FITS convention, top-down
    /// rows): reference pixel at the canvas center — PixInsight image
    /// coordinate `(w/2, h/2)`, i.e. FITS `(w/2 + 0.5, h/2 + 0.5)` — exactly
    /// how MosaicByCoordinates writes its canvases.
    pub fn linear_wcs(&self) -> LinearWcs {
        LinearWcs {
            crval: self.crval,
            crpix: [self.width as f64 / 2.0 + 0.5, self.height as f64 / 2.0 + 0.5],
            cd: [[-self.scale_deg, 0.0], [0.0, self.scale_deg]],
            ctype: ["RA---TAN".to_string(), "DEC--TAN".to_string()],
            radesys: "ICRS".to_string(),
        }
    }
}

/// A reprojected panel cache on the mosaic canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedPanel {
    /// Canvas placement of the cache, `[x0, y0, x1, y1]` exclusive.
    pub bbox: [u64; 4],
    /// The cache file: raw planar f32 LE, `channels × (y1−y0) × (x1−x0)`.
    pub path: PathBuf,
}

/// Points along the boundary of the rect `[0, w] × [0, h]` (PixInsight image
/// coordinates), [`EDGE_SAMPLES`] + 1 per edge.
fn boundary_samples(w: f64, h: f64) -> Vec<(f64, f64)> {
    let mut pts = Vec::with_capacity(4 * (EDGE_SAMPLES + 1));
    for i in 0..=EDGE_SAMPLES {
        let t = i as f64 / EDGE_SAMPLES as f64;
        pts.push((t * w, 0.0));
        pts.push((t * w, h));
        pts.push((0.0, t * h));
        pts.push((w, t * h));
    }
    pts
}

/// Choose the mosaic reference frame for a set of solved panels:
/// center = spherical mean of the panel centers, scale = median panel scale
/// (`√|det CD|`), canvas = union of the panels' reprojected footprints plus
/// [`FRAME_MARGIN_PX`] on every side. The frame's `crval` is recentered on
/// the union bbox so that the reference pixel is the canvas center *and* the
/// margin is symmetric.
///
/// Panics if `models` is empty.
pub fn choose_frame(models: &[WcsModel]) -> MosaicFrame {
    assert!(!models.is_empty(), "choose_frame needs at least one model");

    // Spherical mean of the panel centers (unit-vector average).
    let mut v = [0.0f64; 3];
    for m in models {
        let (ra, dec) = m.pixel_to_sky(m.width as f64 / 2.0, m.height as f64 / 2.0);
        let (ra, dec) = (ra.to_radians(), dec.to_radians());
        v[0] += dec.cos() * ra.cos();
        v[1] += dec.cos() * ra.sin();
        v[2] += dec.sin();
    }
    let mean = [
        v[1].atan2(v[0]).to_degrees().rem_euclid(360.0),
        v[2].atan2(v[0].hypot(v[1])).to_degrees(),
    ];

    // Median panel scale: √|det CD| (geometric mean of the axis scales).
    let mut scales: Vec<f64> = models
        .iter()
        .map(|m| {
            let cd = m.linear.cd;
            (cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]).abs().sqrt()
        })
        .collect();
    scales.sort_by(f64::total_cmp);
    let n = scales.len();
    let scale = if n % 2 == 1 {
        scales[n / 2]
    } else {
        0.5 * (scales[n / 2 - 1] + scales[n / 2])
    };

    // Union footprint in a provisional frame centered on the spherical mean
    // (reference pixel at 0 — only extents matter here).
    let prov = MosaicFrame { crval: mean, scale_deg: scale, width: 0, height: 0 }.linear_wcs();
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for m in models {
        for (px, py) in boundary_samples(m.width as f64, m.height as f64) {
            let (ra, dec) = m.pixel_to_sky(px, py);
            let (fx, fy) = prov.sky_to_pixel(ra, dec);
            x0 = x0.min(fx);
            x1 = x1.max(fx);
            y0 = y0.min(fy);
            y1 = y1.max(fy);
        }
    }

    // Canvas = extent + margins, crval recentered on the union bbox midpoint.
    let width = (x1 - x0).ceil() as u64 + 2 * FRAME_MARGIN_PX;
    let height = (y1 - y0).ceil() as u64 + 2 * FRAME_MARGIN_PX;
    let (cra, cdec) = prov.pixel_to_sky((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    MosaicFrame { crval: [cra, cdec], scale_deg: scale, width, height }
}

/// Lanczos-3 kernel: `sinc(t)·sinc(t/3)` for |t| < 3.
#[inline]
fn lanczos3(t: f64) -> f64 {
    if t == 0.0 {
        return 1.0;
    }
    let pt = std::f64::consts::PI * t;
    3.0 * (pt.sin() * (pt / 3.0).sin()) / (pt * pt)
}

/// Reproject one solved panel onto the mosaic frame, writing the cropped
/// cache `aligned.bin` into `out_dir` (created if needed; callers use the
/// session layout `panels/<id>/` — see [`crate::session::Session::aligned_path`]).
///
/// Per output pixel in the panel's canvas bbox: canvas → sky through the
/// frame's analytic TAN, sky → source pixel through `model` (spline grids
/// when present), Lanczos-3 sample of the source under the
/// full-support-or-zero rule and the content placement law (module docs).
/// Rayon-parallel over output rows; the mapping is evaluated exactly per
/// pixel. Errors when the panel's footprint misses the canvas entirely.
pub fn reproject_panel(
    panel: &XisfPanel,
    model: &WcsModel,
    frame: &MosaicFrame,
    out_dir: &Path,
) -> Result<AlignedPanel> {
    let (sw, sh) = (panel.width() as usize, panel.height() as usize);
    let nch = panel.channels() as usize;
    if (model.width, model.height) != (panel.width(), panel.height()) {
        return Err(Error::format(
            panel.path(),
            format!(
                "solved geometry {}x{} does not match panel {}x{}",
                model.width,
                model.height,
                panel.width(),
                panel.height()
            ),
        ));
    }
    let flin = frame.linear_wcs();

    // Output bbox: forward-map the source boundary. Content lands at the
    // array index equal to the frame span coordinate (placement law), so the
    // span coordinates bound the content indices directly; pad for curvature
    // between samples and the kernel's spread, clamp to the canvas.
    let (mut fx0, mut fy0) = (f64::INFINITY, f64::INFINITY);
    let (mut fx1, mut fy1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (px, py) in boundary_samples(sw as f64, sh as f64) {
        let (ra, dec) = model.pixel_to_sky(px, py);
        let (x, y) = flin.sky_to_pixel(ra, dec);
        // FITS → span/index coordinate.
        let (x, y) = (x - 0.5, y - 0.5);
        fx0 = fx0.min(x);
        fx1 = fx1.max(x);
        fy0 = fy0.min(y);
        fy1 = fy1.max(y);
    }
    let clamp = |v: i64, hi: u64| v.clamp(0, hi as i64) as u64;
    let x0 = clamp(fx0.floor() as i64 - BBOX_PAD, frame.width);
    let x1 = clamp(fx1.ceil() as i64 + 1 + BBOX_PAD, frame.width);
    let y0 = clamp(fy0.floor() as i64 - BBOX_PAD, frame.height);
    let y1 = clamp(fy1.ceil() as i64 + 1 + BBOX_PAD, frame.height);
    if x0 >= x1 || y0 >= y1 {
        return Err(Error::format(
            panel.path(),
            "panel footprint does not intersect the mosaic frame",
        ));
    }
    let (bw, bh) = ((x1 - x0) as usize, (y1 - y0) as usize);

    let planes: Vec<&[f32]> = (0..nch as u64).map(|c| panel.channel(c)).collect();

    // Rayon over output rows; each row holds nch × bw (channel-major).
    let rows_out: Vec<Vec<f32>> = (0..bh)
        .into_par_iter()
        .map(|oy| {
            let mut out = vec![0.0f32; nch * bw];
            let cy = (y0 + oy as u64) as f64;
            for ox in 0..bw {
                let cx = (x0 + ox as u64) as f64;
                // Placement law: output index (cx, cy) carries the sky of
                // frame solution *coordinate* (cx, cy) = FITS (cx+.5, cy+.5).
                let (ra, dec) = flin.pixel_to_sky(cx + 0.5, cy + 0.5);
                let Some((sx, sy)) = model.sky_to_pixel(ra, dec) else {
                    continue; // outside the solved domain: no data
                };
                // Source span coordinate → array position (raw panels honor
                // the span convention: centers at k + 0.5).
                let (u, vv) = (sx - 0.5, sy - 0.5);
                let k0 = u.floor() as i64;
                let l0 = vv.floor() as i64;
                // Full 6×6 support inside the source, or nothing.
                if k0 < 2 || k0 + 3 >= sw as i64 || l0 < 2 || l0 + 3 >= sh as i64 {
                    continue;
                }
                let (mut wx, mut wy) = ([0.0f64; 6], [0.0f64; 6]);
                let (mut sx_w, mut sy_w) = (0.0f64, 0.0f64);
                for i in 0..6 {
                    wx[i] = lanczos3(u - (k0 - 2 + i as i64) as f64);
                    wy[i] = lanczos3(vv - (l0 - 2 + i as i64) as f64);
                    sx_w += wx[i];
                    sy_w += wy[i];
                }
                // Normalize per axis: constant fields reproduce exactly.
                for i in 0..6 {
                    wx[i] /= sx_w;
                    wy[i] /= sy_w;
                }
                let base = (l0 - 2) as usize * sw + (k0 - 2) as usize;
                let mut ok = true;
                for (c, plane) in planes.iter().enumerate() {
                    let mut acc = 0.0f64;
                    for (j, wj) in wy.iter().enumerate() {
                        let row = &plane[base + j * sw..base + j * sw + 6];
                        let mut racc = 0.0f64;
                        for (wi, &s) in wx.iter().zip(row) {
                            racc += wi * f64::from(s);
                        }
                        acc += wj * racc;
                    }
                    if !acc.is_finite() {
                        ok = false; // non-finite source in support: no data
                        break;
                    }
                    out[c * bw + ox] = acc as f32;
                }
                if !ok {
                    for c in 0..nch {
                        out[c * bw + ox] = 0.0;
                    }
                }
            }
            out
        })
        .collect();

    // Cropped planar f32 LE cache, mmap-able by PanelReader.
    std::fs::create_dir_all(out_dir).map_err(|e| Error::io(out_dir, e))?;
    let path = out_dir.join("aligned.bin");
    let file = std::fs::File::create(&path).map_err(|e| Error::io(&path, e))?;
    let mut w = std::io::BufWriter::new(file);
    for c in 0..nch {
        for row in &rows_out {
            let bytes: &[u8] = bytemuck::cast_slice(&row[c * bw..(c + 1) * bw]);
            w.write_all(bytes).map_err(|e| Error::io(&path, e))?;
        }
    }
    w.flush().map_err(|e| Error::io(&path, e))?;

    Ok(AlignedPanel { bbox: [x0, y0, x1, y1], path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::{PropertyValue, XisfProperty};
    use crate::panel_reader::{PanelReader, PanelStorage};
    use crate::session::PanelMeta;
    use crate::synth::write_xisf;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-align-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn prop(id: &str, type_: &str, value: PropertyValue) -> XisfProperty {
        XisfProperty { id: id.into(), type_: type_.into(), value, location: None }
    }

    /// Linear-only model: reference at `refimg` (PixInsight image coords),
    /// matrix `cd` (deg/px, applied to top-down image offsets).
    fn linear_model(
        crval: [f64; 2],
        refimg: [f64; 2],
        cd: [[f64; 2]; 2],
        w: u64,
        h: u64,
    ) -> WcsModel {
        let props = vec![
            prop(
                "PCL:AstrometricSolution:ReferenceCelestialCoordinates",
                "F64Vector",
                PropertyValue::F64Vec(crval.to_vec()),
            ),
            prop(
                "PCL:AstrometricSolution:ReferenceImageCoordinates",
                "F64Vector",
                PropertyValue::F64Vec(refimg.to_vec()),
            ),
            prop(
                "PCL:AstrometricSolution:LinearTransformationMatrix",
                "F64Matrix",
                PropertyValue::F64Mat {
                    rows: 2,
                    cols: 2,
                    data: vec![cd[0][0], cd[0][1], cd[1][0], cd[1][1]],
                },
            ),
            prop(
                "PCL:AstrometricSolution:ProjectionSystem",
                "String",
                PropertyValue::Str("Gnomonic".into()),
            ),
        ];
        WcsModel::from_properties(&props, w, h).expect("valid linear model")
    }

    const S: f64 = 1.0e-3; // 3.6″/px test scale

    #[test]
    fn choose_frame_takes_median_scale_and_mean_center() {
        // Three co-centered panels with scales s, s, 3s: median = s.
        let mk = |scale: f64| {
            linear_model([10.0, 0.0], [50.0, 40.0], [[-scale, 0.0], [0.0, scale]], 100, 80)
        };
        let frame = choose_frame(&[mk(S), mk(3.0 * S), mk(S)]);
        assert!((frame.scale_deg - S).abs() < 1e-15, "median scale, got {}", frame.scale_deg);
        // Center: all panels centered at (10, 0); the 3s panel's footprint
        // dominates symmetrically, so the frame stays centered there.
        assert!((frame.crval[0] - 10.0).abs() < 1e-6, "ra {}", frame.crval[0]);
        assert!(frame.crval[1].abs() < 1e-6, "dec {}", frame.crval[1]);
        // Canvas: the 3s panel spans 300×240 frame pixels; + 2×16 margin.
        assert!((frame.width as i64 - 332).abs() <= 2, "width {}", frame.width);
        assert!((frame.height as i64 - 272).abs() <= 2, "height {}", frame.height);
    }

    #[test]
    fn choose_frame_unions_offset_footprints() {
        // Two same-scale panels at dec 0, centers 0.05° apart in RA
        // (= 50 px): union spans 100 + 50 = 150 px in x, 80 in y.
        let a = linear_model([10.0, 0.0], [50.0, 40.0], [[-S, 0.0], [0.0, S]], 100, 80);
        let b = linear_model([10.05, 0.0], [50.0, 40.0], [[-S, 0.0], [0.0, S]], 100, 80);
        let frame = choose_frame(&[a, b]);
        assert!((frame.width as i64 - 182).abs() <= 2, "width {}", frame.width);
        assert!((frame.height as i64 - 112).abs() <= 2, "height {}", frame.height);
        // Recentered between the two panels.
        assert!((frame.crval[0] - 10.025).abs() < 1e-4, "ra {}", frame.crval[0]);
        assert!(frame.crval[1].abs() < 1e-6, "dec {}", frame.crval[1]);
        // The union footprint fits with the margin on every side: each
        // panel's corners land at least FRAME_MARGIN_PX − 1 inside.
        let flin = frame.linear_wcs();
        let m = FRAME_MARGIN_PX as f64;
        for crval in [[10.0, 0.0], [10.05, 0.0]] {
            let mdl = linear_model(crval, [50.0, 40.0], [[-S, 0.0], [0.0, S]], 100, 80);
            for (px, py) in [(0.0, 0.0), (100.0, 0.0), (0.0, 80.0), (100.0, 80.0)] {
                let (ra, dec) = mdl.pixel_to_sky(px, py);
                let (fx, fy) = flin.sky_to_pixel(ra, dec);
                assert!(
                    fx > m - 1.0
                        && fx < frame.width as f64 - m + 1.0
                        && fy > m - 1.0
                        && fy < frame.height as f64 - m + 1.0,
                    "corner ({px},{py}) of panel {crval:?} at ({fx:.1},{fy:.1})"
                );
            }
        }
    }

    /// Deterministic nonzero test pattern, distinct along x and y.
    fn pattern(x: usize, y: usize, c: usize) -> f32 {
        0.1 + c as f32 * 0.5 + x as f32 * 0.01 + y as f32 * 0.002
    }

    fn write_pattern_panel(dir: &Path, w: usize, h: usize, ch: usize) -> XisfPanel {
        let mut planes = vec![0.0f32; ch * w * h];
        for c in 0..ch {
            for y in 0..h {
                for x in 0..w {
                    planes[(c * h + y) * w + x] = pattern(x, y, c);
                }
            }
        }
        let path = dir.join("src.xisf");
        write_xisf(&path, w as u64, h as u64, ch as u64, &planes).unwrap();
        XisfPanel::open(&path).unwrap()
    }

    /// Pure-translation reprojection: frame and model share crval and matrix;
    /// `refimg` is offset by (half-integer + k) so that — under the content
    /// placement law's inherent (−0.5, −0.5) displacement — the output is an
    /// exact integer-shifted copy of the source, with hard zeros where the
    /// Lanczos support leaves the source frame.
    #[test]
    fn reproject_translation_is_exact_copy_with_zero_rim() {
        let dir = tmpdir("translate");
        let (sw, sh, nch) = (64usize, 48usize, 2usize);
        let panel = write_pattern_panel(&dir, sw, sh, nch);
        let frame = MosaicFrame { crval: [30.0, 0.0], scale_deg: S, width: 40, height: 32 };
        // u = ox + (refimg_x − W/2 − 0.5): refimg_x = 20.5 − 8 → u = ox − 8;
        // v = oy + (refimg_y − H/2 − 0.5): refimg_y = 16.5 + 20 → v = oy + 20.
        let model =
            linear_model([30.0, 0.0], [12.5, 36.5], [[-S, 0.0], [0.0, S]], sw as u64, sh as u64);
        let ap = reproject_panel(&panel, &model, &frame, &dir.join("out")).unwrap();
        // Forward-mapped footprint: f_x = p_x + 7.5 ∈ [7.5, 71.5] → x ∈ [4, 40)
        // after padding/clamping; f_y = p_y − 20.5 ∈ [−20.5, 27.5] → y ∈ [0, 32).
        assert_eq!(ap.bbox, [4, 0, 40, 32]);

        // Read the cache back through PanelReader (integration of the two).
        let meta = PanelMeta {
            id: 0,
            path: ap.path.clone(),
            source: None,
            bbox: ap.bbox,
            nonzero_frac: 0.0,
            ch_min: vec![],
            ch_max: vec![],
            ch_mean: vec![],
            storage: PanelStorage::CroppedCache { bbox: ap.bbox },
        };
        let r = PanelReader::open(&meta, (40, 32, nch as u64)).unwrap();

        // Full support needs floor(u) ∈ [2, sw−4] (same for v). The taps land
        // on exact integers, so the support check at u == 2 / u == sw−4 flips
        // on float ε — skip that one-pixel ambiguity band on each side.
        let strict_in = |t: i64, n: i64| t >= 3 && t <= n - 4;
        let strict_out = |t: i64, n: i64| t <= 1 || t >= n - 2;
        let mut checked = 0u32;
        for c in 0..nch {
            for oy in 0..32usize {
                let (x0, row) = r.row(c as u64, oy as u64).unwrap();
                assert_eq!(x0, 4);
                for ox in 4..40usize {
                    let (u, v) = (ox as i64 - 8, oy as i64 + 20);
                    let got = row[ox - 4];
                    if strict_in(u, sw as i64) && strict_in(v, sh as i64) {
                        let want = pattern(u as usize, v as usize, c);
                        assert!(
                            (got - want).abs() < 1e-5,
                            "ch {c} ({ox},{oy}) -> src ({u},{v}): {got} vs {want}"
                        );
                        checked += 1;
                    } else if strict_out(u, sw as i64) || strict_out(v, sh as i64) {
                        assert_eq!(got, 0.0, "ch {c} ({ox},{oy}) must be no-data");
                    }
                }
            }
        }
        assert!(checked > 500, "translation test barely sampled ({checked} px)");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 90°-rotated source (axis swap — the E-W mirrored quarter-turn real
    /// mosaic panels have): pixel-exact up to interpolation when the geometry
    /// is arranged so source taps land on integers.
    #[test]
    fn reproject_rotation_90_is_pixel_exact() {
        let dir = tmpdir("rot90");
        let (sw, sh, nch) = (48usize, 32usize, 1usize);
        let panel = write_pattern_panel(&dir, sw, sh, nch);
        let frame = MosaicFrame { crval: [30.0, 0.0], scale_deg: S, width: 40, height: 36 };
        // Matrix s·[[0,1],[1,0]]: ξ = s·dy, η = s·dx (axis swap, det < 0 —
        // the standard astro mirror). Composing with the frame:
        //   u = oy + (refimg_x − H/2 − 0.5),  v = −ox + (refimg_y + W/2 − 0.5).
        // refimg = (22.5, 11.5) → u = oy + 4, v = 31 − ox.
        let model =
            linear_model([30.0, 0.0], [22.5, 11.5], [[0.0, S], [S, 0.0]], sw as u64, sh as u64);
        let ap = reproject_panel(&panel, &model, &frame, &dir.join("out")).unwrap();
        let [x0, y0, x1, y1] = ap.bbox;
        let bw = (x1 - x0) as usize;
        let bytes = std::fs::read(&ap.path).unwrap();
        let data: &[f32] = bytemuck::cast_slice(&bytes);

        // Taps land on exact integers; skip the one-pixel float-ambiguity
        // band at the support boundary (see the translation test).
        let strict_in = |t: i64, n: i64| t >= 3 && t <= n - 4;
        let strict_out = |t: i64, n: i64| t <= 1 || t >= n - 2;
        let mut checked = 0u32;
        for oy in y0..y1 {
            for ox in x0..x1 {
                let got = data[(oy - y0) as usize * bw + (ox - x0) as usize];
                let (u, v) = (oy as i64 + 4, 31 - ox as i64);
                if strict_in(u, sw as i64) && strict_in(v, sh as i64) {
                    let want = pattern(u as usize, v as usize, 0);
                    assert!(
                        (got - want).abs() < 1e-5,
                        "({ox},{oy}) -> src ({u},{v}): {got} vs {want}"
                    );
                    checked += 1;
                } else if strict_out(u, sw as i64) || strict_out(v, sh as i64) {
                    assert_eq!(got, 0.0, "({ox},{oy}) must be no-data");
                }
            }
        }
        assert!(checked > 500, "rotation test barely sampled ({checked} px)");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The same frame as the source solution: the output must show the
    /// MosaicByCoordinates placement law — content displaced by exactly
    /// (−0.5, −0.5) px (verified against a Lanczos-3 resampling of the
    /// pattern at the half-pixel offset).
    #[test]
    fn reproject_identity_applies_half_pixel_placement_law() {
        let dir = tmpdir("halfpx");
        let (sw, sh) = (48usize, 40usize);
        let panel = write_pattern_panel(&dir, sw, sh, 1);
        let frame =
            MosaicFrame { crval: [30.0, 0.0], scale_deg: S, width: sw as u64, height: sh as u64 };
        // Model identical to the frame's own solution.
        let model = linear_model(
            [30.0, 0.0],
            [sw as f64 / 2.0, sh as f64 / 2.0],
            [[-S, 0.0], [0.0, S]],
            sw as u64,
            sh as u64,
        );
        let ap = reproject_panel(&panel, &model, &frame, &dir.join("out")).unwrap();
        let bytes = std::fs::read(&ap.path).unwrap();
        let data: &[f32] = bytemuck::cast_slice(&bytes);
        let bw = (ap.bbox[2] - ap.bbox[0]) as usize;

        // The pattern is affine in x and y away from the frame edge, and the
        // normalized Lanczos kernel reproduces affine fields up to its ripple;
        // at a half-pixel offset the symmetric taps cancel the linear ripple,
        // so the expected value is the pattern at (ox − 0.5, oy − 0.5).
        let interior = |o: u64, lo: u64, hi: u64| o >= lo + 4 && o < hi - 4;
        let mut checked = 0u32;
        for oy in ap.bbox[1]..ap.bbox[3] {
            for ox in ap.bbox[0]..ap.bbox[2] {
                if !interior(ox, 0, sw as u64) || !interior(oy, 0, sh as u64) {
                    continue;
                }
                let got = data[(oy - ap.bbox[1]) as usize * bw + (ox - ap.bbox[0]) as usize];
                let want = 0.1 + (ox as f32 - 0.5) * 0.01 + (oy as f32 - 0.5) * 0.002;
                assert!(
                    (got - want).abs() < 1e-5,
                    "({ox},{oy}): {got} vs {want} (placement law violated)"
                );
                checked += 1;
            }
        }
        assert!(checked > 500);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reproject_rejects_disjoint_panel_and_geometry_mismatch() {
        let dir = tmpdir("disjoint");
        let panel = write_pattern_panel(&dir, 32, 24, 1);
        let frame = MosaicFrame { crval: [30.0, 0.0], scale_deg: S, width: 40, height: 32 };
        // Panel 10° away on the sky: footprint misses the canvas entirely.
        let far = linear_model([40.0, 0.0], [16.0, 12.0], [[-S, 0.0], [0.0, S]], 32, 24);
        assert!(reproject_panel(&panel, &far, &frame, &dir.join("out")).is_err());
        // Solved geometry differing from the pixel geometry is refused.
        let wrong = linear_model([30.0, 0.0], [16.0, 12.0], [[-S, 0.0], [0.0, S]], 64, 24);
        assert!(reproject_panel(&panel, &wrong, &frame, &dir.join("out")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- real-data validation (manual; multi-GB gitignored test_data) ----

    /// S2 real-data sanity: choose a frame from all 12 raw models (headers +
    /// grids only — fast), reproject PANEL-1, report timing and compare the
    /// output bbox against the registered PANEL-1 content bbox (different
    /// frame, so not identical — but similar size/aspect).
    /// Run: `cargo test -p mmm-core --release real_reproject -- --ignored --nocapture`
    #[test]
    #[ignore = "needs multi-GB test_data/orion_mosaic_raw_panels (gitignored); run manually"]
    fn real_reproject_panel1_bbox_and_timing() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data/orion_mosaic_raw_panels");
        let path = |n: u32| {
            base.join(format!(
                "masterLight_BIN-1_4944x3284_EXPOSURE-30.00s_FILTER-NoFilter_RGB_PANEL-{n}_autocrop.xisf"
            ))
        };
        let t0 = std::time::Instant::now();
        let models: Vec<WcsModel> = (1..=12)
            .map(|n| {
                let p = XisfPanel::open(&path(n)).unwrap();
                let h = p.header();
                WcsModel::from_properties(&h.properties, h.width, h.height)
                    .expect("raw panel must solve")
            })
            .collect();
        eprintln!("12 models loaded in {:.2} s", t0.elapsed().as_secs_f64());

        let frame = choose_frame(&models);
        eprintln!(
            "frame: crval ({:.4}, {:+.4}) scale {:.3}\"/px canvas {}x{}",
            frame.crval[0],
            frame.crval[1],
            frame.scale_deg * 3600.0,
            frame.width,
            frame.height
        );
        // Same sky, same scale as the MosaicByCoordinates canvas (9255×18310,
        // 1.597″/px): geometry must come out in the same ballpark.
        assert!((frame.scale_deg * 3600.0 - 1.597).abs() < 0.05, "scale");
        assert!(frame.width.abs_diff(9255) < 400, "width {}", frame.width);
        assert!(frame.height.abs_diff(18310) < 400, "height {}", frame.height);

        let out = std::env::temp_dir().join(format!("mmm-align-real-{}", std::process::id()));
        let panel = XisfPanel::open(&path(1)).unwrap();
        let t1 = std::time::Instant::now();
        let ap = reproject_panel(&panel, &models[0], &frame, &out).unwrap();
        let dt = t1.elapsed().as_secs_f64();
        let (bw, bh) = (ap.bbox[2] - ap.bbox[0], ap.bbox[3] - ap.bbox[1]);
        eprintln!(
            "PANEL-1 reprojected in {dt:.2} s -> bbox {:?} ({bw}x{bh}, aspect {:.3})",
            ap.bbox,
            bw as f64 / bh as f64
        );
        // Registered PANEL-1 content bbox is 3317×4967 (different frame —
        // similar size/aspect expected, not identity).
        assert!(bw.abs_diff(3317) < 350, "bbox width {bw}");
        assert!(bh.abs_diff(4967) < 350, "bbox height {bh}");
        let _ = std::fs::remove_dir_all(&out);
    }
}
