//! Blend: stream the photometrically-corrected union of panels as row bands
//! to a [`RowSink`], either feathered (phase-1) or two-band (default).
//!
//! Output canvas = union of panel content bboxes (cropped — never the full
//! mosaic canvas). Per pixel and panel, the weight is
//! `max(clamp(d_px/feather, 0, 1), MIN_WEIGHT)` where `d_px` is 8× the
//! bilinear sample of the panel's L8 chamfer distance map at `(x/8, y/8)` —
//! pixels near a panel's rim get tiny weight so interpolation garbage loses to
//! any overlapping partner, while single-coverage rims still survive
//! normalization. A pixel is covered ⟺ all channels are nonzero; uncovered
//! pixels contribute nothing and fully uncovered output pixels are 0.
//!
//! [`BlendMode::TwoBand`] splits each corrected panel into base (bilinear
//! upsample of the corrected L8 cell means — smooth) + detail (full-res minus
//! base). Bases blend with the wide feather as before; detail comes from
//! exactly one panel per pixel via the seam [`crate::seam::OwnerMap`], so
//! stars are never averaged and sub-pixel misregistration cannot pinch or
//! double them. Detail transitions ramp linearly over ±[`RAMP_PX`] px of the
//! owner boundary — except where a boundary cell is covered by a *compact*
//! component of the connected star mask
//! ([`crate::seam::split_mask_components`]) in either panel, where the
//! transition snaps hard so a star (or a diffraction-spike arm attached to
//! one) crossing the seam is taken whole from one panel. Boundary cells in
//! the mask's *extended-structure* part (bright nebular signal the flood
//! fill crossed, e.g. the whole M42 core) ramp instead, over the widened
//! ±[`WIDE_RAMP_PX`] px — snapping there imprinted the residual panel
//! mismatch as a visible L8-quantized staircase on real data. In the ramped
//! mix each panel's contribution additionally fades over its last
//! ±[`WIDE_RAMP_PX`] px of coverage (rim fade): where a transition is
//! forced onto a panel's coverage rim — the DP seam hugs band edges through
//! fully-masked rows — the partner panel simply ends mid-ramp, and without
//! the fade the unfinished ramp fraction would step at the rim.
//!
//! [`BlendMode::Pyramid`] (the default) keeps the whole TwoBand detail stage
//! byte-for-byte but replaces the *base's* single wide feather with an
//! L8-grid Laplacian-pyramid blend ([`crate::pyramid`]): per channel, each
//! panel's star-free corrected base plane is decomposed into levels at 8, 16,
//! 32, … px scale (up to the feather scale) and blended per level with a
//! Gaussian pyramid of its hard ownership indicator, so each frequency band
//! transitions over a distance proportional to its wavelength instead of
//! being averaged across the whole feather. Masks renormalize per level over
//! the panels actually reaching a cell (validity = the base's sampling reach,
//! so nothing bleeds in from beyond a panel's coverage); where the mask sum
//! is numerically ~0 the cell falls back to the feather-weighted base.
//!
//! In TwoBand/Pyramid mode a cross-panel **defect veto** (default on) restores the
//! defect suppression that averaging used to provide: where ≥2 panels cover a
//! pixel and the cell is star-mask-clear in both compared panels, a detail
//! difference above [`DEFECT_VETO_FACTOR`] × the smaller of the two panels'
//! cell detail RMS marks a single-panel transient (cosmic-ray residue,
//! satellite trail), and the smaller-|d| detail is used instead of the
//! owner's. Trade-off (by design): a *genuine* transient present in only one
//! panel is suppressed too — exactly what we want for trails and cosmics.
//! Feather mode is untouched (averaging already dilutes defects).
//!
//! An opt-in **global flatten** ([`BlendParams::flatten`], default off) fits
//! an order-1/2 polynomial per channel to the merged L8 background
//! ([`crate::flatten`]) and subtracts `f(x, y) − f(canvas center)` during
//! output on every path (full-res feather, two-band, and the L8 preview) —
//! implemented by folding the negated delta into each panel's surface terms,
//! which is exactly equivalent and leaves all cross-panel differences (seams,
//! detail ownership, defect veto) untouched.
//!
//! Bands are computed rayon-parallel (over rows within a band) but delivered
//! to the sink strictly in order. `downsample == 8` blends from the L8 summary
//! means over fully-covered cells instead of touching the full-res mmaps —
//! seconds instead of minutes, for previews. At L8 the base band *is* the
//! whole signal (detail lives below 8 px), so previews use the feather path
//! in both modes.

use rayon::prelude::*;

use crate::flatten::Flatten;
use crate::ipc::source::{FileSource, PanelSource};
use crate::overlap::{OverlapGraph, distance_map};
use crate::panel_reader::PanelReader;
use crate::photometry::Photometry;
use crate::seam::{OwnerMap, compute_owner_map_masked, star_mask};
use crate::session::Session;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::Surfaces;
use crate::{Error, Result};

/// Weight floor for covered pixels: rim pixels survive normalization when
/// alone but lose ~completely to any overlapping partner.
pub const MIN_WEIGHT: f32 = 1e-4;

/// Half-width of the detail ownership ramp around owner boundaries, in canvas
/// pixels (the transition spans ±RAMP_PX).
pub const RAMP_PX: f32 = 16.0;

/// Widened detail-ramp half-width used where an owner-boundary cell is
/// covered by the *extended-structure* part of the star mask
/// ([`crate::seam::split_mask_components`]) in either panel: inside bright
/// structure the residual inter-panel disagreement is at its largest, so the
/// transition gets twice the normal room to hide it. Compact mask components
/// (stars + spike arms) still snap (`detail_transition_maps`).
pub const WIDE_RAMP_PX: f32 = 32.0;

/// Star-lock parameter, retained under its historical name: this is the
/// *seed* factor of the connected star mask ([`crate::seam::star_mask`],
/// grown onto cells > [`crate::seam::MASK_GROW_FACTOR`] × median detail).
/// The detail transition snaps hard wherever an owner-boundary cell is
/// covered by a *compact* component of the mask
/// ([`crate::seam::split_mask_components`]) in either panel — covering spike
/// arms attached to star cores, not just cells that individually exceed the
/// raw threshold. Extended-structure components ramp (±[`WIDE_RAMP_PX`])
/// instead of snapping.
pub const STAR_LOCK_FACTOR: f32 = crate::seam::MASK_SEED_FACTOR;

/// Base-band exclusion parameter, retained under its historical name: also
/// the connected star mask's seed factor. Cells covered by the FULL mask
/// (compact ∪ extended structure) are excluded from the base band (their
/// base value is filled from the surrounding background instead). The base
/// must be star-free: raw cell means near bright stars differ between
/// misregistered panels, and blending them would leave cell-scale coloured
/// blobs around stars and their spikes. Excluding the full mask — not just
/// its compact components — matters because a bright star's reflection halo
/// floods its component past every compactness bound: letting that
/// star+halo content into the pyramid base once mixed the panels'
/// disagreeing halos with level-dependent weights, imprinting a wide dark
/// moat around the star (see `bright_star_halo_leaves_no_dark_moat`).
pub const BASE_STAR_FACTOR: f32 = crate::seam::MASK_SEED_FACTOR;

