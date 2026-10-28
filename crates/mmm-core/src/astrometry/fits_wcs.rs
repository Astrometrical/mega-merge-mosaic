//! FITS WCS (TAN / TAN-SIP) for mmm's panels: mpp-core decodes the cards
//! ([`mpp_core::astrometry::fits`], [`mpp_core::astrometry::WcsSolution`]);
//! this builds mmm's [`WcsModel`] from the result.

use mpp_core::astrometry::WcsSolution;
pub use mpp_core::astrometry::fits::{linear_for_panel, linear_from_keywords};

use crate::astrometry::WcsModel;
use crate::formats::{FitsKeyword, RowOrder};

/// Full model for a FITS panel: the linear solution (reflected into the
/// top-down frame for bottom-up files) plus SIP distortion when the header
/// carries `A_ORDER`. `Err` explains why the header is unusable; a SIP
/// solution that fails validation is refused rather than approximated.
pub fn model_from_keywords(
    cards: &[FitsKeyword],
    width: u64,
    height: u64,
    row_order: RowOrder,
) -> Result<WcsModel, String> {
    let sol = WcsSolution::from_keywords(cards, width, height, row_order)?;
    Ok(match sol.sip {
        None => WcsModel::linear_only(sol.linear, width, height),
        Some(sip) => WcsModel::with_sip(sip, width, height),
    })
}

#[cfg(test)]
mod tests {
    use mpp_core::astrometry::sip::SipSolution;

    use super::*;

    fn kw(name: &str, value: &str) -> FitsKeyword {
        FitsKeyword {
            name: name.into(),
            value: value.into(),
            comment: String::new(),
        }
    }

    fn base() -> Vec<FitsKeyword> {
        vec![
            kw("CTYPE1", "'RA---TAN'"),
            kw("CTYPE2", "'DEC--TAN'"),
            kw("CRVAL1", "83.8"),
            kw("CRVAL2", "-5.4"),
            kw("CRPIX1", "2440.5"),
            kw("CRPIX2", "1618.0"),
        ]
    }

    fn with_cd(mut c: Vec<FitsKeyword>) -> Vec<FitsKeyword> {
        c.extend([
            kw("CD1_1", "-4.0E-4"),
            kw("CD1_2", "1.0E-5"),
            kw("CD2_1", "1.0D-5"),
            kw("CD2_2", "4.0E-4"),
        ]);
        c
    }

    #[test]
    fn model_from_keywords_uses_grids_for_sip_and_linear_otherwise() {
        let lin = model_from_keywords(&with_cd(base()), 4880, 3235, RowOrder::BottomUp).unwrap();
        assert!(!lin.is_spline());
        let mut c = with_cd(base());
        c.extend([
            kw("A_ORDER", "2"),
            kw("B_ORDER", "2"),
            kw("A_2_0", "1.0E-7"),
            kw("B_0_2", "1.0E-7"),
        ]);
        let m = model_from_keywords(&c, 4880, 3235, RowOrder::BottomUp).unwrap();
        assert!(m.is_spline());
        // Grid path and direct evaluation agree, and the inverse round-trips.
        let s = SipSolution::parse(
            &c,
            &linear_from_keywords(&c).unwrap(),
            RowOrder::BottomUp,
            3235,
        )
        .unwrap()
        .unwrap();
        for (x, y) in [(10.0, 10.0), (4000.0, 3000.0), (2440.0, 1617.5)] {
            let (ra, dec) = m.pixel_to_sky(x, y);
            let (xi, eta) = s.image_to_native(x, y);
            let (ra2, dec2) = crate::astrometry::tan_deproject(m.linear.crval, xi, eta);
            assert!(
                (ra - ra2).abs() < 1e-9 && (dec - dec2).abs() < 1e-9,
                "grid vs direct at ({x},{y})"
            );
            let (bx, by) = m.sky_to_pixel(ra, dec).unwrap();
            assert!(
                (bx - x).abs() < 2e-3 && (by - y).abs() < 2e-3,
                "round trip ({x},{y}) → ({bx},{by})"
            );
        }
    }

