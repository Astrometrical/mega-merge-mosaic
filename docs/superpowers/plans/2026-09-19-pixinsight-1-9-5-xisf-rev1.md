# PixInsight 1.9.5 / XISF rev 1 Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Read the XISF 1.0 revision 1 standard astrometric solution (`AstrometricSolution:*`) that PixInsight 1.9.5 writes, keep reading 1.9.4-era `PCL:AstrometricSolution:*` files, and move the module build to PCL 2.10.8 (PixInsight ≥ 1.9.5 only).

**Architecture:** `mmm-core` gains a spec-conformant decoder (`astrometry/standard.rs` + `astrometry/spline.rs`) that evaluates the projective + RBF-spline distortion model and samples it onto the existing `Grid2D` lookup grids, so `WcsModel` and every consumer stay unchanged. The legacy reader moves to `astrometry/legacy.rs`. The module forwards both property families; CI pins PCL 2.10.8 with one pin for all arches; the in-repo update-repository scripts are deleted.

**Tech Stack:** Rust 2024 (rayon, quick-xml, serde), C++20 PCL module, bash/pwsh CI scripts, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-19-pixinsight-1-9-5-xisf-rev1-design.md`

## Global Constraints

- PCL pin: commit `201860a364c05308cde8271f5ca517b69b487fa1`, PCL 2.10.8, `PCL_API_Version 0x0188`; one pin for every arch.
- Release version `1.5.0` in `Cargo.toml`, `MmmVersion.h`, `mmm_protocol.h`, `MegaMergeMosaic.html` (tripwire: `crates/mmm-ipc-worker/tests/version_sync.rs`).
- Every public item in `mmm-core` needs a doc comment; `cargo fmt`, `cargo clippy --all-targets`, `cargo doc` must stay warning-free.
- Tests never depend on `test_data/`; real-data tests are `#[ignore]` with the existing message pattern.
- Legacy `PCL:AstrometricSolution:*` files must keep working exactly as today.
- XISF compression stays unsupported (out of scope).
- Commits: end messages with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Never `git add -A`: the working tree has unrelated uncommitted edits (`MmmInterface.*`, `host/CMakeLists.txt`, `host/test/test_sectionbar_order.cpp`) that must stay out of these commits.
- Reference implementation for all spline math: PCL 2.10.8 sources at `/opt/PixInsight/src/pcl/{SurfaceSpline.cpp,WorldTransformation.cpp}` and `/opt/PixInsight/include/pcl/SurfaceSpline.h`.
- A real 1.9.5-regenerated panel exists at `/tmp/claude-1000/-home-dpaull-dev-mega-merge-mosaic/08cdeb32-17c7-469a-ad0b-f13abd40705f/scratchpad/pi195/panel10-regen.xisf` (header dump in `header.xml` next to it): Global-only `ThinPlateSpline`, `Order 2`, `Polynomial true`, separate X and Y node sets, PixInsight grid cache present.

---

## File structure

| Path | Responsibility |
|---|---|
| `crates/mmm-core/src/astrometry/mod.rs` | (was `astrometry.rs`) `LinearWcs`, `WcsModel`, `Grid2D`, TAN math, cards, `wcs_from_properties`, dispatch legacy/standard, shared grid validation |
| `crates/mmm-core/src/astrometry/legacy.rs` | `PCL:AstrometricSolution:*` linear reader + `SplineWorldTransformation` grid reader (moved code, no behaviour change) |
| `crates/mmm-core/src/astrometry/spline.rs` | RBF kernels, scalar/vector surface splines, term model (Global/Local/Fallback), disc bucket index |
| `crates/mmm-core/src/astrometry/standard.rs` | XISF rev 1 block: version gate, layer 1–3 parsing/validation, homography, grid sampling |
| `crates/mmm-core/src/formats/xisf.rs` | decode all numeric vector/matrix property types |
| `crates/mmm-core/src/synth.rs` | `write_xisf_solved_standard` fixture writer |
| `crates/mmm-core/src/analyze.rs` | `describe_unsolved` learns the standard block |
| `integration/pixinsight/module/AstrometryProps.cpp` | forward both id families |
| `integration/pixinsight/module/{ImageWindowCollector.h,ViewPanelSource.h,makefile-x64,CMakeLists.txt}` | drop 2.8.x workarounds/comments |
| `integration/pixinsight/ci/{pcl-pin.env,build-pcl-macos.sh,build-pcl.sh,build-pcl-windows.ps1,win/PCL.vcxproj,README.md}` | single 2.10.8 pin |
| `.github/workflows/module.yml` | one macOS pin; no repo packaging |
| `integration/pixinsight/repo/` | deleted |
| docs: `README.md`, `docs/DESIGN.md`, `integration/pixinsight/PCL_API_REFERENCE.md`, `integration/pixinsight/module/README.md`, `integration/pixinsight/PROTOCOL.md`, `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html` | version/format statements |

---

### Task 1: Split `astrometry.rs` into a module directory (no behaviour change)

**Files:**
- Move: `crates/mmm-core/src/astrometry.rs` → `crates/mmm-core/src/astrometry/mod.rs`
- Create: `crates/mmm-core/src/astrometry/legacy.rs`
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (extract legacy pieces; factor grid validation)

**Interfaces:**
- Produces (in `mod.rs`, `pub(crate)`): `fn find_value<'a>(props: &'a [XisfProperty], id: &str) -> Option<&'a PropertyValue>`; `fn validate_grids(linear: &LinearWcs, i2n: &Grid2D, n2i: &Grid2D, width: u64, height: u64) -> bool`; `fn expected_nodes(extent: f64, delta: f64) -> Option<u32>`; `fn projection_code(name: &str) -> Option<&'static str>`; `fn std_native_frame_ok(props, ref_id: &str, pole_id: &str) -> bool`.
- Produces (in `legacy.rs`, `pub(super)`): `const LEGACY_PREFIX: &str = "PCL:AstrometricSolution:"`; `const SPLINE_PREFIX`; `fn linear_from_legacy(props) -> Option<LinearWcs>`; `fn model_from_legacy(props, width, height) -> Option<WcsModel>`; `fn has_legacy_spline(props) -> bool`.

- [ ] **Step 1: Move the file**

```bash
cd /home/dpaull/dev/mega-merge-mosaic
git mv crates/mmm-core/src/astrometry.rs crates/mmm-core/src/astrometry/mod.rs
cargo test -p mmm-core astrometry 2>&1 | tail -3
```
Expected: builds and the existing astrometry tests pass unchanged (a `mod.rs` under a directory is the same module path).

- [ ] **Step 2: Create `legacy.rs` with the moved legacy code**

Create `crates/mmm-core/src/astrometry/legacy.rs`:

```rust
//! Legacy (PixInsight ≤ 1.9.4) astrometric solution: `PCL:AstrometricSolution:*`
//! properties, with the spline distortion carried as PixInsight's
//! `SplineWorldTransformation:PointGridInterpolation` lookup grids.
//!
//! Everything here was verified on real 1.9.4 files; see the module docs in
//! `mod.rs` for the empirical facts (grid layout, conventions). PixInsight
//! 1.9.5 no longer writes these ids — see `standard.rs` — but old data stays
//! in circulation, so this reader is kept indefinitely.

use super::{
    Grid2D, LinearWcs, WcsModel, expected_nodes, find_value, projection_code,
    std_native_frame_ok, validate_grids,
};
use crate::formats::XisfProperty;

/// Prefix of every legacy solution property.
pub(super) const LEGACY_PREFIX: &str = "PCL:AstrometricSolution:";
/// Prefix of the legacy spline block.
pub(super) const SPLINE_PREFIX: &str = "PCL:AstrometricSolution:SplineWorldTransformation:";

/// True when any legacy spline property is present.
pub(super) fn has_legacy_spline(props: &[XisfProperty]) -> bool {
    props.iter().any(|p| p.id.starts_with(SPLINE_PREFIX))
}

/// Linear solution from the legacy ids (today's `wcs_from_properties` body).
pub(super) fn linear_from_legacy(props: &[XisfProperty]) -> Option<LinearWcs> {
    let crval = find_value(props, "PCL:AstrometricSolution:ReferenceCelestialCoordinates")?
        .as_f64_vec()?;
    let refimg =
        find_value(props, "PCL:AstrometricSolution:ReferenceImageCoordinates")?.as_f64_vec()?;
    let (rows, cols, m) =
        find_value(props, "PCL:AstrometricSolution:LinearTransformationMatrix")?.as_f64_mat()?;
    if crval.len() != 2 || refimg.len() != 2 || (rows, cols) != (2, 2) {
        return None;
    }
    let proj = find_value(props, "PCL:AstrometricSolution:ProjectionSystem")
        .map_or(Some("Gnomonic"), |v| v.as_str());
    let code = projection_code(proj?)?;
    let radesys = find_value(props, "Observation:CelestialReferenceSystem")
        .and_then(|v| v.as_str())
        .unwrap_or("ICRS")
        .to_string();
    Some(LinearWcs {
        crval: [crval[0], crval[1]],
        crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
        cd: [[m[0], m[1]], [m[2], m[3]]],
        ctype: [format!("{:-<5}{code}", "RA"), format!("{:-<5}{code}", "DEC")],
        radesys,
    })
}

/// Full model from the legacy ids (today's `WcsModel::from_properties` body).
pub(super) fn model_from_legacy(props: &[XisfProperty], width: u64, height: u64) -> Option<WcsModel> {
    let linear = linear_from_legacy(props)?;
    if !has_legacy_spline(props) {
        return Some(WcsModel { linear, image_to_native: None, native_to_image: None, width, height });
    }
    if linear.ctype[0] != "RA---TAN" {
        return None;
    }
    if !std_native_frame_ok(
        props,
        "PCL:AstrometricSolution:ReferenceNativeCoordinates",
        "PCL:AstrometricSolution:CelestialPoleNativeCoordinates",
    ) {
        return None;
    }
    let image_to_native = grid_from_properties(props, "ImageToNative")?;
    let native_to_image = grid_from_properties(props, "NativeToImage")?;
    if !validate_grids(&linear, &image_to_native, &native_to_image, width, height) {
        return None;
    }
    Some(WcsModel {
        linear,
        image_to_native: Some(image_to_native),
        native_to_image: Some(native_to_image),
        width,
        height,
    })
}

/// Read one `PointGridInterpolation` direction into a [`Grid2D`]
/// (moved verbatim from `mod.rs`).
fn grid_from_properties(props: &[XisfProperty], dir: &str) -> Option<Grid2D> {
    // ... paste today's body unchanged, replacing `SPLINE_PREFIX` lookups
    // with the constant above ...
}
```

Paste the existing `grid_from_properties` body verbatim.

- [ ] **Step 3: Rework `mod.rs`**

In `mod.rs`:
1. Add `mod legacy;` after the `use` lines.
2. Replace the body of `wcs_from_properties` with `legacy::linear_from_legacy(props)` (Task 6 adds the standard branch).
3. Replace the body of `WcsModel::from_properties` with `legacy::model_from_legacy(props, width, height)`.
4. Extract the helpers the two readers share:

```rust
/// Value of the property with id `id`, if present.
pub(crate) fn find_value<'a>(props: &'a [XisfProperty], id: &str) -> Option<&'a PropertyValue> {
    props.iter().find(|p| p.id == id).map(|p| &p.value)
}

/// True when the native-frame properties are absent or carry the standard
/// zenithal values (reference (0, 90), pole (180, 90)); a different frame
/// would change the deprojection and is not implemented.
pub(crate) fn std_native_frame_ok(props: &[XisfProperty], ref_id: &str, pole_id: &str) -> bool {
    let ok = |id: &str, expect: [f64; 2]| match find_value(props, id) {
        None => true,
        Some(v) => v.as_f64_vec().is_some_and(|v| {
            v.len() == 2 && (v[0] - expect[0]).abs() < 1e-9 && (v[1] - expect[1]).abs() < 1e-9
        }),
    };
    ok(ref_id, [0.0, 90.0]) && ok(pole_id, [180.0, 90.0])
}

/// Layout validations shared by every grid source (empirically derived, see
/// module docs). `linear` is the panel's linear solution; the grids are in
/// PixInsight image coordinates / tangent-plane degrees.
pub(crate) fn validate_grids(
    linear: &LinearWcs,
    image_to_native: &Grid2D,
    native_to_image: &Grid2D,
    width: u64,
    height: u64,
) -> bool {
    let refimg = [linear.crpix[0] - 0.5, linear.crpix[1] - 0.5];
    let r = image_to_native.rect;
    if r[0].abs() > 1.0 || r[1].abs() > 1.0
        || (r[2] - width as f64).abs() > 1.0 || (r[3] - height as f64).abs() > 1.0
    {
        return false;
    }
    let (xi, eta) = image_to_native.eval(refimg[0], refimg[1]);
    if xi.hypot(eta) > 0.01 {
        return false;
    }
    let (ix, iy) = native_to_image.eval(0.0, 0.0);
    if (ix - refimg[0]).hypot(iy - refimg[1]) > 5.0 {
        return false;
    }
    let m = linear.cd;
    for (cx, cy) in [(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])] {
        let (gx, gy) = image_to_native.eval(cx, cy);
        let (dx, dy) = (cx - refimg[0], cy - refimg[1]);
        let (lx, ly) = (m[0][0] * dx + m[0][1] * dy, m[1][0] * dx + m[1][1] * dy);
        if (gx - lx).hypot(gy - ly) > 0.05 {
            return false;
        }
    }
    true
}
```

Make `expected_nodes` and `projection_code` `pub(crate)`. Delete the old `SPLINE_PREFIX` constant and `grid_from_properties` from `mod.rs`. Keep the whole `#[cfg(test)] mod tests` in `mod.rs`; if a test referenced a moved private item, add `use super::legacy::*;` inside the tests module.

- [ ] **Step 4: Verify no behaviour change**

```bash
cargo fmt && cargo clippy --all-targets -p mmm-core 2>&1 | tail -3 && cargo test -p mmm-core 2>&1 | tail -3 && cargo doc -p mmm-core --no-deps 2>&1 | grep -c warning
```
Expected: clippy clean, all tests pass, `0` doc warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/astrometry
git commit -m "refactor(astrometry): split into mod.rs + legacy.rs ahead of the XISF rev 1 decoder

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: XISF reader decodes every numeric vector/matrix property type

**Files:**
- Modify: `crates/mmm-core/src/formats/xisf.rs` (`finish_property`, `read_attached_f64s`, and the `open` loop that calls it)
- Modify: `crates/mmm-core/src/formats/mod.rs` (doc comments on `PropertyValue::F64Vec`/`F64Mat`)

**Interfaces:**
- Produces: `PropertyValue::F64Vec` / `F64Mat` for the XISF types `I8Vector, UI8Vector, I16Vector, UI16Vector, I32Vector, UI32Vector, I64Vector, UI64Vector, F32Vector, F64Vector` and the ten matching `*Matrix` types; `XisfProperty::type_` keeps the original type name. The standard block needs `I32Vector` (`Local:X:NodeOffsets`, `ControlPoints:Rejected`).

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module at the bottom of `crates/mmm-core/src/formats/xisf.rs` (there is an existing `#[cfg(test)] mod tests`; if not, create one with `use super::*;`). The helper writes a tiny XISF with inline properties using `crate::synth::write_xisf` plus manual XML — simpler: exercise `finish_property` directly.

