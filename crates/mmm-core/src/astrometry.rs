//! Linear WCS extraction from PixInsight XISF astrometric-solution properties.
//!
//! PixInsight (ImageSolver / MosaicByCoordinates) stores plate solutions as
//! XISF `<Property>` elements, not FITS keywords. Verified against a real
//! MosaicByCoordinates output panel (PixInsight 1.9.4, 2026-07), which carries:
//!
//! | Property id                                             | type      | content |
//! |---------------------------------------------------------|-----------|---------|
//! | `PCL:AstrometricSolution:ReferenceCelestialCoordinates` | F64Vector | `[RA, Dec]` of the reference point, degrees |
//! | `PCL:AstrometricSolution:ReferenceImageCoordinates`     | F64Vector | reference point in PixInsight image coordinates (0-based, pixel k spans `[k, k+1]`, y grows downward) |
//! | `PCL:AstrometricSolution:LinearTransformationMatrix`    | F64Matrix | 2×2, deg/px: maps image-coordinate offsets from the reference point to gnomonic plane offsets (ξ, η) |
//! | `PCL:AstrometricSolution:ProjectionSystem`              | String    | e.g. `Gnomonic` |
//! | `Observation:CelestialReferenceSystem`                  | String    | e.g. `ICRS` |
//!
//! (Also present but unused here: `ReferenceNativeCoordinates` = (0, 90) and
//! `CelestialPoleNativeCoordinates` = (180, 90) — the standard zenithal native
//! frame, implied by the CTYPE projection code; `Observation:Center:RA/Dec`
//! duplicate the reference celestial coordinates.)
//!
//! Mapping to FITS WCS cards — valid for top-down row order, which our FITS
//! writer declares via `ROWORDER = 'TOP-DOWN'`:
//!
//! - `CRVAL1/2` = ReferenceCelestialCoordinates,
//! - `CRPIX1/2` = ReferenceImageCoordinates + 0.5 on both axes (PixInsight
//!   pixel centers sit at k + 0.5 in 0-based coords; FITS pixel centers sit at
//!   integer 1-based coords, so FITS = PI + 0.5; no y flip since rows stay
//!   top-down),
//! - `CD1_1..CD2_2` = LinearTransformationMatrix exactly as stored (row 0 →
//!   axis 1 / ξ, row 1 → axis 2 / η),
//! - `CTYPE1/2` = `RA---`/`DEC--` + projection code (Gnomonic → `TAN`).
//!
//! The axis conventions were verified empirically on the real 12-panel Orion
//! mosaic: the stored matrix is diagonal `[[−s, 0], [0, +s]]` with
//! s = 4.4368e−4 deg/px, and PANEL-1 (raw plate-solve center RA 82.89,
//! Dec −0.29 — north-west of the canvas center RA 84.199, Dec −3.240) covers
//! the bottom-right of the canvas. Footprint centroid predicted by applying
//! the matrix directly to top-down pixel coordinates: (7562, 15811); measured
//! from the panel's nonzero coverage: (7616, 15808). The matrix therefore
//! applies to top-down coordinates with no sign flips.
//!
//! **Limitation:** if the solution also carries a distortion model
//! (`PCL:AstrometricSolution:SplineWorldTransformation`), only the linear part
//! is emitted — `LinearTransformationMatrix` is PixInsight's own linear
//! approximation around the reference point, so residuals grow away from it.
//! MosaicByCoordinates canvases (our input) are pure linear gnomonic.

use crate::formats::{FitsKeyword, XisfProperty};

/// A linear FITS WCS: `sky = project(crval, cd · (pixel − crpix))`, with
/// `pixel` in FITS convention (1-based, pixel centers at integers, rows in
/// stored order — top-down for our inputs and outputs).
#[derive(Debug, Clone, PartialEq)]
pub struct LinearWcs {
    /// Reference sky coordinates [RA, Dec] in degrees.
    pub crval: [f64; 2],
    /// Reference pixel [x, y], FITS 1-based.
    pub crpix: [f64; 2],
    /// Linear transformation, degrees per pixel; `cd[0]` is axis 1 (ξ/RA).
    pub cd: [[f64; 2]; 2],
    /// Axis types, e.g. `RA---TAN` / `DEC--TAN`.
    pub ctype: [String; 2],
    /// Celestial reference system for the RADESYS card (e.g. `ICRS`).
    pub radesys: String,
}