/// Defect-veto trigger: in the TwoBand detail stage, |owner detail − other
/// detail| beyond this factor × the cell's detail RMS scale marks a
/// single-panel defect. The scale is the *minimum* of the two compared
/// panels' gain-corrected cell detail RMS: the defect itself inflates the
/// carrying panel's cell RMS (a cell-crossing trail contributes ~0.33× its
/// amplitude), so the owner's own RMS could never flag the very defect it
/// carries — the cleaner panel's RMS is the defect-free noise scale.
pub const DEFECT_VETO_FACTOR: f32 = 6.0;

/// How the detail band is combined across panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// Phase-1 weighted average of full-res corrected pixels.
    Feather,
    /// Feathered base + seam-owned detail (star-safe under misregistration).
    TwoBand,
    /// TwoBand with the star-free base blended as an L8-grid Laplacian
    /// pyramid ([`crate::pyramid`]): levels at 8, 16, 32, … px up to the
    /// feather scale, each blended with Gaussian ownership-mask pyramids
    /// whose transition width is proportional to the level's scale. Mid-scale
    /// structure (8 px–feather) is seam-switched over distances matched to
    /// its wavelength instead of averaged across the whole feather (the
    /// Burt–Adelson result), which keeps differing-PSF or slightly
    /// misregistered panels from softening/doubling it. The detail stage
    /// (ownership, ramp, star-lock, defect veto) is exactly the TwoBand path.
    #[default]
    Pyramid,
}

/// Parameters of the blend.
#[derive(Debug, Clone)]
pub struct BlendParams {
    /// Feather ramp length in canvas pixels.
    pub feather_px: f32,
    /// 1 = full resolution, 8 = blend from the L8 summaries (preview).
    pub downsample: u32,
    /// Output rows per band delivered to the sink.
    pub band_rows: usize,
    /// Feather (phase-1), two-band with star-avoiding seams, or two-band
    /// with the pyramid base (default).
    pub mode: BlendMode,
    /// Optional region of interest in full-res canvas coords `[x0,y0,x1,y1]`
    /// (exclusive); the output is the intersection with the union bbox.
    pub roi: Option<[u64; 4]>,
    /// Cross-panel defect veto in the TwoBand detail stage (see the module
    /// docs and [`DEFECT_VETO_FACTOR`]). No effect in Feather mode.
    pub defect_veto: bool,
    /// Opt-in global background flatten: fit an order-1/2 polynomial per
    /// channel to the *merged* L8 background ([`crate::flatten`]) and
    /// subtract `f(x, y) − f(canvas center)` during output — removes a sky
    /// gradient common to all panels (which per-panel corrections cannot
    /// see) while preserving the central level. `None` = off (default).
    /// Errors when the mosaic is signal-dominated (< 20% background cells).
    pub flatten: Option<u32>,
}

impl Default for BlendParams {
    fn default() -> Self {
        Self {
            feather_px: 256.0,
            downsample: 1,
            band_rows: 256,
            mode: BlendMode::Pyramid,
            roi: None,
            defect_veto: true,
            flatten: None,
        }
    }
}

/// The blend's output bbox: union of panel bboxes, intersected with the ROI
/// when one is set. Errors if the intersection is empty.
pub fn output_bbox(session: &Session, params: &BlendParams) -> Result<[u64; 4]> {
    let u = union_bbox(session)?;
    let Some(r) = params.roi else { return Ok(u) };
    let b = [
        u[0].max(r[0]),
        u[1].max(r[1]),
        u[2].min(r[2]),
        u[3].min(r[3]),
    ];
    if b[0] >= b[2] || b[1] >= b[3] {
        return Err(Error::format(
            &session.dir,
            "ROI does not intersect the mosaic content",
        ));
    }
    Ok(b)
}

/// Streaming consumer of blended rows (planar per band).
pub trait RowSink {
    /// Announce the output geometry before any band is delivered.
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()>;
    /// One band starting at output row `y0`; `rows` is planar:
    /// `ch` planes × `band_rows` × `w`, row-major.
    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()>;
    /// All bands delivered; flush and validate completeness.
    fn finish(&mut self) -> Result<()>;
}

/// Union of the panel content bboxes: `[x0, y0, x1, y1]`, exclusive.
/// Errors if the session has no panel with content.
pub fn union_bbox(session: &Session) -> Result<[u64; 4]> {
    let mut it = session
        .panels
        .iter()
        .filter(|p| p.bbox[2] > p.bbox[0] && p.bbox[3] > p.bbox[1]);
    let first = it
        .next()
        .ok_or_else(|| Error::format(&session.dir, "session has no panels with content"))?;
    Ok(it.fold(first.bbox, |acc, p| {
        [
            acc[0].min(p.bbox[0]),
            acc[1].min(p.bbox[1]),
            acc[2].max(p.bbox[2]),
            acc[3].max(p.bbox[3]),
        ]
    }))
}

/// One panel's blend-time context: correction, geometry, and feather source.
struct PanelPrep {
    /// Content bbox in canvas pixels, `[x0, y0, x1, y1]` exclusive.
    bbox: [u64; 4],
    /// Per-channel photometric gain/offset (identity when absent).
    gains: Vec<f32>,
    offsets: Vec<f32>,
    /// Per-channel residual surface coefficients, padded to the 6 terms
    /// `1, x, y, x², xy, y²` (normalized canvas coords); all-zero when the
    /// session has no surfaces.
    surf: Vec<[f64; 6]>,
    summary: L8Summary,
    /// Chamfer distance to the nearest not-fully-covered L8 cell, in cells.
    dist: Vec<f32>,
    /// Corrected L8 cell means `g·mean + o + s(cell center)`, planar
    /// `channels × cells` — the source of the two-band base via bilinear
    /// upsampling.
    corr8: Vec<f32>,
    /// Gain-corrected channel-max detail RMS per cell — the defect veto's
    /// noise scale (kept even after `summary.detail` is dropped).
    vdet: Vec<f32>,
}

impl PanelPrep {
    /// Row constants for the surface at normalized row coordinate `yn`:
    /// `s(xn) = a + xn·(b + xn·c)` per channel (Horner in `xn`).
    #[inline]
    fn surf_row(&self, yn: f64) -> Vec<(f32, f32, f32)> {
        self.surf
            .iter()
            .map(|t| {
                let a = t[0] + (t[2] + t[5] * yn) * yn;
                let b = t[1] + t[4] * yn;
                (a as f32, b as f32, t[3] as f32)
            })
            .collect()
    }
}

/// Load the panels' L8 summaries in parallel.
pub(crate) fn load_summaries(session: &Session) -> Result<Vec<L8Summary>> {
    session
        .panels
        .par_iter()
        .map(|p| L8Summary::read(&session.summary_path(p.id)))
        .collect()
}

/// Open every panel's pixel data for streaming reads via `source` ([`PanelReader`]
/// validates each panel's storage against the session canvas). `advise_sequential`
/// applies to every source uniformly (a no-op for IPC-backed readers).
fn open_readers(session: &Session, source: &dyn PanelSource) -> Result<Vec<PanelReader>> {
    session
        .panels
        .par_iter()
        .map(|p| {
            let r = source.open_reader(p, session.canvas)?;
            r.advise_sequential();
            Ok(r)
        })
        .collect()
}