```rust
    fn pending(id: &str, type_: &str, location: Option<&str>, length: Option<u64>, rows: Option<u32>, cols: Option<u32>) -> PendingProperty {
        PendingProperty {
            id: id.into(),
            type_: type_.into(),
            value_attr: None,
            location: location.map(str::to_string),
            length,
            rows,
            cols,
        }
    }

    fn b64(bytes: &[u8]) -> String {
        crate::synth::base64_encode_for_tests(bytes)
    }

    #[test]
    fn i32vector_inline_decodes_to_f64vec_keeping_type() {
        let bytes: Vec<u8> = [0i32, 5, 12, -3].iter().flat_map(|v| v.to_le_bytes()).collect();
        let p = finish_property(
            Path::new("x"),
            pending("AstrometricSolution:DistortionModel:ImageToProjection:Local:X:NodeOffsets", "I32Vector", Some("inline:base64"), Some(4), None, None),
            &b64(&bytes),
        )
        .unwrap();
        assert_eq!(p.type_, "I32Vector");
        assert_eq!(p.value, PropertyValue::F64Vec(vec![0.0, 5.0, 12.0, -3.0]));
    }

    #[test]
    fn f32matrix_inline_decodes_row_major() {
        let bytes: Vec<u8> = [1.5f32, 2.0, 3.0, 4.0, 5.0, 6.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let p = finish_property(Path::new("x"), pending("m", "F32Matrix", Some("inline:base64"), None, Some(2), Some(3)), &b64(&bytes)).unwrap();
        assert_eq!(p.value, PropertyValue::F64Mat { rows: 2, cols: 3, data: vec![1.5, 2.0, 3.0, 4.0, 5.0, 6.0] });
    }

    #[test]
    fn i32vector_attachment_size_uses_element_width() {
        // 4 elements × 4 bytes = 16 bytes; an 8-byte-per-element assumption would reject it.
        let p = finish_property(Path::new("x"), pending("v", "I32Vector", Some("attachment:4096:16"), Some(4), None, None), "").unwrap();
        assert_eq!(p.location, Some((4096, 16)));
        assert!(finish_property(Path::new("x"), pending("v", "I32Vector", Some("attachment:4096:32"), Some(4), None, None), "").is_err());
    }

    #[test]
    fn multiline_string_property_keeps_newline() {
        let p = finish_property(Path::new("x"), pending("AstrometricSolution:DistortionModel:ImageToProjection:Terms", "String", None, None, None, None), "Local\nFallback").unwrap();
        assert_eq!(p.value, PropertyValue::Str("Local\nFallback".into()));
    }
```

Add to `crates/mmm-core/src/synth.rs` right after `base64_encode`:

```rust
/// Test-only re-export of the base64 encoder for fixture builders in other modules.
#[cfg(test)]
pub(crate) fn base64_encode_for_tests(bytes: &[u8]) -> String {
    base64_encode(bytes)
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p mmm-core formats::xisf 2>&1 | grep -E "^test |error" | head
```
Expected: the four new tests FAIL (I32Vector currently becomes `Unread`, F32Matrix likewise, attachment check demands `n*8`).

- [ ] **Step 3: Implement**

In `xisf.rs`, add above `finish_property`:

```rust
/// Element kind and byte width of an XISF numeric vector/matrix type name;
/// `None` for non-numeric-array types.
fn numeric_array_type(type_: &str) -> Option<(NumKind, usize, bool)> {
    // (kind, width, is_matrix)
    let (base, is_matrix) = if let Some(b) = type_.strip_suffix("Vector") {
        (b, false)
    } else if let Some(b) = type_.strip_suffix("Matrix") {
        (b, true)
    } else {
        return None;
    };
    let (kind, width) = match base {
        "I8" => (NumKind::I8, 1),
        "UI8" => (NumKind::U8, 1),
        "I16" => (NumKind::I16, 2),
        "UI16" => (NumKind::U16, 2),
        "I32" => (NumKind::I32, 4),
        "UI32" => (NumKind::U32, 4),
        "I64" => (NumKind::I64, 8),
        "UI64" => (NumKind::U64, 8),
        "F32" => (NumKind::F32, 4),
        "F64" => (NumKind::F64, 8),
        _ => return None,
    };
    Some((kind, width, is_matrix))
}

#[derive(Clone, Copy)]
enum NumKind { I8, U8, I16, U16, I32, U32, I64, U64, F32, F64 }

/// Decode little-endian elements of `kind` into f64s (integers convert
/// exactly up to 2^53).
fn decode_numbers(kind: NumKind, bytes: &[u8]) -> Vec<f64> {
    macro_rules! conv {
        ($t:ty) => {
            bytes.chunks_exact(std::mem::size_of::<$t>())
                .map(|c| <$t>::from_le_bytes(c.try_into().unwrap()) as f64)
                .collect()
        };
    }
    match kind {
        NumKind::I8 => conv!(i8), NumKind::U8 => conv!(u8),
        NumKind::I16 => conv!(i16), NumKind::U16 => conv!(u16),
        NumKind::I32 => conv!(i32), NumKind::U32 => conv!(u32),
        NumKind::I64 => conv!(i64), NumKind::U64 => conv!(u64),
        NumKind::F32 => conv!(f32), NumKind::F64 => conv!(f64),
    }
}
```

Then in `finish_property` replace the `"F64Vector" | "F64Matrix" =>` arm with a guard arm `t if numeric_array_type(t).is_some() =>` whose body is today's body with: `let (kind, width, is_matrix) = numeric_array_type(&p.type_).unwrap();`, `n * 8` → `n * width` (both the inline and the attachment checks), the inline chunk decoding replaced by `decode_numbers(kind, &bytes)`, and `if p.type_ == "F64Vector"` → `if !is_matrix`. Keep the `rows/columns` requirement for matrices and `length` for vectors.

In `read_attached_f64s` (rename to `read_attached_numbers`): take `kind`/`width` from `numeric_array_type(&prop.type_)`, replace the `% 8` check by `% width`, and decode with `decode_numbers`. Update its caller in `open` (the loop that resolves attachment-located properties) accordingly — it currently matches on `PropertyValue::F64Vec | F64Mat` with a `location`, which still holds. Update the doc comment in `formats/mod.rs` on `F64Vec`/`F64Mat`: "any XISF numeric vector/matrix type, converted to f64; `type_` records the original type".

- [ ] **Step 4: Run tests**

```bash
cargo test -p mmm-core 2>&1 | tail -3 && cargo clippy --all-targets -p mmm-core 2>&1 | tail -1
```
Expected: all pass (including the four new tests), clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/formats crates/mmm-core/src/synth.rs
git commit -m "feat(xisf): decode every numeric vector/matrix property type (I32Vector etc.)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `spline.rs` — RBF kernels and scalar surface spline evaluation

**Files:**
- Create: `crates/mmm-core/src/astrometry/spline.rs`
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (add `pub(crate) mod spline;`)

**Interfaces:**
- Produces:
```rust
pub(crate) enum Kernel { ThinPlateSpline, VariableOrder, Gaussian, Multiquadric, InverseMultiquadric, InverseQuadratic }
impl Kernel {
    pub(crate) fn parse(id: &str) -> Option<Kernel>;           // spec identifiers, exact match
    pub(crate) fn has_shape(self) -> bool;                     // Gaussian/MQ/IMQ/IQ
    pub(crate) fn requires_polynomial(self) -> bool;           // TPS, VariableOrder
    pub(crate) fn phi(self, r2: f64, e2: f64, order: u32) -> f64;
}
pub(crate) struct ScalarSpline {
    pub kernel: Kernel, pub order: u32, pub polynomial: bool,
    pub x0: f64, pub y0: f64, pub r0: f64,   // normalization: u = r0·(x − x0)
    pub nodes: Vec<[f64; 2]>,                // normalized
    pub coef: Vec<f64>,                      // n radial + q polynomial
    pub eps2: f64,                           // ε² (normalized units); 0 when no shape
}
impl ScalarSpline {
    pub(crate) fn poly_terms(order: u32, polynomial: bool) -> usize;  // q = m(m+1)/2 or 0
    pub(crate) fn validate(&self) -> Result<(), String>;               // lengths, r0 > 0, order ≥ 2, kernel rules
    pub(crate) fn eval(&self, x: f64, y: f64) -> f64;                  // source coordinates
}
```
- Test-only: `pub(crate) mod testfit { pub(crate) fn fit(kernel: Kernel, order: u32, polynomial: bool, eps: f64, nodes: &[[f64; 2]], z: &[f64]) -> ScalarSpline }` — dense interpolation fit using `crate::linalg::solve_dense`.

- [ ] **Step 1: Write the failing tests**

Create `crates/mmm-core/src/astrometry/spline.rs` with the tests at the bottom first:

```rust
#[cfg(test)]
pub(crate) mod testfit {
    //! Dense interpolating fit for fixtures: solves [[K, P], [Pᵀ, 0]]·[c; d] = [z; 0]
    //! (or K·c = z without a polynomial part). Nodes are normalized with
    //! x0/y0 = centroid and r0 = 1 / max |offset| so the layout matches what
    //! encoders write.
    use super::*;

    pub(crate) fn fit(kernel: Kernel, order: u32, polynomial: bool, eps: f64, nodes: &[[f64; 2]], z: &[f64]) -> ScalarSpline {
        let n = nodes.len();
        assert_eq!(z.len(), n);
        let x0 = nodes.iter().map(|p| p[0]).sum::<f64>() / n as f64;
        let y0 = nodes.iter().map(|p| p[1]).sum::<f64>() / n as f64;
        let spread = nodes.iter().map(|p| (p[0] - x0).abs().max((p[1] - y0).abs())).fold(0.0, f64::max);
        let r0 = 1.0 / spread;
        let nn: Vec<[f64; 2]> = nodes.iter().map(|p| [r0 * (p[0] - x0), r0 * (p[1] - y0)]).collect();
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
        ScalarSpline { kernel, order, polynomial, x0, y0, r0, nodes: nn, coef, eps2 }
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
                v.push([i as f64 * span / (n - 1) as f64 + 100.0, j as f64 * span / (n - 1) as f64 + 50.0]);
            }
        }
        v
    }

    #[test]
    fn kernel_identifiers_follow_the_spec_vocabulary() {
        assert_eq!(Kernel::parse("ThinPlateSpline"), Some(Kernel::ThinPlateSpline));
        assert_eq!(Kernel::parse("VariableOrder"), Some(Kernel::VariableOrder));
        assert_eq!(Kernel::parse("InverseQuadratic"), Some(Kernel::InverseQuadratic));
        assert_eq!(Kernel::parse("thinplatespline"), None);
        assert_eq!(Kernel::parse("DDMThinPlateSpline"), None);
        assert!(Kernel::Gaussian.has_shape() && !Kernel::ThinPlateSpline.has_shape());
        assert!(Kernel::VariableOrder.requires_polynomial() && !Kernel::Multiquadric.requires_polynomial());
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
        assert!((Kernel::InverseMultiquadric.phi(r2, e2, 2) - 1.0 / (1.0 + e2 * r2).sqrt()).abs() < 1e-12);
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
        assert!((s.eval(x, y) - f(&[x, y])).abs() < 1e-3, "interior smoothness");
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
        let z: Vec<f64> = nodes.iter().map(|p| (p[0] * 0.01).sin() + (p[1] * 0.02).cos()).collect();
        let s3 = fit(Kernel::VariableOrder, 3, true, 0.0, &nodes, &z);
        let sg = fit(Kernel::Gaussian, 2, false, 1.2, &nodes, &z);
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
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p mmm-core astrometry::spline 2>&1 | grep -E "error\[|^test " | head
```
Expected: compile errors (`Kernel`, `ScalarSpline`, `monomials` undefined).

- [ ] **Step 3: Implement**

Top of `spline.rs` (above the test modules):

```rust
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
    pub(crate) fn has_shape(self) -> bool {
        !matches!(self, Kernel::ThinPlateSpline | Kernel::VariableOrder)
    }
    pub(crate) fn requires_polynomial(self) -> bool {
        matches!(self, Kernel::ThinPlateSpline | Kernel::VariableOrder)
    }
    /// Kernel value for squared normalized distance `r2`.
    pub(crate) fn phi(self, r2: f64, e2: f64, order: u32) -> f64 {
        match self {
            Kernel::ThinPlateSpline => {
                if r2 <= 0.0 { 0.0 } else { 0.5 * r2 * r2.ln() }
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

/// Monomials of total degree `< order` in `u, v`, spec order.
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
#[derive(Debug, Clone)]
pub(crate) struct ScalarSpline {
    pub kernel: Kernel,
    pub order: u32,
    pub polynomial: bool,
    pub x0: f64,
    pub y0: f64,
    pub r0: f64,
    pub nodes: Vec<[f64; 2]>,
    pub coef: Vec<f64>,
    pub eps2: f64,
}

impl ScalarSpline {
    pub(crate) fn poly_terms(order: u32, polynomial: bool) -> usize {
        if polynomial { (order as usize) * (order as usize + 1) / 2 } else { 0 }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(self.r0 > 0.0) || !self.r0.is_finite() {
            return Err("normalization scale must be > 0".into());
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
            return Err(format!("coefficient count {} != nodes + polynomial terms {want}", self.coef.len()));
        }
        if self.nodes.iter().flatten().chain(&self.coef).any(|v| !v.is_finite()) {
            return Err("non-finite spline data".into());
        }
        Ok(())
    }

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
```

Add `pub(crate) mod spline;` to `astrometry/mod.rs`.

- [ ] **Step 4: Run tests**

```bash
cargo test -p mmm-core astrometry::spline 2>&1 | tail -3 && cargo clippy --all-targets -p mmm-core 2>&1 | tail -1
```
Expected: 7 passed, clippy clean (`#[allow(clippy::too_many_arguments)]` is not needed; if clippy flags `needless_range_loop` in `monomials`, rewrite with iterators rather than allow).

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/astrometry
git commit -m "feat(astrometry): RBF kernels and scalar surface spline evaluation (XISF rev 1)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `spline.rs` — vector splines, term model (Global/Local/Fallback), disc index

**Files:**
- Modify: `crates/mmm-core/src/astrometry/spline.rs`

**Interfaces:**
- Produces:
```rust
pub(crate) struct VectorSpline { pub x: ScalarSpline, pub y: ScalarSpline }   // eval(x,y) -> (f64,f64); validate()
pub(crate) struct LocalTerm { pub center: [f64; 2], pub radius: f64, pub spline: VectorSpline }
pub(crate) struct TermModel {
    pub global: Option<VectorSpline>,
    pub local: Vec<LocalTerm>,
    pub fallback: Option<(f64, VectorSpline)>,   // (threshold t0, spline)
    index: DiscIndex,
}
impl TermModel {
    pub(crate) fn new(global: Option<VectorSpline>, local: Vec<LocalTerm>, fallback: Option<(f64, VectorSpline)>) -> Result<TermModel, String>;
    pub(crate) fn residual(&self, x: f64, y: f64) -> (f64, f64);
}
pub(crate) fn wendland_c2(t: f64) -> f64;   // (1−t)⁴(4t+1) for t<1, else 0
```

