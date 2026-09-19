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

/// True for a finite, strictly positive value.
pub(crate) fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
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
        if !positive(self.r0) {
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
        if self.kernel.has_shape() && !positive(self.eps2) {
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

/// Wendland C2 weight `(1−t)⁴(4t+1)` on `[0, 1)`, zero beyond.
pub(crate) fn wendland_c2(t: f64) -> f64 {
    if t >= 1.0 {
        return 0.0;
    }
    let u = 1.0 - t;
    let u2 = u * u;
    u2 * u2 * (4.0 * t + 1.0)
}

/// A vector-valued spline: X and Y component splines (which may or may not
/// share nodes — sharing is an encoding detail, both are stored in full).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VectorSpline {
    /// X component.
    pub x: ScalarSpline,
    /// Y component.
    pub y: ScalarSpline,
}

impl VectorSpline {
    /// Structural checks on both components.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.x.validate().map_err(|e| format!("X: {e}"))?;
        self.y.validate().map_err(|e| format!("Y: {e}"))
    }

    /// Evaluate both components at source coordinates `(x, y)`.
    pub(crate) fn eval(&self, x: f64, y: f64) -> (f64, f64) {
        (self.x.eval(x, y), self.y.eval(x, y))
    }
}

/// A Local term: a spline with compact support on a disc (spec §11.5.3.7.4.1).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LocalTerm {
    /// Support disc center, source coordinates.
    pub center: [f64; 2],
    /// Support disc radius, source units.
    pub radius: f64,
    /// The term's spline.
    pub spline: VectorSpline,
}

/// Uniform bucket index over Local disc centers: each cell lists the discs
/// whose bounding boxes intersect it. Rebuilt from centers and radii alone
/// (the spec keeps spatial structure out of the model).
#[derive(Debug, Clone, Default)]
struct DiscIndex {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
}

impl DiscIndex {
    fn build(local: &[LocalTerm]) -> DiscIndex {
        if local.is_empty() {
            return DiscIndex::default();
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        let mut rsum = 0.0;
        for t in local {
            x0 = x0.min(t.center[0] - t.radius);
            y0 = y0.min(t.center[1] - t.radius);
            x1 = x1.max(t.center[0] + t.radius);
            y1 = y1.max(t.center[1] + t.radius);
            rsum += t.radius;
        }
        // Cell edge ≈ mean disc diameter, capped so the index stays small.
        let cell0 = (2.0 * rsum / local.len() as f64).max(1e-9);
        let nx = (((x1 - x0) / cell0).ceil() as usize).clamp(1, 4096);
        let ny = (((y1 - y0) / cell0).ceil() as usize).clamp(1, 4096);
        let cell = ((x1 - x0) / nx as f64)
            .max((y1 - y0) / ny as f64)
            .max(cell0);
        let mut cells = vec![Vec::new(); nx * ny];
        let clamp = |f: f64, n: usize| (f.floor().max(0.0) as usize).min(n - 1);
        for (i, t) in local.iter().enumerate() {
            let cx0 = clamp((t.center[0] - t.radius - x0) / cell, nx);
            let cx1 = clamp((t.center[0] + t.radius - x0) / cell, nx);
            let cy0 = clamp((t.center[1] - t.radius - y0) / cell, ny);
            let cy1 = clamp((t.center[1] + t.radius - y0) / cell, ny);
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    cells[cy * nx + cx].push(i as u32);
                }
            }
        }
        DiscIndex {
            x0,
            y0,
            cell,
            nx,
            ny,
            cells,
        }
    }

    /// Disc indices whose bounding boxes cover the cell containing `(x, y)`
    /// (a superset of the discs actually covering the point); empty outside
    /// the indexed area.
    fn candidates(&self, x: f64, y: f64) -> &[u32] {
        if self.cells.is_empty() {
            return &[];
        }
        let fx = (x - self.x0) / self.cell;
        let fy = (y - self.y0) / self.cell;
        if fx < 0.0 || fy < 0.0 {
            return &[];
        }
        let (cx, cy) = (fx as usize, fy as usize);
        if cx >= self.nx || cy >= self.ny {
            return &[];
        }
        &self.cells[cy * self.nx + cx]
    }
}

/// The residual field of one direction: a normalized Wendland-weighted sum
/// of term splines (spec §11.5.3.7.4, PCL `RecursivePointSurfaceSpline::Residual`).
#[derive(Debug, Clone)]
pub(crate) struct TermModel {
    /// The Global term (weight 1 everywhere), if any.
    pub global: Option<VectorSpline>,
    /// Local terms with compact support.
    pub local: Vec<LocalTerm>,
    /// Fallback term as `(coverage threshold t0, spline)`, if any.
    pub fallback: Option<(f64, VectorSpline)>,
    index: DiscIndex,
}