/// Load summaries, compute distance maps, and resolve per-panel corrections
/// (fitting the optional global flatten first — it needs the star masks).
fn prep_panels(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    flatten_order: Option<u32>,
) -> Result<Vec<PanelPrep>> {
    let summaries = load_summaries(session)?;
    let masks: Vec<Vec<bool>> = summaries.par_iter().map(star_mask).collect();
    let flat = fit_flatten_opt(&summaries, &masks, phot, surfaces, session, flatten_order)?;
    Ok(prep_from_summaries(
        session,
        phot,
        surfaces,
        flat.as_ref(),
        summaries,
        &masks,
    ))
}

/// Fit the global flatten when requested ([`BlendParams::flatten`]).
fn fit_flatten_opt(
    summaries: &[L8Summary],
    masks: &[Vec<bool>],
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    session: &Session,
    order: Option<u32>,
) -> Result<Option<Flatten>> {
    order
        .map(|o| crate::flatten::fit_flatten(summaries, masks, phot, surfaces, session.canvas, o))
        .transpose()
}

/// Build the per-panel blend contexts from already-loaded summaries;
/// `base_masks` are the cells to exclude from the base band — the panels'
/// FULL connected star masks (stars, spike arms, and mask-flooded extended
/// structure; see [`BASE_STAR_FACTOR`] for why structure components must not
/// enter the base either). When `flat`
/// is set, its delta `f(x,y) − f(center)` is folded (negated) into every
/// panel's surface terms — subtracting the same global field from every
/// panel's correction subtracts it from the blended output on every path
/// (feather, two-band, L8 preview), while cross-panel differences (seams,
/// detail bands) are untouched.
fn prep_from_summaries(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    flat: Option<&Flatten>,
    summaries: Vec<L8Summary>,
    base_masks: &[Vec<bool>],
) -> Vec<PanelPrep> {
    let ch = session.canvas.2 as usize;
    session
        .panels
        .par_iter()
        .zip(summaries)
        .zip(base_masks.par_iter())
        .map(|((p, summary), mask)| {
            let mut dist = distance_map(&summary.coverage, summary.w8, summary.h8);
            // A panel with no uncovered cell at all yields INFINITY; make it a
            // large finite value so bilinear interpolation cannot produce NaN.
            for d in &mut dist {
                if !d.is_finite() {
                    *d = 1e30;
                }
            }
            let (gains, offsets, mut surf) = panel_correction_terms(phot, surfaces, p.id, ch);
            if let Some(f) = flat {
                f.apply_to_surf(&mut surf);
            }

            // Corrected cell means at cell centers: the two-band base plane.
            let mut corr8 = corrected_cell_means(&summary, &gains, &offsets, &surf, session.canvas);

            suppress_stars_in_base(&mut corr8, &summary, ch, mask);

            let cells = summary.w8 as usize * summary.h8 as usize;
            let vdet: Vec<f32> = (0..cells)
                .map(|i| {
                    (0..ch)
                        .map(|c| summary.detail[c * cells + i] * gains[c])
                        .fold(0.0f32, f32::max)
                })
                .collect();

            PanelPrep {
                bbox: p.bbox,
                gains,
                offsets,
                surf,
                summary,
                dist,
                corr8,
                vdet,
            }
        })
        .collect()
}

/// Per-panel correction terms resolved from the photometry/surfaces tables:
/// per-channel gains, offsets, and residual-surface coefficients padded to
/// the 6 terms `1, x, y, x², xy, y²` (normalized canvas coords). Identity /
/// all-zero when a table has no entry for the panel. Shared by the blender's
/// prep and the seam diagnostics ([`crate::diag`]).
pub(crate) fn panel_correction_terms(
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    id: usize,
    ch: usize,
) -> (Vec<f32>, Vec<f32>, Vec<[f64; 6]>) {
    let correction = |table: &Vec<Vec<f64>>, default: f64| -> Vec<f32> {
        (0..ch)
            .map(|c| {
                table
                    .get(c)
                    .and_then(|t| t.get(id))
                    .copied()
                    .unwrap_or(default) as f32
            })
            .collect()
    };
    let surf: Vec<[f64; 6]> = (0..ch)
        .map(|c| {
            let mut padded = [0.0f64; 6];
            if let Some(coeffs) = surfaces
                .and_then(|s| s.coeffs.get(c))
                .and_then(|t| t.get(id))
            {
                padded[..coeffs.len()].copy_from_slice(coeffs);
            }
            padded
        })
        .collect();
    (
        correction(&phot.gains, 1.0),
        correction(&phot.offsets, 0.0),
        surf,
    )
}

/// Corrected L8 cell means `g·mean + o + s(cell center)`, planar
/// `channels × cells` — the raw corrected cell plane (no star suppression),
/// shared by the blender's prep and the seam diagnostics ([`crate::diag`]).
pub(crate) fn corrected_cell_means(
    summary: &L8Summary,
    gains: &[f32],
    offsets: &[f32],
    surf: &[[f64; 6]],
    canvas: (u64, u64, u64),
) -> Vec<f32> {
    let ch = gains.len();
    let (w8, h8) = (summary.w8 as usize, summary.h8 as usize);
    let cells = w8 * h8;
    let (cw, chh) = (canvas.0 as f64, canvas.1 as f64);
    let mut corr8 = vec![0.0f32; ch * cells];
    for c in 0..ch {
        let t = &surf[c];
        for y8 in 0..h8 {
            let yn = (y8 as f64 + 0.5) * BLOCK as f64 / chh;
            let sa = t[0] + (t[2] + t[5] * yn) * yn;
            let sb = t[1] + t[4] * yn;
            for x8 in 0..w8 {
                let xn = (x8 as f64 + 0.5) * BLOCK as f64 / cw;
                let s = sa + xn * (sb + xn * t[3]);
                let i = y8 * w8 + x8;
                corr8[c * cells + i] =
                    summary.mean[c * cells + i] * gains[c] + offsets[c] + s as f32;
            }
        }
    }
    corr8
}

