//! Radial basis function surface splines as serialized by the XISF 1.0
//! revision 1 `AstrometricSolution:DistortionModel:*` properties (spec
//! §11.5.3.7.4). Reference implementation: PCL 2.10.8 `SurfaceSpline.cpp`
//! (`KernelFunction<>`, `PolynomialValue`, `RecursivePointSurfaceSpline::Residual`).
//!
//! A scalar spline at source point `(x, y)` with normalization `(x0, y0, r0)`:
//! `u = r0·(x − x0)`, `v = r0·(y − y0)`,
//! `s = Σ_i c_i·φ(‖(u,v) − N_i‖²) + Σ_k d_k·u^a·v^b`, monomials ordered by
//! total degree then descending power of `u` (1, u, v, u², uv, v², …).
//! Nodes are stored already normalized. Coefficients: `n` radial first, then
//! `q = m(m+1)/2` polynomial (`0` without a polynomial part).

/// Basis function vocabulary (spec Table 17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kernel {
    /// `φ = r²·ln r`; polynomial part required; order sets its degree.
    ThinPlateSpline,
    /// `φ = (r²)^(m−1)·ln(r²)`, `m ≥ 3`; polynomial required.
    VariableOrder,
    /// `φ = exp(−ε²r²)`.
    Gaussian,
    /// `φ = sqrt(1 + ε²r²)`.
    Multiquadric,
    /// `φ = 1/sqrt(1 + ε²r²)`.
    InverseMultiquadric,
    /// `φ = 1/(1 + ε²r²)`.
    InverseQuadratic,
}

impl Kernel {
    /// Parse a spec basis-function identifier (exact match).
    pub(crate) fn parse(id: &str) -> Option<Kernel> {
        Some(match id {
            "ThinPlateSpline" => Kernel::ThinPlateSpline,
            "VariableOrder" => Kernel::VariableOrder,
            "Gaussian" => Kernel::Gaussian,
            "Multiquadric" => Kernel::Multiquadric,
            "InverseMultiquadric" => Kernel::InverseMultiquadric,
            "InverseQuadratic" => Kernel::InverseQuadratic,
            _ => return None,
        })
    }

    /// True for kernels with a shape parameter ε.
    pub(crate) fn has_shape(self) -> bool {
        !matches!(self, Kernel::ThinPlateSpline | Kernel::VariableOrder)
    }

    /// True for kernels whose polynomial part is mandatory.
    pub(crate) fn requires_polynomial(self) -> bool {
        matches!(self, Kernel::ThinPlateSpline | Kernel::VariableOrder)
    }

    /// Kernel value for squared normalized distance `r2` (`e2` = ε²).
    /// The logarithmic kernels take their limit value 0 at `r2 = 0`.
    pub(crate) fn phi(self, r2: f64, e2: f64, order: u32) -> f64 {
        match self {
            Kernel::ThinPlateSpline => {
                if r2 <= 0.0 {
                    0.0
                } else {
                    0.5 * r2 * r2.ln()
                }
            }
            Kernel::VariableOrder => {
                if r2 <= 0.0 {
                    return 0.0;
                }
                let mut e = r2.ln();
                for _ in 1..order {
                    e *= r2;
                }
                e
            }
            Kernel::Gaussian => (-e2 * r2).exp(),
            Kernel::Multiquadric => (1.0 + e2 * r2).sqrt(),
            Kernel::InverseMultiquadric => 1.0 / (1.0 + e2 * r2).sqrt(),
            Kernel::InverseQuadratic => 1.0 / (1.0 + e2 * r2),
        }
    }
}

/// Monomials of total degree `< order` in `u, v`, in spec order (by total
/// degree, then descending power of `u`).
pub(crate) fn monomials(order: u32, u: f64, v: f64) -> Vec<f64> {
    let m = order as usize;
    let mut up = vec![1.0; m];
    let mut vp = vec![1.0; m];
    for k in 1..m {
        up[k] = up[k - 1] * u;
        vp[k] = vp[k - 1] * v;
    }
    let mut out = Vec::with_capacity(m * (m + 1) / 2);
    for dg in 0..m {
        for ix in (0..=dg).rev() {
            out.push(up[ix] * vp[dg - ix]);
        }
    }
    out
}