impl TermModel {
    /// Assemble and validate a term model (spec presence rules: at least one
    /// Global or Local term; Fallback only with Local terms).
    pub(crate) fn new(
        global: Option<VectorSpline>,
        local: Vec<LocalTerm>,
        fallback: Option<(f64, VectorSpline)>,
    ) -> Result<TermModel, String> {
        if global.is_none() && local.is_empty() {
            return Err("empty distortion model (no Global or Local terms)".into());
        }
        if fallback.is_some() && local.is_empty() {
            return Err("a Fallback term requires Local terms".into());
        }
        if let Some(g) = &global {
            g.validate().map_err(|e| format!("Global term: {e}"))?;
        }
        for (k, t) in local.iter().enumerate() {
            if !positive(t.radius) || !t.center.iter().all(|c| c.is_finite()) {
                return Err(format!("Local term {k}: invalid center/radius"));
            }
            t.spline
                .validate()
                .map_err(|e| format!("Local term {k}: {e}"))?;
        }
        if let Some((t0, f)) = &fallback {
            if !positive(*t0) {
                return Err("Fallback threshold must be > 0".into());
            }
            f.validate().map_err(|e| format!("Fallback term: {e}"))?;
        }
        let index = DiscIndex::build(&local);
        Ok(TermModel {
            global,
            local,
            fallback,
            index,
        })
    }

