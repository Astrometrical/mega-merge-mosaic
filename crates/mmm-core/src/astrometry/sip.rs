//! SIP distortion (Shupe et al. 2005) for TAN-SIP FITS solutions.
//!
//! Forward: file pixel `(i, j)` → `(u, v) = (i − CRPIX1, j − CRPIX2)` →
//! `(ξ, η) = CD · (u + f(u, v), v + g(u, v))` with `f = Σ A_p_q uᵖ vᵠ`,
//! `g = Σ B_p_q uᵖ vᵠ`, `p + q ≤ order`. Inverse: the `AP`/`BP` polynomials
//! (when present) only seed a Newton iteration on the forward map, so the
//! inverse is exact to floating-point precision either way. The model is
//! evaluated in file coordinates; mmm's top-down image coordinates are
//! converted at the boundary per the panel's [`RowOrder`].

use crate::astrometry::LinearWcs;
use crate::formats::{FitsKeyword, RowOrder, card_number};

/// Highest polynomial order accepted (astrometry.net uses 2–5).
const MAX_ORDER: usize = 9;

/// Dense SIP polynomial: `coef[p * (order + 1) + q]` multiplies `uᵖ vᵠ`.
#[derive(Debug, Clone)]
struct Poly {
    order: usize,
    coef: Vec<f64>,
}

impl Poly {
    /// Collect the `{prefix}p_q` cards into a dense coefficient table.
    /// `Err` when a term exceeds the declared order or is not a number.
    fn parse(cards: &[FitsKeyword], prefix: &str, order: usize) -> Result<Poly, String> {
        let n = order + 1;
        let mut coef = vec![0.0; n * n];
        for k in cards {
            let Some(rest) = k.name.strip_prefix(prefix) else {
                continue;
            };
            let Some((p, q)) = rest.split_once('_') else {
                continue;
            };
            let (Ok(p), Ok(q)) = (p.parse::<usize>(), q.parse::<usize>()) else {
                continue;
            };
            if p + q > order {
                return Err(format!("{} exceeds {}ORDER = {order}", k.name, prefix));
            }
            coef[p * n + q] =
                card_number(cards, &k.name).ok_or_else(|| format!("{} is not a number", k.name))?;
        }
        if coef.iter().any(|c| !c.is_finite()) {
            return Err(format!("{prefix}coefficients carry non-finite values"));
        }
        Ok(Poly { order, coef })
    }

    /// The polynomial at `(u, v)`.
    fn eval(&self, u: f64, v: f64) -> f64 {
        let n = self.order + 1;
        let mut s = 0.0;
        let mut up = 1.0;
        for p in 0..n {
            let mut vq = 1.0;
            for q in 0..(n - p) {
                s += self.coef[p * n + q] * up * vq;
                vq *= v;
            }
            up *= u;
        }
        s
    }

    /// `(∂/∂u, ∂/∂v)`.
    fn grad(&self, u: f64, v: f64) -> (f64, f64) {
        let n = self.order + 1;
        let (mut du, mut dv) = (0.0, 0.0);
        for p in 0..n {
            for q in 0..(n - p) {
                let c = self.coef[p * n + q];
                if c == 0.0 {
                    continue;
                }
                if p > 0 {
                    du += c * p as f64 * u.powi(p as i32 - 1) * v.powi(q as i32);
                }
                if q > 0 {
                    dv += c * q as f64 * u.powi(p as i32) * v.powi(q as i32 - 1);
                }
            }
        }
        (du, dv)
    }
}