/// One scalar surface spline record (spec §11.5.3.7.4.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScalarSpline {
    /// Basis function.
    pub kernel: Kernel,
    /// Order `m` (polynomial degree + 1; kernel order for `VariableOrder`).
    pub order: u32,
    /// Whether a polynomial part is present.
    pub polynomial: bool,
    /// Normalization offset x.
    pub x0: f64,
    /// Normalization offset y.
    pub y0: f64,
    /// Normalization scale (`u = r0·(x − x0)`).
    pub r0: f64,
    /// Normalized node coordinates.
    pub nodes: Vec<[f64; 2]>,
    /// `n` radial coefficients followed by `q` polynomial coefficients.
    pub coef: Vec<f64>,
    /// ε² in normalized units (0 when the kernel has no shape parameter).
    pub eps2: f64,
}

impl ScalarSpline {
    /// Number of polynomial coefficients: `m(m+1)/2`, or 0 without a
    /// polynomial part.
    pub(crate) fn poly_terms(order: u32, polynomial: bool) -> usize {
        if polynomial {
            (order as usize) * (order as usize + 1) / 2
        } else {
            0
        }
    }

    /// Structural checks: lengths, normalization, order/kernel rules.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(self.r0 > 0.0) || !self.r0.is_finite() {
            return Err("normalization scale must be > 0".into());
        }
        if !self.x0.is_finite() || !self.y0.is_finite() {
            return Err("non-finite normalization offset".into());
        }
        if self.order < 2 || self.order > 16 {
            return Err(format!("unsupported spline order {}", self.order));
        }
        if self.kernel == Kernel::VariableOrder && self.order < 3 {
            return Err("VariableOrder requires order >= 3".into());
        }
        if self.kernel.requires_polynomial() && !self.polynomial {
            return Err("polynomial part is mandatory for this basis function".into());
        }
        if self.kernel.has_shape() && !(self.eps2 > 0.0) {
            return Err("missing or invalid shape parameter".into());
        }
        if self.nodes.len() < 3 {
            return Err("fewer than three spline nodes".into());
        }
        let want = self.nodes.len() + Self::poly_terms(self.order, self.polynomial);
        if self.coef.len() != want {
            return Err(format!(
                "coefficient count {} != nodes + polynomial terms {want}",
                self.coef.len()
            ));
        }
        if self
            .nodes
            .iter()
            .flatten()
            .chain(&self.coef)
            .any(|v| !v.is_finite())
        {
            return Err("non-finite spline data".into());
        }
        Ok(())
    }

    /// Evaluate at source coordinates `(x, y)`.
    pub(crate) fn eval(&self, x: f64, y: f64) -> f64 {
        let u = self.r0 * (x - self.x0);
        let v = self.r0 * (y - self.y0);
        let n = self.nodes.len();
        let mut s = 0.0;
        for (node, c) in self.nodes.iter().zip(&self.coef) {
            let du = u - node[0];
            let dv = v - node[1];
            s += c * self.kernel.phi(du * du + dv * dv, self.eps2, self.order);
        }
        if self.polynomial {
            for (d, mono) in self.coef[n..].iter().zip(monomials(self.order, u, v)) {
                s += d * mono;
            }
        }
        s
    }
}

#[cfg(test)]
pub(crate) mod testfit {
    //! Dense interpolating fit for fixtures: solves `[[K, P], [Pᵀ, 0]]·[c; d]
    //! = [z; 0]` (or `K·c = z` without a polynomial part). Nodes are
    //! normalized with `x0/y0` = centroid and `r0 = 1 / max |offset|`, the
    //! layout encoders write.
    use super::*;