- [ ] **Step 1: Write the failing tests** (append inside `mod tests`)

```rust
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
            LocalTerm { center: [120.0, 80.0], radius: 90.0, spline: g.clone() },
            LocalTerm { center: [180.0, 120.0], radius: 90.0, spline: g.clone() },
        ];
        let m = TermModel::new(None, local, None).unwrap();
        for (x, y) in [(150.0, 100.0), (60.0, 80.0), (250.0, 150.0)] {
            let a = m.residual(x, y);
            let b = g.eval(x, y);
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9, "at ({x},{y})");
        }
    }

    #[test]
    fn fallback_takes_over_where_coverage_fades_and_is_continuous() {
        let nodes = lattice(4, 100.0);
        let l = affine_vector(&nodes, 0.1, 0.2);
        let f = affine_vector(&nodes, -0.3, 0.05);
        let local = vec![LocalTerm { center: [150.0, 100.0], radius: 50.0, spline: l.clone() }];
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
        // Continuity across the t0 boundary: step in x and check no jump.
        let mut prev = m.residual(150.0, 100.0);
        for i in 1..=200 {
            let x = 150.0 + i as f64 * 0.5;
            let cur = m.residual(x, 100.0);
            assert!((cur.0 - prev.0).abs() < 0.5 && (cur.1 - prev.1).abs() < 0.5, "jump at x={x}");
            prev = cur;
        }
    }

    #[test]
    fn no_coverage_without_fallback_uses_nearest_disc() {
        let nodes = lattice(4, 100.0);
        let a = affine_vector(&nodes, 0.1, 0.2);
        let b = affine_vector(&nodes, -0.3, 0.05);
        let local = vec![
            LocalTerm { center: [0.0, 0.0], radius: 10.0, spline: a.clone() },
            LocalTerm { center: [1000.0, 0.0], radius: 10.0, spline: b.clone() },
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
            local.push(LocalTerm { center: [cx, cy], radius: 20.0 + (i % 5) as f64 * 15.0, spline: s.clone() });
        }
        let m = TermModel::new(None, local, None).unwrap();
        for (x, y) in [(0.0, 0.0), (250.0, 150.0), (499.0, 299.0), (-50.0, 400.0), (123.4, 56.7)] {
            let brute: Vec<usize> = m.local.iter().enumerate()
                .filter(|(_, t)| (x - t.center[0]).hypot(y - t.center[1]) < t.radius)
                .map(|(i, _)| i).collect();
            let mut idx: Vec<usize> = m.index.candidates(x, y).iter().map(|&i| i as usize)
                .filter(|&i| { let t = &m.local[i]; (x - t.center[0]).hypot(y - t.center[1]) < t.radius })
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
        assert!(TermModel::new(None, vec![LocalTerm { center: [0.0, 0.0], radius: 0.0, spline: s.clone() }], None).is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p mmm-core astrometry::spline 2>&1 | grep -E "error\[" | head -3
```
Expected: compile errors for `VectorSpline`, `TermModel`, `LocalTerm`, `wendland_c2`.

- [ ] **Step 3: Implement** (append above the test modules)

```rust
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
#[derive(Debug, Clone)]
pub(crate) struct VectorSpline {
    pub x: ScalarSpline,
    pub y: ScalarSpline,
}

impl VectorSpline {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.x.validate().map_err(|e| format!("X: {e}"))?;
        self.y.validate().map_err(|e| format!("Y: {e}"))
    }
    pub(crate) fn eval(&self, x: f64, y: f64) -> (f64, f64) {
        (self.x.eval(x, y), self.y.eval(x, y))
    }
}

/// A Local term: a spline with compact support on a disc (spec §11.5.3.7.4.1).
#[derive(Debug, Clone)]
pub(crate) struct LocalTerm {
    pub center: [f64; 2],
    pub radius: f64,
    pub spline: VectorSpline,
}

/// Uniform bucket index over Local disc centers: each cell lists the discs
/// that intersect it. Rebuilt from centers and radii alone (the spec keeps
/// spatial structure out of the model).
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
        let cell = (2.0 * rsum / local.len() as f64).max(1e-9);
        let nx = (((x1 - x0) / cell).ceil() as usize).clamp(1, 4096);
        let ny = (((y1 - y0) / cell).ceil() as usize).clamp(1, 4096);
        let cell = ((x1 - x0) / nx as f64).max((y1 - y0) / ny as f64).max(cell);
        let mut cells = vec![Vec::new(); nx * ny];
        for (i, t) in local.iter().enumerate() {
            let cx0 = (((t.center[0] - t.radius - x0) / cell).floor().max(0.0) as usize).min(nx - 1);
            let cx1 = (((t.center[0] + t.radius - x0) / cell).floor().max(0.0) as usize).min(nx - 1);
            let cy0 = (((t.center[1] - t.radius - y0) / cell).floor().max(0.0) as usize).min(ny - 1);
            let cy1 = (((t.center[1] + t.radius - y0) / cell).floor().max(0.0) as usize).min(ny - 1);
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    cells[cy * nx + cx].push(i as u32);
                }
            }
        }
        DiscIndex { x0, y0, cell, nx, ny, cells }
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
    pub global: Option<VectorSpline>,
    pub local: Vec<LocalTerm>,
    pub fallback: Option<(f64, VectorSpline)>,
    index: DiscIndex,
}

impl TermModel {
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
            if !(t.radius > 0.0) || !t.radius.is_finite() || !t.center.iter().all(|c| c.is_finite()) {
                return Err(format!("Local term {k}: invalid center/radius"));
            }
            t.spline.validate().map_err(|e| format!("Local term {k}: {e}"))?;
        }
        if let Some((t0, f)) = &fallback {
            if !(*t0 > 0.0) {
                return Err("Fallback threshold must be > 0".into());
            }
            f.validate().map_err(|e| format!("Fallback term: {e}"))?;
        }
        let index = DiscIndex::build(&local);
        Ok(TermModel { global, local, fallback, index })
    }

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
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mmm-core astrometry::spline 2>&1 | tail -3 && cargo clippy --all-targets -p mmm-core 2>&1 | tail -1
```
Expected: 14 passed, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/astrometry/spline.rs
git commit -m "feat(astrometry): vector splines and Global/Local/Fallback term model

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: `standard.rs` — parse the XISF rev 1 block (layers 1–3)

**Files:**
- Create: `crates/mmm-core/src/astrometry/standard.rs`
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (add `pub(crate) mod standard;`)

**Interfaces:**
- Produces:
```rust
pub(crate) const STD_PREFIX: &str = "AstrometricSolution:";
pub(crate) fn has_standard_block(props: &[XisfProperty]) -> bool;   // Version property present
pub(crate) struct Homography(pub [[f64; 3]; 3]);  impl { fn apply(&self, x, y) -> (f64, f64) }
pub(crate) struct Direction { pub projective: Homography, pub distortion: Option<TermModel> }
impl Direction { pub(crate) fn map(&self, x: f64, y: f64) -> (f64, f64) }   // projective + residual
pub(crate) struct StandardSolution {
    pub linear: LinearWcs,
    pub image_to_projection: Option<Direction>,
    pub projection_to_image: Option<Direction>,
    pub notes: Vec<String>,     // why a higher layer was dropped (logged by the caller)
}
pub(crate) fn parse_standard(props: &[XisfProperty]) -> Result<StandardSolution, String>;
pub(crate) fn linear_from_standard(props: &[XisfProperty]) -> Result<LinearWcs, String>;
```
- Consumes: `spline::{Kernel, ScalarSpline, VectorSpline, LocalTerm, TermModel}`, `mod::{find_value, projection_code, LinearWcs}`.
- Test-only builder (used again in Task 6 and 7): `pub(crate) mod fixtures` with `fn layer1(crval, refimg, cd) -> Vec<XisfProperty>`, `fn layer2(props: &mut Vec<XisfProperty>, i2p: [[f64;3];3], p2i: [[f64;3];3])`, `fn layer3_global(props: &mut Vec<XisfProperty>, dir: &str, v: &VectorSpline)`, `fn layer3_local(props: &mut Vec<XisfProperty>, dir: &str, terms: &[LocalTerm], fallback: Option<(f64, &VectorSpline)>)`, `fn prop(id, type_, value) -> XisfProperty`.

- [ ] **Step 1: Write the failing tests**

Create `standard.rs` with these test modules at the bottom:

```rust
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::astrometry::spline::{LocalTerm, ScalarSpline, VectorSpline};
    use crate::formats::PropertyValue;

    pub(crate) fn prop(id: &str, type_: &str, value: PropertyValue) -> XisfProperty {
        XisfProperty { id: id.into(), type_: type_.into(), value, location: None }
    }
    fn vec2(id: &str, v: [f64; 2]) -> XisfProperty { prop(id, "F64Vector", PropertyValue::F64Vec(v.to_vec())) }
    fn mat(id: &str, rows: u32, cols: u32, data: Vec<f64>) -> XisfProperty {
        prop(id, "F64Matrix", PropertyValue::F64Mat { rows, cols, data })
    }
    fn s(id: &str, v: &str) -> XisfProperty { prop(id, "String", PropertyValue::Str(v.into())) }

    pub(crate) fn layer1(crval: [f64; 2], refimg: [f64; 2], cd: [[f64; 2]; 2]) -> Vec<XisfProperty> {
        vec![
            s("AstrometricSolution:Version", "1.0"),
            s("AstrometricSolution:ProjectionSystem", "Gnomonic"),
            vec2("AstrometricSolution:ReferenceCelestialCoordinates", crval),
            vec2("AstrometricSolution:ReferenceImageCoordinates", refimg),
            mat("AstrometricSolution:LinearTransformationMatrix", 2, 2, vec![cd[0][0], cd[0][1], cd[1][0], cd[1][1]]),
            s("AstrometricSolution:CelestialReferenceSystem", "ICRS"),
        ]
    }

    pub(crate) fn layer2(props: &mut Vec<XisfProperty>, i2p: [[f64; 3]; 3], p2i: [[f64; 3]; 3]) {
        let flat = |m: [[f64; 3]; 3]| m.iter().flatten().copied().collect::<Vec<_>>();
        props.push(mat("AstrometricSolution:ProjectiveTransformation:ImageToProjection", 3, 3, flat(i2p)));
        props.push(mat("AstrometricSolution:ProjectiveTransformation:ProjectionToImage", 3, 3, flat(p2i)));
    }

    fn scalar_record(props: &mut Vec<XisfProperty>, p: &str, sp: &ScalarSpline, with_nodes: bool) {
        if with_nodes {
            props.push(prop(&format!("{p}Normalization"), "F64Vector", PropertyValue::F64Vec(vec![sp.x0, sp.y0, sp.r0])));
            props.push(mat(&format!("{p}Nodes"), sp.nodes.len() as u32, 2, sp.nodes.iter().flatten().copied().collect()));
        }
        props.push(prop(&format!("{p}Coefficients"), "F64Vector", PropertyValue::F64Vec(sp.coef.clone())));
        if sp.kernel.has_shape() {
            props.push(prop(&format!("{p}ShapeParameter"), "Float64", PropertyValue::F64(sp.eps2.sqrt())));
        }
    }

    fn header(props: &mut Vec<XisfProperty>, p: &str, sp: &ScalarSpline, terms: &str) {
        props.push(s(&format!("{p}BasisFunction"), match sp.kernel {
            Kernel::ThinPlateSpline => "ThinPlateSpline", Kernel::VariableOrder => "VariableOrder",
            Kernel::Gaussian => "Gaussian", Kernel::Multiquadric => "Multiquadric",
            Kernel::InverseMultiquadric => "InverseMultiquadric", Kernel::InverseQuadratic => "InverseQuadratic",
        }));
        props.push(prop(&format!("{p}Order"), "Int32", PropertyValue::I64(sp.order as i64)));
        props.push(prop(&format!("{p}Polynomial"), "Boolean", PropertyValue::I64(sp.polynomial as i64)));
        props.push(s(&format!("{p}Terms"), terms));
    }

    /// `dir` is `ImageToProjection` or `ProjectionToImage`. Y shares X's nodes
    /// only when they are identical (then Y:Nodes/Normalization are omitted).
    pub(crate) fn layer3_global(props: &mut Vec<XisfProperty>, dir: &str, v: &VectorSpline) {
        let p = format!("AstrometricSolution:DistortionModel:{dir}:");
        header(props, &p, &v.x, "Global");
        scalar_record(props, &format!("{p}Global:X:"), &v.x, true);
        let shared = v.x.nodes == v.y.nodes && v.x.x0 == v.y.x0 && v.x.y0 == v.y.y0 && v.x.r0 == v.y.r0 && v.x.eps2 == v.y.eps2;
        scalar_record(props, &format!("{p}Global:Y:"), &v.y, !shared);
    }

    /// Local terms packed per the spec (shared Y nodes when every term's Y
    /// spline shares its X nodes; otherwise separate Y arrays), plus an
    /// optional Fallback record.
    pub(crate) fn layer3_local(props: &mut Vec<XisfProperty>, dir: &str, terms: &[LocalTerm], fallback: Option<(f64, &VectorSpline)>, extra_global: Option<&VectorSpline>) {
        let p = format!("AstrometricSolution:DistortionModel:{dir}:");
        let s0 = &terms[0].spline.x;
        let mut kinds = Vec::new();
        if extra_global.is_some() { kinds.push("Global"); }
        kinds.push("Local");
        if fallback.is_some() { kinds.push("Fallback"); }
        header(props, &p, s0, &kinds.join("\n"));
        if let Some(g) = extra_global {
            scalar_record(props, &format!("{p}Global:X:"), &g.x, true);
            scalar_record(props, &format!("{p}Global:Y:"), &g.y, true);
        }
        let q = ScalarSpline::poly_terms(s0.order, s0.polynomial);
        let separate = terms.iter().any(|t| t.spline.x.nodes != t.spline.y.nodes);
        let (mut center, mut radius, mut norm, mut off, mut nodes, mut cx) = (vec![], vec![], vec![], vec![0.0], vec![], vec![]);
        let (mut norm2, mut off2, mut nodes2, mut cy) = (vec![], vec![0.0], vec![], vec![]);
        let mut shapes = vec![];
        let mut shapes2 = vec![];
        for t in terms {
            center.extend(t.center);
            radius.push(t.radius);
            let x = &t.spline.x;
            norm.extend([x.x0, x.y0, x.r0]);
            nodes.extend(x.nodes.iter().flatten().copied());
            off.push(off.last().unwrap() + x.nodes.len() as f64);
            cx.extend(&x.coef);
            shapes.push(x.eps2.sqrt());
            let y = &t.spline.y;
            if separate {
                norm2.extend([y.x0, y.y0, y.r0]);
                nodes2.extend(y.nodes.iter().flatten().copied());
                off2.push(off2.last().unwrap() + y.nodes.len() as f64);
                shapes2.push(y.eps2.sqrt());
            }
            cy.extend(&y.coef);
        }
        let n = terms.len() as u32;
        props.push(mat(&format!("{p}Local:Center"), n, 2, center));
        props.push(prop(&format!("{p}Local:Radius"), "F64Vector", PropertyValue::F64Vec(radius)));
        props.push(mat(&format!("{p}Local:X:Normalization"), n, 3, norm));
        props.push(prop(&format!("{p}Local:X:NodeOffsets"), "I32Vector", PropertyValue::F64Vec(off)));
        props.push(mat(&format!("{p}Local:X:Nodes"), (nodes.len() / 2) as u32, 2, nodes));
        props.push(prop(&format!("{p}Local:X:Coefficients"), "F64Vector", PropertyValue::F64Vec(cx)));
        if s0.kernel.has_shape() {
            props.push(prop(&format!("{p}Local:X:ShapeParameter"), "F64Vector", PropertyValue::F64Vec(shapes)));
        }
        if separate {
            props.push(mat(&format!("{p}Local:Y:Normalization"), n, 3, norm2));
            props.push(prop(&format!("{p}Local:Y:NodeOffsets"), "I32Vector", PropertyValue::F64Vec(off2)));
            props.push(mat(&format!("{p}Local:Y:Nodes"), (nodes2.len() / 2) as u32, 2, nodes2));
            if s0.kernel.has_shape() {
                props.push(prop(&format!("{p}Local:Y:ShapeParameter"), "F64Vector", PropertyValue::F64Vec(shapes2)));
            }
        }
        props.push(prop(&format!("{p}Local:Y:Coefficients"), "F64Vector", PropertyValue::F64Vec(cy)));
        let _ = q;
        if let Some((t0, f)) = fallback {
            props.push(prop(&format!("{p}Fallback:Threshold"), "Float64", PropertyValue::F64(t0)));
            scalar_record(props, &format!("{p}Fallback:X:"), &f.x, true);
            scalar_record(props, &format!("{p}Fallback:Y:"), &f.y, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::astrometry::spline::testfit::fit;
    use crate::astrometry::spline::{Kernel, LocalTerm, VectorSpline};
    use crate::formats::PropertyValue;

    const S: f64 = 4.4e-4;
    fn base() -> Vec<XisfProperty> { layer1([84.2, -3.24], [2449.0, 1615.0], [[-S, 0.0], [0.0, S]]) }
    fn affine_h(a: [[f64; 2]; 2], t: [f64; 2]) -> [[f64; 3]; 3] {
        [[a[0][0], a[0][1], t[0]], [a[1][0], a[1][1], t[1]], [0.0, 0.0, 1.0]]
    }
    fn lattice(n: usize, w: f64, h: f64) -> Vec<[f64; 2]> {
        let mut v = Vec::new();
        for i in 0..n { for j in 0..n { v.push([i as f64 * w / (n - 1) as f64, j as f64 * h / (n - 1) as f64]); } }
        v
    }
    fn vector_from(nodes: &[[f64; 2]], f: impl Fn(f64, f64) -> (f64, f64)) -> VectorSpline {
        let zx: Vec<f64> = nodes.iter().map(|p| f(p[0], p[1]).0).collect();
        let zy: Vec<f64> = nodes.iter().map(|p| f(p[0], p[1]).1).collect();
        VectorSpline { x: fit(Kernel::ThinPlateSpline, 2, true, 0.0, nodes, &zx), y: fit(Kernel::ThinPlateSpline, 2, true, 0.0, nodes, &zy) }
    }

    #[test]
    fn layer1_gives_the_linear_solution_with_standard_conventions() {
        let sol = parse_standard(&base()).unwrap();
        assert_eq!(sol.linear.crval, [84.2, -3.24]);
        assert_eq!(sol.linear.crpix, [2449.5, 1615.5]);
        assert_eq!(sol.linear.ctype[0], "RA---TAN");
        assert_eq!(sol.linear.radesys, "ICRS");
        assert!(sol.image_to_projection.is_none() && sol.projection_to_image.is_none());
        assert!(has_standard_block(&base()));
    }

    #[test]
    fn celestial_reference_system_defaults_to_icrs_and_overrides_observation() {
        let mut p = base();
        p.retain(|x| x.id != "AstrometricSolution:CelestialReferenceSystem");
        p.push(prop("Observation:CelestialReferenceSystem", "String", PropertyValue::Str("GCRS".into())));
        assert_eq!(parse_standard(&p).unwrap().linear.radesys, "ICRS");
        p.push(prop("AstrometricSolution:CelestialReferenceSystem", "String", PropertyValue::Str("GCRS".into())));
        assert_eq!(parse_standard(&p).unwrap().linear.radesys, "GCRS");
    }

    #[test]
    fn unsupported_major_version_and_missing_required_properties_fail() {
        let mut p = base();
        p[0] = prop("AstrometricSolution:Version", "String", PropertyValue::Str("2.0".into()));
        assert!(parse_standard(&p).unwrap_err().contains("major"));
        let mut p = base();
        p[0] = prop("AstrometricSolution:Version", "String", PropertyValue::Str("1.3".into()));
        assert!(parse_standard(&p).is_ok(), "minor revisions are additive");
        let mut p = base();
        p.retain(|x| x.id != "AstrometricSolution:LinearTransformationMatrix");
        assert!(parse_standard(&p).unwrap_err().contains("LinearTransformationMatrix"));
        let mut p = base();
        p[1] = prop("AstrometricSolution:ProjectionSystem", "String", PropertyValue::Str("Bonne".into()));
        assert!(parse_standard(&p).is_err(), "unknown projection makes the whole solution unavailable");
    }

    #[test]
    fn layer2_requires_both_matrices_and_applies_a_homography() {
        let mut p = base();
        layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [2449.0 * S, -1615.0 * S]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [2449.0, 1615.0]));
        let sol = parse_standard(&p).unwrap();
        let d = sol.image_to_projection.as_ref().unwrap();
        let (xi, eta) = d.map(2449.0, 1615.0);
        assert!(xi.abs() < 1e-12 && eta.abs() < 1e-12);
        let inv = sol.projection_to_image.as_ref().unwrap().map(xi, eta);
        assert!((inv.0 - 2449.0).abs() < 1e-9 && (inv.1 - 1615.0).abs() < 1e-9);
        // Only one matrix ⇒ layer unavailable, solution still valid (layer 1), with a note.
        let mut p = base();
        layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [0.0, 0.0]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [0.0, 0.0]));
        p.retain(|x| x.id != "AstrometricSolution:ProjectiveTransformation:ProjectionToImage");
        let sol = parse_standard(&p).unwrap();
        assert!(sol.image_to_projection.is_none() && !sol.notes.is_empty());
    }

    #[test]
    fn homography_divides_by_w() {
        let h = Homography([[2.0, 0.0, 1.0], [0.0, 3.0, 0.0], [0.5, 0.0, 1.0]]);
        let (x, y) = h.apply(2.0, 1.0);
        assert!((x - 5.0 / 2.0).abs() < 1e-12 && (y - 3.0 / 2.0).abs() < 1e-12);
    }

    #[test]
    fn layer3_global_round_trips_through_properties() {
        let nodes = lattice(6, 4000.0, 3000.0);
        let v = vector_from(&nodes, |x, y| (1e-4 * (x - 2000.0) + 2e-9 * (x - 2000.0) * (y - 1500.0), 1e-4 * (y - 1500.0)));
        let mut p = base();
        layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [0.0, 0.0]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [0.0, 0.0]));
        layer3_global(&mut p, "ImageToProjection", &v);
        layer3_global(&mut p, "ProjectionToImage", &v);
        let sol = parse_standard(&p).unwrap();
        let d = sol.image_to_projection.as_ref().unwrap();
        let m = d.distortion.as_ref().unwrap();
        for (x, y) in [(0.0, 0.0), (1234.5, 678.9), (4000.0, 3000.0)] {
            let a = m.residual(x, y);
            let b = v.eval(x, y);
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        }
        // Y with its own nodes (different lattice) also round-trips.
        let nodes_y = lattice(5, 4000.0, 3000.0);
        let vy = VectorSpline { x: v.x.clone(), y: vector_from(&nodes_y, |x, y| (0.0, 3e-4 * y + 1e-9 * x * y)).y };
        let mut p = base();
        layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [0.0, 0.0]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [0.0, 0.0]));
        layer3_global(&mut p, "ImageToProjection", &vy);
        layer3_global(&mut p, "ProjectionToImage", &vy);
        assert!(p.iter().any(|x| x.id.ends_with("Global:Y:Nodes")));
        let sol = parse_standard(&p).unwrap();
        let m = sol.image_to_projection.as_ref().unwrap().distortion.as_ref().unwrap();
        let a = m.residual(777.0, 999.0);
        let b = vy.eval(777.0, 999.0);
        assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
    }

    #[test]
    fn layer3_local_and_fallback_round_trip_with_packed_offsets() {
        let n1 = lattice(4, 300.0, 300.0);
        let n2: Vec<[f64; 2]> = lattice(5, 300.0, 300.0).iter().map(|p| [p[0] + 200.0, p[1] + 100.0]).collect();
        let t1 = LocalTerm { center: [150.0, 150.0], radius: 260.0, spline: vector_from(&n1, |x, y| (1e-3 * x, -2e-3 * y)) };
        let t2 = LocalTerm { center: [350.0, 250.0], radius: 260.0, spline: vector_from(&n2, |x, y| (5e-4 * y, 7e-4 * x)) };
        let fb = vector_from(&lattice(4, 600.0, 400.0), |x, y| (1e-5 * x, 1e-5 * y));
        let mut p = base();
        layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [0.0, 0.0]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [0.0, 0.0]));
        layer3_local(&mut p, "ImageToProjection", &[t1.clone(), t2.clone()], Some((0.15, &fb)), None);
        layer3_local(&mut p, "ProjectionToImage", &[t1.clone(), t2.clone()], Some((0.15, &fb)), None);
        assert!(p.iter().any(|x| x.id.ends_with("Terms") && x.value.as_str() == Some("Local\nFallback")));
        let sol = parse_standard(&p).unwrap();
        let m = sol.image_to_projection.as_ref().unwrap().distortion.as_ref().unwrap();
        let expect = crate::astrometry::spline::TermModel::new(None, vec![t1, t2], Some((0.15, fb))).unwrap();
        for (x, y) in [(150.0, 150.0), (350.0, 250.0), (250.0, 200.0), (900.0, 900.0)] {
            let a = m.residual(x, y);
            let b = expect.residual(x, y);
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9, "at ({x},{y})");
        }
    }

    #[test]
    fn unknown_identifiers_and_inconsistent_dimensions_drop_layer3_only() {
        let nodes = lattice(4, 400.0, 300.0);
        let v = vector_from(&nodes, |x, y| (1e-4 * x, 1e-4 * y));
        let build = || {
            let mut p = base();
            layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [0.0, 0.0]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [0.0, 0.0]));
            layer3_global(&mut p, "ImageToProjection", &v);
            layer3_global(&mut p, "ProjectionToImage", &v);
            p
        };
        let mut p = build();
        let i = p.iter().position(|x| x.id.ends_with("ImageToProjection:BasisFunction")).unwrap();
        p[i] = prop(&p[i].id.clone(), "String", PropertyValue::Str("Wendland".into()));
        let sol = parse_standard(&p).unwrap();
        assert!(sol.image_to_projection.as_ref().unwrap().distortion.is_none());
        assert!(sol.notes.iter().any(|n| n.contains("Wendland")));
        let mut p = build();
        let i = p.iter().position(|x| x.id.ends_with("ProjectionToImage:Terms")).unwrap();
        p[i] = prop(&p[i].id.clone(), "String", PropertyValue::Str("Global\nQuadtree".into()));
        assert!(parse_standard(&p).unwrap().projection_to_image.as_ref().unwrap().distortion.is_none());
        let mut p = build();
        let i = p.iter().position(|x| x.id.ends_with("ImageToProjection:Global:X:Coefficients")).unwrap();
        if let PropertyValue::F64Vec(c) = &mut p[i].value { c.pop(); }
        let sol = parse_standard(&p).unwrap();
        assert!(sol.image_to_projection.as_ref().unwrap().distortion.is_none(), "both directions drop together");
        assert!(sol.projection_to_image.as_ref().unwrap().distortion.is_none());
    }

    #[test]
    fn layer3_without_layer2_is_unavailable() {
        let nodes = lattice(4, 400.0, 300.0);
        let v = vector_from(&nodes, |x, y| (1e-4 * x, 1e-4 * y));
        let mut p = base();
        layer3_global(&mut p, "ImageToProjection", &v);
        layer3_global(&mut p, "ProjectionToImage", &v);
        let sol = parse_standard(&p).unwrap();
        assert!(sol.image_to_projection.is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p mmm-core astrometry::standard 2>&1 | grep -E "error\[" | head -3
```
Expected: compile errors (`parse_standard`, `Homography`, … undefined).

- [ ] **Step 3: Implement** (top of `standard.rs`)

