//! Legacy (PixInsight ≤ 1.9.4) astrometric solution: `PCL:AstrometricSolution:*`
//! properties, with the spline distortion carried as PixInsight's
//! `SplineWorldTransformation:PointGridInterpolation` lookup grids.
//!
//! Everything here was verified on real 1.9.4 files; see the module docs in
//! `mod.rs` for the empirical facts (grid layout, conventions). PixInsight
//! 1.9.5 no longer writes these ids — see `standard.rs` — but old data stays
//! in circulation, so this reader is kept indefinitely.

use super::{
    Grid2D, LinearWcs, WcsModel, expected_nodes, find_value, projection_code, std_native_frame_ok,
    validate_grids,
};
use crate::formats::XisfProperty;

/// Prefix of the legacy spline block.
pub(super) const SPLINE_PREFIX: &str = "PCL:AstrometricSolution:SplineWorldTransformation:";

/// True when any legacy spline property is present.
pub(super) fn has_legacy_spline(props: &[XisfProperty]) -> bool {
    props.iter().any(|p| p.id.starts_with(SPLINE_PREFIX))
}

/// Linear solution from the legacy ids.
///
/// Returns `None` if any required property (reference celestial/image
/// coordinates, linear transformation matrix) is missing or malformed, or if
/// the projection system is one we cannot express as a FITS CTYPE code.
pub(super) fn linear_from_legacy(props: &[XisfProperty]) -> Option<LinearWcs> {
    let crval = find_value(
        props,
        "PCL:AstrometricSolution:ReferenceCelestialCoordinates",
    )?
    .as_f64_vec()?;
    let refimg =
        find_value(props, "PCL:AstrometricSolution:ReferenceImageCoordinates")?.as_f64_vec()?;
    let (rows, cols, m) =
        find_value(props, "PCL:AstrometricSolution:LinearTransformationMatrix")?.as_f64_mat()?;
    if crval.len() != 2 || refimg.len() != 2 || (rows, cols) != (2, 2) {
        return None;
    }

    // Missing projection property defaults to Gnomonic — the only projection
    // MosaicByCoordinates produces; an explicit unknown one refuses (better no
    // WCS than a wrong CTYPE).
    let proj = find_value(props, "PCL:AstrometricSolution:ProjectionSystem")
        .map_or(Some("Gnomonic"), |v| v.as_str());
    let code = projection_code(proj?)?;

    let radesys = find_value(props, "Observation:CelestialReferenceSystem")
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

/// Full model from the legacy ids.
///
/// Linear-only solutions yield a model without grids. If any
/// `SplineWorldTransformation` property is present, both interpolation
/// grids must be present, well-formed, and pass the layout validations
/// described in the module docs; otherwise the whole solution is refused
/// (`None`) — never silently approximated by its linear part.
pub(super) fn model_from_legacy(
    props: &[XisfProperty],
    width: u64,
    height: u64,
) -> Option<WcsModel> {
    let linear = linear_from_legacy(props)?;
    if !has_legacy_spline(props) {
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