    pub(crate) fn fit(
        kernel: Kernel,
        order: u32,
        polynomial: bool,
        eps: f64,
        nodes: &[[f64; 2]],
        z: &[f64],
    ) -> ScalarSpline {
        let n = nodes.len();
        assert_eq!(z.len(), n);
        let x0 = nodes.iter().map(|p| p[0]).sum::<f64>() / n as f64;
        let y0 = nodes.iter().map(|p| p[1]).sum::<f64>() / n as f64;
        let spread = nodes
            .iter()
            .map(|p| (p[0] - x0).abs().max((p[1] - y0).abs()))
            .fold(0.0, f64::max);
        let r0 = 1.0 / spread;
        let nn: Vec<[f64; 2]> = nodes
            .iter()
            .map(|p| [r0 * (p[0] - x0), r0 * (p[1] - y0)])
            .collect();
        let q = ScalarSpline::poly_terms(order, polynomial);
        let m = n + q;
        let eps2 = eps * eps;
        let mut a = vec![0.0; m * m];
        let mut b = vec![0.0; m];
        for i in 0..n {
            for j in 0..n {
                let dx = nn[i][0] - nn[j][0];
                let dy = nn[i][1] - nn[j][1];
                a[i * m + j] = kernel.phi(dx * dx + dy * dy, eps2, order);
            }
            let mono = monomials(order, nn[i][0], nn[i][1]);
            for (k, v) in mono.iter().take(q).enumerate() {
                a[i * m + n + k] = *v;
                a[(n + k) * m + i] = *v;
            }
            b[i] = z[i];
        }
        let coef = crate::linalg::solve_dense(&mut a, &mut b, m).expect("fit system solvable");
        ScalarSpline {
            kernel,
            order,
            polynomial,
            x0,
            y0,
            r0,
            nodes: nn,
            coef,
            eps2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testfit::fit;

    fn lattice(n: usize, span: f64) -> Vec<[f64; 2]> {
        let mut v = Vec::new();
        for i in 0..n {
            for j in 0..n {
                v.push([
                    i as f64 * span / (n - 1) as f64 + 100.0,
                    j as f64 * span / (n - 1) as f64 + 50.0,
                ]);
            }
        }
        v
    }

    #[test]
    fn kernel_identifiers_follow_the_spec_vocabulary() {
        assert_eq!(
            Kernel::parse("ThinPlateSpline"),
            Some(Kernel::ThinPlateSpline)
        );
        assert_eq!(Kernel::parse("VariableOrder"), Some(Kernel::VariableOrder));
        assert_eq!(
            Kernel::parse("InverseQuadratic"),
            Some(Kernel::InverseQuadratic)
        );
        assert_eq!(Kernel::parse("thinplatespline"), None);
        assert_eq!(Kernel::parse("DDMThinPlateSpline"), None);
        assert!(Kernel::Gaussian.has_shape() && !Kernel::ThinPlateSpline.has_shape());
        assert!(
            Kernel::VariableOrder.requires_polynomial()
                && !Kernel::Multiquadric.requires_polynomial()
        );
    }

    #[test]
    fn kernels_match_pcl_formulas() {
        let r2 = 2.25; // r = 1.5
        assert!((Kernel::ThinPlateSpline.phi(r2, 0.0, 2) - r2 * 1.5f64.ln()).abs() < 1e-12);
        assert!((Kernel::VariableOrder.phi(r2, 0.0, 3) - r2 * r2 * r2.ln()).abs() < 1e-12);
        assert_eq!(Kernel::ThinPlateSpline.phi(0.0, 0.0, 2), 0.0);
        assert_eq!(Kernel::VariableOrder.phi(0.0, 0.0, 4), 0.0);
        let e2 = 0.3;
        assert!((Kernel::Gaussian.phi(r2, e2, 2) - (-e2 * r2).exp()).abs() < 1e-12);
        assert!((Kernel::Multiquadric.phi(r2, e2, 2) - (1.0 + e2 * r2).sqrt()).abs() < 1e-12);
        assert!(
            (Kernel::InverseMultiquadric.phi(r2, e2, 2) - 1.0 / (1.0 + e2 * r2).sqrt()).abs()
                < 1e-12
        );
        assert!((Kernel::InverseQuadratic.phi(r2, e2, 2) - 1.0 / (1.0 + e2 * r2)).abs() < 1e-12);
    }

    #[test]
    fn monomial_order_is_degree_then_descending_x_power() {
        // order 3 → 1, x, y, x², xy, y²
        let m = monomials(3, 2.0, 3.0);
        assert_eq!(m, vec![1.0, 2.0, 3.0, 4.0, 6.0, 9.0]);
        assert_eq!(ScalarSpline::poly_terms(2, true), 3);
        assert_eq!(ScalarSpline::poly_terms(4, true), 10);
        assert_eq!(ScalarSpline::poly_terms(4, false), 0);
    }

    #[test]
    fn tps_interpolates_its_nodes_and_reproduces_a_smooth_field() {
        let nodes = lattice(6, 400.0);
        let f = |p: &[f64; 2]| 0.002 * p[0] - 0.001 * p[1] + 3e-6 * (p[0] - 300.0) * (p[1] - 250.0);
        let z: Vec<f64> = nodes.iter().map(f).collect();
        let s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &z);
        s.validate().unwrap();
        for (p, v) in nodes.iter().zip(&z) {
            assert!((s.eval(p[0], p[1]) - v).abs() < 1e-9, "node reproduction");
        }
        let (x, y) = (233.0, 171.0);
        assert!(
            (s.eval(x, y) - f(&[x, y])).abs() < 1e-3,
            "interior smoothness"
        );
    }

    #[test]
    fn tps_with_zero_radial_coefficients_is_exactly_affine() {
        let nodes = lattice(3, 10.0);
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &vec![0.0; 9]);
        s.coef = vec![0.0; 9];
        s.coef.extend([1.0, 2.0, -3.0]); // d0 + d1·u + d2·v in normalized coords
        let (x, y) = (107.0, 52.5);
        let (u, v) = (s.r0 * (x - s.x0), s.r0 * (y - s.y0));
        assert!((s.eval(x, y) - (1.0 + 2.0 * u - 3.0 * v)).abs() < 1e-12);
    }