/// A parsed TAN-SIP solution (see the module docs for the conventions).
#[derive(Debug, Clone)]
pub struct SipSolution {
    /// The header's linear solution in mmm's top-down frame.
    linear: LinearWcs,
    /// `CRPIX` in the file's own (1-based) pixel frame.
    file_crpix: [f64; 2],
    /// `CD` in the file's own row order.
    file_cd: [[f64; 2]; 2],
    /// Inverse of `file_cd` (the linear part of the inverse map).
    file_cd_inv: [[f64; 2]; 2],
    /// `A_p_q`: the ξ-axis pixel correction.
    a: Poly,
    /// `B_p_q`: the η-axis pixel correction.
    b: Poly,
    /// `AP_p_q`, when the header carries an approximate inverse.
    ap: Option<Poly>,
    /// `BP_p_q`, when the header carries an approximate inverse.
    bp: Option<Poly>,
    /// Row order of the file the cards came from.
    row_order: RowOrder,
    /// Panel height in rows (for the bottom-up row flip).
    height: u64,
}

impl SipSolution {
    /// Parse the SIP cards accompanying `file_linear` (the header's own,
    /// unreflected linear solution). `Ok(None)` when the header carries no
    /// `A_ORDER`; `Err` on malformed or oversized polynomials.
    pub fn parse(
        cards: &[FitsKeyword],
        file_linear: &LinearWcs,
        row_order: RowOrder,
        height: u64,
    ) -> Result<Option<SipSolution>, String> {
        let order_of = |name: &str| -> Result<Option<usize>, String> {
            match card_number(cards, name) {
                None => Ok(None),
                Some(v) if v >= 0.0 && v.fract() == 0.0 && (v as usize) <= MAX_ORDER => {
                    Ok(Some(v as usize))
                }
                Some(v) => Err(format!("{name} = {v} is not an order in 0..={MAX_ORDER}")),
            }
        };
        let Some(a_order) = order_of("A_ORDER")? else {
            return Ok(None);
        };
        let b_order = order_of("B_ORDER")?.unwrap_or(a_order);
        let a = Poly::parse(cards, "A_", a_order)?;
        let b = Poly::parse(cards, "B_", b_order)?;
        let ap = match order_of("AP_ORDER")? {
            Some(o) => Some(Poly::parse(cards, "AP_", o)?),
            None => None,
        };
        let bp = match order_of("BP_ORDER")? {
            Some(o) => Some(Poly::parse(cards, "BP_", o)?),
            None => None,
        };
        let m = file_linear.cd;
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        if !(det.is_finite() && det != 0.0) {
            return Err("CD matrix is singular".into());
        }
        let file_cd_inv = [
            [m[1][1] / det, -m[0][1] / det],
            [-m[1][0] / det, m[0][0] / det],
        ];
        let linear = match row_order {
            RowOrder::TopDown => file_linear.clone(),
            RowOrder::BottomUp => file_linear.reflect_rows(height),
        };
        Ok(Some(SipSolution {
            linear,
            file_crpix: file_linear.crpix,
            file_cd: m,
            file_cd_inv,
            a,
            b,
            ap,
            bp,
            row_order,
            height,
        }))
    }

    /// The linear solution in mmm's top-down frame.
    pub fn linear(&self) -> &LinearWcs {
        &self.linear
    }

    /// Image (0-based, top-down, span) → file pixel offsets `(u, v)`.
    fn image_to_uv(&self, x_img: f64, y_img: f64) -> (f64, f64) {
        let i = x_img + 0.5;
        let j = match self.row_order {
            RowOrder::TopDown => y_img + 0.5,
            RowOrder::BottomUp => self.height as f64 - y_img + 0.5,
        };
        (i - self.file_crpix[0], j - self.file_crpix[1])
    }

    /// File pixel offsets `(u, v)` → image (0-based, top-down, span).
    fn uv_to_image(&self, u: f64, v: f64) -> (f64, f64) {
        let i = u + self.file_crpix[0];
        let j = v + self.file_crpix[1];
        let y_img = match self.row_order {
            RowOrder::TopDown => j - 0.5,
            RowOrder::BottomUp => self.height as f64 - j + 0.5,
        };
        (i - 0.5, y_img)
    }

    /// `CD · (u + f(u, v), v + g(u, v))`.
    fn forward_uv(&self, u: f64, v: f64) -> (f64, f64) {
        let du = u + self.a.eval(u, v);
        let dv = v + self.b.eval(u, v);
        let m = self.file_cd;
        (m[0][0] * du + m[0][1] * dv, m[1][0] * du + m[1][1] * dv)
    }