```rust
//! XISF 1.0 revision 1 standard astrometric solution (`AstrometricSolution:*`,
//! spec §11.5.3.7), the format PixInsight ≥ 1.9.5 writes. Verified on a real
//! 1.9.5-regenerated Orion panel (Global-only `ThinPlateSpline`, order 2,
//! separate X/Y node sets, 3.0k–3.2k nodes per component).
//!
//! Layers: 1 projection (required), 2 projective 3×3 in both directions,
//! 3 RBF distortion model in both directions (requires 2). A layer with a
//! missing/inconsistent property or an unrecognized vocabulary identifier is
//! *unavailable* and every higher layer with it; the decoder steps down and
//! records why in [`StandardSolution::notes`]. Layer 4 (provenance) and the
//! PixInsight-private `PCL:AstrometricSolution:{Generation,Grid}:*` extras are
//! ignored. Image coordinates are PixInsight's (0-based, pixel k spans
//! [k, k+1]); projection plane coordinates are TAN (ξ, η) in degrees.

use super::spline::{Kernel, LocalTerm, ScalarSpline, TermModel, VectorSpline};
use super::{LinearWcs, find_value, projection_code, std_native_frame_ok};
use crate::formats::{PropertyValue, XisfProperty};

/// Prefix of every standard solution property.
pub(crate) const STD_PREFIX: &str = "AstrometricSolution:";

/// True when the standard block's required `Version` property is present.
pub(crate) fn has_standard_block(props: &[XisfProperty]) -> bool {
    find_value(props, "AstrometricSolution:Version").is_some()
}

/// A 3×3 projective transformation on homogeneous coordinates.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Homography(pub [[f64; 3]; 3]);

impl Homography {
    pub(crate) fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let h = &self.0;
        let w = h[2][0] * x + h[2][1] * y + h[2][2];
        ((h[0][0] * x + h[0][1] * y + h[0][2]) / w, (h[1][0] * x + h[1][1] * y + h[1][2]) / w)
    }
}

/// One direction of the image-plane step: projective pre-model plus the
/// optional spline residual field.
#[derive(Debug, Clone)]
pub(crate) struct Direction {
    pub projective: Homography,
    pub distortion: Option<TermModel>,
}

impl Direction {
    pub(crate) fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let (px, py) = self.projective.apply(x, y);
        match &self.distortion {
            Some(m) => {
                let (rx, ry) = m.residual(x, y);
                (px + rx, py + ry)
            }
            None => (px, py),
        }
    }
}

/// Decoded standard solution: the linear layer plus, when available, the
/// projective/spline directions.
#[derive(Debug, Clone)]
pub(crate) struct StandardSolution {
    pub linear: LinearWcs,
    pub image_to_projection: Option<Direction>,
    pub projection_to_image: Option<Direction>,
    pub notes: Vec<String>,
}

fn require<'a>(props: &'a [XisfProperty], suffix: &str) -> Result<&'a PropertyValue, String> {
    find_value(props, &format!("{STD_PREFIX}{suffix}")).ok_or_else(|| format!("missing AstrometricSolution:{suffix}"))
}

fn version_major(props: &[XisfProperty]) -> Result<u32, String> {
    let v = require(props, "Version")?.as_str().ok_or("AstrometricSolution:Version is not a string")?;
    let major = v.trim().split('.').next().and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| format!("malformed AstrometricSolution:Version '{v}'"))?;
    if major != 1 {
        return Err(format!("unsupported AstrometricSolution major revision {major} (this decoder implements 1.x)"));
    }
    Ok(major)
}

/// Layer 1 only, in FITS-convention [`LinearWcs`] form.
pub(crate) fn linear_from_standard(props: &[XisfProperty]) -> Result<LinearWcs, String> {
    version_major(props)?;
    let proj = require(props, "ProjectionSystem")?.as_str().ok_or("ProjectionSystem is not a string")?;
    let code = projection_code(proj).ok_or_else(|| format!("unrecognized ProjectionSystem '{proj}'"))?;
    let crval = require(props, "ReferenceCelestialCoordinates")?.as_f64_vec().ok_or("ReferenceCelestialCoordinates is not a vector")?;
    let refimg = require(props, "ReferenceImageCoordinates")?.as_f64_vec().ok_or("ReferenceImageCoordinates is not a vector")?;
    let (rows, cols, m) = require(props, "LinearTransformationMatrix")?.as_f64_mat().ok_or("LinearTransformationMatrix is not a matrix")?;
    if crval.len() != 2 || refimg.len() != 2 || (rows, cols) != (2, 2) {
        return Err("layer 1 vector/matrix dimensions are inconsistent".into());
    }
    if crval.iter().chain(refimg).chain(m).any(|v| !v.is_finite()) {
        return Err("layer 1 carries non-finite values".into());
    }
    let radesys = find_value(props, "AstrometricSolution:CelestialReferenceSystem")
        .and_then(|v| v.as_str()).unwrap_or("ICRS").to_string();
    Ok(LinearWcs {
        crval: [crval[0], crval[1]],
        crpix: [refimg[0] + 0.5, refimg[1] + 0.5],
        cd: [[m[0], m[1]], [m[2], m[3]]],
        ctype: [format!("{:-<5}{code}", "RA"), format!("{:-<5}{code}", "DEC")],
        radesys,
    })
}

fn homography(props: &[XisfProperty], suffix: &str) -> Result<Homography, String> {
    let (r, c, d) = require(props, suffix)?.as_f64_mat().ok_or_else(|| format!("{suffix} is not a matrix"))?;
    if (r, c) != (3, 3) || d.iter().any(|v| !v.is_finite()) {
        return Err(format!("{suffix} must be a finite 3×3 matrix"));
    }
    Ok(Homography([[d[0], d[1], d[2]], [d[3], d[4], d[5]], [d[6], d[7], d[8]]]))
}

/// Read one scalar spline record at prefix `p` (e.g. `…:Global:X:`), sharing
/// nodes/normalization/shape from `shared` (`…:Global:X:`) when absent.
fn scalar_spline(props: &[XisfProperty], p: &str, shared: Option<&str>, kernel: Kernel, order: u32, polynomial: bool) -> Result<ScalarSpline, String> {
    let get = |pre: &str, s: &str| find_value(props, &format!("{pre}{s}"));
    let (nodes_v, norm_v, shape_v) = match (get(p, "Nodes"), get(p, "Normalization")) {
        (Some(n), Some(nm)) => (n, nm, get(p, "ShapeParameter")),
        _ => {
            let sh = shared.ok_or_else(|| format!("missing {p}Nodes"))?;
            (
                get(sh, "Nodes").ok_or_else(|| format!("missing {sh}Nodes"))?,
                get(sh, "Normalization").ok_or_else(|| format!("missing {sh}Normalization"))?,
                get(p, "ShapeParameter").or_else(|| get(sh, "ShapeParameter")),
            )
        }
    };
    let (r, c, nd) = nodes_v.as_f64_mat().ok_or_else(|| format!("{p}Nodes is not a matrix"))?;
    if c != 2 || nd.len() != (r as usize) * 2 {
        return Err(format!("{p}Nodes must be n×2"));
    }
    let nodes: Vec<[f64; 2]> = nd.chunks_exact(2).map(|q| [q[0], q[1]]).collect();
    let norm = norm_v.as_f64_vec().ok_or_else(|| format!("{p}Normalization is not a vector"))?;
    if norm.len() != 3 {
        return Err(format!("{p}Normalization must have 3 elements"));
    }
    let coef = get(p, "Coefficients").ok_or_else(|| format!("missing {p}Coefficients"))?
        .as_f64_vec().ok_or_else(|| format!("{p}Coefficients is not a vector"))?.to_vec();
    let eps2 = if kernel.has_shape() {
        let e = shape_v.ok_or_else(|| format!("missing {p}ShapeParameter"))?.as_f64().ok_or_else(|| format!("{p}ShapeParameter is not a number"))?;
        e * e
    } else {
        if shape_v.is_some() {
            return Err(format!("{p}ShapeParameter must not be specified for this basis function"));
        }
        0.0
    };
    let s = ScalarSpline { kernel, order, polynomial, x0: norm[0], y0: norm[1], r0: norm[2], nodes, coef, eps2 };
    s.validate().map_err(|e| format!("{p}: {e}"))?;
    Ok(s)
}

fn vector_spline(props: &[XisfProperty], t: &str, kernel: Kernel, order: u32, polynomial: bool) -> Result<VectorSpline, String> {
    let px = format!("{t}X:");
    let py = format!("{t}Y:");
    let x = scalar_spline(props, &px, None, kernel, order, polynomial)?;
    let y = scalar_spline(props, &py, Some(&px), kernel, order, polynomial)?;
    Ok(VectorSpline { x, y })
}

/// Layer 3 for one direction; `Err` means unavailable (with the reason).
fn distortion_model(props: &[XisfProperty], dir: &str) -> Result<TermModel, String> {
    let p = format!("{STD_PREFIX}DistortionModel:{dir}:");
    let get = |s: &str| find_value(props, &format!("{p}{s}"));
    let kernel_id = get("BasisFunction").ok_or_else(|| format!("missing {p}BasisFunction"))?.as_str().ok_or("BasisFunction is not a string")?;
    let kernel = Kernel::parse(kernel_id).ok_or_else(|| format!("unrecognized BasisFunction '{kernel_id}'"))?;
    let order = get("Order").ok_or_else(|| format!("missing {p}Order"))?.as_f64().ok_or("Order is not a number")?;
    if order < 2.0 || order > 16.0 || order.fract() != 0.0 {
        return Err(format!("invalid Order {order}"));
    }
    let order = order as u32;
    let polynomial = match get("Polynomial") {
        None => true,
        Some(v) => v.as_f64().map(|b| b != 0.0).ok_or("Polynomial is not a boolean")?,
    };
    let terms_s = get("Terms").ok_or_else(|| format!("missing {p}Terms"))?.as_str().ok_or("Terms is not a string")?;
    let (mut has_global, mut has_local, mut has_fallback) = (false, false, false);
    for kind in terms_s.lines().map(str::trim).filter(|s| !s.is_empty()) {
        match kind {
            "Global" => has_global = true,
            "Local" => has_local = true,
            "Fallback" => has_fallback = true,
            other => return Err(format!("unrecognized term kind '{other}' in {p}Terms")),
        }
    }
    let global = if has_global { Some(vector_spline(props, &format!("{p}Global:"), kernel, order, polynomial)?) } else { None };
    let local = if has_local { local_terms(props, &p, kernel, order, polynomial)? } else { Vec::new() };
    let fallback = if has_fallback {
        let t0 = get("Fallback:Threshold").ok_or_else(|| format!("missing {p}Fallback:Threshold"))?.as_f64().ok_or("Fallback:Threshold is not a number")?;
        Some((t0, vector_spline(props, &format!("{p}Fallback:"), kernel, order, polynomial)?))
    } else {
        None
    };
    TermModel::new(global, local, fallback)
}

/// Unpack the packed Local term arrays (spec §11.5.3.7.4.4).
fn local_terms(props: &[XisfProperty], p: &str, kernel: Kernel, order: u32, polynomial: bool) -> Result<Vec<LocalTerm>, String> {
    let get = |s: &str| find_value(props, &format!("{p}Local:{s}"));
    let need = |s: &str| get(s).ok_or_else(|| format!("missing {p}Local:{s}"));
    let mat = |s: &str| -> Result<(usize, usize, Vec<f64>), String> {
        let (r, c, d) = need(s)?.as_f64_mat().ok_or_else(|| format!("{p}Local:{s} is not a matrix"))?;
        Ok((r as usize, c as usize, d.to_vec()))
    };
    let vec = |s: &str| -> Result<Vec<f64>, String> {
        Ok(need(s)?.as_f64_vec().ok_or_else(|| format!("{p}Local:{s} is not a vector"))?.to_vec())
    };
    let q = ScalarSpline::poly_terms(order, polynomial);
    let (n, cc, center) = mat("Center")?;
    if cc != 2 || n < 1 {
        return Err("Local:Center must be N×2 with N ≥ 1".into());
    }
    let radius = vec("Radius")?;
    if radius.len() != n {
        return Err("Local:Radius length != N".into());
    }
    let shape = kernel.has_shape();

    // One component's packed record → per-term scalar splines.
    let component = |c: &str| -> Result<Vec<ScalarSpline>, String> {
        let (nr, nc, norm) = mat(&format!("{c}:Normalization"))?;
        if (nr, nc) != (n, 3) {
            return Err(format!("Local:{c}:Normalization must be N×3"));
        }
        let off = vec(&format!("{c}:NodeOffsets"))?;
        if off.len() != n + 1 || off[0] != 0.0 || off.windows(2).any(|w| w[1] - w[0] < 3.0 || w[1].fract() != 0.0) {
            return Err(format!("invalid Local:{c}:NodeOffsets"));
        }
        let total = off[n] as usize;
        let (tr, tc, nodes) = mat(&format!("{c}:Nodes"))?;
        if tc != 2 || tr != total {
            return Err(format!("Local:{c}:Nodes must be {total}×2"));
        }
        let coef = vec(&format!("{c}:Coefficients"))?;
        if coef.len() != total + n * q {
            return Err(format!("Local:{c}:Coefficients length != total nodes + N·q"));
        }
        let shapes = if shape {
            let s = vec(&format!("{c}:ShapeParameter"))?;
            if s.len() != n {
                return Err(format!("Local:{c}:ShapeParameter length != N"));
            }
            Some(s)
        } else {
            if get(&format!("{c}:ShapeParameter")).is_some() {
                return Err(format!("Local:{c}:ShapeParameter must not be specified"));
            }
            None
        };
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let (o0, o1) = (off[k] as usize, off[k + 1] as usize);
            let s = ScalarSpline {
                kernel,
                order,
                polynomial,
                x0: norm[k * 3],
                y0: norm[k * 3 + 1],
                r0: norm[k * 3 + 2],
                nodes: nodes[o0 * 2..o1 * 2].chunks_exact(2).map(|w| [w[0], w[1]]).collect(),
                coef: coef[o0 + k * q..o1 + (k + 1) * q].to_vec(),
                eps2: shapes.as_ref().map_or(0.0, |s| s[k] * s[k]),
            };
            s.validate().map_err(|e| format!("Local term {k} {c}: {e}"))?;
            out.push(s);
        }
        Ok(out)
    };

    let xs = component("X")?;
    let ys = if get("Y:Nodes").is_some() {
        component("Y")?
    } else {
        // Y shares X's nodes, normalization and shape; only coefficients differ.
        let coef = vec("Y:Coefficients")?;
        let total: usize = xs.iter().map(|s| s.nodes.len()).sum();
        if coef.len() != total + n * q {
            return Err("Local:Y:Coefficients length != total nodes + N·q".into());
        }
        let mut out = Vec::with_capacity(n);
        let mut o = 0;
        for (k, x) in xs.iter().enumerate() {
            let m = x.nodes.len();
            let mut y = x.clone();
            y.coef = coef[o + k * q..o + m + (k + 1) * q].to_vec();
            o += m;
            out.push(y);
        }
        out
    };
    Ok(xs.into_iter().zip(ys).enumerate().map(|(k, (x, y))| LocalTerm {
        center: [center[k * 2], center[k * 2 + 1]],
        radius: radius[k],
        spline: VectorSpline { x, y },
    }).collect())
}

/// Decode the standard block. `Err` only when layer 1 is unusable (or the
/// major revision is unsupported); higher layers degrade with a note.
pub(crate) fn parse_standard(props: &[XisfProperty]) -> Result<StandardSolution, String> {
    let linear = linear_from_standard(props)?;
    if !std_native_frame_ok(props, "AstrometricSolution:ReferenceNativeCoordinates", "AstrometricSolution:CelestialPoleNativeCoordinates") {
        return Err("non-standard native frame (ReferenceNativeCoordinates/CelestialPoleNativeCoordinates) is not supported".into());
    }
    let mut notes = Vec::new();
    let both = |a: Result<Homography, String>, b: Result<Homography, String>| match (a, b) {
        (Ok(a), Ok(b)) => Ok((a, b)),
        (Err(e), _) | (_, Err(e)) => Err(e),
    };
    let has_layer2 = find_value(props, "AstrometricSolution:ProjectiveTransformation:ImageToProjection").is_some()
        || find_value(props, "AstrometricSolution:ProjectiveTransformation:ProjectionToImage").is_some();
    let has_layer3 = props.iter().any(|p| p.id.starts_with("AstrometricSolution:DistortionModel:"));
    let (i2p, p2i) = match both(homography(props, "ProjectiveTransformation:ImageToProjection"), homography(props, "ProjectiveTransformation:ProjectionToImage")) {
        Ok(h) => h,
        Err(e) => {
            if has_layer2 || has_layer3 {
                notes.push(format!("layer 2 (projective transformation) unavailable: {e}; using the linear solution"));
            }
            return Ok(StandardSolution { linear, image_to_projection: None, projection_to_image: None, notes });
        }
    };
    let distortion = if has_layer3 {
        match (distortion_model(props, "ImageToProjection"), distortion_model(props, "ProjectionToImage")) {
            (Ok(a), Ok(b)) => Some((a, b)),
            (Err(e), _) | (_, Err(e)) => {
                notes.push(format!("layer 3 (distortion model) unavailable: {e}; using the projective transformation"));
                None
            }
        }
    } else {
        None
    };
    let (da, db) = match distortion {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
    Ok(StandardSolution {
        linear,
        image_to_projection: Some(Direction { projective: i2p, distortion: da }),
        projection_to_image: Some(Direction { projective: p2i, distortion: db }),
        notes,
    })
}
```