    #[test]
    fn variable_order_3_and_gaussian_without_polynomial_interpolate_nodes() {
        let nodes = lattice(5, 200.0);
        let z: Vec<f64> = nodes
            .iter()
            .map(|p| (p[0] * 0.01).sin() + (p[1] * 0.02).cos())
            .collect();
        let s3 = fit(Kernel::VariableOrder, 3, true, 0.0, &nodes, &z);
        let sg = fit(Kernel::Gaussian, 2, false, 1.2, &nodes, &z);
        s3.validate().unwrap();
        sg.validate().unwrap();
        for (p, v) in nodes.iter().zip(&z) {
            assert!((s3.eval(p[0], p[1]) - v).abs() < 1e-8);
            assert!((sg.eval(p[0], p[1]) - v).abs() < 1e-6);
        }
    }

    #[test]
    fn validate_rejects_inconsistent_records() {
        let nodes = lattice(3, 10.0);
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &vec![1.0; 9]);
        s.coef.pop();
        assert!(s.validate().is_err(), "coefficient count");
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &vec![1.0; 9]);
        s.r0 = 0.0;
        assert!(s.validate().is_err(), "r0");
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &vec![1.0; 9]);
        s.polynomial = false;
        assert!(s.validate().is_err(), "TPS needs polynomial");
        let mut s = fit(Kernel::VariableOrder, 3, true, 0.0, &nodes, &vec![1.0; 9]);
        s.order = 2;
        assert!(s.validate().is_err(), "VariableOrder needs order >= 3");
    }
}
