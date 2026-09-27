//! Linear WCS (and, with `sip`, SIP distortion) from FITS header cards.
//!
//! Supported: `CTYPE` `RA---TAN` / `RA---TAN-SIP` (either axis order),
//! `CRVAL`, `CRPIX`, and the matrix as `CD`, `PC × CDELT`, or `CDELT` with
//! `CROTA2`. Refused (never approximated): other projections, `TPV`,
//! nonzero `PV` terms, FK4 frames, a non-2000 `EQUINOX`, a non-180°
//! `LONPOLE`, and non-degree `CUNIT`s.

use crate::astrometry::sip::SipSolution;
use crate::astrometry::{LinearWcs, WcsModel};
use crate::formats::{FitsKeyword, RowOrder, card_number, card_string};

/// Projection code of a CTYPE value (`RA---TAN-SIP` → `TAN`), and whether
/// it is the RA or Dec axis.
fn ctype_parts(v: &str) -> Option<(bool, String)> {
    let v = v.trim();
    let is_ra = v.starts_with("RA--");
    let is_dec = v.starts_with("DEC-");
    if !(is_ra || is_dec) || v.len() < 8 {
        return None;
    }
    let code = v[5..8].to_string();
    Some((is_ra, code))
}

/// Refuses the frame/unit cards mmm would otherwise silently approximate:
/// FK4 frames (no B1950 → J2000 conversion is performed), an `EQUINOX` other
/// than 2000, a `LONPOLE` other than the TAN default of 180°, and axis units
/// other than degrees. `Err` names the offending card.
fn refuse_frame_and_units(cards: &[FitsKeyword]) -> Result<(), String> {
    let raw = |n: &str| {
        cards
            .iter()
            .find(|k| k.name.eq_ignore_ascii_case(n))
            .map(|k| k.value.trim().to_string())
    };
    if let Some(r) = card_string(cards, "RADESYS") {
        let r = r.trim().to_ascii_uppercase();
        if r == "FK4" || r == "FK4-NO-E" {
            return Err(format!(
                "RADESYS '{r}' is unsupported (no B1950 frame conversion; only ICRS/FK5 J2000)"
            ));
        }
    }
    if let Some(v) = raw("EQUINOX") {
        match card_number(cards, "EQUINOX") {
            Some(e) if (e - 2000.0).abs() <= 1e-6 => {}
            _ => {
                return Err(format!(
                    "EQUINOX = {v} is unsupported (only J2000 coordinates are accepted)"
                ));
            }
        }
    }
    if let Some(v) = raw("LONPOLE") {
        match card_number(cards, "LONPOLE") {
            Some(l) if (l - 180.0).abs() <= 1e-9 => {}
            _ => {
                return Err(format!(
                    "LONPOLE = {v} is unsupported (only the TAN default of 180 is accepted)"
                ));
            }
        }
    }
    for n in ["CUNIT1", "CUNIT2"] {
        if let Some(v) = raw(n) {
            let unit = card_string(cards, n).unwrap_or_else(|| v.clone());
            if !unit.trim().eq_ignore_ascii_case("deg") {
                return Err(format!(
                    "{n} = {v} is unsupported (only 'deg' axis units are accepted)"
                ));
            }
        }
    }
    Ok(())
}