Add `pub(crate) mod standard;` to `mod.rs`.

- [ ] **Step 4: Run tests**

```bash
cargo test -p mmm-core astrometry::standard 2>&1 | tail -3 && cargo clippy --all-targets -p mmm-core 2>&1 | tail -1
```
Expected: 9 passed, clippy clean. If clippy flags `type_complexity` on the closures, extract named `fn`s instead of allowing.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/astrometry
git commit -m "feat(astrometry): parse the XISF rev 1 AstrometricSolution block (layers 1-3)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Sample the standard model onto grids; wire dispatch, `wcs_from_properties`, `describe_unsolved`

**Files:**
- Modify: `crates/mmm-core/src/astrometry/standard.rs` (add `sample_grids`)
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (`WcsModel::from_properties`, `wcs_from_properties`, module docs)
- Modify: `crates/mmm-core/src/analyze.rs` (`describe_unsolved`)

**Interfaces:**
- Produces: `pub(crate) fn sample_grids(sol: &StandardSolution, width: u64, height: u64) -> Option<(Grid2D, Grid2D)>` (image→native, native→image); `WcsModel::from_properties` dispatches standard first; `wcs_from_properties` likewise; `pub fn describe_unsolved(props) -> String` (analyze.rs, still private) names the layer.

- [ ] **Step 1: Write the failing tests**

Append to `standard.rs`'s `mod tests`:

```rust
    #[test]
    fn sampled_grids_reproduce_the_model_and_pass_validation() {
        let (w, h) = (640u64, 480u64);
        let (rx, ry) = (320.0, 240.0);
        let nodes = lattice(6, 640.0, 480.0);
        // Distortion as a smooth residual on top of the exact linear map.
        let dist = |x: f64, y: f64| (2e-8 * (x - rx) * (y - ry), -1.5e-8 * (x - rx) * (x - rx) / 100.0);
        let v = vector_from(&nodes, dist);
        let inv = vector_from(&nodes.iter().map(|p| { let (dx, dy) = dist(p[0], p[1]); [-S * (p[0] - rx) + dx, S * (p[1] - ry) + dy] }).collect::<Vec<_>>(), |xi, eta| {
            // inverse residual so that P2I(I2P(p)) ≈ p to first order
            let x = rx - xi / S;
            let y = ry + eta / S;
            let (dx, dy) = dist(x, y);
            (dx / S, -dy / S)
        });
        let mut p = layer1([84.2, -3.24], [rx, ry], [[-S, 0.0], [0.0, S]]);
        layer2(&mut p, affine_h([[-S, 0.0], [0.0, S]], [rx * S, -ry * S]), affine_h([[-1.0 / S, 0.0], [0.0, 1.0 / S]], [rx, ry]));
        layer3_global(&mut p, "ImageToProjection", &v);
        layer3_global(&mut p, "ProjectionToImage", &inv);
        let sol = parse_standard(&p).unwrap();
        let (i2n, n2i) = sample_grids(&sol, w, h).unwrap();
        assert_eq!(i2n.rect, [0.0, 0.0, 640.0, 480.0]);
        assert_eq!(i2n.delta, 8.0);
        let d = sol.image_to_projection.as_ref().unwrap();
        for (x, y) in [(3.0, 5.0), (321.7, 239.1), (600.0, 470.0)] {
            let g = i2n.eval(x, y);
            let m = d.map(x, y);
            assert!((g.0 - m.0).abs() < 1e-7 && (g.1 - m.1).abs() < 1e-7, "grid vs model at ({x},{y})");
        }
        assert!(crate::astrometry::validate_grids(&sol.linear, &i2n, &n2i, w, h));
        let (xi, eta) = i2n.eval(100.0, 100.0);
        let back = n2i.eval(xi, eta);
        assert!((back.0 - 100.0).abs() < 0.05 && (back.1 - 100.0).abs() < 0.05);
    }

    #[test]
    fn layer1_only_has_no_grids() {
        let sol = parse_standard(&base()).unwrap();
        assert!(sample_grids(&sol, 100, 100).is_none());
    }
```

Append to `mod.rs`'s `mod tests`:

```rust
    #[test]
    fn standard_block_takes_precedence_over_legacy_ids() {
        use crate::astrometry::standard::fixtures::layer1;
        let mut props = orion_props(); // legacy ids
        props.extend(layer1([10.0, 20.0], [5.0, 6.0], [[-1e-3, 0.0], [0.0, 1e-3]]));
        let w = wcs_from_properties(&props).unwrap();
        assert_eq!(w.crval, [10.0, 20.0]);
        let m = WcsModel::from_properties(&props, 100, 100).unwrap();
        assert_eq!(m.linear.crval, [10.0, 20.0]);
        assert!(!m.is_spline());
    }

    #[test]
    fn unsupported_standard_major_version_yields_none_even_with_legacy_ids() {
        let mut props = orion_props();
        props.push(prop("AstrometricSolution:Version", "String", PropertyValue::Str("2.0".into())));
        assert!(wcs_from_properties(&props).is_none());
        assert!(WcsModel::from_properties(&props, 100, 100).is_none());
    }

    #[test]
    fn standard_projective_only_solution_yields_a_grid_model() {
        use crate::astrometry::standard::fixtures::{layer1, layer2};
        const S: f64 = 1e-3;
        let (rx, ry) = (50.0, 40.0);
        let mut props = layer1([10.0, 20.0], [rx, ry], [[-S, 0.0], [0.0, S]]);
        layer2(&mut props, [[-S, 0.0, rx * S], [0.0, S, -ry * S], [0.0, 0.0, 1.0]], [[-1.0 / S, 0.0, rx], [0.0, 1.0 / S, ry], [0.0, 0.0, 1.0]]);
        let m = WcsModel::from_properties(&props, 100, 80).unwrap();
        assert!(m.is_spline());
        let (ra, dec) = m.pixel_to_sky(rx, ry);
        assert!((ra - 10.0).abs() < 1e-9 && (dec - 20.0).abs() < 1e-9);
        let back = m.sky_to_pixel(ra, dec).unwrap();
        assert!((back.0 - rx).abs() < 1e-6 && (back.1 - ry).abs() < 1e-6);
    }
```

Add to `crates/mmm-core/tests/analyze.rs` (find the existing tests' `use` block and add `use mmm_core::analyze::solved_frame; use mmm_core::ipc::protocol::PanelDesc; use mmm_core::formats::{PropertyValue, XisfProperty};` if absent):

```rust
#[test]
fn unsolved_reason_names_the_standard_layer() {
    let props = vec![
        XisfProperty { id: "AstrometricSolution:Version".into(), type_: "String".into(), value: PropertyValue::Str("1.0".into()), location: None },
        XisfProperty { id: "AstrometricSolution:ProjectionSystem".into(), type_: "String".into(), value: PropertyValue::Str("Gnomonic".into()), location: None },
    ];
    let panels = vec![PanelDesc { panel_id: 0, width: 10, height: 10, channels: 1, properties: props }];
    let err = solved_frame(&panels).unwrap_err();
    assert!(err.contains("AstrometricSolution:ReferenceCelestialCoordinates"), "{err}");
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p mmm-core astrometry 2>&1 | grep -E "error\[|FAILED|panicked" | head -5
```
Expected: compile error (`sample_grids` undefined) and, once stubbed, the mod.rs tests fail because dispatch still goes to legacy only.

- [ ] **Step 3: Implement**

In `standard.rs`:

```rust
use super::{Grid2D, expected_nodes};
use rayon::prelude::*;

/// Grid spacing in image pixels for the image→projection sampling (PixInsight
/// 1.9.4 used 8 px; the validation tolerances were tuned on that spacing).
const IMAGE_DELTA_PX: f64 = 8.0;

/// Sample a solution's projective/spline directions onto lookup grids in the
/// layout `Grid2D` expects. `None` when the solution has only layer 1.
pub(crate) fn sample_grids(sol: &StandardSolution, width: u64, height: u64) -> Option<(Grid2D, Grid2D)> {
    let i2p = sol.image_to_projection.as_ref()?;
    let p2i = sol.projection_to_image.as_ref()?;
    let (w, h) = (width as f64, height as f64);
    let image_to_native = sample(i2p, [0.0, 0.0, w, h], IMAGE_DELTA_PX)?;

    // Projection-plane domain: bounding box of the mapped image corners and
    // edge midpoints, padded by one cell.
    let m = sol.linear.cd;
    let scale = (m[0][0] * m[1][1] - m[0][1] * m[1][0]).abs().sqrt();
    if !(scale > 0.0) || !scale.is_finite() {
        return None;
    }
    let delta = IMAGE_DELTA_PX * scale;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h), (w / 2.0, 0.0), (w / 2.0, h), (0.0, h / 2.0), (w, h / 2.0)] {
        let (xi, eta) = i2p.map(x, y);
        x0 = x0.min(xi);
        y0 = y0.min(eta);
        x1 = x1.max(xi);
        y1 = y1.max(eta);
    }
    let native_to_image = sample(p2i, [x0 - delta, y0 - delta, x1 + delta, y1 + delta], delta)?;
    Some((image_to_native, native_to_image))
}

fn sample(dir: &Direction, rect: [f64; 4], delta: f64) -> Option<Grid2D> {
    let cols = expected_nodes(rect[2] - rect[0], delta)?;
    let rows = expected_nodes(rect[3] - rect[1], delta)?;
    let n = rows as usize * cols as usize;
    let mut gx = vec![0.0; n];
    let mut gy = vec![0.0; n];
    gx.par_chunks_mut(cols as usize).zip(gy.par_chunks_mut(cols as usize)).enumerate().for_each(|(r, (rx, ry))| {
        let y = rect[1] + r as f64 * delta;
        for c in 0..cols as usize {
            let x = rect[0] + c as f64 * delta;
            let (ox, oy) = dir.map(x, y);
            rx[c] = ox;
            ry[c] = oy;
        }
    });
    if gx.iter().chain(&gy).any(|v| !v.is_finite()) {
        return None;
    }
    Some(Grid2D { rect, delta, rows, cols, gx, gy })
}
```

In `mod.rs`:

```rust
pub fn wcs_from_properties(props: &[XisfProperty]) -> Option<LinearWcs> {
    if standard::has_standard_block(props) {
        return standard::linear_from_standard(props).ok();
    }
    legacy::linear_from_legacy(props)
}

impl WcsModel {
    pub fn from_properties(props: &[XisfProperty], width: u64, height: u64) -> Option<WcsModel> {
        if !standard::has_standard_block(props) {
            return legacy::model_from_legacy(props, width, height);
        }
        let sol = match standard::parse_standard(props) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("standard astrometric solution rejected: {e}");
                return None;
            }
        };
        for n in &sol.notes {
            tracing::warn!("astrometric solution: {n}");
        }
        if sol.linear.ctype[0] != "RA---TAN" && sol.image_to_projection.is_some() {
            return None; // grid math is TAN-specific
        }
        match standard::sample_grids(&sol, width, height) {
            None => Some(WcsModel { linear: sol.linear, image_to_native: None, native_to_image: None, width, height }),
            Some((i2n, n2i)) => {
                if !validate_grids(&sol.linear, &i2n, &n2i, width, height) {
                    return None;
                }
                Some(WcsModel { linear: sol.linear, image_to_native: Some(i2n), native_to_image: Some(n2i), width, height })
            }
        }
    }
}
```

Update the `mod.rs` module doc header: replace the first paragraph ("PixInsight (ImageSolver / MosaicByCoordinates) stores plate solutions as XISF `<Property>` elements…verified against … PixInsight 1.9.4") with a short two-format statement: PixInsight ≥ 1.9.5 writes the XISF rev 1 standard block (`AstrometricSolution:*`, decoded in `standard.rs`, spline model evaluated and sampled onto the same grids); ≤ 1.9.4 wrote `PCL:AstrometricSolution:*` with precomputed grids (`legacy.rs`). Keep the rest of the existing empirical documentation, retitling the legacy table "Legacy (≤ 1.9.4) property ids".

In `analyze.rs`, replace `describe_unsolved`:

```rust
fn describe_unsolved(props: &[XisfProperty]) -> String {
    use crate::astrometry::standard::{has_standard_block, parse_standard};
    if has_standard_block(props) {
        return match parse_standard(props) {
            Err(e) => format!("standard astrometric solution unusable: {e}"),
            Ok(_) => "standard astrometric solution decoded but its sampled grids failed validation \
                      (non-gnomonic projection, non-standard native frame, or a model inconsistent \
                      with its own linear solution)"
                .to_string(),
        };
    }
    const REQUIRED: [&str; 3] = [
        "PCL:AstrometricSolution:ReferenceCelestialCoordinates",
        "PCL:AstrometricSolution:ReferenceImageCoordinates",
        "PCL:AstrometricSolution:LinearTransformationMatrix",
    ];
    let missing: Vec<&str> = REQUIRED.into_iter().filter(|id| !props.iter().any(|p| p.id == *id)).collect();
    if !missing.is_empty() {
        format!("no astrometric solution (neither AstrometricSolution:Version nor the legacy ids; missing: {})", missing.join(", "))
    } else if props.iter().any(|p| p.id.starts_with("PCL:AstrometricSolution:SplineWorldTransformation:")) {
        "legacy spline solution present but its interpolation grids are missing or failed validation".to_string()
    } else {
        "legacy astrometric solution properties are present but invalid (unsupported projection or malformed values)".to_string()
    }
}
```

Make `standard` `pub(crate)` visible to `analyze.rs` (it is, via `pub(crate) mod standard;`).

- [ ] **Step 4: Run tests**

```bash
cargo fmt && cargo test -p mmm-core 2>&1 | tail -3 && cargo clippy --all-targets -p mmm-core 2>&1 | tail -1 && cargo doc -p mmm-core --no-deps 2>&1 | grep -c warning
```
Expected: all pass, clippy clean, `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/astrometry crates/mmm-core/src/analyze.rs crates/mmm-core/tests/analyze.rs
git commit -m "feat(astrometry): decode PixInsight 1.9.5 standard solutions into WcsModel grids

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: Synthetic standard-block fixture writer and file-level integration test

**Files:**
- Modify: `crates/mmm-core/src/synth.rs` (`write_xisf_solved_standard`, `standard_wcs_property_xml`)
- Modify: `crates/mmm-core/src/lib.rs` (doc list of `synth` exports, if it enumerates them)
- Modify: `crates/mmm-core/tests/probe_panels.rs` (one new test)

**Interfaces:**
- Produces: `pub fn write_xisf_solved_standard(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32], wcs: &SynthWcs) -> Result<()>` — same as `write_xisf_solved` but emitting the XISF rev 1 layer-1 ids (`AstrometricSolution:Version` = `1.0`, `ProjectionSystem`, `ReferenceCelestialCoordinates`, `ReferenceImageCoordinates`, `LinearTransformationMatrix`, `CelestialReferenceSystem` = `ICRS`).

- [ ] **Step 1: Write the failing test** (append to `crates/mmm-core/tests/probe_panels.rs`)

```rust
#[test]
fn standard_rev1_solved_panels_probe_like_legacy_ones() {
    use mmm_core::synth::write_xisf_solved_standard;
    let dir = tmpdir("solved-standard");
    let scale_deg = 1.0e-3_f64;
    let (w, h) = (64u64, 48u64);
    let planes = vec![0.5f32; (w * h) as usize];
    let wcs = SynthWcs { crval: [10.0, 0.0], refimg: [32.0, 24.0], cd: [[-scale_deg, 0.0], [0.0, scale_deg]] };
    let legacy = dir.join("legacy.xisf");
    let standard = dir.join("standard.xisf");
    write_xisf_solved(&legacy, w, h, 1, &planes, &wcs).unwrap();
    write_xisf_solved_standard(&standard, w, h, 1, &planes, &wcs).unwrap();
    let ids: Vec<String> = XisfPanel::open(&standard).unwrap().header().properties.iter().map(|p| p.id.clone()).collect();
    assert!(ids.iter().any(|i| i == "AstrometricSolution:Version"));
    assert!(!ids.iter().any(|i| i.starts_with("PCL:")));
    let a = probe_panels(&[legacy], InputSelect::Auto).unwrap();
    let b = probe_panels(&[standard], InputSelect::Auto).unwrap();
    assert_eq!(format!("{:?}", a.frame), format!("{:?}", b.frame), "same solution, same frame");
}
```

(If `probe_panels`' reply has no `frame` field, compare `solved_frame` over the two `PanelDesc`s as the existing test does.)

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p mmm-core --test probe_panels 2>&1 | grep -E "error\[|FAILED" | head -3
```
Expected: compile error (`write_xisf_solved_standard` not found).