/// Make the base band star-free and rim-safe: a cell's raw mean is trusted
/// only when the cell is *fully covered* and outside `mask` — the panel's
/// FULL connected star mask ([`crate::seam::star_mask`]: star cores, spike
/// arms, and any extended structure the flood fill crossed). Structure must
/// be excluded along with the compact parts: a bright star's reflection
/// halo floods its component past the compactness bounds, and letting the
/// star+halo cell means into the pyramid base mixes the panels' disagreeing
/// halos with level-dependent mask weights — the halo's Laplacian negative
/// lobes stop cancelling and a wide dark moat appears around the star (the
/// regression pinned by `bright_star_halo_leaves_no_dark_moat`). The
/// resulting cross-panel mismatch of structure flux lives in the detail
/// band instead, where the widened structure ramp (±[`WIDE_RAMP_PX`])
/// spreads it smoothly — that ramp, not base inclusion, is what fixed the
/// M42 seam staircase. Everything else the bilinear base taps can reach —
/// masked cells, partially covered rim cells (registration-interpolation
/// garbage on real data), and the one ring of uncovered cells beyond the rim
/// — is replaced by an onion-peel fill from the trusted background cells.
/// The per-panel identity `out = base + (full − base)` is unaffected by what
/// the base contains — but the *cross-panel* base difference near bright
/// stars and panel rims (which a feathered base blend would imprint as
/// cell-scale blobs and rim streaks under misregistration) collapses to the
/// background mismatch.
fn suppress_stars_in_base(corr8: &mut [f32], summary: &L8Summary, nch: usize, mask: &[bool]) {
    let (w, h) = (summary.w8 as usize, summary.h8 as usize);
    let cells = w * h;
    debug_assert_eq!(mask.len(), cells);

    let source: Vec<bool> = (0..cells)
        .map(|i| summary.coverage[i] >= 1.0 && !mask[i])
        .collect();
    if !source.iter().any(|&s| s) {
        return; // nothing trustworthy to fill from: keep raw values
    }

    // Cells the base sampling can reach: covered cells dilated by 2 (the
    // bilinear taps of a covered pixel stay within 1 cell of its own cell).
    // Only non-source cells in this zone need filling; the rest of the (96%
    // empty) canvas grid is never sampled and keeps its raw value.
    let reach = base_reach(&summary.coverage, w, h);

    // Onion-peel: each pass fills cells that touch already-available cells
    // with the mean of those neighbours, per channel. Star blobs and rim
    // bands are a few cells wide, so this converges quickly; the fill only
    // has to *agree across panels*, and it is determined by the
    // (photometrically matched) surrounding trusted values.
    let mut avail = source.clone();
    let mut unfilled: Vec<usize> = (0..cells).filter(|&i| reach[i] && !source[i]).collect();
    let mut sums = vec![0.0f64; nch];
    for _ in 0..256 {
        if unfilled.is_empty() {
            break;
        }
        let mut next = Vec::with_capacity(unfilled.len());
        let mut newly = Vec::new();
        for &i in &unfilled {
            let (x, y) = (i % w, i / w);
            sums.iter_mut().for_each(|s| *s = 0.0);
            let mut n = 0u32;
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let j = yy * w + xx;
                    if avail[j] {
                        n += 1;
                        for (c, s) in sums.iter_mut().enumerate() {
                            *s += corr8[c * cells + j] as f64;
                        }
                    }
                }
            }
            if n > 0 {
                for c in 0..nch {
                    corr8[c * cells + i] = (sums[c] / n as f64) as f32;
                }
                newly.push(i);
            } else {
                next.push(i);
            }
        }
        if newly.is_empty() {
            break; // isolated remnants keep their raw value (harmless)
        }
        for i in newly {
            avail[i] = true;
        }
        unfilled = next;
    }
}

/// Cells the bilinear base sampling can reach for a panel: covered cells
/// (coverage > 0) dilated by 2 — the base's validity zone. Shared by the
/// star-suppression fill (which fills exactly this zone) and the pyramid
/// base's per-panel validity plane.
fn base_reach(coverage: &[f32], w: usize, h: usize) -> Vec<bool> {
    let mut reach: Vec<bool> = coverage.iter().map(|&c| c > 0.0).collect();
    for _ in 0..2 {
        let prev = reach.clone();
        for y in 0..h {
            for x in 0..w {
                if prev[y * w + x] {
                    continue;
                }
                let near = (y.saturating_sub(1)..=(y + 1).min(h - 1)).any(|yy| {
                    (x.saturating_sub(1)..=(x + 1).min(w - 1)).any(|xx| prev[yy * w + xx])
                });
                if near {
                    reach[y * w + x] = true;
                }
            }
        }
    }
    reach
}

/// Bilinear sample of an L8-grid plane at fractional cell coordinates.
#[inline]
fn bilinear(d: &[f32], w8: u32, h8: u32, gx: f32, gy: f32) -> f32 {
    let (w, h) = (w8 as usize, h8 as usize);
    let gx = gx.clamp(0.0, (w - 1) as f32);
    let gy = gy.clamp(0.0, (h - 1) as f32);
    let x0 = gx as usize;
    let y0 = gy as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = gx - x0 as f32;
    let fy = gy - y0 as f32;
    let top = d[y0 * w + x0] * (1.0 - fx) + d[y0 * w + x1] * fx;
    let bot = d[y1 * w + x0] * (1.0 - fx) + d[y1 * w + x1] * fx;
    top * (1.0 - fy) + bot * fy
}

/// Feather weight from a distance in pixels.
#[inline]
fn weight(d_px: f32, inv_feather: f32) -> f32 {
    (d_px * inv_feather).clamp(0.0, 1.0).max(MIN_WEIGHT)
}

/// Blend the session's panels into `sink`, applying the photometric
/// corrections and (when given) the residual surfaces: `v' = g·v + o + s(x,y)`.
/// Panel pixels are read from disk via [`FileSource`] — see
/// [`blend_with_source`] to inject a different [`PanelSource`] (e.g. an IPC
/// host).
pub fn blend(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    graph: &OverlapGraph,
    params: &BlendParams,
    sink: &mut dyn RowSink,
) -> Result<()> {
    blend_with_source(session, phot, surfaces, graph, params, &FileSource, sink)
}

/// [`blend`] with an injectable [`PanelSource`] for panel pixels — files
/// (the default, via [`FileSource`]) or the IPC host (via
/// [`crate::ipc::source::IpcSource`]). The blend algorithm itself never
/// learns which.
pub fn blend_with_source(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    graph: &OverlapGraph,
    params: &BlendParams,
    source: &dyn PanelSource,
    sink: &mut dyn RowSink,
) -> Result<()> {
    if session.panels.is_empty() {
        return Err(Error::format(&session.dir, "session has no panels"));
    }
    if params.feather_px.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err(Error::format(
            &session.dir,
            format!("feather_px must be positive, got {}", params.feather_px),
        ));
    }
    match (params.downsample, params.mode) {
        (1, BlendMode::Feather) => blend_full(session, phot, surfaces, params, source, sink),
        (1, BlendMode::TwoBand | BlendMode::Pyramid) => {
            blend_twoband(session, phot, surfaces, graph, params, source, sink)
        }
        // At 1/8 the base band is the whole signal: previews feather-blend
        // the L8 means in all modes.
        (8, _) => blend_l8(session, phot, surfaces, params, sink),
        (d, _) => Err(Error::format(
            &session.dir,
            format!("unsupported downsample {d} (only 1 or 8)"),
        )),
    }
}

/// Precomputed bilinear taps for one fractional L8-grid position; the grid is
/// shared by every panel, so one set of taps samples any plane.
struct BiLin {
    i: [usize; 4],
    w: [f32; 4],
}

impl BiLin {
    #[inline]
    fn at(w8: u32, h8: u32, gx: f32, gy: f32) -> BiLin {
        let (w, h) = (w8 as usize, h8 as usize);
        let gx = gx.clamp(0.0, (w - 1) as f32);
        let gy = gy.clamp(0.0, (h - 1) as f32);
        let x0 = gx as usize;
        let y0 = gy as usize;
        let x1 = (x0 + 1).min(w - 1);
        let y1 = (y0 + 1).min(h - 1);
        let fx = gx - x0 as f32;
        let fy = gy - y0 as f32;
        BiLin {
            i: [y0 * w + x0, y0 * w + x1, y1 * w + x0, y1 * w + x1],
            w: [
                (1.0 - fx) * (1.0 - fy),
                fx * (1.0 - fy),
                (1.0 - fx) * fy,
                fx * fy,
            ],
        }
    }

    #[inline]
    fn sample(&self, plane: &[f32]) -> f32 {
        plane[self.i[0]] * self.w[0]
            + plane[self.i[1]] * self.w[1]
            + plane[self.i[2]] * self.w[2]
            + plane[self.i[3]] * self.w[3]
    }
}