impl LinearWcs {
    /// Map a FITS pixel coordinate to (RA, Dec) in degrees.
    ///
    /// Uses the gnomonic (TAN) deprojection — exact for `RA---TAN`, which is
    /// what MosaicByCoordinates canvases use. Intended for validation and ROI
    /// math, not high-volume transforms.
    pub fn pixel_to_sky(&self, x: f64, y: f64) -> (f64, f64) {
        let dx = x - self.crpix[0];
        let dy = y - self.crpix[1];
        let xi = (self.cd[0][0] * dx + self.cd[0][1] * dy).to_radians();
        let eta = (self.cd[1][0] * dx + self.cd[1][1] * dy).to_radians();
        let (a0, d0) = (self.crval[0].to_radians(), self.crval[1].to_radians());
        let den = d0.cos() - eta * d0.sin();
        let ra = a0 + xi.atan2(den);
        let dec = ((d0.sin() + eta * d0.cos()) / xi.hypot(den)).atan();
        (ra.to_degrees().rem_euclid(360.0), dec.to_degrees())
    }

    /// Map (RA, Dec) in degrees to a FITS pixel coordinate (TAN projection).
    pub fn sky_to_pixel(&self, ra: f64, dec: f64) -> (f64, f64) {
        let (a0, d0) = (self.crval[0].to_radians(), self.crval[1].to_radians());
        let (a, d) = (ra.to_radians(), dec.to_radians());
        let cos_da = (a - a0).cos();
        let den = d0.sin() * d.sin() + d0.cos() * d.cos() * cos_da;
        let xi = (d.cos() * (a - a0).sin() / den).to_degrees();
        let eta = ((d0.cos() * d.sin() - d0.sin() * d.cos() * cos_da) / den).to_degrees();
        let det = self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0];
        let dx = (self.cd[1][1] * xi - self.cd[0][1] * eta) / det;
        let dy = (self.cd[0][0] * eta - self.cd[1][0] * xi) / det;
        (self.crpix[0] + dx, self.crpix[1] + dy)
    }
}

/// Extract the linear WCS from PixInsight astrometric-solution properties.
///
/// Returns `None` if any required property (reference celestial/image
/// coordinates, linear transformation matrix) is missing or malformed, or if
/// the projection system is one we cannot express as a FITS CTYPE code.
pub fn wcs_from_properties(props: &[XisfProperty]) -> Option<LinearWcs> {
    let find = |id: &str| props.iter().find(|p| p.id == id).map(|p| &p.value);

    let crval = find("PCL:AstrometricSolution:ReferenceCelestialCoordinates")?.as_f64_vec()?;
    let refimg = find("PCL:AstrometricSolution:ReferenceImageCoordinates")?.as_f64_vec()?;
    let (rows, cols, m) = find("PCL:AstrometricSolution:LinearTransformationMatrix")?
        .as_f64_mat()?;
    if crval.len() != 2 || refimg.len() != 2 || (rows, cols) != (2, 2) {
        return None;
    }

    // Missing projection property defaults to Gnomonic — the only projection
    // MosaicByCoordinates produces; an explicit unknown one refuses (better no
    // WCS than a wrong CTYPE).
    let proj = find("PCL:AstrometricSolution:ProjectionSystem")
        .map_or(Some("Gnomonic"), |v| v.as_str());
    let code = projection_code(proj?)?;

    let radesys = find("Observation:CelestialReferenceSystem")
        .and_then(|v| v.as_str())
        .unwrap_or("ICRS")
        .to_string();

    Some(LinearWcs {
        crval: [crval[0], crval[1]],
        // PixInsight image coords (pixel centers at k + 0.5, 0-based) →
        // FITS pixel coords (centers at integers, 1-based): +0.5 both axes.
        crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
        cd: [[m[0], m[1]], [m[2], m[3]]],
        ctype: [format!("{:-<5}{code}", "RA"), format!("{:-<5}{code}", "DEC")],
        radesys,
    })
}