    /// Image coordinate → native tangent plane `(ξ, η)` in degrees.
    pub fn image_to_native(&self, x_img: f64, y_img: f64) -> (f64, f64) {
        let (u, v) = self.image_to_uv(x_img, y_img);
        self.forward_uv(u, v)
    }

    /// The Newton starting point for `(ξ, η)`: the AP/BP inverse when the
    /// header has one, else the plain linear inverse. Image coordinates.
    ///
    /// Exists so tests can see the seed on its own (the inverse itself always
    /// refines it), hence test-only — nothing in the model needs it.
    #[cfg(test)]
    pub(crate) fn inverse_seed(&self, xi: f64, eta: f64) -> (f64, f64) {
        let (u, v) = self.seed_uv(xi, eta);
        self.uv_to_image(u, v)
    }

    /// [`Self::inverse_seed`] in file pixel offsets.
    fn seed_uv(&self, xi: f64, eta: f64) -> (f64, f64) {
        let inv = self.file_cd_inv;
        let (bu, bv) = (
            inv[0][0] * xi + inv[0][1] * eta,
            inv[1][0] * xi + inv[1][1] * eta,
        );
        match (&self.ap, &self.bp) {
            (Some(ap), Some(bp)) => (bu + ap.eval(bu, bv), bv + bp.eval(bu, bv)),
            _ => (bu, bv),
        }
    }

    /// Native tangent plane → image coordinate; NaN when Newton fails.
    pub fn native_to_image(&self, xi: f64, eta: f64) -> (f64, f64) {
        let (mut u, mut v) = self.seed_uv(xi, eta);
        for _ in 0..8 {
            let (fx, fy) = self.forward_uv(u, v);
            let (rx, ry) = (fx - xi, fy - eta);
            // Jacobian of forward_uv: CD · [[1 + a_u, a_v], [b_u, 1 + b_v]].
            let (au, av) = self.a.grad(u, v);
            let (bu, bv) = self.b.grad(u, v);
            let m = self.file_cd;
            let j00 = m[0][0] * (1.0 + au) + m[0][1] * bu;
            let j01 = m[0][0] * av + m[0][1] * (1.0 + bv);
            let j10 = m[1][0] * (1.0 + au) + m[1][1] * bu;
            let j11 = m[1][0] * av + m[1][1] * (1.0 + bv);
            let det = j00 * j11 - j01 * j10;
            if !(det.is_finite() && det != 0.0) {
                return (f64::NAN, f64::NAN);
            }
            let du = (j11 * rx - j01 * ry) / det;
            let dv = (j00 * ry - j10 * rx) / det;
            u -= du;
            v -= dv;
            if !(u.is_finite() && v.is_finite()) {
                return (f64::NAN, f64::NAN);
            }
            if du.abs() < 1e-9 && dv.abs() < 1e-9 {
                return self.uv_to_image(u, v);
            }
        }
        (f64::NAN, f64::NAN)
    }

