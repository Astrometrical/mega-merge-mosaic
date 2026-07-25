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
//! Extraction to [`LinearWcs`] keeps PixInsight's top-down y axis (matching
//! both the XISF input and our stored row order):
//!
//! - `CRVAL1/2` = ReferenceCelestialCoordinates,
//! - `CRPIX1/2` = ReferenceImageCoordinates + 0.5 on both axes (PixInsight
//!   pixel centers sit at k + 0.5 in 0-based coords; FITS pixel centers sit at
//!   integer 1-based coords, so FITS = PI + 0.5),
//! - `CD1_1..CD2_2` = LinearTransformationMatrix exactly as stored (row 0 →
//!   axis 1 / ξ, row 1 → axis 2 / η),
//! - `CTYPE1/2` = `RA---`/`DEC--` + projection code (Gnomonic → `TAN`).
//!
//! **Card emission flips the y axis.** FITS WCS consumers — PixInsight
//! included, when it converts FITS keywords back to an astrometric solution —
//! interpret the WCS in the standard bottom-up frame (y = 1 at the bottom row
//! of the sky image; PCL maps `y_image_topdown = H + 0.5 − y_fits`),
//! regardless of `ROWORDER`. Our writer stores rows top-down, so
//! [`wcs_cards`] emits the solution reflected into that frame:
//! `CRPIX2 → H + 1 − CRPIX2` and the CD second *column* (`CD1_2`, `CD2_2` —
//! the dy-dependent terms) negated. Emitting the top-down solution verbatim
//! produced a north–south mirrored solution in PixInsight (verified against
//! catalog stars on the real Orion mosaic: stars landed at `2·CRPIX2 − y`,
//! empty sky at the predicted spots).
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
//!
//! ## Spline solutions ([`WcsModel`], raw solved panels)
//!
//! Solved-but-unregistered panels additionally carry
//! `PCL:AstrometricSolution:SplineWorldTransformation:*` properties. Every
//! fact below was verified on the real Orion raw panels (PixInsight 1.9.4;
//! `Version` = `2.0`, `RBFType` = `DDMThinPlateSpline`, `SplineOrder` = 2):
//!
//! - `PointGridInterpolation:{ImageToNative,NativeToImage}:{GridX,GridY,Rect,
//!   Delta}` are dense precomputed lookup grids for both directions of the
//!   spline transformation, serialized as attachment-located F64Matrix blocks.
//!   We interpolate these; the thin-plate-spline control points
//!   (`ControlPoints:*`, `PointSurfaceSpline:*`) are never evaluated.
//! - **"Native" coordinates are gnomonic tangent-plane offsets (ξ, η) in
//!   degrees about `ReferenceCelestialCoordinates`** — exactly the
//!   intermediate world coordinates of the TAN math in [`LinearWcs`] (ξ grows
//!   east, η north; sky = TAN-deproject about the reference point). Verified:
//!   the ImageToNative grid evaluates to (4e−6, 1.4e−5) deg ≈ 0.05″ from
//!   (0, 0) at `ReferenceImageCoordinates`; composing grid → TAN deprojection
//!   agrees with the linear WCS to 0.05″ at the reference pixel and to
//!   0.6–1.7″ at the frame corners (the actual distortion of this rig).
//! - **Grid layout**: `GridX`/`GridY` are row-major `rows × cols`; node
//!   `(r, c)` holds the transform output for input point
//!   `(x0 + c·Δ, y0 + r·Δ)` with `Rect = [x0, y0, x1, y1]`; `GridX` is the
//!   output x (native ξ, or image x), `GridY` the output y. Node counts are
//!   `1 + ⌈extent/Δ⌉` per axis. Orion panel 1 (4904×3232): ImageToNative
//!   Δ = 8 px, Rect = [0, 0, 4904, 3232] → 614 cols × 405 rows;
//!   NativeToImage Δ = 3.556e−3 deg, Rect ≈ [−0.733, −1.099, 0.733, 1.098]
//!   deg → 414 cols × 620 rows. Panel 7 (4888×3221) → 612×404, confirming
//!   per-panel dims. The layout is pinned by the corner consistency check —
//!   these panels are rotated ~90°, so a transposed/flipped layout is off by
//!   ~1 deg against the ≲ 180″ allowance.
//! - Grid inputs are PixInsight image coordinates (0-based, top-down, pixel k
//!   spanning [k, k+1]); `ImageToNative:Rect` equals the solved image bounds.
//! - `SplineWorldTransformation:LinearApproximation` (2×3) is
//!   `[LinearTransformationMatrix | −M·refimg]`, i.e. native ≈ M·(p −
//!   refimg); `Projective:{ImageToNative,NativeToImage}` are the same affine
//!   maps plus tiny projective terms (~2e−8). Both are consistency data only.
//! - `ReferenceNativeCoordinates` = (0, 90) and
//!   `CelestialPoleNativeCoordinates` = (180, 90): the standard zenithal
//!   native frame. [`WcsModel::from_properties`] refuses grids if either
//!   property carries a different (unimplemented) rotation.
//! - **MosaicByCoordinates content placement**: in the *registered* frames, a
//!   sky position's content sits at the ARRAY index equal to its
//!   span-convention solution coordinate — displaced (−0.5, −0.5) px from the
//!   span reading (star centers at solution coordinate (i, j) for array pixel
//!   (i, j), not (i + 0.5, j + 0.5)). Measured via the cross-frame star chain
//!   on the real Orion set: canvas-frame residual (+0.500, +0.500) ± 0.03 px,
//!   axis-aligned in the canvas frame although the raw panels are rotated
//!   ~90° — the rotation decouples the two sides, so of the four possible
//!   raw/canvas pixel-center conventions only "raw solution = span
//!   (centers at k + 0.5, as this module assumes), registered content =
//!   index-equals-span-coordinate" predicts the observed vector (the
//!   runner-up flips the sign of Δy). The raw-panel solutions themselves are
//!   span-convention and [`WcsModel`] handles them correctly.
//!
//! Layout assumptions are enforced per file at load time: grid dims must match
//! `Rect`/`Delta`, `ImageToNative:Rect` must equal the image bounds, the grid
//! must return ≈ (0, 0) at the reference pixel (inverse: the reference pixel
//! at (0, 0)), and grid corners must reproduce the linear solution within the
//! distortion allowance. Any violation yields `None` — a panel is treated as
//! unsolved rather than solved wrongly.

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
        let xi = self.cd[0][0] * dx + self.cd[0][1] * dy;
        let eta = self.cd[1][0] * dx + self.cd[1][1] * dy;
        tan_deproject(self.crval, xi, eta)
    }

    /// Map (RA, Dec) in degrees to a FITS pixel coordinate (TAN projection).
    pub fn sky_to_pixel(&self, ra: f64, dec: f64) -> (f64, f64) {
        let (xi, eta, _) = tan_project(self.crval, ra, dec);
        let det = self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0];
        let dx = (self.cd[1][1] * xi - self.cd[0][1] * eta) / det;
        let dy = (self.cd[0][0] * eta - self.cd[1][0] * xi) / det;
        (self.crpix[0] + dx, self.crpix[1] + dy)
    }
}

/// Gnomonic (TAN) native plane (ξ, η) → celestial (RA, Dec) about `crval`,
/// everything in degrees. ξ grows toward east, η toward north — the standard
/// zenithal native frame with `ReferenceNativeCoordinates` (0, 90) and
/// `CelestialPoleNativeCoordinates` (180, 90).
fn tan_deproject(crval: [f64; 2], xi_deg: f64, eta_deg: f64) -> (f64, f64) {
    let (xi, eta) = (xi_deg.to_radians(), eta_deg.to_radians());
    let (a0, d0) = (crval[0].to_radians(), crval[1].to_radians());
    let den = d0.cos() - eta * d0.sin();
    let ra = a0 + xi.atan2(den);
    let dec = ((d0.sin() + eta * d0.cos()) / xi.hypot(den)).atan();
    (ra.to_degrees().rem_euclid(360.0), dec.to_degrees())
}