/// The linear WCS exactly as the header states it (file pixel frame,
/// 1-based, stored row order). `Err` carries the user-facing reason.
pub fn linear_from_keywords(cards: &[FitsKeyword]) -> Result<LinearWcs, String> {
    let ctype1 = card_string(cards, "CTYPE1").ok_or("CTYPE1 missing")?;
    let ctype2 = card_string(cards, "CTYPE2").ok_or("CTYPE2 missing")?;
    let (ra1, code1) =
        ctype_parts(&ctype1).ok_or_else(|| format!("CTYPE1 '{ctype1}' is not a celestial axis"))?;
    let (ra2, code2) =
        ctype_parts(&ctype2).ok_or_else(|| format!("CTYPE2 '{ctype2}' is not a celestial axis"))?;
    if ra1 == ra2 {
        return Err(format!(
            "CTYPE1/CTYPE2 '{ctype1}'/'{ctype2}' are not one RA and one Dec axis"
        ));
    }
    if code1 == "TPV" || code2 == "TPV" {
        return Err("TPV distortion is unsupported (re-solve with TAN-SIP)".into());
    }
    if code1 != "TAN" || code2 != "TAN" {
        let (card, code) = if code1 != "TAN" {
            ("CTYPE1", &code1)
        } else {
            ("CTYPE2", &code2)
        };
        return Err(format!(
            "{card} projection '{code}' is unsupported (only the gnomonic TAN / TAN-SIP projection is supported)"
        ));
    }
    if let Some(k) = cards.iter().find(|k| {
        (k.name.starts_with("PV1_") || k.name.starts_with("PV2_"))
            && card_number(cards, &k.name).unwrap_or(0.0) != 0.0
    }) {
        return Err(format!(
            "{} distortion terms are unsupported (re-solve with TAN-SIP)",
            k.name.split('_').next().unwrap_or("PV")
        ));
    }
    refuse_frame_and_units(cards)?;
    let num = |n: &str| card_number(cards, n).ok_or_else(|| format!("{n} missing"));
    let crval = [num("CRVAL1")?, num("CRVAL2")?];
    let crpix = [num("CRPIX1")?, num("CRPIX2")?];
    let has = |n: &str| cards.iter().any(|k| k.name.eq_ignore_ascii_case(n));
    let cd = if ["CD1_1", "CD1_2", "CD2_1", "CD2_2"].iter().any(|n| has(n)) {
        let g = |n: &str| card_number(cards, n).unwrap_or(0.0);
        [[g("CD1_1"), g("CD1_2")], [g("CD2_1"), g("CD2_2")]]
    } else if has("CDELT1") && has("CDELT2") {
        let (d1, d2) = (num("CDELT1")?, num("CDELT2")?);
        if ["PC1_1", "PC1_2", "PC2_1", "PC2_2"].iter().any(|n| has(n)) {
            let g = |n: &str, dflt: f64| card_number(cards, n).unwrap_or(dflt);
            [
                [d1 * g("PC1_1", 1.0), d1 * g("PC1_2", 0.0)],
                [d2 * g("PC2_1", 0.0), d2 * g("PC2_2", 1.0)],
            ]
        } else {
            let (s, c) = card_number(cards, "CROTA2")
                .unwrap_or(0.0)
                .to_radians()
                .sin_cos();
            [[d1 * c, -d2 * s], [d1 * s, d2 * c]]
        }
    } else {
        return Err(
            "no linear transformation (CD matrix, PC + CDELT, or CDELT + CROTA2) found".into(),
        );
    };
    let radesys = card_string(cards, "RADESYS")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| {
            if has("EQUINOX") {
                "FK5".into()
            } else {
                "ICRS".into()
            }
        });
    // Normalize to RA-first: swap the two world axes (rows of CD, CRVAL).
    let (crval, cd, ctype) = if ra1 {
        (
            crval,
            cd,
            [ctype1.trim().to_string(), ctype2.trim().to_string()],
        )
    } else {
        (
            [crval[1], crval[0]],
            [cd[1], cd[0]],
            [ctype2.trim().to_string(), ctype1.trim().to_string()],
        )
    };
    let ctype = [ctype[0][..8].to_string(), ctype[1][..8].to_string()];
    Ok(LinearWcs {
        crval,
        crpix,
        cd,
        ctype,
        radesys,
    })
}

/// The panel's linear WCS in mmm's top-down frame: the file's solution,
/// reflected over `height` rows when the file is stored bottom-up.
pub fn linear_for_panel(
    cards: &[FitsKeyword],
    height: u64,
    row_order: RowOrder,
) -> Result<LinearWcs, String> {
    let w = linear_from_keywords(cards)?;
    Ok(match row_order {
        RowOrder::TopDown => w,
        RowOrder::BottomUp => w.reflect_rows(height),
    })
}

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
    let file_linear = linear_from_keywords(cards)?;
    match SipSolution::parse(cards, &file_linear, row_order, height)? {
        None => {
            let linear = match row_order {
                RowOrder::TopDown => file_linear,
                RowOrder::BottomUp => file_linear.reflect_rows(height),
            };
            Ok(WcsModel::linear_only(linear, width, height))
        }
        Some(sip) => {
            sip.validate(width, height)?;
            Ok(WcsModel::with_sip(sip, width, height))
        }
    }
}

#[cfg(test)]
mod tests {
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

    const CD: [[f64; 2]; 2] = [[-4.0e-4, 1.0e-5], [1.0e-5, 4.0e-4]];

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
    fn cd_pc_and_crota_forms_agree() {
        let a = linear_from_keywords(&with_cd(base())).unwrap();
        assert_eq!(a.cd, CD);
        assert_eq!(a.crval, [83.8, -5.4]);
        assert_eq!(a.crpix, [2440.5, 1618.0]);
        assert_eq!(a.ctype, ["RA---TAN".to_string(), "DEC--TAN".to_string()]);
        assert_eq!(a.radesys, "ICRS");

        let mut pc = base();
        pc.extend([
            kw("PC1_1", "1"),
            kw("PC1_2", "-0.025"),
            kw("PC2_1", "0.025"),
            kw("PC2_2", "1"),
            kw("CDELT1", "-4.0E-4"),
            kw("CDELT2", "4.0E-4"),
        ]);
        let b = linear_from_keywords(&pc).unwrap();
        for (i, (brow, crow)) in b.cd.iter().zip(CD.iter()).enumerate() {
            for (j, (bv, cv)) in brow.iter().zip(crow.iter()).enumerate() {
                assert!((bv - cv).abs() < 1e-15, "pc form {i}{j}");
            }
        }

        let mut rot = base();
        rot.extend([
            kw("CDELT1", "-4.0E-4"),
            kw("CDELT2", "4.0E-4"),
            kw("CROTA2", "30"),
        ]);
        let c = linear_from_keywords(&rot).unwrap();
        let (s, co) = 30f64.to_radians().sin_cos();
        // CDELT+CROTA2: CD1_1 = CDELT1·cos, CD1_2 = −CDELT2·sin, CD2_1 = CDELT1·sin, CD2_2 = CDELT2·cos
        let want = [[-4.0e-4 * co, -4.0e-4 * s], [-4.0e-4 * s, 4.0e-4 * co]];
        for (i, (crow, wrow)) in c.cd.iter().zip(want.iter()).enumerate() {
            for (j, (cv, wv)) in crow.iter().zip(wrow.iter()).enumerate() {
                assert!((cv - wv).abs() < 1e-15, "crota form {i}{j}: {cv} vs {wv}");
            }
        }
    }