/// Full-resolution blend streamed from the panels' mmaps.
fn blend_full(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    params: &BlendParams,
    source: &dyn PanelSource,
    sink: &mut dyn RowSink,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let preps = prep_panels(session, phot, surfaces, params.flatten)?;
    let panels = open_readers(session, source)?;

    let bbox = output_bbox(session, params)?;
    let (cx0, cy0) = (bbox[0], bbox[1]);
    let out_w = (bbox[2] - bbox[0]) as usize;
    let out_h = (bbox[3] - bbox[1]) as usize;
    tracing::info!(
        out_w,
        out_h,
        nch,
        prep_s = t0.elapsed().as_secs_f64(),
        "blend full-res"
    );

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_block = 1.0 / BLOCK as f32;
    let inv_cw = 1.0f32 / session.canvas.0 as f32;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let mut band = vec![0.0f32; nch * band_rows * out_w];
    let mut nonfinite = 0u64;

    for y0 in (0..out_h).step_by(band_rows) {
        let rows_here = band_rows.min(out_h - y0);
        let band_cy0 = cy0 + y0 as u64;
        let band_cy1 = band_cy0 + rows_here as u64;
        let active: Vec<usize> = preps
            .iter()
            .enumerate()
            .filter(|(_, p)| p.bbox[1] < band_cy1 && p.bbox[3] > band_cy0)
            .map(|(i, _)| i)
            .collect();

        // Compute rows in parallel; the collected Vec preserves row order so
        // the sink always receives bands (and rows within them) in order.
        let rows_out: Vec<Vec<f32>> = (0..rows_here)
            .into_par_iter()
            .map(|r| {
                let cy = band_cy0 + r as u64;
                let mut acc = vec![0.0f32; nch * out_w];
                let mut wsum = vec![0.0f32; out_w];
                let gy = cy as f32 * inv_block;
                // Per panel: (storage x0, clipped channel row) per channel.
                let mut rows: Vec<(usize, &[f32])> = Vec::with_capacity(nch);
                for &pi in &active {
                    let p = &preps[pi];
                    if cy < p.bbox[1] || cy >= p.bbox[3] {
                        continue;
                    }
                    rows.clear();
                    for c in 0..nch as u64 {
                        // Content bbox ⊆ storage bbox, so the row exists.
                        let (rx0, row) = panels[pi]
                            .row(c, cy)
                            .expect("content row within storage bbox");
                        rows.push((rx0 as usize, row));
                    }
                    // Residual surface, reduced to per-row constants:
                    // s(xn) = a + xn·(b + xn·c) per channel (Horner).
                    let srow = p.surf_row(cy as f64 * inv_ch);
                    let xs = p.bbox[0].max(cx0);
                    let xe = p.bbox[2].min(bbox[2]);
                    for x in xs..xe {
                        let xi = x as usize;
                        if rows.iter().any(|&(rx0, row)| row[xi - rx0] == 0.0) {
                            continue; // uncovered: any channel zero
                        }
                        let gx = x as f32 * inv_block;
                        let d_px =
                            BLOCK as f32 * bilinear(&p.dist, p.summary.w8, p.summary.h8, gx, gy);
                        let wgt = weight(d_px, inv_feather);
                        let o = (x - cx0) as usize;
                        wsum[o] += wgt;
                        let xn = x as f32 * inv_cw;
                        for (c, &(rx0, row)) in rows.iter().enumerate() {
                            let (sa, sb, sc) = srow[c];
                            acc[c * out_w + o] += wgt
                                * (row[xi - rx0] * p.gains[c]
                                    + p.offsets[c]
                                    + sa
                                    + xn * (sb + xn * sc));
                        }
                    }
                }
                normalize(&mut acc, &wsum, out_w, nch);
                acc
            })
            .collect();

        let bs = &mut band[..nch * rows_here * out_w];
        assemble_band(bs, &rows_out, out_w, nch);
        nonfinite += zero_non_finite(bs);
        sink.band(y0 as u64, bs)?;
    }
    warn_non_finite(nonfinite);
    sink.finish()?;
    tracing::info!(total_s = t0.elapsed().as_secs_f64(), "blend full-res done");
    Ok(())
}

/// Detail-transition planes for the two-band blend, from the owner map:
/// per owning panel a ramp plane `r ∈ [0,1]` rising linearly from 0 outside
/// the panel's owned region to 1 inside — over ±[`RAMP_PX`] normally, over
/// ±[`WIDE_RAMP_PX`] in cells near owner boundaries that touch the
/// *extended-structure* part of the star mask (bright nebular signal, where
/// the inter-panel disagreement the ramp must hide is largest) — plus a
/// per-cell star-lock flag marking ramp-wide zones around owner boundaries
/// that touch *compact* mask components (transition must snap, not ramp,
/// there — a star or a spike arm crossing the seam is taken whole from one
/// panel). Snapping used to apply to the whole connected mask; over extended
/// structure (M42) that imprinted the full residual panel mismatch as a hard
/// L8-quantized step — the user-reported staircase — so structure now ramps,
/// wider than background. Where compact and structure zones overlap the snap
/// wins (star integrity beats smoothness).
fn detail_transition_maps(
    owner: &OwnerMap,
    compact: &[Vec<bool>],
    structure: &[Vec<bool>],
) -> (Vec<Option<Vec<f32>>>, Vec<bool>) {
    let (w8, h8) = (owner.w8, owner.h8);
    let (w, h) = (w8 as usize, h8 as usize);
    let ramp_cells = RAMP_PX / BLOCK as f32; // ±2 cells by default
    let wide_cells = WIDE_RAMP_PX / BLOCK as f32; // ±4 cells by default

    // Zone flags from the owner boundaries: `hard` (snap) around boundaries
    // touching compact mask components, `wide` (widened ramp) around
    // boundaries touching extended structure.
    let mut hard = vec![false; w * h];
    let mut wide = vec![false; w * h];
    {
        let mark = |flags: &mut [bool], x: usize, y: usize, r: i64| {
            for yy in (y as i64 - r).max(0)..=(y as i64 + r).min(h as i64 - 1) {
                for xx in (x as i64 - r).max(0)..=(x as i64 + r).min(w as i64 - 1) {
                    flags[yy as usize * w + xx as usize] = true;
                }
            }
        };
        let r_hard = ramp_cells.ceil() as i64;
        let r_wide = wide_cells.ceil() as i64;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let o = owner.owner[i];
                if o == u16::MAX {
                    continue;
                }
                for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                    if nx >= w || ny >= h {
                        continue;
                    }
                    let j = ny * w + nx;
                    let n = owner.owner[j];
                    if n == u16::MAX || n == o {
                        continue;
                    }
                    if compact[o as usize][i] || compact[n as usize][j] {
                        mark(&mut hard, x, y, r_hard);
                        mark(&mut hard, nx, ny, r_hard);
                    }
                    if structure[o as usize][i] || structure[n as usize][j] {
                        mark(&mut wide, x, y, r_wide);
                        mark(&mut wide, nx, ny, r_wide);
                    }
                }
            }
        }
    }

    // Ramp per panel that owns at least one cell: signed chamfer distance to
    // the ownership boundary, mapped linearly onto [0,1] over the cell's
    // half-width (±wide_cells in structure zones, ±ramp_cells elsewhere).
    // The planes are bilinearly sampled at blend time, so a per-cell width
    // choice still yields a pixel-continuous transition.
    let ramps: Vec<Option<Vec<f32>>> = (0..compact.len())
        .into_par_iter()
        .map(|p| {
            let ind: Vec<f32> = owner
                .owner
                .iter()
                .map(|&o| if o == p as u16 { 1.0 } else { 0.0 })
                .collect();
            if ind.iter().all(|&v| v == 0.0) {
                return None;
            }
            let inv: Vec<f32> = ind.iter().map(|&v| 1.0 - v).collect();
            let d_in = distance_map(&ind, w8, h8); // 0 outside owned region
            let d_out = distance_map(&inv, w8, h8); // 0 inside owned region
            Some(
                d_in.iter()
                    .zip(&d_out)
                    .zip(&wide)
                    .map(|((&di, &dj), &wd)| {
                        let hw = if wd { wide_cells } else { ramp_cells };
                        (0.5 + (di - dj) / (2.0 * hw)).clamp(0.0, 1.0)
                    })
                    .collect(),
            )
        })
        .collect();

    (ramps, hard)
}