/// Celestial → TAN native plane about `crval`, degrees. The third value is the
/// projection denominator (cosine of the angular distance to the tangent
/// point): ≤ 0 means the point is 90° or more away and (ξ, η) are meaningless.
fn tan_project(crval: [f64; 2], ra: f64, dec: f64) -> (f64, f64, f64) {
    let (a0, d0) = (crval[0].to_radians(), crval[1].to_radians());
    let (a, d) = (ra.to_radians(), dec.to_radians());
    let cos_da = (a - a0).cos();
    let den = d0.sin() * d.sin() + d0.cos() * d.cos() * cos_da;
    let xi = (d.cos() * (a - a0).sin() / den).to_degrees();
    let eta = ((d0.cos() * d.sin() - d0.sin() * d.cos() * cos_da) / den).to_degrees();
    (xi, eta, den)
}

/// [`tan_project`] with the hemisphere guard applied.
fn tan_project_checked(crval: [f64; 2], ra: f64, dec: f64) -> Option<(f64, f64)> {
    let (xi, eta, den) = tan_project(crval, ra, dec);
    (den > 1e-9).then_some((xi, eta))
}

/// One direction of PixInsight's `PointGridInterpolation`: a dense lookup grid
/// of a 2-D coordinate transform. Layout (verified empirically, see module
/// docs): `gx`/`gy` are row-major `rows × cols`; node `(r, c)` holds the
/// transform output for input point `(rect[0] + c·delta, rect[1] + r·delta)`.
#[derive(Debug, Clone)]
pub struct Grid2D {
    /// Input-domain bounds `[x0, y0, x1, y1]`.
    pub rect: [f64; 4],
    /// Node spacing in input units.
    pub delta: f64,
    pub rows: u32,
    pub cols: u32,
    /// Output x at each node, row-major `rows × cols`.
    pub gx: Vec<f64>,
    /// Output y at each node, row-major `rows × cols`.
    pub gy: Vec<f64>,
}

impl Grid2D {
    /// Evaluate at `(x, y)` with bicubic Catmull-Rom interpolation.
    ///
    /// Inputs are clamped to `rect`. Out-of-range ghost nodes at the borders
    /// are linearly extrapolated from the edge node pair, which keeps affine
    /// fields exact through the border cells.
    pub fn eval(&self, x: f64, y: f64) -> (f64, f64) {
        let fx = (x.clamp(self.rect[0], self.rect[2]) - self.rect[0]) / self.delta;
        let fy = (y.clamp(self.rect[1], self.rect[3]) - self.rect[1]) / self.delta;
        let (c0, tx) = Self::cell(fx, self.cols);
        let (r0, ty) = Self::cell(fy, self.rows);
        let wx = catmull_rom_weights(tx);
        let wy = catmull_rom_weights(ty);
        let (mut ox, mut oy) = (0.0, 0.0);
        for (j, wj) in wy.iter().enumerate() {
            let r = r0 + j as i64 - 1;
            for (i, wi) in wx.iter().enumerate() {
                let c = c0 + i as i64 - 1;
                let w = wi * wj;
                ox += w * self.node(&self.gx, r, c);
                oy += w * self.node(&self.gy, r, c);
            }
        }
        (ox, oy)
    }

    /// Cell index (node at the cell's low edge) and fractional offset within.
    fn cell(f: f64, n: u32) -> (i64, f64) {
        let c = (f.floor() as i64).clamp(0, n as i64 - 2);
        (c, f - c as f64)
    }

    /// Node value; indices one step outside the grid (the bicubic support at
    /// border cells) are linearly extrapolated from the nearest node pair.
    fn node(&self, g: &[f64], r: i64, c: i64) -> f64 {
        let (rows, cols) = (self.rows as i64, self.cols as i64);
        let at = |r: i64, c: i64| g[(r * cols + c) as usize];
        let row = |r: i64| {
            if c < 0 {
                2.0 * at(r, 0) - at(r, 1)
            } else if c >= cols {
                2.0 * at(r, cols - 1) - at(r, cols - 2)
            } else {
                at(r, c)
            }
        };
        if r < 0 {
            2.0 * row(0) - row(1)
        } else if r >= rows {
            2.0 * row(rows - 1) - row(rows - 2)
        } else {
            row(r)
        }
    }
}

/// Catmull-Rom (Keys, a = −1/2) weights for nodes at offsets −1..=2 around
/// fractional position `t` ∈ [0, 1]. Exact for polynomials up to degree 2.
fn catmull_rom_weights(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        -0.5 * t3 + t2 - 0.5 * t,
        1.5 * t3 - 2.5 * t2 + 1.0,
        -1.5 * t3 + 2.0 * t2 + 0.5 * t,
        0.5 * t3 - 0.5 * t2,
    ]
}

/// Full astrometric model for one solved panel: the linear TAN solution plus,
/// when the panel was solved with distortion correction, PixInsight's
/// precomputed spline lookup grids for both directions.
///
/// All pixel coordinates on this type are **PixInsight image coordinates**:
/// 0-based, top-down rows, pixel k spanning [k, k+1] (center at k + 0.5) —
/// the frame the grids are serialized in. ([`LinearWcs`] itself is FITS
/// 1-based; the +0.5 conversion happens internally on the fallback path.)
#[derive(Debug, Clone)]
pub struct WcsModel {
    /// Always present: fallback for linear-only solutions and sanity anchor.
    pub linear: LinearWcs,
    /// Pixel → native tangent-plane grid (present iff the solution is spline).
    pub image_to_native: Option<Grid2D>,
    /// Native tangent-plane → pixel grid (present iff the solution is spline).
    pub native_to_image: Option<Grid2D>,
    /// Solved image width in pixels.
    pub width: u64,
    /// Solved image height in pixels.
    pub height: u64,
}

const SPLINE_PREFIX: &str = "PCL:AstrometricSolution:SplineWorldTransformation:";