    /// Residual at source coordinates `(x, y)`.
    pub(crate) fn residual(&self, x: f64, y: f64) -> (f64, f64) {
        let (mut sx, mut sy, mut ws) = (0.0, 0.0, 0.0);
        if let Some(g) = &self.global {
            let (gx, gy) = g.eval(x, y);
            sx += gx;
            sy += gy;
            ws += 1.0;
        }
        for &i in self.index.candidates(x, y) {
            let t = &self.local[i as usize];
            let tt = (x - t.center[0]).hypot(y - t.center[1]) / t.radius;
            if tt < 1.0 {
                let w = wendland_c2(tt);
                let (vx, vy) = t.spline.eval(x, y);
                sx += w * vx;
                sy += w * vy;
                ws += w;
            }
        }
        if let Some((t0, f)) = &self.fallback
            && ws < *t0
        {
            let wc = wendland_c2(ws / t0);
            let (fx, fy) = f.eval(x, y);
            return ((sx + wc * fx) / (ws + wc), (sy + wc * fy) / (ws + wc));
        }
        if ws > 0.0 {
            return (sx / ws, sy / ws);
        }
        // No coverage and no Fallback: nearest Local term by normalized distance.
        let nearest = self
            .local
            .iter()
            .map(|t| (x - t.center[0]).hypot(y - t.center[1]) / t.radius)
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        match nearest {
            Some(i) => self.local[i].spline.eval(x, y),
            None => (0.0, 0.0),
        }
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
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &[0.0; 9]);
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
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &[1.0; 9]);
        s.coef.pop();
        assert!(s.validate().is_err(), "coefficient count");
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &[1.0; 9]);
        s.r0 = 0.0;
        assert!(s.validate().is_err(), "r0");
        let mut s = fit(Kernel::ThinPlateSpline, 2, true, 0.0, &nodes, &[1.0; 9]);
        s.polynomial = false;
        assert!(s.validate().is_err(), "TPS needs polynomial");
        let mut s = fit(Kernel::VariableOrder, 3, true, 0.0, &nodes, &[1.0; 9]);
        s.order = 2;
        assert!(s.validate().is_err(), "VariableOrder needs order >= 3");
    }

    fn affine_vector(nodes: &[[f64; 2]], a: f64, b: f64) -> VectorSpline {
        let zx: Vec<f64> = nodes.iter().map(|p| a * p[0] + 0.5 * p[1]).collect();
        let zy: Vec<f64> = nodes.iter().map(|p| b * p[1] - 0.25 * p[0]).collect();
        VectorSpline {
            x: fit(Kernel::ThinPlateSpline, 2, true, 0.0, nodes, &zx),
            y: fit(Kernel::ThinPlateSpline, 2, true, 0.0, nodes, &zy),
        }
    }

    #[test]
    fn wendland_c2_has_unit_value_and_compact_support() {
        assert_eq!(wendland_c2(0.0), 1.0);
        assert!((wendland_c2(0.5) - 0.0625 * 3.0).abs() < 1e-12);
        assert_eq!(wendland_c2(1.0), 0.0);
        assert_eq!(wendland_c2(1.7), 0.0);
    }

    #[test]
    fn global_only_model_equals_the_vector_spline() {
        let nodes = lattice(4, 100.0);
        let g = affine_vector(&nodes, 0.1, 0.2);
        let m = TermModel::new(Some(g.clone()), Vec::new(), None).unwrap();
        for (x, y) in [(100.0, 50.0), (137.0, 91.0), (600.0, -20.0)] {
            let a = m.residual(x, y);
            let b = g.eval(x, y);
            assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
        }
    }

    #[test]
    fn identical_local_terms_partition_to_the_same_value() {
        // Two overlapping discs with the same spline: the normalized weighted
        // sum must equal that spline everywhere both (or either) cover.
        let nodes = lattice(4, 100.0);
        let g = affine_vector(&nodes, 0.1, 0.2);
        let local = vec![
            LocalTerm {
                center: [120.0, 80.0],
                radius: 90.0,
                spline: g.clone(),
            },
            LocalTerm {
                center: [180.0, 120.0],
                radius: 90.0,
                spline: g.clone(),
            },
        ];
        let m = TermModel::new(None, local, None).unwrap();
        for (x, y) in [(150.0, 100.0), (60.0, 80.0), (250.0, 150.0)] {
            let a = m.residual(x, y);
            let b = g.eval(x, y);
            assert!(
                (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9,
                "at ({x},{y})"
            );
        }
    }

    #[test]
    fn fallback_takes_over_where_coverage_fades_and_is_continuous() {
        let nodes = lattice(4, 100.0);
        let l = affine_vector(&nodes, 0.1, 0.2);
        let f = affine_vector(&nodes, -0.3, 0.05);
        let local = vec![LocalTerm {
            center: [150.0, 100.0],
            radius: 50.0,
            spline: l.clone(),
        }];
        let t0 = 0.2;
        let m = TermModel::new(None, local, Some((t0, f.clone()))).unwrap();
        // Far outside the disc: fallback alone.
        let far = m.residual(400.0, 400.0);
        let ff = f.eval(400.0, 400.0);
        assert!((far.0 - ff.0).abs() < 1e-12 && (far.1 - ff.1).abs() < 1e-12);
        // At the center: weight 1 ≥ t0, local alone.
        let c = m.residual(150.0, 100.0);
        let ll = l.eval(150.0, 100.0);
        assert!((c.0 - ll.0).abs() < 1e-12);
        // Continuity across the t0 boundary: the largest change over a step
        // must shrink with the step size (a discontinuity would give the same
        // largest change at every step size). Sampled through the blend
        // region and out past the disc edge.
        let max_jump = |step: f64| {
            let n = (100.0 / step) as usize;
            (0..n)
                .map(|i| {
                    let x = 150.0 + i as f64 * step;
                    let a = m.residual(x, 100.0);
                    let b = m.residual(x + step, 100.0);
                    (b.0 - a.0).abs().max((b.1 - a.1).abs())
                })
                .fold(0.0, f64::max)
        };
        let (coarse, fine) = (max_jump(0.5), max_jump(0.05));
        assert!(coarse <= 12.0 * fine, "coarse {coarse} vs fine {fine}");
    }

    #[test]
    fn no_coverage_without_fallback_uses_nearest_disc() {
        let nodes = lattice(4, 100.0);
        let a = affine_vector(&nodes, 0.1, 0.2);
        let b = affine_vector(&nodes, -0.3, 0.05);
        let local = vec![
            LocalTerm {
                center: [0.0, 0.0],
                radius: 10.0,
                spline: a.clone(),
            },
            LocalTerm {
                center: [1000.0, 0.0],
                radius: 10.0,
                spline: b.clone(),
            },
        ];
        let m = TermModel::new(None, local, None).unwrap();
        let near_b = m.residual(900.0, 5.0);
        let bb = b.eval(900.0, 5.0);
        assert!((near_b.0 - bb.0).abs() < 1e-12 && (near_b.1 - bb.1).abs() < 1e-12);
    }

    #[test]
    fn disc_index_matches_brute_force_candidates() {
        let nodes = lattice(3, 10.0);
        let s = affine_vector(&nodes, 0.1, 0.2);
        let mut local = Vec::new();
        for i in 0..40 {
            let (cx, cy) = ((i * 37 % 500) as f64, (i * 91 % 300) as f64);
            local.push(LocalTerm {
                center: [cx, cy],
                radius: 20.0 + (i % 5) as f64 * 15.0,
                spline: s.clone(),
            });
        }
        let m = TermModel::new(None, local, None).unwrap();
        for (x, y) in [
            (0.0, 0.0),
            (250.0, 150.0),
            (499.0, 299.0),
            (-50.0, 400.0),
            (123.4, 56.7),
        ] {
            let covers = |i: usize| {
                let t = &m.local[i];
                (x - t.center[0]).hypot(y - t.center[1]) < t.radius
            };
            let brute: Vec<usize> = (0..m.local.len()).filter(|&i| covers(i)).collect();
            let mut idx: Vec<usize> = m
                .index
                .candidates(x, y)
                .iter()
                .map(|&i| i as usize)
                .filter(|&i| covers(i))
                .collect();
            idx.sort_unstable();
            assert_eq!(idx, brute, "at ({x},{y})");
        }
    }

    #[test]
    fn term_model_rejects_fallback_without_local_and_empty_models() {
        let nodes = lattice(3, 10.0);
        let s = affine_vector(&nodes, 0.1, 0.2);
        assert!(TermModel::new(None, Vec::new(), None).is_err());
        assert!(TermModel::new(Some(s.clone()), Vec::new(), Some((0.1, s.clone()))).is_err());
        assert!(
            TermModel::new(
                None,
                vec![LocalTerm {
                    center: [0.0, 0.0],
                    radius: 0.0,
                    spline: s.clone()
                }],
                None
            )
            .is_err()
        );
    }
}