/// Merged star-free base planes for [`BlendMode::Pyramid`], on the L8 grid:
/// per channel, the collapse of the panels' masked Laplacian pyramids of the
/// corrected star-free base planes (`corr8`), blended per level with Gaussian
/// pyramids of each panel's hard ownership indicator (`owner == panel`).
/// Validity for the normalized-convolution build is [`base_reach`] — beyond
/// it a panel contributes nothing at any level, so a sparse panel's zeros
/// never bleed into the blend. Returns the per-channel planes plus a per-cell
/// definedness flag: `false` where the mask sum vanished at some level
/// (numerical guard) — those pixels fall back to the feather-weighted base.
fn pyramid_base_planes(
    preps: &[PanelPrep],
    owner: &OwnerMap,
    nch: usize,
    feather_px: f32,
) -> (Vec<Vec<f32>>, Vec<bool>) {
    use crate::pyramid::{CellPyramid, blend_pyramids_guarded, build_masked, mask_pyramid};

    let (w8, h8) = (owner.w8, owner.h8);
    let (w, h) = (w8 as usize, h8 as usize);
    let cells = w * h;
    let n_levels = crate::pyramid::n_levels_for_feather(feather_px);

    // Panels owning no cell have zero mask weight at every level: skip them.
    let contributors: Vec<usize> = (0..preps.len())
        .filter(|&p| owner.owner.contains(&(p as u16)))
        .collect();
    if contributors.is_empty() {
        return (vec![vec![0.0; cells]; nch], vec![false; cells]);
    }

    struct PanelPyr {
        data: Vec<CellPyramid>,
        mask: CellPyramid,
    }
    let pyrs: Vec<PanelPyr> = contributors
        .par_iter()
        .map(|&pi| {
            let p = &preps[pi];
            let valid: Vec<f32> = base_reach(&p.summary.coverage, w, h)
                .into_iter()
                .map(|r| if r { 1.0 } else { 0.0 })
                .collect();
            let mask: Vec<f32> = owner
                .owner
                .iter()
                .map(|&o| if o == pi as u16 { 1.0 } else { 0.0 })
                .collect();
            let data = (0..nch)
                .map(|c| {
                    build_masked(
                        &p.corr8[c * cells..(c + 1) * cells],
                        &valid,
                        w8,
                        h8,
                        n_levels,
                    )
                })
                .collect();
            // The mask pyramid is clamped to the same validity plane the
            // data build uses, so this panel carries no blend weight where
            // its base pyramid would be sentinel/extrapolated garbage.
            PanelPyr {
                data,
                mask: mask_pyramid(&mask, &valid, w8, h8, n_levels),
            }
        })
        .collect();

    let masks: Vec<&CellPyramid> = pyrs.iter().map(|p| &p.mask).collect();
    let mut planes = Vec::with_capacity(nch);
    let mut defined = Vec::new();
    for c in 0..nch {
        let datas: Vec<&CellPyramid> = pyrs.iter().map(|p| &p.data[c]).collect();
        let (plane, def) = blend_pyramids_guarded(&datas, &masks);
        if c == 0 {
            defined = def; // masks/validity are channel-independent
        }
        planes.push(plane);
    }
    (planes, defined)
}

/// Two-band full-resolution blend: feathered base + seam-owned detail.
fn blend_twoband(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    graph: &OverlapGraph,
    params: &BlendParams,
    source: &dyn PanelSource,
    sink: &mut dyn RowSink,
) -> Result<()> {
    blend_twoband_impl(session, phot, surfaces, graph, params, source, sink, true)
}