/// PixInsight projection-system name → 3-letter FITS projection code.
fn projection_code(name: &str) -> Option<&'static str> {
    Some(match name {
        "Gnomonic" => "TAN",
        "Stereographic" => "STG",
        "Orthographic" => "SIN",
        "ZenithalEqualArea" => "ZEA",
        "Mercator" => "MER",
        "PlateCarree" => "CAR",
        "HammerAitoff" => "AIT",
        _ => return None,
    })
}

/// Produce FITS WCS cards, with CRPIX shifted by minus the crop origin (0-based
/// canvas pixels) so the canvas solution stays valid on a cropped output.
pub fn wcs_cards(w: &LinearWcs, crop_origin: (u64, u64)) -> Vec<FitsKeyword> {
    let kw = |name: &str, value: String, comment: &str| FitsKeyword {
        name: name.to_string(),
        value,
        comment: comment.to_string(),
    };
    let q = |s: &str| format!("'{s}'");
    vec![
        kw("CTYPE1", q(&w.ctype[0]), "projection"),
        kw("CTYPE2", q(&w.ctype[1]), "projection"),
        kw("CRVAL1", w.crval[0].to_string(), "[deg] RA at reference point"),
        kw("CRVAL2", w.crval[1].to_string(), "[deg] Dec at reference point"),
        // CRPIX is 1-based but the shift is a pure translation, so the base
        // cancels: new = old - crop_origin.
        kw("CRPIX1", (w.crpix[0] - crop_origin.0 as f64).to_string(), "reference pixel x"),
        kw("CRPIX2", (w.crpix[1] - crop_origin.1 as f64).to_string(), "reference pixel y"),
        kw("CD1_1", w.cd[0][0].to_string(), "[deg/px] transformation matrix"),
        kw("CD1_2", w.cd[0][1].to_string(), "[deg/px] transformation matrix"),
        kw("CD2_1", w.cd[1][0].to_string(), "[deg/px] transformation matrix"),
        kw("CD2_2", w.cd[1][1].to_string(), "[deg/px] transformation matrix"),
        kw("CUNIT1", q("deg"), "axis unit"),
        kw("CUNIT2", q("deg"), "axis unit"),
        kw("RADESYS", q(&w.radesys), "celestial reference system"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::PropertyValue;

    fn prop(id: &str, type_: &str, value: PropertyValue) -> XisfProperty {
        XisfProperty { id: id.into(), type_: type_.into(), value, location: None }
    }

    /// Property set replicating the real Orion mosaic canvas solution.
    fn orion_props() -> Vec<XisfProperty> {
        const S: f64 = 4.436849683786501e-4;
        vec![
            prop(
                "PCL:AstrometricSolution:ReferenceCelestialCoordinates",
                "F64Vector",
                PropertyValue::F64Vec(vec![84.19917732858325, -3.240007111323072]),
            ),
            prop(
                "PCL:AstrometricSolution:ReferenceImageCoordinates",
                "F64Vector",
                PropertyValue::F64Vec(vec![4627.5, 9155.0]),
            ),
            prop(
                "PCL:AstrometricSolution:LinearTransformationMatrix",
                "F64Matrix",
                PropertyValue::F64Mat { rows: 2, cols: 2, data: vec![-S, 0.0, 0.0, S] },
            ),
            prop(
                "PCL:AstrometricSolution:ProjectionSystem",
                "String",
                PropertyValue::Str("Gnomonic".into()),
            ),
            prop(
                "Observation:CelestialReferenceSystem",
                "String",
                PropertyValue::Str("ICRS".into()),
            ),
        ]
    }

    #[test]
    fn extracts_wcs_from_pixinsight_properties() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        assert_eq!(w.crval, [84.19917732858325, -3.240007111323072]);
        // FITS = PixInsight image coords + 0.5 on both axes.
        assert_eq!(w.crpix, [4628.0, 9155.5]);
        assert_eq!(w.cd[0][0], -4.436849683786501e-4);
        assert_eq!(w.cd[1][1], 4.436849683786501e-4);
        assert_eq!(w.cd[0][1], 0.0);
        assert_eq!(w.ctype, ["RA---TAN".to_string(), "DEC--TAN".to_string()]);
        assert_eq!(w.radesys, "ICRS");
    }

    #[test]
    fn missing_or_unknown_pieces_yield_none() {
        let mut p = orion_props();
        p.retain(|p| !p.id.ends_with("LinearTransformationMatrix"));
        assert!(wcs_from_properties(&p).is_none(), "matrix required");

        let mut p = orion_props();
        p.iter_mut()
            .find(|p| p.id.ends_with("ProjectionSystem"))
            .unwrap()
            .value = PropertyValue::Str("FancyUnknownProjection".into());
        assert!(wcs_from_properties(&p).is_none(), "unknown projection must not emit wrong CTYPE");

        assert!(wcs_from_properties(&[]).is_none());
    }

    #[test]
    fn center_pixel_maps_to_reference_coordinates() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        // Canvas 9255×18310: center in FITS coords is ((w+1)/2, (h+1)/2).
        let (ra, dec) = w.pixel_to_sky(4628.0, 9155.5);
        assert!((ra - 84.19917732858325).abs() < 1e-12);
        assert!((dec - -3.240007111323072).abs() < 1e-12);
    }

    #[test]
    fn orientation_matches_real_mosaic_layout() {
        // Empirical ground truth from the real data: raw PANEL-1 was plate-
        // solved at RA 82.8949, Dec −0.2871 (NW of the canvas center) and its
        // registered footprint sits in the canvas bottom-right quadrant with
        // centroid ≈ (7616, 15808) (top-down y). See module docs.
        let w = wcs_from_properties(&orion_props()).unwrap();
        let (x, y) = w.sky_to_pixel(82.8949, -0.2871);
        assert!((x - 7566.0).abs() < 15.0, "x = {x}");
        assert!((y - 15813.0).abs() < 15.0, "y = {y}");
    }

    #[test]
    fn sky_pixel_round_trip() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        for &(x, y) in &[(1.0, 1.0), (9255.0, 18310.0), (100.5, 17000.25), (4628.0, 9155.5)] {
            let (ra, dec) = w.pixel_to_sky(x, y);
            let (x2, y2) = w.sky_to_pixel(ra, dec);
            assert!((x - x2).abs() < 1e-6 && (y - y2).abs() < 1e-6, "({x},{y}) -> ({x2},{y2})");
        }
    }

    /// Rebuild a LinearWcs from emitted cards (test-side parse).
    fn wcs_from_cards(cards: &[FitsKeyword]) -> LinearWcs {
        let get = |n: &str| {
            cards.iter().find(|k| k.name == n).unwrap_or_else(|| panic!("card {n} missing"))
        };
        let f = |n: &str| get(n).value.parse::<f64>().unwrap();
        let s = |n: &str| get(n).value.trim_matches('\'').trim().to_string();
        LinearWcs {
            crval: [f("CRVAL1"), f("CRVAL2")],
            crpix: [f("CRPIX1"), f("CRPIX2")],
            cd: [[f("CD1_1"), f("CD1_2")], [f("CD2_1"), f("CD2_2")]],
            ctype: [s("CTYPE1"), s("CTYPE2")],
            radesys: s("RADESYS"),
        }
    }

    #[test]
    fn cards_carry_full_wcs_and_quote_strings() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        let cards = wcs_cards(&w, (0, 0));
        let get = |n: &str| &cards.iter().find(|k| k.name == n).unwrap().value;
        assert_eq!(get("CTYPE1"), "'RA---TAN'");
        assert_eq!(get("CTYPE2"), "'DEC--TAN'");
        assert_eq!(get("CUNIT1"), "'deg'");
        assert_eq!(get("RADESYS"), "'ICRS'");
        assert_eq!(wcs_from_cards(&cards), w, "values round-trip through card text exactly");
    }

    #[test]
    fn crop_shift_keeps_sky_of_a_fixed_pixel_invariant() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        let full = wcs_from_cards(&wcs_cards(&w, (0, 0)));
        let cropped = wcs_from_cards(&wcs_cards(&w, (1000, 2500)));
        assert_eq!(cropped.crpix[0], full.crpix[0] - 1000.0);
        assert_eq!(cropped.crpix[1], full.crpix[1] - 2500.0);
        // A fixed canvas pixel keeps its sky coordinates after cropping.
        let (ra1, dec1) = full.pixel_to_sky(6100.0, 12000.0);
        let (ra2, dec2) = cropped.pixel_to_sky(6100.0 - 1000.0, 12000.0 - 2500.0);
        assert!((ra1 - ra2).abs() < 1e-9 && (dec1 - dec2).abs() < 1e-9);
    }

    /// Manual smoke against the real 2 GB panel (gitignored test_data).
    /// Run: `cargo test -p mmm-core real_panel -- --ignored --nocapture`
    #[test]
    #[ignore = "needs multi-GB test_data/orion_mosaic (gitignored); run manually"]
    fn real_panel_center_matches_ra_dec_keywords() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../test_data/orion_mosaic/masterLight_BIN-1_4944x3284_EXPOSURE-30.00s_FILTER-NoFilter_RGB_PANEL-1_autocrop_ra.xisf",
        );
        let panel = crate::formats::xisf::XisfPanel::open(&path).unwrap();
        let h = panel.header();
        let w = wcs_from_properties(&h.properties).expect("astrometric solution properties");

        let kw = |name: &str| {
            h.fits_keywords
                .iter()
                .find(|k| k.name == name)
                .unwrap_or_else(|| panic!("keyword {name}"))
                .value
                .trim()
                .parse::<f64>()
                .unwrap()
        };
        let (ra_kw, dec_kw) = (kw("RA"), kw("DEC"));
        let (cx, cy) = ((h.width as f64 + 1.0) / 2.0, (h.height as f64 + 1.0) / 2.0);
        let (ra, dec) = w.pixel_to_sky(cx, cy);
        let err_arcsec =
            (((ra - ra_kw) * dec_kw.to_radians().cos()).hypot(dec - dec_kw)) * 3600.0;
        eprintln!("CRVAL {:?}  CRPIX {:?}  CD {:?}  CTYPE {:?}", w.crval, w.crpix, w.cd, w.ctype);
        eprintln!(
            "canvas center ({cx}, {cy}) -> RA {ra:.10}, Dec {dec:.10}; \
             RA/DEC keywords ({ra_kw}, {dec_kw}); error {err_arcsec:.3e} arcsec"
        );
        assert!(err_arcsec < 1.0, "center error {err_arcsec} arcsec");

        // Pixel scale ≈ 1.6 arcsec/px for this rig.
        let scale = w.cd[0][0].hypot(w.cd[1][0]) * 3600.0;
        assert!((scale - 1.597).abs() < 0.05, "scale {scale} arcsec/px");

        // Orientation: raw PANEL-1's own plate-solve center (NW on the sky)
        // must land in the canvas bottom-right quadrant (see module docs).
        let (px, py) = w.sky_to_pixel(82.8949, -0.2871);
        eprintln!("raw PANEL-1 sky center -> canvas pixel ({px:.1}, {py:.1})");
        assert!(px > cx && py > cy, "orientation: expected bottom-right, got ({px:.1}, {py:.1})");
    }
}