impl WcsModel {
    /// Build the model from a panel's XISF properties and image geometry.
    ///
    /// Linear-only solutions yield a model without grids. If any
    /// `SplineWorldTransformation` property is present, both interpolation
    /// grids must be present, well-formed, and pass the layout validations
    /// described in the module docs; otherwise the whole solution is refused
    /// (`None`) — never silently approximated by its linear part.
    pub fn from_properties(props: &[XisfProperty], width: u64, height: u64) -> Option<WcsModel> {
        let linear = wcs_from_properties(props)?;
        if !props.iter().any(|p| p.id.starts_with(SPLINE_PREFIX)) {
            return Some(WcsModel {
                linear,
                image_to_native: None,
                native_to_image: None,
                width,
                height,
            });
        }

        // The grid math below is specific to the gnomonic tangent plane.
        if linear.ctype[0] != "RA---TAN" {
            return None;
        }
        // Refuse a non-standard native frame — the deprojection would differ.
        let std_native = |id: &str, expect: [f64; 2]| match props.iter().find(|p| p.id == id) {
            None => true, // absent: the standard zenithal values are implied
            Some(p) => p.value.as_f64_vec().is_some_and(|v| {
                v.len() == 2 && (v[0] - expect[0]).abs() < 1e-9 && (v[1] - expect[1]).abs() < 1e-9
            }),
        };
        if !std_native(
            "PCL:AstrometricSolution:ReferenceNativeCoordinates",
            [0.0, 90.0],
        ) || !std_native(
            "PCL:AstrometricSolution:CelestialPoleNativeCoordinates",
            [180.0, 90.0],
        ) {
            return None;
        }

        let image_to_native = grid_from_properties(props, "ImageToNative")?;
        let native_to_image = grid_from_properties(props, "NativeToImage")?;

        // Layout validations (empirically derived; see module docs). refimg is
        // the reference point in PixInsight image coordinates.
        let refimg = [linear.crpix[0] - 0.5, linear.crpix[1] - 0.5];
        let r = image_to_native.rect;
        if r[0].abs() > 1.0
            || r[1].abs() > 1.0
            || (r[2] - width as f64).abs() > 1.0
            || (r[3] - height as f64).abs() > 1.0
        {
            return None; // the grid domain must be the solved image bounds
        }
        // The spline is anchored at the reference point: grid(refimg) ≈ (0,0)
        // native (measured 0.05″ on real data; 36″ allowance).
        let (xi, eta) = image_to_native.eval(refimg[0], refimg[1]);
        if xi.hypot(eta) > 0.01 {
            return None;
        }
        // ... and the inverse grid returns the reference pixel from (0, 0).
        let (ix, iy) = native_to_image.eval(0.0, 0.0);
        if (ix - refimg[0]).hypot(iy - refimg[1]) > 5.0 {
            return None;
        }
        // Grid corners must approximately reproduce the linear solution — the
        // check that pins down row/column ordering (measured 0.6–1.7″ on real
        // data; 180″ distortion allowance vs ~1 deg for a wrong layout).
        let m = linear.cd;
        for (cx, cy) in [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])] {
            let (gx, gy) = image_to_native.eval(cx, cy);
            let (dx, dy) = (cx - refimg[0], cy - refimg[1]);
            let (lx, ly) = (m[0][0] * dx + m[0][1] * dy, m[1][0] * dx + m[1][1] * dy);
            if (gx - lx).hypot(gy - ly) > 0.05 {
                return None;
            }
        }

        Some(WcsModel {
            linear,
            image_to_native: Some(image_to_native),
            native_to_image: Some(native_to_image),
            width,
            height,
        })
    }

    /// Map a PixInsight image coordinate to (RA, Dec) in degrees, through the
    /// distortion grid when present.
    pub fn pixel_to_sky(&self, x: f64, y: f64) -> (f64, f64) {
        match &self.image_to_native {
            Some(g) => {
                let (xi, eta) = g.eval(x, y);
                tan_deproject(self.linear.crval, xi, eta)
            }
            None => self.linear.pixel_to_sky(x + 0.5, y + 0.5),
        }
    }

    /// Map (RA, Dec) in degrees to a PixInsight image coordinate.
    ///
    /// `None` when the point is at/beyond 90° from the tangent point, or (on
    /// the grid path) outside the serialized native domain plus one grid cell
    /// of margin — beyond that the extrapolation is not trustworthy.
    pub fn sky_to_pixel(&self, ra: f64, dec: f64) -> Option<(f64, f64)> {
        let (xi, eta) = tan_project_checked(self.linear.crval, ra, dec)?;
        match &self.native_to_image {
            Some(g) => {
                let m = g.delta;
                if xi < g.rect[0] - m
                    || xi > g.rect[2] + m
                    || eta < g.rect[1] - m
                    || eta > g.rect[3] + m
                {
                    return None;
                }
                Some(g.eval(xi, eta))
            }
            None => {
                let (x, y) = self.linear.sky_to_pixel(ra, dec);
                Some((x - 0.5, y - 0.5))
            }
        }
    }

    /// True when the solution carries (and the model uses) distortion grids.
    pub fn is_spline(&self) -> bool {
        self.image_to_native.is_some()
    }
}

/// Read one `PointGridInterpolation` direction (`ImageToNative` or
/// `NativeToImage`) into a [`Grid2D`], validating dimensional consistency.
fn grid_from_properties(props: &[XisfProperty], dir: &str) -> Option<Grid2D> {
    let get = |suffix: &str| {
        let id = format!("{SPLINE_PREFIX}PointGridInterpolation:{dir}:{suffix}");
        props.iter().find(|p| p.id == id).map(|p| &p.value)
    };
    let rect_v = get("Rect")?.as_f64_vec()?;
    let delta = get("Delta")?.as_f64()?;
    let (rows, cols, gx) = get("GridX")?.as_f64_mat()?;
    let (ry, cy, gy) = get("GridY")?.as_f64_mat()?;
    if rect_v.len() != 4
        || !delta.is_finite()
        || delta <= 0.0
        || (rows, cols) != (ry, cy)
        || rows < 2
        || cols < 2
    {
        return None;
    }
    let rect = [rect_v[0], rect_v[1], rect_v[2], rect_v[3]];
    let n = rows as usize * cols as usize;
    if gx.len() != n || gy.len() != n {
        return None; // empty data ⇒ unresolved attachment block
    }
    // Node counts must match Rect + Delta: 1 + ⌈extent/Δ⌉ per axis (PixInsight
    // formula, verified across panels with both exact and fractional extents).
    if expected_nodes(rect[2] - rect[0], delta) != Some(cols)
        || expected_nodes(rect[3] - rect[1], delta) != Some(rows)
    {
        return None;
    }
    Some(Grid2D {
        rect,
        delta,
        rows,
        cols,
        gx: gx.to_vec(),
        gy: gy.to_vec(),
    })
}