/// [`blend_twoband`] with the connected star masks optionally disabled
/// (all-false masks: no seam penalty, no star-lock, no base exclusion) —
/// test-only escape hatch proving the mask's protection bites.
#[allow(clippy::too_many_arguments)]
fn blend_twoband_impl(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    graph: &OverlapGraph,
    params: &BlendParams,
    source: &dyn PanelSource,
    sink: &mut dyn RowSink,
    use_star_mask: bool,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let summaries = load_summaries(session)?;
    let masks: Vec<Vec<bool>> = if use_star_mask {
        summaries.par_iter().map(star_mask).collect()
    } else {
        summaries
            .iter()
            .map(|s| vec![false; s.w8 as usize * s.h8 as usize])
            .collect()
    };
    // Compact components (stars + spike arms) vs extended structure (nebular
    // cores the mask flooded across): the star-lock snap acts on the compact
    // part only (structure ramps, ±WIDE_RAMP_PX); the seam DP cost, the
    // defect-veto exemption, and the base exclusion keep the FULL mask (see
    // `seam::split_mask_components` — a reflection halo flooded into a
    // structure component must not enter the base band).
    let (w8s, h8s) = (summaries[0].w8, summaries[0].h8);
    let splits: Vec<(Vec<bool>, Vec<bool>)> = masks
        .par_iter()
        .map(|m| crate::seam::split_mask_components(m, w8s, h8s))
        .collect();
    let compact: Vec<Vec<bool>> = splits.iter().map(|(c, _)| c.clone()).collect();
    let structure: Vec<Vec<bool>> = splits.into_iter().map(|(_, s)| s).collect();
    let owner = compute_owner_map_masked(
        &summaries,
        graph,
        phot,
        surfaces,
        session.canvas,
        params.feather_px,
        &masks,
    );
    let (ramps, hard) = detail_transition_maps(&owner, &compact, &structure);
    let flat = fit_flatten_opt(&summaries, &masks, phot, surfaces, session, params.flatten)?;
    let mut preps = prep_from_summaries(session, phot, surfaces, flat.as_ref(), summaries, &masks);
    for p in &mut preps {
        p.summary.detail = Vec::new(); // only needed for the maps above
    }
    // Pyramid mode: the merged star-free base, replacing the per-pixel
    // feather-weighted base accumulation (which stays as the numerical-guard
    // fallback). Everything else below is byte-for-byte the TwoBand path.
    let pyr_base = (params.mode == BlendMode::Pyramid)
        .then(|| pyramid_base_planes(&preps, &owner, nch, params.feather_px));
    let panels = open_readers(session, source)?;

    let bbox = output_bbox(session, params)?;
    let (cx0, cy0) = (bbox[0], bbox[1]);
    let out_w = (bbox[2] - bbox[0]) as usize;
    let out_h = (bbox[3] - bbox[1]) as usize;
    let (w8, h8) = (owner.w8, owner.h8);
    tracing::info!(
        out_w,
        out_h,
        nch,
        prep_s = t0.elapsed().as_secs_f64(),
        "blend two-band"
    );

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_block = 1.0 / BLOCK as f32;
    let inv_cw = 1.0f32 / session.canvas.0 as f32;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let defect_veto = params.defect_veto;
    let mut band = vec![0.0f32; nch * band_rows * out_w];
    let mut nonfinite = 0u64;

    for y0 in (0..out_h).step_by(band_rows) {
        let rows_here = band_rows.min(out_h - y0);
        let band_cy0 = cy0 + y0 as u64;
        let band_cy1 = band_cy0 + rows_here as u64;
        let active: Vec<usize> = preps
            .iter()
            .enumerate()
            .filter(|(_, p)| p.bbox[1] < band_cy1 && p.bbox[3] > band_cy0)
            .map(|(i, _)| i)
            .collect();

        let rows_out: Vec<Vec<f32>> = (0..rows_here)
            .into_par_iter()
            .map(|r| {
                let cy = band_cy0 + r as u64;
                let mut out = vec![0.0f32; nch * out_w];
                // Panels covering this row: index, clipped mmap rows (storage
                // x0 + slice, canvas coords), surface row terms.
                struct PRow<'a> {
                    pi: usize,
                    rows: Vec<(usize, &'a [f32])>,
                    srow: Vec<(f32, f32, f32)>,
                }
                let prow: Vec<PRow> = active
                    .iter()
                    .filter(|&&pi| cy >= preps[pi].bbox[1] && cy < preps[pi].bbox[3])
                    .map(|&pi| PRow {
                        pi,
                        rows: (0..nch as u64)
                            .map(|c| {
                                // Content bbox ⊆ storage bbox: the row exists.
                                let (rx0, row) = panels[pi]
                                    .row(c, cy)
                                    .expect("content row within storage bbox");
                                (rx0 as usize, row)
                            })
                            .collect(),
                        srow: preps[pi].surf_row(cy as f64 * inv_ch),
                    })
                    .collect();
                if prow.is_empty() {
                    return out;
                }

                let gy_d = cy as f32 * inv_block; // distance-map alignment
                let gy_c = (cy as f32 + 0.5) * inv_block - 0.5; // cell centers
                let own_row = (cy / BLOCK as u64) as usize * w8 as usize;
                // Per-pixel scratch: which prow entries cover the pixel, their
                // feather weight, rim distance (px), and base values.
                let mut cov: Vec<(usize, f32, f32)> = Vec::with_capacity(prow.len());
                let mut bases = vec![0.0f32; prow.len() * nch];
                let mut base_acc = vec![0.0f32; nch];
                let mut det = vec![0.0f32; nch];

                for x in bbox[0]..bbox[2] {
                    let xi = x as usize;
                    let o = (x - cx0) as usize;
                    let gx_d = x as f32 * inv_block;
                    let gx_c = (x as f32 + 0.5) * inv_block - 0.5;
                    let mut bl_d: Option<BiLin> = None;
                    let mut bl_c: Option<BiLin> = None;

                    cov.clear();
                    base_acc.iter_mut().for_each(|v| *v = 0.0);
                    let mut sum_w = 0.0f32;
                    for (k, pr) in prow.iter().enumerate() {
                        let p = &preps[pr.pi];
                        if x < p.bbox[0] || x >= p.bbox[2] {
                            continue;
                        }
                        if pr.rows.iter().any(|&(rx0, row)| row[xi - rx0] == 0.0) {
                            continue; // uncovered: any channel zero
                        }
                        let bld = bl_d.get_or_insert_with(|| BiLin::at(w8, h8, gx_d, gy_d));
                        let blc = bl_c.get_or_insert_with(|| BiLin::at(w8, h8, gx_c, gy_c));
                        let d_px = BLOCK as f32 * bld.sample(&p.dist);
                        let wgt = weight(d_px, inv_feather);
                        let cells = p.summary.w8 as usize * p.summary.h8 as usize;
                        for (c, acc) in base_acc.iter_mut().enumerate() {
                            let b = blc.sample(&p.corr8[c * cells..(c + 1) * cells]);
                            bases[k * nch + c] = b;
                            *acc += wgt * b;
                        }
                        cov.push((k, wgt, d_px));
                        sum_w += wgt;
                    }
                    if cov.is_empty() {
                        continue; // fully uncovered output pixel stays 0
                    }

                    // Detail: one panel per pixel (or a short ramp of two).
                    let cell = own_row + (x / BLOCK as u64) as usize;
                    let cell_owner = owner.owner[cell];
                    let xn = x as f32 * inv_cw;
                    let full = |k: usize, c: usize| -> f32 {
                        let pr = &prow[k];
                        let p = &preps[pr.pi];
                        let (sa, sb, sc) = pr.srow[c];
                        let (rx0, row) = pr.rows[c];
                        row[xi - rx0] * p.gains[c] + p.offsets[c] + sa + xn * (sb + xn * sc)
                    };
                    let fallback =
                        || -> usize { cov.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0 };
                    det.iter_mut().for_each(|v| *v = 0.0);
                    if hard[cell] {
                        // Star-lock: snap to the owner (or the best-weighted
                        // covering panel when the owner is uncovered here).
                        let k = cov
                            .iter()
                            .find(|&&(k, _, _)| prow[k].pi as u16 == cell_owner)
                            .map(|&(k, _, _)| k)
                            .unwrap_or_else(fallback);
                        for (c, d) in det.iter_mut().enumerate() {
                            *d = full(k, c) - bases[k * nch + c];
                        }
                    } else {
                        let blc = bl_c.as_ref().expect("set for any covered pixel");
                        let mut rsum = 0.0f32;
                        for &(k, _, d_px) in &cov {
                            let Some(ramp) = &ramps[prow[k].pi] else {
                                continue;
                            };
                            // Rim fade: a panel's detail contribution falls
                            // to 0 over its last ±WIDE_RAMP_PX of coverage.
                            // Where the DP seam (or coverage itself) puts the
                            // ownership transition at a panel's rim, the ramp
                            // cannot complete — the partner simply ends — and
                            // an unfaded mix would hand over the remaining
                            // ramp fraction as a hard step exactly on the rim
                            // (~half the cross-panel structure mismatch, the
                            // residual staircase). Interior seams have both
                            // panels deep in coverage (fade = 1): unchanged.
                            let rv = blc.sample(ramp) * (d_px / WIDE_RAMP_PX).clamp(0.0, 1.0);
                            if rv <= 1e-6 {
                                continue;
                            }
                            rsum += rv;
                            for (c, d) in det.iter_mut().enumerate() {
                                *d += rv * (full(k, c) - bases[k * nch + c]);
                            }
                        }
                        if rsum > 0.0 {
                            det.iter_mut().for_each(|v| *v /= rsum);
                        } else {
                            let k = fallback();
                            for (c, d) in det.iter_mut().enumerate() {
                                *d = full(k, c) - bases[k * nch + c];
                            }
                        }
                    }

                    // Cross-panel defect veto: a cosmic-ray residue or
                    // satellite trail survives stacking in exactly one panel,
                    // so where a second panel covers the pixel and the cell
                    // is star-mask-clear in both, a detail difference far
                    // above the cell's noise scale marks a single-panel
                    // transient — take the smaller-|d| detail. (A genuine
                    // one-panel transient is suppressed too; for trails and
                    // cosmics that is the desired behaviour.)
                    if defect_veto && cov.len() >= 2 {
                        let ko = cov
                            .iter()
                            .find(|&&(k, _, _)| prow[k].pi as u16 == cell_owner)
                            .map(|&(k, _, _)| k);
                        let kp = ko.and_then(|ko| {
                            cov.iter()
                                .filter(|&&(k, _, _)| k != ko)
                                .max_by(|a, b| a.1.total_cmp(&b.1))
                                .map(|&(k, _, _)| k)
                        });
                        if let (Some(ko), Some(kp)) = (ko, kp) {
                            let (po, pp) = (prow[ko].pi, prow[kp].pi);
                            if !masks[po][cell] && !masks[pp][cell] {
                                let thresh = DEFECT_VETO_FACTOR
                                    * preps[po].vdet[cell].min(preps[pp].vdet[cell]);
                                for (c, d) in det.iter_mut().enumerate() {
                                    let d_o = full(ko, c) - bases[ko * nch + c];
                                    let d_p = full(kp, c) - bases[kp * nch + c];
                                    if (d_o - d_p).abs() > thresh {
                                        *d = if d_o.abs() <= d_p.abs() { d_o } else { d_p };
                                    }
                                }
                            }
                        }
                    }

                    match &pyr_base {
                        // Pyramid base: one bilinear sample of the merged
                        // plane; the detail term is untouched.
                        Some((planes, defined)) if defined[cell] => {
                            let blc = bl_c.as_ref().expect("set for any covered pixel");
                            for (c, plane) in planes.iter().enumerate() {
                                out[c * out_w + o] = blc.sample(plane) + det[c];
                            }
                        }
                        _ => {
                            let inv_sw = 1.0 / sum_w;
                            for c in 0..nch {
                                out[c * out_w + o] = base_acc[c] * inv_sw + det[c];
                            }
                        }
                    }
                }
                out
            })
            .collect();

        let bs = &mut band[..nch * rows_here * out_w];
        assemble_band(bs, &rows_out, out_w, nch);
        nonfinite += zero_non_finite(bs);
        sink.band(y0 as u64, bs)?;
    }
    warn_non_finite(nonfinite);
    sink.finish()?;
    tracing::info!(total_s = t0.elapsed().as_secs_f64(), "blend two-band done");
    Ok(())
}