    #[test]
    fn radesys_defaults_and_axis_swap() {
        let mut c = with_cd(base());
        c.push(kw("EQUINOX", "2000.0"));
        assert_eq!(linear_from_keywords(&c).unwrap().radesys, "FK5");
        c.push(kw("RADESYS", "'ICRS    '"));
        assert_eq!(linear_from_keywords(&c).unwrap().radesys, "ICRS");

        // Dec on axis 1: everything is swapped into RA-first form.
        let swapped = vec![
            kw("CTYPE1", "'DEC--TAN'"),
            kw("CTYPE2", "'RA---TAN'"),
            kw("CRVAL1", "-5.4"),
            kw("CRVAL2", "83.8"),
            kw("CRPIX1", "2440.5"),
            kw("CRPIX2", "1618.0"),
            kw("CD1_1", "1.0E-5"),
            kw("CD1_2", "4.0E-4"),
            kw("CD2_1", "-4.0E-4"),
            kw("CD2_2", "1.0E-5"),
        ];
        let s = linear_from_keywords(&swapped).unwrap();
        assert_eq!(s.crval, [83.8, -5.4]);
        assert_eq!(s.cd, CD, "rows swapped so row 0 is the RA (ξ) axis");
        assert_eq!(s.crpix, [2440.5, 1618.0], "pixel axes are untouched");
    }

    #[test]
    fn refusals() {
        let mut c = with_cd(base());
        c[0] = kw("CTYPE1", "'RA---SIN'");
        c[1] = kw("CTYPE2", "'DEC--SIN'");
        assert!(linear_from_keywords(&c).unwrap_err().contains("projection"));
        let mut c = with_cd(base());
        c[1] = kw("CTYPE2", "'DEC--SIN'");
        let err = linear_from_keywords(&c).unwrap_err();
        assert!(err.contains("SIN"), "{err}");
        assert!(err.contains("CTYPE2"), "{err}");
        assert!(!err.contains("'TAN'"), "{err}");
        let mut c = with_cd(base());
        c[0] = kw("CTYPE1", "'RA---TPV'");
        c[1] = kw("CTYPE2", "'DEC--TPV'");
        assert!(linear_from_keywords(&c).unwrap_err().contains("TPV"));
        let mut c = with_cd(base());
        c.push(kw("PV2_3", "0.001"));
        assert!(linear_from_keywords(&c).unwrap_err().contains("PV"));
        let mut c = with_cd(base());
        c.push(kw("PV2_1", "0.0"));
        assert!(
            linear_from_keywords(&c).is_ok(),
            "zero PV terms are harmless"
        );
        assert!(
            linear_from_keywords(&base()).unwrap_err().contains("CD"),
            "no matrix"
        );
        let mut c = with_cd(base());
        c.retain(|k| k.name != "CRVAL2");
        assert!(linear_from_keywords(&c).unwrap_err().contains("CRVAL2"));
    }

    #[test]
    fn refuses_non_j2000_frames_lonpole_and_non_degree_units() {
        let refuse = |extra: Vec<FitsKeyword>, card: &str| {
            let mut c = with_cd(base());
            c.extend(extra);
            let e = linear_from_keywords(&c).unwrap_err();
            assert!(e.contains(card), "expected {card} in: {e}");
        };
        refuse(vec![kw("RADESYS", "'FK4'")], "RADESYS");
        refuse(vec![kw("RADESYS", "'FK4-NO-E'")], "RADESYS");
        refuse(vec![kw("EQUINOX", "1950.0")], "EQUINOX");
        refuse(vec![kw("EQUINOX", "'J2000'")], "EQUINOX");
        refuse(vec![kw("LONPOLE", "0.0")], "LONPOLE");
        refuse(vec![kw("CUNIT1", "'arcsec'")], "CUNIT1");
        refuse(vec![kw("CUNIT2", "'rad     '")], "CUNIT2");
        // Accepted: the values these cards take for a plain J2000 TAN solution.
        let mut c = with_cd(base());
        c.extend([
            kw("RADESYS", "'FK5'"),
            kw("EQUINOX", "2000.0"),
            kw("LONPOLE", "180.0"),
            kw("CUNIT1", "'deg     '"),
            kw("CUNIT2", "'DEG'"),
        ]);
        assert_eq!(linear_from_keywords(&c).unwrap().radesys, "FK5");
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
}