- [ ] **Step 3: Implement** in `synth.rs` next to `wcs_property_xml`:

```rust
/// `<Property>` elements for a linear Gnomonic solution in the XISF 1.0
/// revision 1 standard form (`AstrometricSolution:*`, what PixInsight ≥ 1.9.5
/// writes) — layer 1 only.
fn standard_wcs_property_xml(wcs: &SynthWcs) -> String {
    let cd = [wcs.cd[0][0], wcs.cd[0][1], wcs.cd[1][0], wcs.cd[1][1]];
    format!(
        concat!(
            r#"<Property id="AstrometricSolution:Version" type="String">1.0</Property>"#,
            r#"<Property id="AstrometricSolution:ProjectionSystem" type="String">Gnomonic</Property>"#,
            r#"<Property id="AstrometricSolution:ReferenceCelestialCoordinates" type="F64Vector" length="2" location="inline:base64">{crval}</Property>"#,
            r#"<Property id="AstrometricSolution:ReferenceImageCoordinates" type="F64Vector" length="2" location="inline:base64">{refimg}</Property>"#,
            r#"<Property id="AstrometricSolution:LinearTransformationMatrix" type="F64Matrix" rows="2" columns="2" location="inline:base64">{cd}</Property>"#,
            r#"<Property id="AstrometricSolution:CelestialReferenceSystem" type="String">ICRS</Property>"#,
        ),
        crval = b64_f64s(&wcs.crval),
        refimg = b64_f64s(&wcs.refimg),
        cd = b64_f64s(&cd),
    )
}

/// [`write_xisf`] plus a linear astrometric solution in the XISF revision 1
/// standard form (PixInsight ≥ 1.9.5). [`write_xisf_solved`] writes the
/// legacy (≤ 1.9.4) form; both must remain readable.
pub fn write_xisf_solved_standard(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32], wcs: &SynthWcs) -> Result<()> {
    write_xisf_impl(path, w, h, ch, planes, &standard_wcs_property_xml(wcs))
}
```

Update the doc comment on `write_xisf_solved` to say "legacy (PixInsight ≤ 1.9.4) ids".

- [ ] **Step 4: Run tests**

```bash
cargo test -p mmm-core 2>&1 | tail -3 && cargo clippy --all-targets -p mmm-core 2>&1 | tail -1
```
Expected: pass, clean.

- [ ] **Step 5: Commit**