/// Preview blend from the L8 summary means over fully-covered cells; output
/// geometry is the L8 grid cropped to the union bbox / 8.
fn blend_l8(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    params: &BlendParams,
    sink: &mut dyn RowSink,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let preps = prep_panels(session, phot, surfaces, params.flatten)?;
    let bbox = output_bbox(session, params)?;
    let (w8, h8) = (preps[0].summary.w8, preps[0].summary.h8);
    let b = BLOCK as u64;
    let gx0 = (bbox[0] / b) as u32;
    let gy0 = (bbox[1] / b) as u32;
    let gx1 = (bbox[2].div_ceil(b) as u32).min(w8);
    let gy1 = (bbox[3].div_ceil(b) as u32).min(h8);
    let out_w = (gx1 - gx0) as usize;
    let out_h = (gy1 - gy0) as usize;
    tracing::info!(
        out_w,
        out_h,
        nch,
        prep_s = t0.elapsed().as_secs_f64(),
        "blend from L8"
    );

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_cw = 1.0f64 / session.canvas.0 as f64;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let mut band = vec![0.0f32; nch * band_rows * out_w];
    let mut nonfinite = 0u64;

    for y0 in (0..out_h).step_by(band_rows) {
        let rows_here = band_rows.min(out_h - y0);
        let rows_out: Vec<Vec<f32>> = (0..rows_here)
            .into_par_iter()
            .map(|r| {
                let y8 = gy0 + (y0 + r) as u32;
                let mut acc = vec![0.0f32; nch * out_w];
                let mut wsum = vec![0.0f32; out_w];
                // Surfaces are evaluated at cell centers, matching the fit.
                let yn = (y8 as f64 + 0.5) * b as f64 * inv_ch;
                for p in &preps {
                    if y8 as u64 * b >= p.bbox[3] || (y8 as u64 + 1) * b <= p.bbox[1] {
                        continue;
                    }
                    let s = &p.summary;
                    let srow = p.surf_row(yn);
                    for x8 in gx0..gx1 {
                        if s.cov(x8, y8) < 1.0 {
                            continue; // only fully covered cells blend
                        }
                        let d_px = BLOCK as f32 * p.dist[y8 as usize * s.w8 as usize + x8 as usize];
                        let wgt = weight(d_px, inv_feather);
                        let o = (x8 - gx0) as usize;
                        wsum[o] += wgt;
                        let xn = ((x8 as f64 + 0.5) * b as f64 * inv_cw) as f32;
                        for c in 0..nch {
                            let (sa, sb, sc) = srow[c];
                            acc[c * out_w + o] += wgt
                                * (s.cell(c as u32, x8, y8) * p.gains[c]
                                    + p.offsets[c]
                                    + sa
                                    + xn * (sb + xn * sc));
                        }
                    }
                }
                normalize(&mut acc, &wsum, out_w, nch);
                acc
            })
            .collect();

        let bs = &mut band[..nch * rows_here * out_w];
        assemble_band(bs, &rows_out, out_w, nch);
        nonfinite += zero_non_finite(bs);
        sink.band(y0 as u64, bs)?;
    }
    warn_non_finite(nonfinite);
    sink.finish()?;
    tracing::info!(total_s = t0.elapsed().as_secs_f64(), "blend from L8 done");
    Ok(())
}

/// Zero every non-finite sample in `band`, returning how many were hit.
///
/// The blend guarantees its sinks all-finite bands: NaN/Inf in the *input*
/// (some stackers emit NaN for rejected pixels; NaN is nonzero, so such
/// pixels count as covered) propagates through the weighted blend math into
/// the output, and a downstream consumer — the PixInsight module's output
/// window in particular — must never receive it. Non-finite samples become
/// 0.0, the no-data sentinel.
fn zero_non_finite(band: &mut [f32]) -> u64 {
    let mut n = 0u64;
    for v in band.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
            n += 1;
        }
    }
    n
}

/// Log-once companion to [`zero_non_finite`]: warn at end of a blend that
/// zeroed any non-finite samples; silent for the common all-finite case.
fn warn_non_finite(count: u64) {
    if count > 0 {
        tracing::warn!(
            count,
            "non-finite output samples zeroed (do the input panels contain NaN/Inf pixels?)"
        );
    }
}

/// Divide accumulated `w·v` by `Σw` where covered; uncovered stays 0.
fn normalize(acc: &mut [f32], wsum: &[f32], out_w: usize, nch: usize) {
    for (o, &w) in wsum.iter().enumerate() {
        if w > 0.0 {
            let inv = 1.0 / w;
            for c in 0..nch {
                acc[c * out_w + o] *= inv;
            }
        }
    }
}

/// Repack per-row `[ch × w]` results into the planar band layout
/// `ch planes × band_rows × w`.
fn assemble_band(band: &mut [f32], rows_out: &[Vec<f32>], out_w: usize, nch: usize) {
    let rows_here = rows_out.len();
    for (r, rowv) in rows_out.iter().enumerate() {
        for c in 0..nch {
            band[(c * rows_here + r) * out_w..][..out_w]
                .copy_from_slice(&rowv[c * out_w..][..out_w]);
        }
    }
}

/// Unit tests live in a sibling file to keep this module navigable; the
/// `#[path]` child module keeps their access to private items unchanged.
#[cfg(test)]
#[path = "blend_tests.rs"]
mod tests;