/// PixInsight node count for a grid axis: `1 + ⌈extent/Δ⌉`, with a tolerance
/// for extents that are floating-point-exact multiples of `Δ`.
fn expected_nodes(extent: f64, delta: f64) -> Option<u32> {
    if !extent.is_finite() || extent <= 0.0 {
        return None;
    }
    let cells = extent / delta;
    let cells = if (cells - cells.round()).abs() < 1e-6 {
        cells.round()
    } else {
        cells.ceil()
    };
    (cells >= 1.0 && cells < f64::from(u32::MAX)).then(|| cells as u32 + 1)
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
    let (rows, cols, m) =
        find("PCL:AstrometricSolution:LinearTransformationMatrix")?.as_f64_mat()?;
    if crval.len() != 2 || refimg.len() != 2 || (rows, cols) != (2, 2) {
        return None;
    }

    // Missing projection property defaults to Gnomonic — the only projection
    // MosaicByCoordinates produces; an explicit unknown one refuses (better no
    // WCS than a wrong CTYPE).
    let proj =
        find("PCL:AstrometricSolution:ProjectionSystem").map_or(Some("Gnomonic"), |v| v.as_str());
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
        ctype: [
            format!("{:-<5}{code}", "RA"),
            format!("{:-<5}{code}", "DEC"),
        ],
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

/// Produce FITS WCS cards for an output crop of height `out_height` starting
/// at `crop_origin` (0-based canvas pixels).
///
/// `w` is the canvas solution in top-down stored-row coordinates. The cards
/// are emitted in the standard FITS frame — y = 1 at the *bottom* of the sky
/// image, the convention every WCS consumer (PixInsight, astropy, ds9)
/// assumes — while our writer stores rows top-down. The reflection
/// `y_fits = H + 1 − y_topdown` (H = `out_height`) turns CRPIX2 into
/// `H + 1 − (crpix2 − crop_y)` and negates the dy-dependent CD terms
/// (`CD1_2`, `CD2_2`). CRPIX1 only gets the crop translation.
pub fn wcs_cards(w: &LinearWcs, crop_origin: (u64, u64), out_height: u64) -> Vec<FitsKeyword> {
    let kw = |name: &str, value: String, comment: &str| FitsKeyword {
        name: name.to_string(),
        value,
        comment: comment.to_string(),
    };
    let q = |s: &str| format!("'{s}'");
    // Cards are emitted in the PI top-down convention, verbatim from the
    // solution, shifted by the crop translation (CRPIX is 1-based but the
    // shift is pure translation, so the base cancels). With top-down row
    // storage this is simultaneously correct in data-index terms (Astropy,
    // DS9 — verified against catalog stars) and in PixInsight's display-space
    // interpretation of the cards. Do NOT reflect the y axis here: two
    // independent PixInsight annotation tests proved reflected cards mirror,
    // whichever way the pixel rows are stored. `out_height` is kept in the
    // signature for the historical record and deliberately unused.
    // (An unresolved user-side mirror remains an open issue; the CLI's
    // `--wcs-frame flipped` escape hatch calls [`wcs_cards_flipped`].)
    let _ = out_height;
    let crpix1 = w.crpix[0] - crop_origin.0 as f64;
    let crpix2 = w.crpix[1] - crop_origin.1 as f64;
    vec![
        kw("CTYPE1", q(&w.ctype[0]), "projection"),
        kw("CTYPE2", q(&w.ctype[1]), "projection"),
        kw(
            "CRVAL1",
            w.crval[0].to_string(),
            "[deg] RA at reference point",
        ),
        kw(
            "CRVAL2",
            w.crval[1].to_string(),
            "[deg] Dec at reference point",
        ),
        kw("CRPIX1", crpix1.to_string(), "reference pixel x"),
        kw("CRPIX2", crpix2.to_string(), "reference pixel y (top-down)"),
        kw(
            "CD1_1",
            w.cd[0][0].to_string(),
            "[deg/px] transformation matrix",
        ),
        kw(
            "CD1_2",
            w.cd[0][1].to_string(),
            "[deg/px] transformation matrix",
        ),
        kw(
            "CD2_1",
            w.cd[1][0].to_string(),
            "[deg/px] transformation matrix",
        ),
        kw(
            "CD2_2",
            w.cd[1][1].to_string(),
            "[deg/px] transformation matrix",
        ),
        kw("CUNIT1", q("deg"), "axis unit"),
        kw("CUNIT2", q("deg"), "axis unit"),
        kw("RADESYS", q(&w.radesys), "celestial reference system"),
    ]
}

/// [`wcs_cards`] reflected into the bottom-up frame: CRPIX2 mirrored about
/// the output height, dy-dependent CD column negated. An escape hatch for
/// readers that interpret cards in the standard bottom-up frame against our
/// top-down rows (CLI: `--wcs-frame flipped`); see the open mirror issue.
pub fn wcs_cards_flipped(
    w: &LinearWcs,
    crop_origin: (u64, u64),
    out_height: u64,
) -> Vec<FitsKeyword> {
    let mut cards = wcs_cards(w, crop_origin, out_height);
    let crpix2 = w.crpix[1] - crop_origin.1 as f64;
    // Avoid "-0" card text when a matrix element is exactly zero.
    let neg_s = |v: f64| {
        if v == 0.0 {
            "0".to_string()
        } else {
            (-v).to_string()
        }
    };
    for kw in &mut cards {
        match kw.name.as_str() {
            "CRPIX2" => kw.value = (out_height as f64 + 1.0 - crpix2).to_string(),
            "CD1_2" => kw.value = neg_s(w.cd[0][1]),
            "CD2_2" => kw.value = neg_s(w.cd[1][1]),
            _ => {}
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::PropertyValue;

    fn prop(id: &str, type_: &str, value: PropertyValue) -> XisfProperty {
        XisfProperty {
            id: id.into(),
            type_: type_.into(),
            value,
            location: None,
        }
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
                PropertyValue::F64Mat {
                    rows: 2,
                    cols: 2,
                    data: vec![-S, 0.0, 0.0, S],
                },
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
        assert!(
            wcs_from_properties(&p).is_none(),
            "unknown projection must not emit wrong CTYPE"
        );

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
        for &(x, y) in &[
            (1.0, 1.0),
            (9255.0, 18310.0),
            (100.5, 17000.25),
            (4628.0, 9155.5),
        ] {
            let (ra, dec) = w.pixel_to_sky(x, y);
            let (x2, y2) = w.sky_to_pixel(ra, dec);
            assert!(
                (x - x2).abs() < 1e-6 && (y - y2).abs() < 1e-6,
                "({x},{y}) -> ({x2},{y2})"
            );
        }
    }

    /// Rebuild a LinearWcs from emitted cards (test-side parse).
    fn wcs_from_cards(cards: &[FitsKeyword]) -> LinearWcs {
        let get = |n: &str| {
            cards
                .iter()
                .find(|k| k.name == n)
                .unwrap_or_else(|| panic!("card {n} missing"))
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

    /// Real Orion canvas height (top-down rows the solution refers to).
    const ORION_H: u64 = 18310;

    #[test]
    fn cards_carry_full_wcs_and_quote_strings() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        let cards = wcs_cards(&w, (0, 0), ORION_H);
        let get = |n: &str| &cards.iter().find(|k| k.name == n).unwrap().value;
        assert_eq!(get("CTYPE1"), "'RA---TAN'");
        assert_eq!(get("CTYPE2"), "'DEC--TAN'");
        assert_eq!(get("CUNIT1"), "'deg'");
        assert_eq!(get("RADESYS"), "'ICRS'");
        // No "-0" text for the zero off-diagonal elements.
        assert_eq!(get("CD1_2"), "0");

        // Cards carry the solution verbatim in the top-down convention
        // (matching top-down row storage — data-index space and PixInsight
        // display space coincide).
        let c = wcs_from_cards(&cards);
        assert_eq!(c.crval, w.crval);
        assert_eq!(c.crpix, w.crpix);
        for &(x, y) in &[
            (1.0, 1.0),
            (9255.0, 18310.0),
            (100.5, 17000.25),
            (4628.0, 9155.5),
        ] {
            let (ra1, dec1) = w.pixel_to_sky(x, y);
            let (ra2, dec2) = c.pixel_to_sky(x, y);
            assert!(
                (ra1 - ra2).abs() < 1e-9 && (dec1 - dec2).abs() < 1e-9,
                "({x},{y})"
            );
        }
    }

    /// Cards must be the verbatim top-down solution — no y reflection.
    /// Two independent PixInsight annotation tests showed that reflected
    /// cards mirror north-south whichever way pixel rows are stored; with
    /// top-down storage, verbatim cards are also data-index-correct (verified
    /// against catalog stars in the real mosaic). Non-diagonal matrix so any
    /// stray negation fails.
    #[test]
    fn cards_are_verbatim_top_down() {
        const H: u64 = 4000;
        let w = LinearWcs {
            crval: [84.0, -3.0],
            crpix: [1500.0, 1200.0],
            // Rotated ~30°, with the E-W mirror astro images have.
            cd: [[-3.8e-4, 2.2e-4], [2.2e-4, 3.8e-4]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        };
        let c = wcs_from_cards(&wcs_cards(&w, (0, 0), H));
        assert_eq!(c.cd, w.cd);
        assert_eq!(c.crpix, w.crpix);

        // Round trip: an object at top-down pixel (x, y) projects through the
        // cards back to exactly (x, y).
        for &(x, y) in &[(100.0, 250.0), (3000.0, 3900.0), (1500.0, 1200.0)] {
            let (ra, dec) = w.pixel_to_sky(x, y);
            let (cx, cy) = c.sky_to_pixel(ra, dec);
            assert!((cx - x).abs() < 1e-6, "x: {cx} vs {x}");
            assert!((cy - y).abs() < 1e-6, "y: {cy} vs {y}");
        }
    }

    /// The `--wcs-frame flipped` escape hatch: cards reflected bottom-up.
    /// An object at top-down pixel (x, y) projects through the flipped cards
    /// to (x, H+1-y). Non-diagonal matrix so a wrong negation choice fails.
    #[test]
    fn flipped_cards_reflect_y_bottom_up() {
        const H: u64 = 4000;
        let w = LinearWcs {
            crval: [84.0, -3.0],
            crpix: [1500.0, 1200.0],
            cd: [[-3.8e-4, 2.2e-4], [2.2e-4, 3.8e-4]],
            ctype: ["RA---TAN".into(), "DEC--TAN".into()],
            radesys: "ICRS".into(),
        };
        let c = wcs_from_cards(&wcs_cards_flipped(&w, (0, 0), H));
        assert_eq!(c.cd[0][0], w.cd[0][0]);
        assert_eq!(c.cd[1][0], w.cd[1][0]);
        assert_eq!(c.cd[0][1], -w.cd[0][1]);
        assert_eq!(c.cd[1][1], -w.cd[1][1]);
        assert_eq!(c.crpix[1], H as f64 + 1.0 - w.crpix[1]);
        for &(x, y) in &[(100.0, 250.0), (3000.0, 3900.0), (1500.0, 1200.0)] {
            let (ra, dec) = w.pixel_to_sky(x, y);
            let (cx, cy) = c.sky_to_pixel(ra, dec);
            assert!((cx - x).abs() < 1e-6, "x: {cx} vs {x}");
            assert!((cy - (H as f64 + 1.0 - y)).abs() < 1e-6, "y: {cy}");
        }
    }

    #[test]
    fn crop_shift_keeps_sky_of_a_fixed_pixel_invariant() {
        let w = wcs_from_properties(&orion_props()).unwrap();
        let full = wcs_from_cards(&wcs_cards(&w, (0, 0), ORION_H));
        // Crop origin (1000, 2500): CRPIX shifts by minus the origin.
        let (oy, crop_h) = (2500u64, ORION_H - 2500);
        let cropped = wcs_from_cards(&wcs_cards(&w, (1000, oy), crop_h));
        assert_eq!(cropped.crpix[0], full.crpix[0] - 1000.0);
        assert_eq!(cropped.crpix[1], full.crpix[1] - oy as f64);
        // A fixed canvas pixel keeps its sky coordinates after cropping.
        let (ra1, dec1) = full.pixel_to_sky(6100.0, 12000.0);
        let (ra2, dec2) = cropped.pixel_to_sky(6100.0 - 1000.0, 12000.0 - oy as f64);
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
        let err_arcsec = (((ra - ra_kw) * dec_kw.to_radians().cos()).hypot(dec - dec_kw)) * 3600.0;
        eprintln!(
            "CRVAL {:?}  CRPIX {:?}  CD {:?}  CTYPE {:?}",
            w.crval, w.crpix, w.cd, w.ctype
        );
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
        assert!(
            px > cx && py > cy,
            "orientation: expected bottom-right, got ({px:.1}, {py:.1})"
        );
    }

    // ---- Grid2D ----

    /// Sample an analytic mapping on the PixInsight node layout.
    fn grid_from_fn(rect: [f64; 4], delta: f64, f: impl Fn(f64, f64) -> (f64, f64)) -> Grid2D {
        let cols = expected_nodes(rect[2] - rect[0], delta).unwrap();
        let rows = expected_nodes(rect[3] - rect[1], delta).unwrap();
        let mut gx = Vec::with_capacity((rows * cols) as usize);
        let mut gy = Vec::with_capacity((rows * cols) as usize);
        for r in 0..rows {
            for c in 0..cols {
                let (ox, oy) = f(
                    rect[0] + f64::from(c) * delta,
                    rect[1] + f64::from(r) * delta,
                );
                gx.push(ox);
                gy.push(oy);
            }
        }
        Grid2D {
            rect,
            delta,
            rows,
            cols,
            gx,
            gy,
        }
    }

    #[test]
    fn grid_eval_reproduces_node_values() {
        let g = grid_from_fn([0.0, 0.0, 40.0, 24.0], 8.0, |x, y| {
            (0.3 * x - y, 0.01 * y * y + x)
        });
        for r in 0..g.rows {
            for c in 0..g.cols {
                let (x, y) = (f64::from(c) * 8.0, f64::from(r) * 8.0);
                let i = (r * g.cols + c) as usize;
                let (ox, oy) = g.eval(x, y);
                assert!(
                    (ox - g.gx[i]).abs() < 1e-12,
                    "node ({r},{c}) x: {ox} vs {}",
                    g.gx[i]
                );
                assert!(
                    (oy - g.gy[i]).abs() < 1e-12,
                    "node ({r},{c}) y: {oy} vs {}",
                    g.gy[i]
                );
            }
        }
    }

    #[test]
    fn grid_eval_exact_for_affine_fields_everywhere() {
        // Linear ghost-node extrapolation keeps affine fields exact even in
        // the border cells.
        let f = |x: f64, y: f64| (1.5 + 0.25 * x - 0.75 * y, -2.0 + 0.5 * x + 0.125 * y);
        let g = grid_from_fn([0.0, 0.0, 100.0, 60.0], 10.0, f);
        for &(x, y) in &[
            (0.0, 0.0),
            (0.3, 0.7),
            (99.9, 59.9),
            (50.0, 30.0),
            (3.1, 59.0),
            (97.0, 1.0),
        ] {
            let (ox, oy) = g.eval(x, y);
            let (ex, ey) = f(x, y);
            assert!(
                (ox - ex).abs() < 1e-9 && (oy - ey).abs() < 1e-9,
                "({x},{y}): ({ox},{oy})"
            );
        }
    }

    #[test]
    fn grid_eval_exact_for_quadratics_in_interior() {
        // Catmull-Rom (Keys a = −1/2) reproduces degree-2 polynomials exactly
        // wherever the full 4×4 support lies inside the grid.
        let f = |x: f64, y: f64| {
            (
                0.01 * x * x + 0.02 * x * y - 0.005 * y * y + x,
                0.5 * y + 0.003 * y * y - 0.001 * x * x,
            )
        };
        let g = grid_from_fn([0.0, 0.0, 100.0, 100.0], 10.0, f);
        for &(x, y) in &[
            (15.0, 15.0),
            (50.5, 42.3),
            (84.9, 15.1),
            (33.3, 66.6),
            (20.0, 80.0),
        ] {
            let (ox, oy) = g.eval(x, y);
            let (ex, ey) = f(x, y);
            assert!(
                (ox - ex).abs() < 1e-9 && (oy - ey).abs() < 1e-9,
                "({x},{y}): ({ox},{oy})"
            );
        }
    }

    #[test]
    fn grid_eval_clamps_to_rect() {
        let f = |x: f64, y: f64| (2.0 * x + y, x - 3.0 * y);
        let g = grid_from_fn([0.0, 0.0, 100.0, 60.0], 10.0, f);
        assert_eq!(g.eval(-50.0, 30.0), g.eval(0.0, 30.0));
        assert_eq!(g.eval(120.0, 70.0), g.eval(100.0, 60.0));
    }

    // ---- TAN native ↔ celestial rotation ----

    #[test]
    fn tan_native_celestial_round_trips_including_poles() {
        for crval in [
            [84.2, -3.24],
            [0.3, 45.0],
            [359.5, -89.5],
            [123.0, 89.9],
            [200.0, 90.0],
            [10.0, -90.0],
        ] {
            for native in [(0.0, 0.0), (0.5, -1.1), (-2.0, 1.5), (0.0, 3.0)] {
                let (ra, dec) = tan_deproject(crval, native.0, native.1);
                let (xi, eta) = tan_project_checked(crval, ra, dec).expect("within hemisphere");
                assert!(
                    (xi - native.0).abs() < 1e-9 && (eta - native.1).abs() < 1e-9,
                    "crval {crval:?} native {native:?} -> ({xi},{eta})"
                );
            }
            // The native origin is the reference point.
            let (ra, dec) = tan_deproject(crval, 0.0, 0.0);
            assert!((dec - crval[1]).abs() < 1e-9, "crval {crval:?}: dec {dec}");
            if crval[1].abs() < 90.0 {
                let dra = ((ra - crval[0] + 180.0).rem_euclid(360.0) - 180.0).abs();
                assert!(dra < 1e-9, "crval {crval:?}: ra {ra}");
            }
        }
    }

    #[test]
    fn tan_project_rejects_far_hemisphere() {
        assert!(
            tan_project_checked([0.0, 0.0], 180.0, 0.0).is_none(),
            "antipode"
        );
        assert!(
            tan_project_checked([0.0, 0.0], 90.0, 0.0).is_none(),
            "exactly 90 deg away"
        );
        assert!(
            tan_project_checked([0.0, 0.0], 95.0, 20.0).is_none(),
            "beyond 90 deg"
        );
        assert!(
            tan_project_checked([0.0, 0.0], 89.0, 0.0).is_some(),
            "inside hemisphere"
        );
    }

    // ---- WcsModel ----

    fn grid_props(dir: &str, g: &Grid2D) -> Vec<XisfProperty> {
        let base = format!(
            "PCL:AstrometricSolution:SplineWorldTransformation:PointGridInterpolation:{dir}"
        );
        vec![
            prop(
                &format!("{base}:Rect"),
                "F64Vector",
                PropertyValue::F64Vec(g.rect.to_vec()),
            ),
            prop(
                &format!("{base}:Delta"),
                "Float64",
                PropertyValue::F64(g.delta),
            ),
            prop(
                &format!("{base}:GridX"),
                "F64Matrix",
                PropertyValue::F64Mat {
                    rows: g.rows,
                    cols: g.cols,
                    data: g.gx.clone(),
                },
            ),
            prop(
                &format!("{base}:GridY"),
                "F64Matrix",
                PropertyValue::F64Mat {
                    rows: g.rows,
                    cols: g.cols,
                    data: g.gy.clone(),
                },
            ),
        ]
    }

    /// Property set mimicking a raw solved panel (rotated ~90° like the real
    /// Orion raw panels), with distortion-free grids generated from the linear
    /// mapping itself — the grid path must then agree with the linear path.
    fn spline_props(w: f64, h: f64) -> Vec<XisfProperty> {
        const M: [[f64; 2]; 2] = [[-6.577e-6, 4.437e-4], [4.437e-4, 6.639e-6]];
        let crval = [82.895, -0.287];
        let refimg = [w / 2.0 - 3.7, h / 2.0 + 1.2];
        let lin = move |x: f64, y: f64| {
            let (dx, dy) = (x - refimg[0], y - refimg[1]);
            (M[0][0] * dx + M[0][1] * dy, M[1][0] * dx + M[1][1] * dy)
        };
        let det = M[0][0] * M[1][1] - M[0][1] * M[1][0];
        let inv = move |xi: f64, eta: f64| {
            (
                refimg[0] + (M[1][1] * xi - M[0][1] * eta) / det,
                refimg[1] + (M[0][0] * eta - M[1][0] * xi) / det,
            )
        };
        let i2n = grid_from_fn([0.0, 0.0, w, h], 64.0, lin);
        // Native bounds of the image (linear map ⇒ corners suffice), padded.
        let corners = [lin(0.0, 0.0), lin(w, 0.0), lin(0.0, h), lin(w, h)];
        let pad = 0.01;
        let nx0 = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min) - pad;
        let nx1 = corners
            .iter()
            .map(|c| c.0)
            .fold(f64::NEG_INFINITY, f64::max)
            + pad;
        let ny0 = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min) - pad;
        let ny1 = corners
            .iter()
            .map(|c| c.1)
            .fold(f64::NEG_INFINITY, f64::max)
            + pad;
        let n2i = grid_from_fn([nx0, ny0, nx1, ny1], 0.01, inv);

        let mut props = vec![
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
                    data: vec![M[0][0], M[0][1], M[1][0], M[1][1]],
                },
            ),
            prop(
                "PCL:AstrometricSolution:ProjectionSystem",
                "String",
                PropertyValue::Str("Gnomonic".into()),
            ),
            prop(
                "PCL:AstrometricSolution:ReferenceNativeCoordinates",
                "F64Vector",
                PropertyValue::F64Vec(vec![0.0, 90.0]),
            ),
            prop(
                "PCL:AstrometricSolution:CelestialPoleNativeCoordinates",
                "F64Vector",
                PropertyValue::F64Vec(vec![180.0, 90.0]),
            ),
            prop(
                "PCL:AstrometricSolution:SplineWorldTransformation:Version",
                "String",
                PropertyValue::Str("2.0".into()),
            ),
        ];
        props.extend(grid_props("ImageToNative", &i2n));
        props.extend(grid_props("NativeToImage", &n2i));
        props
    }

    #[test]
    fn spline_model_grid_path_matches_linear_for_linear_grids() {
        let (w, h) = (1000u64, 700u64);
        let model = WcsModel::from_properties(&spline_props(w as f64, h as f64), w, h).unwrap();
        assert!(model.is_spline());
        for &(x, y) in &[(0.0, 0.0), (500.0, 350.0), (999.0, 699.0), (13.7, 683.2)] {
            let (ra_g, dec_g) = model.pixel_to_sky(x, y);
            let (ra_l, dec_l) = model.linear.pixel_to_sky(x + 0.5, y + 0.5);
            assert!(
                (ra_g - ra_l).abs() < 1e-9 && (dec_g - dec_l).abs() < 1e-9,
                "({x},{y}): grid ({ra_g},{dec_g}) vs linear ({ra_l},{dec_l})"
            );
        }
    }

    #[test]
    fn spline_model_round_trips_pixels() {
        let (w, h) = (1000u64, 700u64);
        let model = WcsModel::from_properties(&spline_props(w as f64, h as f64), w, h).unwrap();
        for &(x, y) in &[
            (0.5, 0.5),
            (500.0, 350.0),
            (999.5, 699.5),
            (13.7, 683.2),
            (990.0, 5.0),
        ] {
            let (ra, dec) = model.pixel_to_sky(x, y);
            let (x2, y2) = model.sky_to_pixel(ra, dec).expect("inside grid domain");
            assert!(
                (x - x2).abs() < 1e-6 && (y - y2).abs() < 1e-6,
                "({x},{y}) -> ({x2},{y2})"
            );
        }
    }

    #[test]
    fn spline_sky_to_pixel_rejects_outside_grid_domain() {
        let (w, h) = (1000u64, 700u64);
        let model = WcsModel::from_properties(&spline_props(w as f64, h as f64), w, h).unwrap();
        // ~5 deg from the reference point: far outside the native rect+margin.
        let (ra, dec) = tan_deproject(model.linear.crval, 5.0, 0.0);
        assert!(model.sky_to_pixel(ra, dec).is_none(), "outside grid domain");
        // Antipodal point: beyond the tangent hemisphere.
        let (ra, dec) = (model.linear.crval[0] + 180.0, -model.linear.crval[1]);
        assert!(
            model.sky_to_pixel(ra.rem_euclid(360.0), dec).is_none(),
            "far hemisphere"
        );
        // The reference point itself maps.
        assert!(
            model
                .sky_to_pixel(model.linear.crval[0], model.linear.crval[1])
                .is_some()
        );
    }

    #[test]
    fn linear_only_model_falls_back_and_converts_conventions() {
        let model = WcsModel::from_properties(&orion_props(), 9255, 18310).unwrap();
        assert!(!model.is_spline());
        // PixInsight image coordinates of the reference point (FITS − 0.5).
        let (ra, dec) = model.pixel_to_sky(4627.5, 9155.0);
        assert!((ra - 84.19917732858325).abs() < 1e-12);
        assert!((dec - -3.240007111323072).abs() < 1e-12);
        let (x, y) = model.sky_to_pixel(ra, dec).unwrap();
        assert!(
            (x - 4627.5).abs() < 1e-9 && (y - 9155.0).abs() < 1e-9,
            "({x},{y})"
        );
    }

    #[test]
    fn spline_model_rejects_unverifiable_layouts() {
        let (w, h) = (1000u64, 700u64);
        let base = spline_props(w as f64, h as f64);

        // Untouched base parses (guards the guards below).
        assert!(WcsModel::from_properties(&base, w, h).is_some());

        // Spline marker present but grids missing: never approximate silently.
        let p: Vec<_> = base
            .iter()
            .filter(|p| !p.id.contains("PointGridInterpolation"))
            .cloned()
            .collect();
        assert!(
            WcsModel::from_properties(&p, w, h).is_none(),
            "grids required once spline"
        );

        // Transposed matrix dims are inconsistent with Rect/Delta.
        let mut p = base.clone();
        for q in &mut p {
            if (q.id.contains(":GridX") || q.id.contains(":GridY"))
                && let PropertyValue::F64Mat { rows, cols, .. } = &mut q.value
            {
                std::mem::swap(rows, cols);
            }
        }
        assert!(
            WcsModel::from_properties(&p, w, h).is_none(),
            "transposed dims"
        );

        // Corrupted grid values fail the reference-point/corner checks.
        let mut p = base.clone();
        for q in &mut p {
            if q.id.contains("ImageToNative:GridX")
                && let PropertyValue::F64Mat { data, .. } = &mut q.value
            {
                for v in data.iter_mut() {
                    *v += 0.5;
                }
            }
        }
        assert!(
            WcsModel::from_properties(&p, w, h).is_none(),
            "corrupt grid values"
        );

        // Non-standard native frame: rotation math would differ.
        let mut p = base.clone();
        p.iter_mut()
            .find(|q| q.id.ends_with("ReferenceNativeCoordinates"))
            .unwrap()
            .value = PropertyValue::F64Vec(vec![0.0, 45.0]);
        assert!(
            WcsModel::from_properties(&p, w, h).is_none(),
            "non-standard native frame"
        );

        // Geometry mismatch: grid rect is no longer the image bounds.
        assert!(
            WcsModel::from_properties(&base, 2 * w, h).is_none(),
            "geometry mismatch"
        );
    }

    #[test]
    fn model_without_solution_is_none() {
        assert!(WcsModel::from_properties(&[], 100, 100).is_none());
    }

    // ---- real-data validations (manual; multi-GB gitignored test_data) ----
    // Run: `cargo test -p mmm-core real_ -- --ignored --nocapture`

    use crate::formats::xisf::XisfPanel;

    fn test_data(rel: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data")
            .join(rel)
    }

    fn raw_panel_path(n: u32) -> std::path::PathBuf {
        test_data(&format!(
            "orion_mosaic_raw_panels/masterLight_BIN-1_4944x3284_EXPOSURE-30.00s_FILTER-NoFilter_RGB_PANEL-{n}_autocrop.xisf"
        ))
    }

    fn registered_panel_path(n: u32) -> std::path::PathBuf {
        test_data(&format!(
            "orion_mosaic/masterLight_BIN-1_4944x3284_EXPOSURE-30.00s_FILTER-NoFilter_RGB_PANEL-{n}_autocrop_ra.xisf"
        ))
    }

    fn open_raw_model(n: u32) -> (XisfPanel, WcsModel) {
        let panel = XisfPanel::open(&raw_panel_path(n)).unwrap();
        let h = panel.header();
        let model = WcsModel::from_properties(&h.properties, h.width, h.height)
            .expect("raw panel must yield a model");
        (panel, model)
    }

    /// Small-angle angular separation in arcseconds.
    fn arcsec_apart(a: (f64, f64), b: (f64, f64)) -> f64 {
        let dra = ((a.0 - b.0 + 180.0).rem_euclid(360.0) - 180.0) * a.1.to_radians().cos();
        dra.hypot(a.1 - b.1) * 3600.0
    }

    /// Validation 1: grid path vs linear solution at frame center (< 2″) with
    /// corner deviations reported (= the distortion magnitude).
    #[test]
    #[ignore = "needs multi-GB test_data/orion_mosaic_raw_panels (gitignored); run manually"]
    fn real_raw_panel_grid_vs_linear() {
        let (panel, model) = open_raw_model(1);
        assert!(model.is_spline(), "raw panel must carry distortion grids");
        let (w, h) = (panel.width() as f64, panel.height() as f64);
        let lin = |x: f64, y: f64| model.linear.pixel_to_sky(x + 0.5, y + 0.5);
        let center = arcsec_apart(model.pixel_to_sky(w / 2.0, h / 2.0), lin(w / 2.0, h / 2.0));
        eprintln!("center grid-vs-linear: {center:.3}\"");
        for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
            let d = arcsec_apart(model.pixel_to_sky(x, y), lin(x, y));
            eprintln!("corner ({x:6.0},{y:6.0}) grid-vs-linear: {d:.3}\" (distortion)");
        }
        assert!(center < 2.0, "center deviation {center}\"");
    }

    /// Validation 2: pixel → sky → pixel through both grids, 20×20 samples.
    #[test]
    #[ignore = "needs multi-GB test_data/orion_mosaic_raw_panels (gitignored); run manually"]
    fn real_raw_panel_grid_round_trip() {
        let (panel, model) = open_raw_model(1);
        let (w, h) = (panel.width() as f64, panel.height() as f64);
        let (mut sum_sq, mut max, mut n) = (0.0f64, 0.0f64, 0u32);
        for j in 0..20 {
            for i in 0..20 {
                let x = (f64::from(i) + 0.5) * w / 20.0;
                let y = (f64::from(j) + 0.5) * h / 20.0;
                let (ra, dec) = model.pixel_to_sky(x, y);
                let (x2, y2) = model.sky_to_pixel(ra, dec).expect("inside domain");
                let e = (x - x2).hypot(y - y2);
                sum_sq += e * e;
                max = max.max(e);
                n += 1;
            }
        }
        let rms = (sum_sq / f64::from(n)).sqrt();
        eprintln!("round-trip over {n} samples: RMS {rms:.4} px, max {max:.4} px");
        assert!(rms < 0.05, "round-trip RMS {rms} px");
    }

    /// All 12 raw panels must load as spline models (per-file layout
    /// validations pass on the whole set).
    #[test]
    #[ignore = "needs multi-GB test_data/orion_mosaic_raw_panels (gitignored); run manually"]
    fn real_all_raw_panels_yield_spline_models() {
        for n in 1..=12 {
            let (panel, model) = open_raw_model(n);
            assert!(model.is_spline(), "panel {n}");
            eprintln!(
                "panel {n}: {}x{} grids i2n {}x{} n2i {}x{}",
                panel.width(),
                panel.height(),
                model.image_to_native.as_ref().unwrap().cols,
                model.image_to_native.as_ref().unwrap().rows,
                model.native_to_image.as_ref().unwrap().cols,
                model.native_to_image.as_ref().unwrap().rows,
            );
        }
    }

    /// Median of a 21×21 window: local background estimate for centroiding.
    fn local_median(v: &[f32], w: usize, x: usize, y: usize) -> f32 {
        let mut vals: Vec<f32> = (y - 10..=y + 10)
            .flat_map(|yy| v[yy * w + x - 10..=yy * w + x + 10].iter().copied())
            .collect();
        vals.sort_by(f32::total_cmp);
        vals[vals.len() / 2]
    }

    /// Intensity-weighted centroid over ±6 px around a peak, in PixInsight
    /// image coordinates. `None` if the window touches no-data or the frame
    /// edge.
    fn centroid(v: &[f32], w: usize, h: usize, x: usize, y: usize, bg: f32) -> Option<(f64, f64)> {
        const R: i64 = 6;
        let (mut sw, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
        for dy in -R..=R {
            for dx in -R..=R {
                let (px, py) = (x as i64 + dx, y as i64 + dy);
                if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 {
                    return None;
                }
                let val = v[py as usize * w + px as usize];
                if val == 0.0 {
                    return None; // no-data in window: unusable
                }
                let wt = f64::from(val - bg).max(0.0);
                sw += wt;
                sx += wt * (px as f64 + 0.5);
                sy += wt * (py as f64 + 0.5);
            }
        }
        (sw > 0.0).then(|| (sx / sw, sy / sw))
    }

    /// Bright isolated stars in the panel's green channel: local maxima above
    /// the 99.9th percentile, ≥ 100 px apart, brightest first.
    fn detect_stars(panel: &XisfPanel, count: usize) -> Vec<(f64, f64)> {
        let (w, h) = (panel.width() as usize, panel.height() as usize);
        let v = panel.channel(1);
        let mut sample: Vec<f32> = v
            .iter()
            .step_by(1009)
            .copied()
            .filter(|x| *x > 0.0)
            .collect();
        sample.sort_by(f32::total_cmp);
        let thresh = sample[sample.len() - sample.len() / 1000 - 1];
        const M: usize = 40;
        let mut peaks: Vec<(f32, usize, usize)> = Vec::new();
        for y in M..h - M {
            for x in M..w - M {
                let val = v[y * w + x];
                if val < thresh {
                    continue;
                }
                let is_max = (-7i64..=7).all(|dy| {
                    (-7i64..=7).all(|dx| {
                        v[(y as i64 + dy) as usize * w + (x as i64 + dx) as usize] <= val
                            || (dx == 0 && dy == 0)
                    })
                });
                if is_max {
                    peaks.push((val, x, y));
                }
            }
        }
        peaks.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut kept: Vec<(f64, f64)> = Vec::new();
        for (_, x, y) in peaks {
            if kept.len() == count {
                break;
            }
            if kept
                .iter()
                .all(|(kx, ky)| (kx - x as f64).hypot(ky - y as f64) > 100.0)
                && let Some(c) = centroid(v, w, h, x, y, local_median(v, w, x, y))
            {
                kept.push(c);
            }
        }
        kept
    }

    /// Find the star nearest a predicted position: brightest pixel within
    /// ±10 px, then centroid. `None` if outside the frame or on no-data.
    fn refine_at(panel: &XisfPanel, guess: (f64, f64)) -> Option<(f64, f64)> {
        let (w, h) = (panel.width() as usize, panel.height() as usize);
        let v = panel.channel(1);
        let (gx, gy) = (guess.0.floor() as i64, guess.1.floor() as i64);
        let mut best: Option<(f32, usize, usize)> = None;
        for dy in -10i64..=10 {
            for dx in -10i64..=10 {
                let (px, py) = (gx + dx, gy + dy);
                if px < 11 || py < 11 || px >= w as i64 - 11 || py >= h as i64 - 11 {
                    return None;
                }
                let val = v[py as usize * w + px as usize];
                if best.is_none_or(|(b, _, _)| val > b) {
                    best = Some((val, px as usize, py as usize));
                }
            }
        }
        let (_, px, py) = best?;
        centroid(v, w, h, px, py, local_median(v, w, px, py))
    }

    /// Validation 3 (the S1 acceptance test): raw-panel stars → sky via the
    /// spline model → registered-canvas pixels via the registered panel's
    /// linear WCS → compare against the same stars detected in the registered
    /// frame. Median residual < 0.5 px proves the whole model chain.
    #[test]
    #[ignore = "needs multi-GB test_data (gitignored); run manually"]
    fn real_cross_frame_star_chain() {
        let (raw, model) = open_raw_model(1);
        assert!(model.is_spline(), "raw panel must carry distortion grids");
        let reg = XisfPanel::open(&registered_panel_path(1)).unwrap();
        let reg_wcs =
            wcs_from_properties(&reg.header().properties).expect("registered linear solution");

        let stars = detect_stars(&raw, 25);
        assert!(
            stars.len() >= 12,
            "only {} stars detected in the raw panel",
            stars.len()
        );
        let mut residuals = Vec::new();
        let (mut sdx, mut sdy) = (Vec::new(), Vec::new());
        for &(x, y) in &stars {
            let (ra, dec) = model.pixel_to_sky(x, y);
            let (fx, fy) = reg_wcs.sky_to_pixel(ra, dec);
            // MosaicByCoordinates content placement (measured; module docs):
            // the star's ARRAY index equals its span solution coordinate
            // (fits − 0.5), so its span-coordinate position — what `refine_at`
            // measures — is (fits − 0.5) + 0.5 = fits. Reading the registered
            // frame in the plain span convention leaves a constant
            // (+0.5, +0.5) canvas-frame residual, reported below.
            let pred = (fx, fy);
            let Some(det) = refine_at(&reg, pred) else {
                continue;
            };
            let r = (det.0 - pred.0).hypot(det.1 - pred.1);
            eprintln!(
                "raw ({x:7.2},{y:7.2}) -> sky ({ra:10.6},{dec:+9.6}) -> pred ({:8.2},{:8.2}) \
                 det ({:8.2},{:8.2}) resid {r:.3} px",
                pred.0, pred.1, det.0, det.1
            );
            residuals.push(r);
            sdx.push(det.0 - pred.0 + 0.5); // naive span reading, documents the
            sdy.push(det.1 - pred.1 + 0.5); // measured half-pixel placement law
        }
        assert!(
            residuals.len() >= 10,
            "only {} stars matched",
            residuals.len()
        );
        residuals.sort_by(f64::total_cmp);
        sdx.sort_by(f64::total_cmp);
        sdy.sort_by(f64::total_cmp);
        let median = residuals[residuals.len() / 2];
        let (mdx, mdy) = (sdx[sdx.len() / 2], sdy[sdy.len() / 2]);
        eprintln!(
            "cross-frame residuals: n={} median={median:.3} px max={:.3} px",
            residuals.len(),
            residuals.last().unwrap()
        );
        eprintln!(
            "naive span-reading offset (content placement law): median ({mdx:+.3}, {mdy:+.3}) px"
        );
        assert!(median < 0.5, "median residual {median} px");
        // Lock the measured placement law itself: the naive reading must show
        // the (+0.5, +0.5) constant, not some other convention combination.
        assert!(
            (mdx - 0.5).abs() < 0.2 && (mdy - 0.5).abs() < 0.2,
            "content placement law changed: naive offset ({mdx}, {mdy})"
        );
    }
}