    #[test]
    fn reflection_matches_wcs_cards_flipped_and_is_an_involution() {
        let w = linear_from_keywords(&with_cd(base())).unwrap();
        let h = 3235u64;
        let r = w.reflect_rows(h);
        assert_eq!(r.crpix, [2440.5, h as f64 + 1.0 - 1618.0]);
        assert_eq!(r.cd, [[-4.0e-4, -1.0e-5], [1.0e-5, -4.0e-4]]);
        assert_eq!(r.reflect_rows(h), w);
        // The same numbers `wcs_cards_flipped` writes for the bottom-up frame.
        let cards = crate::astrometry::wcs_cards_flipped(&w, (0, 0), h);
        let get = |n: &str| {
            cards
                .iter()
                .find(|k| k.name == n)
                .unwrap()
                .value
                .parse::<f64>()
                .unwrap()
        };
        assert_eq!(get("CRPIX2"), r.crpix[1]);
        assert_eq!(get("CD1_2"), r.cd[0][1]);
        assert_eq!(get("CD2_2"), r.cd[1][1]);
        // Same sky at mirrored rows: file (i, j) ↔ top-down (i, H+1−j).
        let (ra, dec) = w.pixel_to_sky(100.0, 200.0);
        let (ra2, dec2) = r.pixel_to_sky(100.0, h as f64 + 1.0 - 200.0);
        assert!((ra - ra2).abs() < 1e-12 && (dec - dec2).abs() < 1e-12);
        assert_eq!(
            linear_for_panel(&with_cd(base()), h, RowOrder::BottomUp).unwrap(),
            r
        );
        assert_eq!(
            linear_for_panel(&with_cd(base()), h, RowOrder::TopDown).unwrap(),
            w
        );
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        width: u64,
        height: u64,
        cards: Vec<(String, String, String)>,
        forward: Vec<[f64; 4]>,
        inverse: Vec<[f64; 4]>,
    }

    #[test]
    fn matches_astropy_oracle_through_grids() {
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../../tests/fixtures/sip_oracle.json")).unwrap();
        for case in cases {
            let cards: Vec<FitsKeyword> = case
                .cards
                .iter()
                .map(|(n, v, c)| FitsKeyword {
                    name: n.clone(),
                    value: v.clone(),
                    comment: c.clone(),
                })
                .collect();
            let ro = if cards
                .iter()
                .any(|k| k.name == "ROWORDER" && k.value.contains("TOP-DOWN"))
            {
                RowOrder::TopDown
            } else {
                RowOrder::BottomUp
            };
            let h = case.height;
            let model = model_from_keywords(&cards, case.width, h, ro).unwrap();
            assert!(model.is_spline(), "{}", case.name);
            let to_img = |i: f64, j: f64| -> (f64, f64) {
                match ro {
                    RowOrder::TopDown => (i - 0.5, j - 0.5),
                    RowOrder::BottomUp => (i - 0.5, h as f64 - j + 0.5),
                }
            };
            for &[i, j, ra, dec] in &case.forward {
                let (x, y) = to_img(i, j);
                let (r2, d2) = model.pixel_to_sky(x, y);
                let dra = ((r2 - ra + 180.0).rem_euclid(360.0) - 180.0) * dec.to_radians().cos();
                assert!(
                    dra.abs() < 2e-7 && (d2 - dec).abs() < 2e-7,
                    "{} grid fwd at ({i},{j}): {dra:e} {:e}",
                    case.name,
                    d2 - dec
                );
            }
            for &[ra, dec, i, j] in &case.inverse {
                let (x, y) = to_img(i, j);
                let (gx, gy) = model.sky_to_pixel(ra, dec).expect("inside native domain");
                assert!(
                    (gx - x).abs() < 5e-3 && (gy - y).abs() < 5e-3,
                    "{} grid inv: ({gx},{gy}) vs ({x},{y})",
                    case.name
                );
            }
        }
    }
}