```bash
git add crates/mmm-core/src/synth.rs crates/mmm-core/tests/probe_panels.rs crates/mmm-core/src/lib.rs
git commit -m "test(synth): standard XISF rev 1 solved-panel fixture writer

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: PixInsight module — forward both property families; drop 2.8.x workarounds; build locally against PCL 2.10.8

**Files:**
- Modify: `integration/pixinsight/module/AstrometryProps.cpp:157-160`, `AstrometryProps.h` (doc comment)
- Modify: `integration/pixinsight/module/ImageWindowCollector.h:90-96`, `ViewPanelSource.h:65-71`
- Modify: `integration/pixinsight/module/makefile-x64:51-62, 96-100`, `integration/pixinsight/module/CMakeLists.txt:68-72`
- Modify: `integration/pixinsight/PROTOCOL.md` (§6 example id, §11 wording)

**Interfaces:**
- Produces: `extract_astrometry_props` forwards ids starting with `AstrometricSolution:` or `PCL:AstrometricSolution:` (excluding `PCL:AstrometricSolution:Grid:` and `PCL:AstrometricSolution:Generation:`), plus `Observation:CelestialReferenceSystem`.

- [ ] **Step 1: Edit `AstrometryProps.cpp`**

Replace the filter in `extract_astrometry_props( const PropertyArray& )`:

```cpp
   for ( const Property& p : properties )
   {
      const IsoString& id = p.Id();
      // XISF rev 1 standard block (PixInsight >= 1.9.5) and the legacy block
      // (<= 1.9.4; old data stays in circulation). The worker never reads
      // PixInsight's private evaluation cache / solver parameters, and the
      // Grid:* matrices are megabytes each, so they are dropped here.
      const bool standard = id.StartsWith( "AstrometricSolution:" );
      const bool legacy   = id.StartsWith( "PCL:AstrometricSolution:" )
                         && !id.StartsWith( "PCL:AstrometricSolution:Grid:" )
                         && !id.StartsWith( "PCL:AstrometricSolution:Generation:" );
      const bool refsys   = id == "Observation:CelestialReferenceSystem";
      if ( !standard && !legacy && !refsys )
         continue;
```

Update the header comment in `AstrometryProps.h` (lines ~4 and ~28-31) to the same statement. In `PROTOCOL.md` §6 (`XisfProperty` table, "e.g. `PCL:AstrometricSolution:ReferenceCelestialCoordinates`") add "or `AstrometricSolution:ReferenceCelestialCoordinates` (XISF rev 1, PixInsight ≥ 1.9.5)"; in §11 ("plate solution travels as the raw XISF `<Property>` elements PixInsight …") add one sentence: both the standard `AstrometricSolution:*` block and the legacy `PCL:AstrometricSolution:*` block are forwarded; the worker decides.

- [ ] **Step 2: Remove the `noexcept` workarounds**

In `ImageWindowCollector.h` replace lines 90-96 with:

```cpp
   ~ImageWindowCollector() override = default;
```

In `ViewPanelSource.h` replace lines 65-71 with:

```cpp
   ~ViewPanelSource() override = default;
```

(PCL 2.10.8 declares `UIObject::~UIObject()` noexcept, so the implicit spec is noexcept.)

- [ ] **Step 3: Update build comments**

`makefile-x64` lines 51-62: replace the MMM_MACOS_ARCH paragraph with: "MMM_MACOS_ARCH selects the macOS slice: arm64 (default; Apple Silicon cores) or x64 (Intel Macs). PixInsight ≥ 1.9.5 is the minimum supported core on every arch. Arch flags mirror PCL 2.10.8's own macosx makefiles: x64 adds -msse4.2 and targets macOS 12; arm64 targets macOS 14." Keep the flag lines. Lines 96-100: change the `-isystem` justification to "PCL headers via -isystem so header-internal diagnostics never trip the CI warning-free gate; our own code stays fully under -Wall." `CMakeLists.txt` 68-72: same wording for `SYSTEM`.

- [ ] **Step 4: Build PCL 2.10.8 and the module locally**

Build the pinned PCL with the CI script into a scratch prefix (this needs Task 9's pin; if Task 9 is not done yet, run with the SHA overridden: `sed` is not needed — pass the pin by editing `pcl-pin.env` first as Task 9 Step 1 describes, or do Task 9 first). Then:

```bash
cd /home/dpaull/dev/mega-merge-mosaic
P=/tmp/claude-1000/-home-dpaull-dev-mega-merge-mosaic/08cdeb32-17c7-469a-ad0b-f13abd40705f/scratchpad/pcl-2.10.8
bash integration/pixinsight/ci/build-pcl.sh --out "$P" --work "$P-src" 2>&1 | tail -3
bash integration/pixinsight/ci/build-module-linux.sh --pcl "$P" --stage "$P-stage" 2>&1 | tail -5
```
Expected: `PCL built: …/libPCL-pxi.a`, then `staged unsigned payload in …/bin: mmm-ipc-worker mmm-pxm.so` with no `warning:` lines. If the module fails on a 2.10.8 API change, fix the module source (never patch PCL).

Also build the host golden tests as CI does:

```bash
cmake -S integration/pixinsight/host -B /tmp/claude-1000/-home-dpaull-dev-mega-merge-mosaic/08cdeb32-17c7-469a-ad0b-f13abd40705f/scratchpad/host-build >/dev/null && cmake --build /tmp/claude-1000/-home-dpaull-dev-mega-merge-mosaic/08cdeb32-17c7-469a-ad0b-f13abd40705f/scratchpad/host-build 2>&1 | tail -2 && ctest --test-dir /tmp/claude-1000/-home-dpaull-dev-mega-merge-mosaic/08cdeb32-17c7-469a-ad0b-f13abd40705f/scratchpad/host-build --output-on-failure 2>&1 | tail -3
```
Expected: `100% tests passed`. Note: the working tree has an uncommitted `host/CMakeLists.txt` change adding a SectionBar test; if that test fails for unrelated reasons, report it, do not fix it here.

- [ ] **Step 5: Commit** (only the files of this task)

```bash
git add integration/pixinsight/module/AstrometryProps.cpp integration/pixinsight/module/AstrometryProps.h integration/pixinsight/module/ImageWindowCollector.h integration/pixinsight/module/ViewPanelSource.h integration/pixinsight/module/makefile-x64 integration/pixinsight/module/CMakeLists.txt integration/pixinsight/PROTOCOL.md
git commit -m "feat(pixinsight): forward XISF rev 1 AstrometricSolution:* properties; drop PCL 2.8.x workarounds

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: CI — single PCL 2.10.8 pin, lifted vcxproj, no repo packaging

**Files:**
- Modify: `integration/pixinsight/ci/pcl-pin.env`, `build-pcl-macos.sh`, `build-pcl.sh` (comments), `build-pcl-windows.ps1` (comments), `ci/README.md`
- Replace: `integration/pixinsight/ci/win/PCL.vcxproj` ← `/opt/PixInsight/src/pcl/windows/vc17/PCL.vcxproj`
- Modify: `.github/workflows/module.yml`
- Delete: `integration/pixinsight/repo/` (whole directory)
- Modify: `integration/pixinsight/module/README.md:211-212` (cross-reference)

- [ ] **Step 1: Rewrite `pcl-pin.env`**

```bash
# Pinned open-source PCL revision for CI module builds. Single source of truth.
#
# Compatibility policy: the module supports PixInsight >= 1.9.5 only. The PCL
# static lib bakes PCL_API_Version into the module as its declared API
# requirement; a core refuses any module declaring a newer API than its own,
# so this pin is exactly the PCL of the OLDEST supported core (1.9.5), and
# newer cores accept it via backward compatibility. Core API history:
#   1.9.0 initial (PCL 2.8.3)           -> 0x0182
#   1.9.0 update .. 1.9.3 early (2.8.x) -> 0x0183
#   1.9.3 late (PCL 2.9.x)              -> 0x0186
#   1.9.4 (PCL 2.10.3/2.10.4)           -> 0x0187
#   1.9.5 (PCL 2.10.8)                  -> 0x0188   <- this pin
# 1.9.4 and older cores are served by the frozen v1.4.2 module packages and
# are deliberately not supported by newer builds (XISF rev 1 astrometric
# solutions require the 1.9.5 core).
#
# gitlab.com/pixinsight/PCL @ this SHA == PCL 2.10.8 / PixInsight 1.9.5
# ("Update to PixInsight 1.9.5 Lockhart / PCL 2.10.8", 2026-09-17).
# One pin for every target: 2.10.3+ builds natively for macOS arm64 and x64.
PCL_REPO_URL="https://gitlab.com/pixinsight/PCL.git"
PCL_SHA="201860a364c05308cde8271f5ca517b69b487fa1"
PCL_VER_MAJOR=2
PCL_VER_MINOR=10
PCL_VER_RELEASE=8
# Expected `#define PCL_API_Version 0x<hex>` in include/pcl/api/APIInterface.h
# at PCL_SHA. The build-pcl* scripts fail hard on mismatch: shipping a module
# with a too-new API version silently locks out older cores.
PCL_API_VERSION_HEX=0188
```

- [ ] **Step 2: `build-pcl-macos.sh`**

- Header comment (lines 5-9): "arm64 (default) serves Apple Silicon cores; x64 serves Intel Macs. Both build from the single pin in pcl-pin.env (PCL ≥ 2.10.3 has native arm64 makefiles)."
- Delete lines 29-36 (the `if [ "$ARCH" = arm64 ]` pin override block).
- Delete lines 75-82 (the zlib `fdopen` sed and its comment).
- Lines 68-70 comment: "Each arch uses the pin's native makefiles: makefile-x64 and makefile-arm64 both exist at 2.10.8."
- Lines 123-126: drop "(pre-1.9.4: unsuffixed x64-only …)" and say "upstream's make-3rdparty-<arch>.sh drivers do exactly this; drive the same makes ourselves so the script does not depend on the driver's name".

- [ ] **Step 3: `build-pcl.sh` / `build-pcl-windows.ps1`**

No logic change. In `build-pcl.sh` line 60-63 comment ("CUDADevice.cpp compiles without the CUDA toolkit in this PCL version…") leave as is unless Task 8's local build showed otherwise. In `build-pcl-windows.ps1` line 2-3: "The upstream commit omits src/pcl/windows/vc17/PCL.vcxproj, so we drop the repo-pinned project (lifted from a licensed 1.9.5 install, PCL 2.10.8) into the fetched tree."

- [ ] **Step 4: Replace `win/PCL.vcxproj`**

```bash
cp /opt/PixInsight/src/pcl/windows/vc17/PCL.vcxproj integration/pixinsight/ci/win/PCL.vcxproj
```
Then edit its top comment lines (the `#`-prefixed generator banner is inside an XML comment; keep it) and add a one-line XML comment right after the `<?xml …?>` line: `<!-- Lifted from a licensed PixInsight 1.9.5 install (PCL 2.10.8, Makefile Generator v1.153); untrimmed: matches the pinned tree's 192 sources. See ci/README.md. -->`. Verify the drift guard would pass against the pinned tree using the local install (same commit):

```bash
python3 - <<'EOF'
import re,os
proj=open('integration/pixinsight/ci/win/PCL.vcxproj').read()
listed=sorted(set(os.path.basename(m) for m in re.findall(r'<ClCompile Include="([^"]+)"',proj)))
actual=sorted(f for f in os.listdir('/opt/PixInsight/src/pcl') if f.endswith('.cpp'))
print("listed",len(listed),"actual",len(actual))
print("missing",sorted(set(listed)-set(actual)),"extra",sorted(set(actual)-set(listed)))
EOF
```
Expected: `listed 192 actual 192`, both lists empty. Also confirm the fetched pinned tree (from Task 8's `$P-src`) has the same file list: `ls $P-src/src/pcl/*.cpp | wc -l` → `192`.

- [ ] **Step 5: `module.yml`**

- Header comment: "macOS ships both arches: arm64 for Apple Silicon, x64 for Intel Macs; both from the single PCL 2.10.8 pin."
- Linux job: delete the two steps "Generate + validate repository package and updates.xri (no publish)" and "Run repo generator tests"; remove `repo-out/**` from the upload `path`.
- macOS job: the "Read PCL pin" step becomes the same as Linux (`echo "sha=$PCL_SHA"`); update the matrix comments (arm64: "Apple Silicon cores"; x64: "Intel Macs; native Intel runner so tests run natively").
- Validate: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/module.yml'))"` (install `pyyaml` if missing, or use `ruby -ryaml -e 'YAML.load_file(".github/workflows/module.yml")'`).

- [ ] **Step 6: Delete the repository packaging scripts and fix references**

```bash
git rm -r integration/pixinsight/repo
grep -rn "repo/" integration/pixinsight --include=*.md --include=*.yml --include=*.sh --include=*.ps1 | grep -v "PCL_REPO_URL\|gitlab" 
```
Fix every hit: `module/README.md:211-212` → "The distribution package is assembled by the Astrometrical tools website from the CI artifacts (`stage/bin/**`, `stage/doc/**`); it ships this file the same way." `ci/README.md`: remove the repo-pipeline mentions in the layout list and the "Scope" section (the "Module signing" paragraph stays).

- [ ] **Step 7: `ci/README.md`**

- Layout bullet for `pcl-pin.env`: "The pin tracks the **oldest supported PixInsight core**, which is 1.9.5 (PCL 2.10.8, API 0x0188); older cores are served by the frozen v1.4.2 packages. One pin for all four jobs."
- "The pinned `win/PCL.vcxproj`" section: lifted from the 1.9.5 install (PCL 2.10.8, Makefile Generator v1.153), untrimmed, 192 sources; the drift guard is a no-op at this pin.
- Delete the "macOS: two arches, two pins" section; replace with a short "macOS: two arches, one pin" paragraph (why both arches still ship; the SDK retarget; no zlib patch needed at 2.10.8).

- [ ] **Step 8: Syntax checks and commit**

```bash
bash -n integration/pixinsight/ci/build-pcl-macos.sh integration/pixinsight/ci/build-pcl.sh && grep -c "PCL_ARM64" integration/pixinsight/ci/*.sh integration/pixinsight/ci/*.env .github/workflows/module.yml
git add integration/pixinsight/ci .github/workflows/module.yml integration/pixinsight/module/README.md
git commit -m "ci(pixinsight): pin PCL 2.10.8 (PixInsight 1.9.5) for every arch; drop in-repo update-repo packaging

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```
Expected: `bash -n` silent; the grep prints `0` for every file.

---

### Task 10: Version 1.5.0 and documentation

**Files:**
- Modify: `Cargo.toml:12`, `integration/pixinsight/module/MmmVersion.h:12-17`, `integration/pixinsight/host/mmm_protocol.h:33`, `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html:91`
- Modify: `README.md:220-228`, `docs/DESIGN.md:300`, `integration/pixinsight/PCL_API_REFERENCE.md:1-19, 994-1060`, `integration/pixinsight/module/README.md` (requirements), `MegaMergeMosaic.html` (add a "Requirements" sentence near the version line)

- [ ] **Step 1: Bump the four version stamps**

`Cargo.toml`: `version = "1.5.0"`. `MmmVersion.h`: `MINOR 5`, `REVISION 0`, `BUILD 1`, `MMM_VERSION_STRING "1.5.0"`. `mmm_protocol.h`: `kExpectedWorkerVersion = "1.5.0"`. HTML: `Version 1.5.0 &mdash; Category: Mosaic`. Then `cargo build -p mmm-ipc-worker` once so `Cargo.lock` updates.

```bash
cargo test -p mmm-ipc-worker --test version_sync 2>&1 | tail -2
```
Expected: 3 passed.

- [ ] **Step 2: Docs**

- `README.md` "Unaligned mode" bullet: "needs each panel to carry a PixInsight astrometric solution: the XISF 1.0 revision 1 `AstrometricSolution:*` block (PixInsight ≥ 1.9.5, spline distortion models are evaluated) or the legacy `PCL:AstrometricSolution:*` block (≤ 1.9.4, distortion grids used when present)". Add to the module/PixInsight section (search for "PixInsight module" in README): "The PixInsight module requires PixInsight ≥ 1.9.5; v1.4.2 remains available for 1.9.0–1.9.4."
- `docs/DESIGN.md:300`: replace the WCS bullet's first sentence with "PixInsight ≥ 1.9.5 stores the solution as the XISF rev 1 `AstrometricSolution:*` block (layers: linear, projective 3×3, RBF spline distortion), which `astrometry/standard.rs` decodes and samples onto lookup grids; ≤ 1.9.4 stored `PCL:AstrometricSolution:*` with precomputed grids (`astrometry/legacy.rs`)". Add a dated "Phase: PixInsight 1.9.5 (2026-09-19)" results paragraph after the latest phase section: pin 2.10.8, API 0x0188, one pin per arch, standard decoder, legacy kept, repo scripts removed (link the spec).
- `PCL_API_REFERENCE.md`: header line 3-4 → "PCL 2.10.8 (PixInsight 1.9.5)"; replace the "Build-pin caveat" block with: "CI builds against PCL **2.10.8** (PixInsight 1.9.5), pinned in `ci/pcl-pin.env`, for every target; the module's declared `PCL_API_Version` (0x0188) requires a ≥ 1.9.5 core by design." In the property-id section (~994) add a subsection "XISF rev 1 standard ids (1.9.5+)" listing the layer-1 ids, `ProjectiveTransformation:{ImageToProjection,ProjectionToImage}`, the `DistortionModel:<dir>:*` family, `ControlPoints:*`, and the private `PCL:AstrometricSolution:{Generation,Grid}:*`, noting the legacy list below it is what ≤ 1.9.4 wrote and 1.9.5 deletes on regeneration.
- `integration/pixinsight/module/README.md`: add "Requires PixInsight ≥ 1.9.5 (PCL 2.10.8 API 0x0188). Older cores: use v1.4.2." near the top (after the title paragraph).
- `MegaMergeMosaic.html`: add `<p>Requires PixInsight 1.9.5 or later.</p>` after the tagline line (keep the version line's exact `Version 1.5.0 &mdash;` text).

- [ ] **Step 3: Full verification**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings 2>&1 | tail -1 && cargo test 2>&1 | grep -E "^test result" && cargo doc --no-deps 2>&1 | grep -c warning
```
Expected: fmt clean, clippy clean, every `test result: ok`, `0`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock integration/pixinsight/module/MmmVersion.h integration/pixinsight/host/mmm_protocol.h integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html README.md docs/DESIGN.md integration/pixinsight/PCL_API_REFERENCE.md integration/pixinsight/module/README.md
git commit -m "chore: v1.5.0 — PixInsight 1.9.5 / XISF rev 1 support; docs

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 11: Real-data verification (manual smoke test, ignored tests)

**Files:**
- Create: `test_data/orion_mosaic_raw_panels_195/` (gitignored; 12 regenerated panels)
- Modify: `crates/mmm-core/src/astrometry/mod.rs` (`#[ignore]` real-data tests for the 1.9.5 files)
- Modify: `CLAUDE.md` (`test_data/` layout line)

- [ ] **Step 1: Regenerate all 12 raw panels with PixInsight 1.9.5**

Extend the scratchpad script `pi195/regen.js` into a loop over the 12 raw panels writing `test_data/orion_mosaic_raw_panels_195/<same basename>.xisf` (≈2.4 GB; 470 GB free), then run:

```bash
S=/tmp/claude-1000/-home-dpaull-dev-mega-merge-mosaic/08cdeb32-17c7-469a-ad0b-f13abd40705f/scratchpad/pi195
mkdir -p test_data/orion_mosaic_raw_panels_195
timeout 1800 /opt/PixInsight/bin/PixInsight.sh -n --automation-mode --no-startup-scripts --no-splash --no-startup-check-updates --no-startup-gui-messages "-r=$S/regen-all.js" --force-exit > $S/pi-all.txt 2>&1; ls test_data/orion_mosaic_raw_panels_195 | wc -l
```
Expected: `12`.

- [ ] **Step 2: Add ignored real-data tests** (in `mod.rs` tests, next to the existing `real_*` tests)

```rust
    fn raw_panel_195_path(n: u32) -> std::path::PathBuf {
        test_data(&format!(
            "orion_mosaic_raw_panels_195/masterLight_BIN-1_4944x3284_EXPOSURE-30.00s_FILTER-NoFilter_RGB_PANEL-{n}_autocrop.xisf"
        ))
    }

    /// The decoded standard model must agree with PixInsight's own grid cache
    /// (PCL:AstrometricSolution:Grid:*, present in these regenerated files)
    /// and with the legacy 1.9.4 model of the same panel.
    #[test]
    #[ignore = "needs multi-GB test_data/orion_mosaic_raw_panels_195 (gitignored); run manually"]
    fn real_195_standard_model_matches_pixinsight_grid_cache_and_legacy() {
        for n in 1..=12 {
            let panel = XisfPanel::open(&raw_panel_195_path(n)).unwrap();
            let h = panel.header();
            assert!(standard::has_standard_block(&h.properties));
            let model = WcsModel::from_properties(&h.properties, h.width, h.height).expect("standard model");
            assert!(model.is_spline());
            // PixInsight's cache, read ad hoc (never in product code).
            let get = |s: &str| find_value(&h.properties, &format!("PCL:AstrometricSolution:Grid:ImageToProjection:{s}")).unwrap();
            let rect = get("Rect").as_f64_vec().unwrap();
            let delta = get("Delta").as_f64().unwrap();
            let (rows, cols, gx) = get("GridX").as_f64_mat().unwrap();
            let (_, _, gy) = get("GridY").as_f64_mat().unwrap();
            let mut worst = 0.0f64;
            for r in (0..rows as usize).step_by(7) {
                for c in (0..cols as usize).step_by(7) {
                    let (x, y) = (rect[0] + c as f64 * delta, rect[1] + r as f64 * delta);
                    let ours = model.image_to_native.as_ref().unwrap().eval(x, y);
                    let theirs = (gx[r * cols as usize + c], gy[r * cols as usize + c]);
                    worst = worst.max((ours.0 - theirs.0).hypot(ours.1 - theirs.1) * 3600.0);
                }
            }
            eprintln!("panel {n}: worst |ours − PI cache| = {worst:.4}\"");
            assert!(worst < 0.05, "panel {n}: {worst}\" vs PixInsight grid cache");
            // Same sky for the same pixel as the 1.9.4 legacy solution (< 0.2″).
            let (lp, lm) = open_raw_model(n);
            assert_eq!((lp.width(), lp.height()), (panel.width(), panel.height()));
            for (x, y) in [(100.0, 100.0), (2449.0, 1615.0), (4800.0, 3200.0)] {
                let a = model.pixel_to_sky(x, y);
                let b = lm.pixel_to_sky(x, y);
                assert!(arcsec_apart(a, b) < 0.2, "panel {n} at ({x},{y}): {}\"", arcsec_apart(a, b));
            }
        }
    }
```

```bash
cargo test -p mmm-core --release real_195 -- --ignored --nocapture 2>&1 | grep -E "panel|test result"
```
Expected: 12 "worst" lines well under 0.05″ and `test result: ok`.

- [ ] **Step 3: CLI smoke test on the regenerated set**

```bash
cargo build --release && rm -rf test_data/orion_195_check.mmm-session && target/release/mmm analyze --input solved test_data/orion_mosaic_raw_panels_195/*.xisf --session test_data/orion_195_check.mmm-session 2>&1 | tail -8
```
(Use the exact `analyze` flags the README documents for solved mode.) Expected: "mosaic frame: 9255x18310 px"-class output matching the 1.9.4 run in `test_data/orion_check.mmm-session` (same frame size ±2 px, same center to < 1″). Record the numbers in the final report. If the frame differs materially, stop and investigate before continuing.

- [ ] **Step 4: Update `CLAUDE.md` and commit**

Add under `test_data/`: "`orion_mosaic_raw_panels_195/` — the same raw panels re-saved by PixInsight 1.9.5 (`AstrometricSolution:*` standard block)".

```bash
git add crates/mmm-core/src/astrometry/mod.rs CLAUDE.md
git commit -m "test(astrometry): ignored real-data checks against PixInsight 1.9.5 regenerated panels

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Self-review

- Spec coverage: §2 policy → Tasks 9/10 (1.9.5-only, v1.5.0), Task 6 (legacy kept), Task 9 (repo scripts removed), compression untouched. §4.1–4.4 → Tasks 8, 9, 10. §5.1–5.7 → Tasks 1–6 (layout, dispatch, layers, spline math, sampling, XISF types, error reporting). §6 → Task 8. §7 → Tasks 5 fixtures + Task 7 writer. §8 → Task 11. §9 out of scope respected.
- Placeholders: none; every step has code or an exact command.
- Type consistency: `find_value`, `validate_grids`, `expected_nodes`, `projection_code`, `std_native_frame_ok` defined in Task 1 and used in Tasks 5–6; `Kernel`/`ScalarSpline` (Task 3) → `VectorSpline`/`LocalTerm`/`TermModel` (Task 4) → `standard.rs` (Task 5) → `sample_grids` (Task 6); `fixtures::{layer1, layer2, layer3_global, layer3_local, prop}` (Task 5) reused in Task 6; `write_xisf_solved_standard` (Task 7) used in its own test.