    /// Consistency checks before the model is used (mirrors
    /// `standard::validate_model`): the reference pixel maps near the origin,
    /// corners stay within 0.05° of the linear map, and the inverse
    /// round-trips at center and corners to 1e-3 px.
    pub fn validate(&self, width: u64, height: u64) -> Result<(), String> {
        let refimg = [self.linear.crpix[0] - 0.5, self.linear.crpix[1] - 0.5];
        let (xi, eta) = self.image_to_native(refimg[0], refimg[1]);
        if !xi.is_finite() || !eta.is_finite() || xi.hypot(eta) > 0.01 {
            return Err(format!(
                "SIP maps the reference pixel {:.4}° from the projection origin",
                xi.hypot(eta)
            ));
        }
        let (w, h) = (width as f64, height as f64);
        let m = self.linear.cd;
        for (cx, cy) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h), (w / 2.0, h / 2.0)] {
            let (gx, gy) = self.image_to_native(cx, cy);
            let (dx, dy) = (cx - refimg[0], cy - refimg[1]);
            let (lx, ly) = (m[0][0] * dx + m[0][1] * dy, m[1][0] * dx + m[1][1] * dy);
            let dev = (gx - lx).hypot(gy - ly);
            if !dev.is_finite() || dev > 0.05 {
                return Err(format!(
                    "SIP deviates {dev:.3}° from its own linear solution at ({cx:.0}, {cy:.0}); \
                     the model is inconsistent"
                ));
            }
            let (bx, by) = self.native_to_image(gx, gy);
            let rt = (bx - cx).hypot(by - cy);
            if !rt.is_finite() || rt > 1e-3 {
                return Err(format!(
                    "SIP inverse does not round-trip at ({cx:.0}, {cy:.0}): {rt:.2e} px"
                ));
            }
        }
        Ok(())
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

    /// A quadratic SIP with modest coefficients on a 1000×800 field.
    fn cards(with_inverse: bool) -> Vec<FitsKeyword> {
        let mut c = vec![
            kw("A_ORDER", "2"),
            kw("B_ORDER", "2"),
            kw("A_2_0", "1.5E-6"),
            kw("A_1_1", "-2.0E-6"),
            kw("A_0_2", "1.0E-6"),
            kw("B_2_0", "-1.0E-6"),
            kw("B_1_1", "1.2E-6"),
            kw("B_0_2", "2.0E-6"),
        ];
        if with_inverse {
            // Approximate inverse (first-order negation of the forward terms).
            c.extend([
                kw("AP_ORDER", "2"),
                kw("BP_ORDER", "2"),
                kw("AP_2_0", "-1.5E-6"),
                kw("AP_1_1", "2.0E-6"),
                kw("AP_0_2", "-1.0E-6"),
                kw("BP_2_0", "1.0E-6"),
                kw("BP_1_1", "-1.2E-6"),
                kw("BP_0_2", "-2.0E-6"),
            ]);
        }
        c
    }

    fn file_linear() -> LinearWcs {
        LinearWcs {
            crval: [83.8, -5.4],
            crpix: [500.5, 400.5],
            cd: [[-2.0e-4, 0.0], [0.0, 2.0e-4]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        }
    }

    #[test]
    fn forward_matches_hand_evaluation_in_both_row_orders() {
        for ro in [RowOrder::BottomUp, RowOrder::TopDown] {
            let s = SipSolution::parse(&cards(false), &file_linear(), ro, 800)
                .unwrap()
                .unwrap();
            // File pixel (700, 150): u = 199.5, v = −250.5.
            let (u, v) = (199.5f64, -250.5f64);
            let f = 1.5e-6 * u * u - 2.0e-6 * u * v + 1.0e-6 * v * v;
            let g = -1.0e-6 * u * u + 1.2e-6 * u * v + 2.0e-6 * v * v;
            let want = (-2.0e-4 * (u + f), 2.0e-4 * (v + g));
            let y_img = match ro {
                RowOrder::TopDown => 150.0 - 0.5,
                RowOrder::BottomUp => 800.0 - 150.0 + 0.5,
            };
            let got = s.image_to_native(700.0 - 0.5, y_img);
            assert!(
                (got.0 - want.0).abs() < 1e-15 && (got.1 - want.1).abs() < 1e-15,
                "{ro:?}: {got:?} vs {want:?}"
            );
            // At the reference pixel the distortion vanishes.
            let ref_img = [s.linear().crpix[0] - 0.5, s.linear().crpix[1] - 0.5];
            let at_ref = s.image_to_native(ref_img[0], ref_img[1]);
            assert!(at_ref.0.abs() < 1e-18 && at_ref.1.abs() < 1e-18);
        }
    }

    #[test]
    fn inverse_round_trips_with_and_without_ap_bp() {
        for with_inv in [false, true] {
            let s = SipSolution::parse(&cards(with_inv), &file_linear(), RowOrder::BottomUp, 800)
                .unwrap()
                .unwrap();
            for (x, y) in [
                (0.0, 0.0),
                (999.0, 0.0),
                (0.0, 799.0),
                (999.0, 799.0),
                (123.4, 567.8),
                (500.0, 400.0),
            ] {
                let (xi, eta) = s.image_to_native(x, y);
                let (bx, by) = s.native_to_image(xi, eta);
                assert!(
                    (bx - x).abs() < 1e-7 && (by - y).abs() < 1e-7,
                    "inv={with_inv} ({x},{y}) → ({bx},{by})"
                );
            }
            s.validate(1000, 800).unwrap();
        }
        // The AP/BP seed alone (no Newton) is already far closer than the
        // plain linear seed: proves the inverse cards were parsed and applied.
        let s = SipSolution::parse(&cards(true), &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        let (xi, eta) = s.image_to_native(950.0, 50.0);
        let (sx, sy) = s.inverse_seed(xi, eta);
        let ap_err = (sx - 950.0).hypot(sy - 50.0);
        assert!(ap_err < 1e-3, "AP/BP seed is {ap_err} px off");
        let none = SipSolution::parse(&cards(false), &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        let (nx, ny) = none.inverse_seed(xi, eta);
        let lin_err = (nx - 950.0).hypot(ny - 50.0);
        assert!(
            lin_err > 0.1 && lin_err > 100.0 * ap_err,
            "linear seed {lin_err} px is not clearly worse than the AP/BP seed {ap_err} px"
        );
    }

    #[test]
    fn parse_edge_cases() {
        assert!(
            SipSolution::parse(&[], &file_linear(), RowOrder::BottomUp, 800)
                .unwrap()
                .is_none()
        );
        let mut c = cards(false);
        c.retain(|k| k.name != "B_ORDER");
        assert!(
            SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
                .unwrap()
                .is_some(),
            "B_ORDER defaults to A_ORDER"
        );
        let mut c = cards(false);
        c[0] = kw("A_ORDER", "12");
        assert!(
            SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
                .unwrap_err()
                .contains("A_ORDER")
        );
        let mut c = cards(false);
        c.push(kw("A_3_0", "1.0"));
        assert!(
            SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
                .unwrap_err()
                .contains("A_3_0"),
            "term above the declared order"
        );
        // A singular CD matrix has no usable inverse.
        let mut lin = file_linear();
        lin.cd = [[1.0e-4, 2.0e-4], [0.5e-4, 1.0e-4]];
        let e = SipSolution::parse(&cards(false), &lin, RowOrder::BottomUp, 800).unwrap_err();
        assert!(e.contains("singular"), "{e}");
    }

    #[test]
    fn orders_up_to_nine_parse_and_higher_are_refused() {
        let c = vec![
            kw("A_ORDER", "9"),
            kw("B_ORDER", "9"),
            kw("A_9_0", "1.0E-26"),
            kw("A_1_1", "-2.0E-6"),
            kw("B_0_9", "1.0E-26"),
            kw("B_2_0", "-1.0E-6"),
        ];
        let s = SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        // The order-9 term is actually evaluated: image (1000, 0) is file
        // pixel (1000.5, 800.5), i.e. u = 500, v = 400.
        let (u, v) = (500.0f64, 400.0f64);
        let f = 1.0e-26 * u.powi(9) - 2.0e-6 * u * v;
        let want = -2.0e-4 * (u + f);
        let (xi, _) = s.image_to_native(1000.0, 0.0);
        assert!((xi - want).abs() < 1e-15, "{xi} vs {want}");
        s.validate(1000, 800).unwrap();
        for (x, y) in [(0.0, 0.0), (999.0, 799.0), (321.0, 123.0)] {
            let (xi, eta) = s.image_to_native(x, y);
            let (bx, by) = s.native_to_image(xi, eta);
            assert!((bx - x).abs() < 1e-7 && (by - y).abs() < 1e-7, "({x},{y})");
        }
        let mut c10 = c.clone();
        c10[0] = kw("A_ORDER", "10");
        let e = SipSolution::parse(&c10, &file_linear(), RowOrder::BottomUp, 800).unwrap_err();
        assert!(e.contains("A_ORDER") && e.contains("0..=9"), "{e}");
    }

    #[test]
    fn only_both_inverse_polynomials_seed_the_newton_iteration() {
        // AP without BP is not a usable inverse: the seed stays linear.
        let mut c = cards(true);
        c.retain(|k| !k.name.starts_with("BP_"));
        let half = SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        let plain = SipSolution::parse(&cards(false), &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        let (xi, eta) = half.image_to_native(950.0, 50.0);
        assert_eq!(half.inverse_seed(xi, eta), plain.inverse_seed(xi, eta));
        // The inverse still lands on the pixel, seed or no seed.
        let (bx, by) = half.native_to_image(xi, eta);
        assert!((bx - 950.0).abs() < 1e-7 && (by - 50.0).abs() < 1e-7);
    }

    #[test]
    fn newton_yields_nan_when_the_forward_map_is_not_invertible() {
        // f = −u collapses the ξ axis: the Jacobian is singular everywhere.
        let c = vec![kw("A_ORDER", "1"), kw("A_1_0", "-1.0"), kw("B_ORDER", "1")];
        let s = SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        let (bx, by) = s.native_to_image(0.0, 0.0);
        assert!(bx.is_nan() && by.is_nan(), "({bx},{by})");
        // ... and such a model never reaches the grid sampler.
        assert!(s.validate(1000, 800).is_err());
    }

    #[test]
    fn validation_rejects_wild_distortion() {
        let mut c = cards(false);
        c[2] = kw("A_2_0", "1.0E-3"); // 1000× too large: corners deviate by degrees
        let s = SipSolution::parse(&c, &file_linear(), RowOrder::BottomUp, 800)
            .unwrap()
            .unwrap();
        let e = s.validate(1000, 800).unwrap_err();
        assert!(e.contains("linear"), "{e}");
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

    fn oracle() -> Vec<Case> {
        serde_json::from_str(include_str!("../../tests/fixtures/sip_oracle.json"))
            .expect("fixture parses")
    }

    #[test]
    fn matches_astropy_oracle_directly_and_through_grids() {
        use crate::astrometry::fits_wcs::{linear_from_keywords, model_from_keywords};
        use crate::astrometry::tan_deproject;
        for case in oracle() {
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
            let file_lin = linear_from_keywords(&cards).unwrap();
            let sip = SipSolution::parse(&cards, &file_lin, ro, h)
                .unwrap()
                .expect("SIP present");
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
                let (xi, eta) = sip.image_to_native(x, y);
                let (r1, d1) = tan_deproject(model.linear.crval, xi, eta);
                let dra = ((r1 - ra + 180.0).rem_euclid(360.0) - 180.0) * dec.to_radians().cos();
                assert!(
                    dra.abs() < 1e-9 && (d1 - dec).abs() < 1e-9,
                    "{} direct fwd at ({i},{j}): {dra:e} {:e}",
                    case.name,
                    d1 - dec
                );
                let (r2, d2) = model.pixel_to_sky(x, y);
                let dra2 = ((r2 - ra + 180.0).rem_euclid(360.0) - 180.0) * dec.to_radians().cos();
                assert!(
                    dra2.abs() < 2e-7 && (d2 - dec).abs() < 2e-7,
                    "{} grid fwd at ({i},{j}): {dra2:e} {:e}",
                    case.name,
                    d2 - dec
                );
            }
            for &[ra, dec, i, j] in &case.inverse {
                let (x, y) = to_img(i, j);
                let (xi, eta) =
                    crate::astrometry::tan_project_checked(model.linear.crval, ra, dec).unwrap();
                let (bx, by) = sip.native_to_image(xi, eta);
                assert!(
                    (bx - x).abs() < 1e-4 && (by - y).abs() < 1e-4,
                    "{} direct inv: ({bx},{by}) vs ({x},{y})",
                    case.name
                );
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
