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
/// (stars + spike arms) still snap ([`detail_transition_maps`]).
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
    let b = [u[0].max(r[0]), u[1].max(r[1]), u[2].min(r[2]), u[3].min(r[3])];
    if b[0] >= b[2] || b[1] >= b[3] {
        return Err(Error::format(&session.dir, "ROI does not intersect the mosaic content"));
    }
    Ok(b)
}

/// Streaming consumer of blended rows (planar per band).
pub trait RowSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()>;
    /// One band starting at output row `y0`; `rows` is planar:
    /// `ch` planes × `band_rows` × `w`, row-major.
    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()>;
    fn finish(&mut self) -> Result<()>;
}

/// Union of the panel content bboxes: `[x0, y0, x1, y1]`, exclusive.
/// Errors if the session has no panel with content.
pub fn union_bbox(session: &Session) -> Result<[u64; 4]> {
    let mut it = session.panels.iter().filter(|p| p.bbox[2] > p.bbox[0] && p.bbox[3] > p.bbox[1]);
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
    session.panels.par_iter().map(|p| L8Summary::read(&session.summary_path(p.id))).collect()
}

/// Open every panel's pixel data for streaming reads ([`PanelReader`]
/// validates each panel's storage against the session canvas).
fn open_readers(session: &Session) -> Result<Vec<PanelReader>> {
    session
        .panels
        .par_iter()
        .map(|p| {
            let r = PanelReader::open(p, session.canvas)?;
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
    Ok(prep_from_summaries(session, phot, surfaces, flat.as_ref(), summaries, &masks))
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
        .map(|o| {
            crate::flatten::fit_flatten(
                summaries,
                masks,
                phot,
                surfaces,
                session.canvas,
                o,
                &session.dir,
            )
        })
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
            let mut corr8 =
                corrected_cell_means(&summary, &gains, &offsets, &surf, session.canvas);

            suppress_stars_in_base(&mut corr8, &summary, ch, mask);

            let cells = summary.w8 as usize * summary.h8 as usize;
            let vdet: Vec<f32> = (0..cells)
                .map(|i| {
                    (0..ch)
                        .map(|c| summary.detail[c * cells + i] * gains[c])
                        .fold(0.0f32, f32::max)
                })
                .collect();

            PanelPrep { bbox: p.bbox, gains, offsets, surf, summary, dist, corr8, vdet }
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
            .map(|c| table.get(c).and_then(|t| t.get(id)).copied().unwrap_or(default) as f32)
            .collect()
    };
    let surf: Vec<[f64; 6]> = (0..ch)
        .map(|c| {
            let mut padded = [0.0f64; 6];
            if let Some(coeffs) = surfaces.and_then(|s| s.coeffs.get(c)).and_then(|t| t.get(id)) {
                padded[..coeffs.len()].copy_from_slice(coeffs);
            }
            padded
        })
        .collect();
    (correction(&phot.gains, 1.0), correction(&phot.offsets, 0.0), surf)
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

    let source: Vec<bool> =
        (0..cells).map(|i| summary.coverage[i] >= 1.0 && !mask[i]).collect();
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
pub fn blend(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    graph: &OverlapGraph,
    params: &BlendParams,
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
        (1, BlendMode::Feather) => blend_full(session, phot, surfaces, params, sink),
        (1, BlendMode::TwoBand | BlendMode::Pyramid) => {
            blend_twoband(session, phot, surfaces, graph, params, sink)
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
    sink: &mut dyn RowSink,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let preps = prep_panels(session, phot, surfaces, params.flatten)?;
    let panels = open_readers(session)?;

    let bbox = output_bbox(session, params)?;
    let (cx0, cy0) = (bbox[0], bbox[1]);
    let out_w = (bbox[2] - bbox[0]) as usize;
    let out_h = (bbox[3] - bbox[1]) as usize;
    tracing::info!(out_w, out_h, nch, prep_s = t0.elapsed().as_secs_f64(), "blend full-res");

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_block = 1.0 / BLOCK as f32;
    let inv_cw = 1.0f32 / session.canvas.0 as f32;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let mut band = vec![0.0f32; nch * band_rows * out_w];

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
                        let (rx0, row) =
                            panels[pi].row(c, cy).expect("content row within storage bbox");
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
                        let d_px = BLOCK as f32
                            * bilinear(&p.dist, p.summary.w8, p.summary.h8, gx, gy);
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
        sink.band(y0 as u64, bs)?;
    }
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
            let ind: Vec<f32> =
                owner.owner.iter().map(|&o| if o == p as u16 { 1.0 } else { 0.0 }).collect();
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
            let mask: Vec<f32> =
                owner.owner.iter().map(|&o| if o == pi as u16 { 1.0 } else { 0.0 }).collect();
            let data = (0..nch)
                .map(|c| {
                    build_masked(&p.corr8[c * cells..(c + 1) * cells], &valid, w8, h8, n_levels)
                })
                .collect();
            PanelPyr { data, mask: mask_pyramid(&mask, w8, h8, n_levels) }
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
    sink: &mut dyn RowSink,
) -> Result<()> {
    blend_twoband_impl(session, phot, surfaces, graph, params, sink, true)
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
    sink: &mut dyn RowSink,
    use_star_mask: bool,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let nch = session.canvas.2 as usize;
    let summaries = load_summaries(session)?;
    let masks: Vec<Vec<bool>> = if use_star_mask {
        summaries.par_iter().map(star_mask).collect()
    } else {
        summaries.iter().map(|s| vec![false; s.w8 as usize * s.h8 as usize]).collect()
    };
    // Compact components (stars + spike arms) vs extended structure (nebular
    // cores the mask flooded across): the star-lock snap acts on the compact
    // part only (structure ramps, ±WIDE_RAMP_PX); the seam DP cost, the
    // defect-veto exemption, and the base exclusion keep the FULL mask (see
    // `seam::split_mask_components` — a reflection halo flooded into a
    // structure component must not enter the base band).
    let (w8s, h8s) = (summaries[0].w8, summaries[0].h8);
    let splits: Vec<(Vec<bool>, Vec<bool>)> =
        masks.par_iter().map(|m| crate::seam::split_mask_components(m, w8s, h8s)).collect();
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
    let mut preps =
        prep_from_summaries(session, phot, surfaces, flat.as_ref(), summaries, &masks);
    for p in &mut preps {
        p.summary.detail = Vec::new(); // only needed for the maps above
    }
    // Pyramid mode: the merged star-free base, replacing the per-pixel
    // feather-weighted base accumulation (which stays as the numerical-guard
    // fallback). Everything else below is byte-for-byte the TwoBand path.
    let pyr_base = (params.mode == BlendMode::Pyramid)
        .then(|| pyramid_base_planes(&preps, &owner, nch, params.feather_px));
    let panels = open_readers(session)?;

    let bbox = output_bbox(session, params)?;
    let (cx0, cy0) = (bbox[0], bbox[1]);
    let out_w = (bbox[2] - bbox[0]) as usize;
    let out_h = (bbox[3] - bbox[1]) as usize;
    let (w8, h8) = (owner.w8, owner.h8);
    tracing::info!(out_w, out_h, nch, prep_s = t0.elapsed().as_secs_f64(), "blend two-band");

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_block = 1.0 / BLOCK as f32;
    let inv_cw = 1.0f32 / session.canvas.0 as f32;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let defect_veto = params.defect_veto;
    let mut band = vec![0.0f32; nch * band_rows * out_w];

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
                    let fallback = || -> usize {
                        cov.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0
                    };
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
                            let Some(ramp) = &ramps[prow[k].pi] else { continue };
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
                            let rv =
                                blc.sample(ramp) * (d_px / WIDE_RAMP_PX).clamp(0.0, 1.0);
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
        sink.band(y0 as u64, bs)?;
    }
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
    tracing::info!(out_w, out_h, nch, prep_s = t0.elapsed().as_secs_f64(), "blend from L8");

    sink.begin(out_w as u64, out_h as u64, nch as u64)?;
    let band_rows = params.band_rows.max(1);
    let inv_feather = 1.0 / params.feather_px;
    let inv_cw = 1.0f64 / session.canvas.0 as f64;
    let inv_ch = 1.0f64 / session.canvas.1 as f64;
    let mut band = vec![0.0f32; nch * band_rows * out_w];

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
                        let d_px =
                            BLOCK as f32 * p.dist[y8 as usize * s.w8 as usize + x8 as usize];
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
        sink.band(y0 as u64, bs)?;
    }
    sink.finish()?;
    tracing::info!(total_s = t0.elapsed().as_secs_f64(), "blend from L8 done");
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze;
    use crate::synth::write_xisf;
    use std::path::{Path, PathBuf};

    struct MemSink {
        w: usize,
        h: usize,
        ch: usize,
        data: Vec<f32>,
        finished: bool,
    }

    impl MemSink {
        fn new() -> Self {
            Self { w: 0, h: 0, ch: 0, data: Vec::new(), finished: false }
        }

        fn at(&self, c: usize, x: usize, y: usize) -> f32 {
            self.data[(c * self.h + y) * self.w + x]
        }
    }

    impl RowSink for MemSink {
        fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
            self.w = w as usize;
            self.h = h as usize;
            self.ch = ch as usize;
            self.data = vec![f32::NAN; self.w * self.h * self.ch];
            Ok(())
        }

        fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
            assert_eq!(rows.len() % (self.ch * self.w), 0);
            let band_rows = rows.len() / (self.ch * self.w);
            for c in 0..self.ch {
                for r in 0..band_rows {
                    let src = &rows[(c * band_rows + r) * self.w..][..self.w];
                    let off = (c * self.h + y0 as usize + r) * self.w;
                    self.data[off..off + self.w].copy_from_slice(src);
                }
            }
            Ok(())
        }

        fn finish(&mut self) -> Result<()> {
            self.finished = true;
            Ok(())
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-blend-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Two constant single-channel panels on a 128×64 canvas:
    /// A = 0.2 over x∈[8,80), y∈[8,56); B = 0.4 over x∈[48,120), y∈[16,64).
    /// Union bbox = [8,8,120,64) → output 112×56.
    fn make_panels(dir: &Path) -> (Session, OverlapGraph) {
        let (w, h) = (128u64, 64u64);
        let mut frame = vec![0f32; (w * h) as usize];
        let fill = |frame: &mut [f32], v: f32, x0: u64, y0: u64, x1: u64, y1: u64| {
            for y in y0..y1 {
                for x in x0..x1 {
                    frame[(y * w + x) as usize] = v;
                }
            }
        };
        fill(&mut frame, 0.2, 8, 8, 80, 56);
        let a = dir.join("a.xisf");
        write_xisf(&a, w, h, 1, &frame).unwrap();

        frame.fill(0.0);
        fill(&mut frame, 0.4, 48, 16, 120, 64);
        let b = dir.join("b.xisf");
        write_xisf(&b, w, h, 1, &frame).unwrap();

        let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        (session, graph)
    }

    fn identity_phot(n_panels: usize, ch: usize) -> Photometry {
        Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0; n_panels]; ch],
            offsets: vec![vec![0.0; n_panels]; ch],
        }
    }

    #[test]
    fn union_bbox_covers_all_panels() {
        let dir = tmpdir("bbox");
        let (session, _) = make_panels(&dir);
        assert_eq!(union_bbox(&session).unwrap(), [8, 8, 120, 64]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn feather_blend_two_overlapping_panels() {
        let dir = tmpdir("feather");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let params = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 16, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        // Output is the cropped union bbox, and every pixel was written.
        assert_eq!((sink.w, sink.h, sink.ch), (112, 56, 1));
        assert!(sink.finished);
        assert!(sink.data.iter().all(|v| v.is_finite()), "no NaN/Inf anywhere");

        // Canvas (x, y) → output (x−8, y−8).
        let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);

        // Zero-covered region untouched (only B covers x≥80, but B starts y=16).
        assert_eq!(at(100, 10), 0.0);

        // Single-coverage interiors: weights normalize out exactly.
        assert!((at(24, 32) - 0.2).abs() < 1e-6, "A interior: {}", at(24, 32));
        assert!((at(104, 40) - 0.4).abs() < 1e-6, "B interior: {}", at(104, 40));

        // Overlap midpoint: both panels at full weight → exact average.
        assert!((at(64, 36) - 0.3).abs() < 1e-6, "overlap midpoint: {}", at(64, 36));

        // Monotone ramp from A's value to B's value across the overlap.
        let mut prev = f32::NEG_INFINITY;
        for x in 48..80 {
            let v = at(x, 36);
            assert!(v >= prev - 1e-5, "ramp not monotone at x={x}: {v} < {prev}");
            prev = v;
        }
        assert!(at(48, 36) < 0.3 && at(79, 36) > 0.3, "ramp spans the average");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn roi_matches_full_blend_subregion() {
        let dir = tmpdir("roi");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let full = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 16, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut full_sink = MemSink::new();
        blend(&session, &phot, None, &graph, &full, &mut full_sink).unwrap();

        // ROI spanning the overlap: canvas [40,100)x[20,60), clipped to union y<64.
        let roi = BlendParams { roi: Some([40, 20, 100, 60]), ..full.clone() };
        let mut roi_sink = MemSink::new();
        blend(&session, &phot, None, &graph, &roi, &mut roi_sink).unwrap();

        assert_eq!((roi_sink.w, roi_sink.h), (60, 40));
        for y in 0..roi_sink.h {
            for x in 0..roi_sink.w {
                // ROI output (x,y) = canvas (40+x, 20+y) = full output (32+x, 12+y).
                let (a, b) = (roi_sink.at(0, x, y), full_sink.at(0, x + 32, y + 12));
                assert!((a - b).abs() < 1e-6, "mismatch at roi ({x},{y}): {a} vs {b}");
            }
        }

        // Disjoint ROI errors.
        let miss = BlendParams { roi: Some([0, 0, 4, 4]), ..full.clone() };
        assert!(blend(&session, &phot, None, &graph, &miss, &mut MemSink::new()).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn photometric_corrections_are_applied() {
        let dir = tmpdir("phot");
        let (session, graph) = make_panels(&dir);
        let phot = Photometry {
            edge_fits: vec![],
            gains: vec![vec![2.0, 1.0]],
            offsets: vec![vec![0.01, 0.0]],
        };
        let params = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 64, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);
        // A interior: 0.2·2 + 0.01 = 0.41; B untouched; overlap = mean of both.
        assert!((at(24, 32) - 0.41).abs() < 1e-6);
        assert!((at(104, 40) - 0.4).abs() < 1e-6);
        assert!((at(64, 36) - 0.405).abs() < 1e-6);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn surfaces_are_applied_during_accumulation() {
        let dir = tmpdir("surf");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        // Panel A gets s(x,y) = 0.05 + 0.1·x − 0.02·y + 0.2·x² (normalized
        // canvas coords, canvas 128×64); panel B gets zero.
        let surf = crate::surfaces::Surfaces {
            order: 2,
            coeffs: vec![vec![
                vec![0.05, 0.1, -0.02, 0.2, 0.0, 0.0],
                vec![0.0; 6],
            ]],
            max_abs_s: vec![],
            bg_mad: vec![],
        };
        let s_at = |x: f64, y: f64| {
            let (xn, yn) = (x / 128.0, y / 64.0);
            0.05 + 0.1 * xn - 0.02 * yn + 0.2 * xn * xn
        };

        let params = BlendParams { feather_px: 16.0, downsample: 1, band_rows: 16, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut sink = MemSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params, &mut sink).unwrap();
        let at = |x: u64, y: u64| sink.at(0, (x - 8) as usize, (y - 8) as usize);

        // A interior: 0.2 + s(x,y); B interior unchanged; overlap midpoint
        // (both full weight): mean of corrected values.
        let (ax, ay) = (24u64, 32u64);
        let expect_a = 0.2 + s_at(ax as f64, ay as f64) as f32;
        assert!((at(ax, ay) - expect_a).abs() < 1e-5, "A interior {} vs {expect_a}", at(ax, ay));
        assert!((at(104, 40) - 0.4).abs() < 1e-6, "B interior must stay uncorrected");
        let expect_mid = 0.5 * ((0.2 + s_at(64.0, 36.0) as f32) + 0.4);
        assert!(
            (at(64, 36) - expect_mid).abs() < 1e-5,
            "overlap midpoint {} vs {expect_mid}",
            at(64, 36)
        );

        // Same corrections must hold on the L8 preview path (cell centers).
        let params8 = BlendParams { feather_px: 16.0, downsample: 8, band_rows: 4, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut sink8 = MemSink::new();
        blend(&session, &phot, Some(&surf), &graph, &params8, &mut sink8).unwrap();
        // Canvas cell (3,4) is A-only; center pixel (28, 36).
        let got = sink8.at(0, 3 - 1, 4 - 1);
        let expect_cell = 0.2 + s_at(28.0, 36.0) as f32;
        assert!((got - expect_cell).abs() < 1e-5, "L8 A-only cell {got} vs {expect_cell}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn downsample_blends_from_l8_summaries() {
        let dir = tmpdir("l8");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let params = BlendParams { feather_px: 16.0, downsample: 8, band_rows: 3, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        // L8 grid cropped to union bbox / 8: x8∈[1,15), y8∈[1,8) → 14×7.
        assert_eq!((sink.w, sink.h, sink.ch), (14, 7, 1));
        assert!(sink.data.iter().all(|v| v.is_finite()));

        // Canvas cell (x8, y8) → output (x8−1, y8−1).
        let at = |x8: usize, y8: usize| sink.at(0, x8 - 1, y8 - 1);
        assert!((at(3, 4) - 0.2).abs() < 1e-6, "A-only cell: {}", at(3, 4));
        assert!((at(8, 4) - 0.3).abs() < 1e-6, "overlap cell: {}", at(8, 4));
        assert_eq!(at(13, 1), 0.0, "cell covered by neither panel");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Mandatory phase-3E test 3 (spike integrity, end-to-end): two panels
    /// share bright spiked stars in their overlap; panel 1 is misregistered
    /// by 0.6 px and its spikes are rotated 0.02 rad (per-session camera
    /// rotation). Every pixel along each merged arm must match ONE panel's
    /// corrected pixels — and the same check must FAIL with the star masks
    /// disabled, proving the mask (not luck) protects the arms.
    #[test]
    fn twoband_spike_arms_match_one_panel() {
        use crate::analyze::analyze_opts;
        use crate::synth::{SynthSpec, generate};

        let dir = tmpdir("spikes");
        let spec = SynthSpec {
            canvas: (768, 640),
            channels: 1,
            grid: (2, 1),
            overlap_frac: 0.25,
            n_stars: 90,
            noise_sigma: 0.002,
            panel_gain_range: (0.95, 1.1),
            panel_offset_range: (-0.003, 0.005),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![(0.0, 0.0), (0.8, 0.0)],
            panel_spike_angle: vec![0.0, 0.02],
            panel_defects: vec![],
            // Seed chosen (deterministically) so a bright spiked star lands on
            // the band's midline, where the mask-disabled seam runs within
            // ramp reach of its arms — the kink scenario observed on real
            // data. Verified: mask on ≤ 0.0004 on all arms; mask off 0.0203
            // on one arm (thresh 0.0120).
            mid_blobs: 0,
            shift_blobs: false,
            seed: 9,
        };
        let res = generate(&spec, &dir.join("panels")).unwrap();
        let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
        let phot = Photometry::load(&session.photometry_path()).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode: BlendMode::TwoBand,
            roi: None,
            // This test isolates the star mask's protection: in the run(false)
            // teeth check every arm cell is unmasked and thus veto-eligible,
            // and the veto could partially repair the arm mismatch the check
            // must detect. The veto has its own tests.
            defect_veto: false,
            flatten: None,
        };

        let run = |use_mask: bool, mode: BlendMode| -> MemSink {
            let params = BlendParams { mode, ..params.clone() };
            let mut sink = MemSink::new();
            blend_twoband_impl(&session, &phot, None, &graph, &params, &mut sink, use_mask)
                .unwrap();
            sink
        };
        let with_mask = run(true, BlendMode::TwoBand);
        let with_mask_pyr = run(true, BlendMode::Pyramid);
        let no_mask = run(false, BlendMode::TwoBand);

        // Corrected panel pixels in the blend's photometric frame.
        let w = spec.canvas.0 as usize;
        let corrected: Vec<Vec<f32>> = res
            .panel_paths
            .iter()
            .enumerate()
            .map(|(p, path)| {
                let panel = crate::formats::xisf::XisfPanel::open(path).unwrap();
                let (g, o) = (phot.gains[0][p] as f32, phot.offsets[0][p] as f32);
                panel.channel(0).iter().map(|&v| v * g + o).collect()
            })
            .collect();
        let bbox = union_bbox(&session).unwrap();

        // Spiked stars whose full spike footprint (arm length + margin) lies
        // inside BOTH panel windows — arms crossing the overlap band.
        let overlap_stars: Vec<(f64, f64, f64)> = res
            .spiked
            .iter()
            .copied()
            .filter(|&(sx, sy, len)| {
                res.windows.iter().all(|&[x0, y0, x1, y1]| {
                    sx - len >= x0 as f64 + 4.0
                        && sx + len < x1 as f64 - 4.0
                        && sy - len >= y0 as f64 + 4.0
                        && sy + len < y1 as f64 - 4.0
                })
            })
            .collect();
        assert!(
            !overlap_stars.is_empty(),
            "seed must place at least one spiked star fully inside the overlap"
        );

        // Pixels of one arm: 3×3 neighbourhoods along the arm at BOTH panels'
        // spike angles (the merged arm may come from either panel), skipping
        // the core where the panels nearly agree anyway.
        let arm_pixels = |sx: f64, sy: f64, len: f64, arm: usize| -> Vec<(usize, usize)> {
            let mut px = Vec::new();
            for &a0 in &spec.panel_spike_angle {
                let th = a0 as f64 + arm as f64 * std::f64::consts::FRAC_PI_2;
                let (s, c) = th.sin_cos();
                let mut t = 3.0;
                while t <= len {
                    let (x, y) = ((sx + t * c).round() as i64, (sy + t * s).round() as i64);
                    for yy in y - 1..=y + 1 {
                        for xx in x - 1..=x + 1 {
                            px.push((xx as usize, yy as usize));
                        }
                    }
                    t += 1.0;
                }
            }
            px
        };
        // Distance of the merged arm to the closest single corrected panel.
        let one_panel_dist = |sink: &MemSink, pixels: &[(usize, usize)]| -> f32 {
            (0..2)
                .map(|p| {
                    pixels
                        .iter()
                        .map(|&(x, y)| {
                            let merged = sink.at(
                                0,
                                x - bbox[0] as usize,
                                y - bbox[1] as usize,
                            );
                            (merged - corrected[p][y * w + x]).abs()
                        })
                        .fold(0.0f32, f32::max)
                })
                .fold(f32::INFINITY, f32::min)
        };

        let thresh = 6.0 * spec.noise_sigma;
        let mut no_mask_fails = 0;
        let mut arms = 0;
        for &(sx, sy, len) in &overlap_stars {
            for arm in 0..4 {
                let pixels = arm_pixels(sx, sy, len, arm);
                let d_mask = one_panel_dist(&with_mask, &pixels);
                let d_pyr = one_panel_dist(&with_mask_pyr, &pixels);
                let d_none = one_panel_dist(&no_mask, &pixels);
                eprintln!(
                    "spiked star ({sx:6.1},{sy:6.1}) arm {arm}: \
                     masked {d_mask:.4}, pyramid {d_pyr:.4}, unmasked {d_none:.4} \
                     (thresh {thresh:.4})"
                );
                assert!(
                    d_mask < thresh,
                    "arm {arm} of star at ({sx},{sy}) matches no single panel: {d_mask}"
                );
                assert!(
                    d_pyr < thresh,
                    "Pyramid: arm {arm} of star at ({sx},{sy}) matches no single panel: {d_pyr}"
                );
                if d_none >= thresh {
                    no_mask_fails += 1;
                }
                arms += 1;
            }
        }
        assert!(
            no_mask_fails > 0,
            "mask-disabled blend passed the one-panel check on all {arms} arms — \
             the mask is not biting"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Mandatory phase-3F test 1: a satellite-trail piece and a cosmic ray in
    /// ONE panel's overlap region are vetoed — the merged pixels match the
    /// clean panel within noise — while `defect_veto: false` leaves them at
    /// full strength (proving both directions).
    #[test]
    fn twoband_defect_veto_suppresses_overlap_defects() {
        use crate::analyze::analyze_opts;
        use crate::seam::compute_owner_map;
        use crate::synth::{SynthSpec, generate};

        let dir = tmpdir("veto");
        // 2×1 grid on 768×480: windows [0,432) and [336,768), overlap band
        // x∈[336,432). The band is 12×60 cells — under seam::DP_MIN_LONG on
        // both axes — so ownership keeps the deterministic Voronoi midline at
        // x≈384 and the defects (x ≥ 414) are owned by panel 1, the panel
        // that carries them: without the veto they show at full strength.
        let noise = 0.0025f32;
        // Amplitudes thread the veto's needle deliberately: |Δdetail| ≈ 0.97
        // amp must exceed 6× the clean panel's cell RMS (≈ noise), while the
        // defect cell's own RMS (0.17×/0.12× amp for 2 px/1 px per cell) must
        // stay under the star mask's 3×-median seed threshold — like the
        // real defects that matter, bright but not star-bright. The trail
        // starts at x=414 (cell phase 6) so its 4 px straddle two cells.
        let trail = (1usize, 414u64, 268u64, 4u32, 0.032f32);
        let ray = (1usize, 421u64, 156u64, 1u32, 0.040f32);
        let spec = SynthSpec {
            canvas: (768, 480),
            channels: 1,
            grid: (2, 1),
            overlap_frac: 0.25,
            n_stars: 30,
            noise_sigma: noise,
            panel_gain_range: (0.95, 1.1),
            panel_offset_range: (-0.003, 0.005),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![],
            panel_spike_angle: vec![],
            panel_defects: vec![trail, ray],
            mid_blobs: 0,
            shift_blobs: false,
            seed: 11,
        };
        let res = generate(&spec, &dir.join("panels")).unwrap();
        let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
        let phot = Photometry::load(&session.photometry_path()).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

        // Preconditions (defects must sit where the veto is allowed to act):
        // their cells star-mask-clear in both panels and owned by panel 1.
        let summaries: Vec<L8Summary> = session
            .panels
            .iter()
            .map(|p| L8Summary::read(&session.summary_path(p.id)).unwrap())
            .collect();
        let masks: Vec<Vec<bool>> = summaries.iter().map(crate::seam::star_mask).collect();
        let owner =
            compute_owner_map(&summaries, &graph, &phot, None, session.canvas, 24.0);
        for &(_, dx, dy, len, _) in &spec.panel_defects {
            for x in dx..dx + len as u64 {
                let cell =
                    (dy / BLOCK as u64) as usize * owner.w8 as usize + (x / BLOCK as u64) as usize;
                assert!(
                    !masks[0][cell] && !masks[1][cell],
                    "defect cell at ({x},{dy}) must be star-mask-clear (move it or reseed)"
                );
                assert_eq!(
                    owner.owner[cell], 1,
                    "defect cell at ({x},{dy}) must be owned by the defect panel"
                );
            }
        }

        let run = |veto: bool, mode: BlendMode| -> MemSink {
            let params = BlendParams {
                feather_px: 24.0,
                downsample: 1,
                band_rows: 64,
                mode,
                roi: None,
                defect_veto: veto,
                flatten: None,
            };
            let mut sink = MemSink::new();
            blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();
            sink
        };
        let on = run(true, BlendMode::TwoBand);
        let off = run(false, BlendMode::TwoBand);
        let on_pyr = run(true, BlendMode::Pyramid);
        let off_pyr = run(false, BlendMode::Pyramid);

        // Clean panel (0), corrected into the blend's photometric frame.
        let w = spec.canvas.0 as usize;
        let clean = crate::formats::xisf::XisfPanel::open(&res.panel_paths[0]).unwrap();
        let (g0, o0) = (phot.gains[0][0] as f32, phot.offsets[0][0] as f32);
        let corrected0: Vec<f32> = clean.channel(0).iter().map(|&v| v * g0 + o0).collect();
        let bbox = union_bbox(&session).unwrap();

        // Max |merged − clean panel| over the defect pixels ±1.
        let defect_dist = |sink: &MemSink, d: (usize, u64, u64, u32, f32)| -> f32 {
            let (_, dx, dy, len, _) = d;
            let mut worst = 0.0f32;
            for y in dy - 1..=dy + 1 {
                for x in dx - 1..dx + len as u64 + 1 {
                    let merged =
                        sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
                    worst = worst.max((merged - corrected0[y as usize * w + x as usize]).abs());
                }
            }
            worst
        };

        let thresh = 6.0 * noise;
        for (mode, on, off) in
            [(BlendMode::TwoBand, &on, &off), (BlendMode::Pyramid, &on_pyr, &off_pyr)]
        {
            for (name, d) in [("trail", trail), ("ray", ray)] {
                let d_on = defect_dist(on, d);
                let d_off = defect_dist(off, d);
                eprintln!(
                    "{mode:?} {name}: veto on {d_on:.4}, veto off {d_off:.4} (thresh {thresh:.4})"
                );
                assert!(
                    d_on < thresh,
                    "{mode:?} {name}: veto ON must match the clean panel within noise, got {d_on}"
                );
                assert!(
                    d_off > thresh,
                    "{mode:?} {name}: veto OFF must show the defect (teeth), got {d_off}"
                );
            }
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Mandatory phase-3F test 3: a defect *outside* any overlap has nothing
    /// to be compared against — the owner's value stands untouched, veto ON.
    #[test]
    fn defect_outside_overlap_is_untouched() {
        use crate::analyze::analyze_opts;
        use crate::synth::{SynthSpec, generate};

        let dir = tmpdir("veto-single");
        // Windows [0,288) and [224,512): x=400 is covered by panel 1 alone.
        let defect = (1usize, 400u64, 190u64, 4u32, 0.03f32);
        let spec = SynthSpec {
            canvas: (512, 384),
            channels: 1,
            grid: (2, 1),
            overlap_frac: 0.25,
            n_stars: 20,
            noise_sigma: 0.0025,
            panel_gain_range: (0.95, 1.1),
            panel_offset_range: (-0.003, 0.005),
            panel_gradient_range: (0.0, 0.0),
            global_gradient: (0.0, 0.0, 0.0),
            panel_shift: vec![],
            panel_spike_angle: vec![],
            panel_defects: vec![defect],
            mid_blobs: 0,
            shift_blobs: false,
            seed: 5,
        };
        let res = generate(&spec, &dir.join("panels")).unwrap();
        let session = analyze_opts(&res.panel_paths, &dir.join("s.mmm-session"), None).unwrap();
        let phot = Photometry::load(&session.photometry_path()).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();

        let params = BlendParams {
            feather_px: 24.0,
            downsample: 1,
            band_rows: 64,
            mode: BlendMode::TwoBand,
            roi: None,
            defect_veto: true,
            flatten: None,
        };
        let mut sink = MemSink::new();
        blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

        let w = spec.canvas.0 as usize;
        let panel = crate::formats::xisf::XisfPanel::open(&res.panel_paths[1]).unwrap();
        let (g1, o1) = (phot.gains[0][1] as f32, phot.offsets[0][1] as f32);
        let corrected1: Vec<f32> = panel.channel(0).iter().map(|&v| v * g1 + o1).collect();
        let bbox = union_bbox(&session).unwrap();
        let (_, dx, dy, len, amp) = defect;

        // Single coverage: base + (full − base) reconstructs the corrected
        // input exactly — including the defect, at full strength.
        let mut worst = 0.0f32;
        for y in dy - 2..=dy + 2 {
            for x in dx - 2..dx + len as u64 + 2 {
                let merged = sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
                worst = worst.max((merged - corrected1[y as usize * w + x as usize]).abs());
            }
        }
        eprintln!("outside-overlap defect: max |merged − corrected input| = {worst:.2e}");
        assert!(worst < 1e-4, "owner value must stand untouched, max diff {worst}");

        // And the defect really is present in the output (not vetoed away):
        // the defect pixel towers over the row 4 px below by ~the amplitude.
        let at = |x: u64, y: u64| sink.at(0, (x - bbox[0]) as usize, (y - bbox[1]) as usize);
        let step = at(dx + 1, dy) - at(dx + 1, dy + 4);
        assert!(
            step > 0.5 * amp * g1,
            "defect must survive in single coverage: step {step} vs amp {amp}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Deterministic per-pixel "noise" (splitmix-style hash), identical for
    /// every generation of the same canvas — it cancels exactly out of the
    /// cross-panel and cross-run differences the structure tests measure,
    /// while giving the star mask a realistic nonzero median detail floor.
    fn hash_noise(x: u64, y: u64) -> f32 {
        let mut s = (y.wrapping_mul(0x1_0000_0001) ^ x).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        s ^= s >> 33;
        s = s.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        s ^= s >> 33;
        (((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2.0 * 0.002) as f32
    }

    /// Two panels reproducing the M42-staircase geometry: a vertical overlap
    /// band x∈[224,288) over the full 640-px height, crossed by a bright
    /// *horizontal* ridge of extended structure (σ=50 px around y=320) with
    /// cell-scale texture and one embedded bright star — so the connected
    /// star mask floods across the whole band at structure latitudes exactly
    /// as it does over the real M42 core. Panel B carries a residual
    /// mismatch proportional to the structure brightness (peak `mismatch`)
    /// plus a small background anchor that keeps the DP seam mid-band in the
    /// unmasked rows. Panel A's exact pixel values are returned so tests can
    /// measure the merged output's deviation profile `D = out − A`.
    fn structure_band_panels(
        dir: &Path,
        mismatch: f32,
    ) -> (Session, OverlapGraph, Vec<f32>) {
        std::fs::create_dir_all(dir).unwrap();
        let (w, h) = (512u64, 640u64);
        let ridge = |y: f64| 0.3 * (-((y - 320.0) * (y - 320.0)) / (2.0 * 50.0 * 50.0)).exp();
        let sky_a = |x: u64, y: u64| -> f32 {
            let (xf, yf) = (x as f64, y as f64);
            let s = ridge(yf);
            let tex = s * 0.15 * (1.19 * xf).sin() * (1.33 * yf).sin();
            let dx = xf - 256.0;
            let dy = yf - 320.0;
            let star = 1.0 * (-(dx * dx + dy * dy) / (2.0 * 2.0 * 2.0)).exp();
            0.05 + (s + tex + star) as f32 + hash_noise(x, y)
        };
        // Residual inter-panel mismatch ∝ structure brightness, plus a mild
        // background anchor (cheapest at the band's midline) so the seam has
        // a preferred mid-band line in the unmasked background rows.
        let delta = |x: u64, y: u64| -> f32 {
            let anchor = 0.002 * (((x as f64 - 256.0) / 32.0).powi(2)).min(4.0);
            (mismatch as f64 * ridge(y as f64) / 0.3 + anchor) as f32
        };

        // The full-canvas A-frame sky — the reference D is measured against
        // (defined everywhere, unlike panel A's zero-padded window).
        let truth_a: Vec<f32> =
            (0..w * h).map(|i| sky_a(i % w, i / w)).collect();

        let mut frame = vec![0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..288u64 {
                frame[(y * w + x) as usize] = truth_a[(y * w + x) as usize];
            }
        }
        let a = dir.join("a.xisf");
        write_xisf(&a, w, h, 1, &frame).unwrap();

        frame.fill(0.0);
        for y in 0..h {
            for x in 224..512u64 {
                frame[(y * w + x) as usize] = truth_a[(y * w + x) as usize] + delta(x, y);
            }
        }
        let b = dir.join("b.xisf");
        write_xisf(&b, w, h, 1, &frame).unwrap();

        let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        (session, graph, truth_a)
    }

    /// Mandatory M42-staircase-fix test 2 (seam-through-structure
    /// continuity): where the connected star mask floods across bright
    /// extended structure spanning the whole overlap band, the merged output
    /// must stay continuous across the owner boundary despite a residual
    /// inter-panel mismatch — the mismatch must ramp, not snap.
    ///
    /// Metric: D(x, y) = merged − panel A's exact pixels. Away from the seam
    /// D is ~0 (A side) or ~the mismatch (B side); the max adjacent-pixel
    /// jump of D along rows crossing the band measures how abruptly the
    /// transition happens. A no-mismatch control run bounds the machinery's
    /// baseline jump. Teeth: with star-lock snapping on the full flooded
    /// mask this measured a max jump of 0.0283 ≈ the full band mismatch in
    /// both TwoBand and Pyramid mode (control 0.0103) — the L8-quantized
    /// staircase; the bound below fails loudly on that behaviour. The ramp
    /// alone still left 0.0147 ≈ *half* the mismatch, stepping at panel A's
    /// coverage rim (x=287) where the DP hugs the band edge and the ramp is
    /// truncated by coverage — the rim fade in the detail mix removes that
    /// too. Current: TwoBand 0.0015 (control 0.0003), Pyramid 0.0015
    /// (control 0.0006), with the base excluding the full star mask.
    #[test]
    fn seam_through_structure_ramps_the_mismatch() {
        use crate::seam::split_mask_components;

        let dir = tmpdir("structseam");
        let mismatch = 0.03f32;
        let (session, graph, truth_a) = structure_band_panels(&dir.join("mm"), mismatch);
        let (ctrl_session, ctrl_graph, ctrl_truth) =
            structure_band_panels(&dir.join("ctrl"), 0.0);
        let phot = identity_phot(2, 1);

        // Preconditions — the scenario must reproduce the M42 diagnosis:
        // at structure latitudes the connected mask covers the entire band
        // in both panels (flooded through bright structure), and every one
        // of those cells classifies as extended structure, not compact.
        let summaries = load_summaries(&session).unwrap();
        let (w8, h8) = (summaries[0].w8, summaries[0].h8);
        let masks: Vec<Vec<bool>> = summaries.iter().map(star_mask).collect();
        let splits: Vec<(Vec<bool>, Vec<bool>)> =
            masks.iter().map(|m| split_mask_components(m, w8, h8)).collect();
        for y8 in 34..46u32 {
            for x8 in 28..36u32 {
                let i = (y8 * w8 + x8) as usize;
                for p in 0..2 {
                    assert!(
                        masks[p][i],
                        "band cell ({x8},{y8}) must be mask-flooded in panel {p}"
                    );
                    assert!(
                        splits[p].1[i] && !splits[p].0[i],
                        "band cell ({x8},{y8}) must classify as structure in panel {p}"
                    );
                }
            }
        }

        // The owner boundary at structure latitudes must run through
        // structure-masked cells (the seam has no mask-free corridor —
        // exactly the M42 situation), wherever the DP put it.
        let owner = crate::seam::compute_owner_map_masked(
            &summaries,
            &graph,
            &phot,
            None,
            session.canvas,
            64.0,
            &masks,
        );
        for y8 in 34..46u32 {
            let bx = (1..w8)
                .find(|&x8| owner.at(x8 - 1, y8) == 0 && owner.at(x8, y8) == 1)
                .expect("each band row must have an A→B owner boundary");
            let i = (y8 * w8 + bx) as usize;
            assert!(
                splits[0].1[i] || splits[1].1[i],
                "boundary cell ({bx},{y8}) must be structure-masked"
            );
        }

        let run = |session: &Session, graph: &OverlapGraph, mode: BlendMode| -> MemSink {
            let params = BlendParams {
                feather_px: 64.0,
                downsample: 1,
                band_rows: 64,
                mode,
                roi: None,
                defect_veto: true,
                flatten: None,
            };
            let mut sink = MemSink::new();
            blend(session, &phot, None, graph, &params, &mut sink).unwrap();
            sink
        };

        // Max adjacent-pixel jump of D = merged − truth_A along band-crossing
        // rows at structure latitudes (output coords == canvas coords: both
        // panels' content starts at the origin).
        let jump = |sink: &MemSink, truth: &[f32]| -> f32 {
            let mut worst = 0.0f32;
            for y in (248..392u64).step_by(4) {
                let d = |x: u64| {
                    sink.at(0, x as usize, y as usize) - truth[(y * 512 + x) as usize]
                };
                // Whole shared band incl. its edge cells: the DP may hug a
                // band edge in fully-masked rows, putting the transition at
                // x=224/288 — the panel rims, where the ramp is truncated by
                // coverage and only the rim fade keeps D continuous;
                // single-coverage pixels either side reconstruct their panel
                // exactly, so D stays well-defined throughout.
                for x in 220..292u64 {
                    worst = worst.max((d(x + 1) - d(x)).abs());
                }
            }
            worst
        };

        for mode in [BlendMode::TwoBand, BlendMode::Pyramid] {
            let out = run(&session, &graph, mode);
            let ctrl = run(&ctrl_session, &ctrl_graph, mode);

            // The mismatch really is in the data and survives to the far
            // side: in B-only coverage well beyond the feather scale the
            // output reconstructs B, D = mismatch (peak row) + anchor.
            let d_far = out.at(0, 420, 320) - truth_a[320 * 512 + 420];
            assert!(
                d_far > 0.8 * mismatch,
                "far-side D must carry the mismatch, got {d_far}"
            );

            let j_mm = jump(&out, &truth_a);
            let j_ctrl = jump(&ctrl, &ctrl_truth);
            eprintln!(
                "{mode:?}: max D-jump {j_mm:.4} (control {j_ctrl:.4}, mismatch {mismatch})"
            );
            assert!(
                j_mm < (2.0 * j_ctrl).max(0.008),
                "{mode:?}: seam through structure steps by {j_mm} \
                 (control baseline {j_ctrl}) — the mismatch must ramp, not snap"
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two panels reproducing the bright-star reflection-halo geometry behind
    /// the user-reported dark moat: a vertical overlap band x∈[160,480) on a
    /// 640×640 canvas, one bright star at (320,320) whose broad reflection
    /// halo *differs between the panels* (halos are internal reflections —
    /// optics- and position-dependent — so overlapping panels genuinely
    /// disagree there: A carries 0.4·G(σ=24 px), B carries 0.3·G(σ=34 px)
    /// centered 3 px off). The halo floods each panel's connected star mask
    /// well past every compactness bound, so the star+halo component
    /// classifies as extended structure in both panels — the exact trigger
    /// of the regression. Shared deterministic noise gives the mask a
    /// realistic median-detail floor and cancels out of cross-panel diffs.
    fn halo_moat_panels(dir: &Path) -> (Session, OverlapGraph) {
        std::fs::create_dir_all(dir).unwrap();
        let (w, h) = (640u64, 640u64);
        let (sx, sy) = (320.0f64, 320.0f64);
        let value = |x: u64, y: u64, amp: f64, sigma: f64, halo_dx: f64| -> f32 {
            let (dx, dy) = (x as f64 - sx, y as f64 - sy);
            let core = 2.0 * (-(dx * dx + dy * dy) / (2.0 * 1.5 * 1.5)).exp();
            let hx = dx - halo_dx;
            let halo = amp * (-(hx * hx + dy * dy) / (2.0 * sigma * sigma)).exp();
            0.05 + (core + halo) as f32 + hash_noise(x, y)
        };
        let mut frame = vec![0f32; (w * h) as usize];
        for y in 0..h {
            for x in 0..480u64 {
                frame[(y * w + x) as usize] = value(x, y, 0.3, 26.0, 0.0);
            }
        }
        let a = dir.join("a.xisf");
        write_xisf(&a, w, h, 1, &frame).unwrap();

        frame.fill(0.0);
        for y in 0..h {
            for x in 160..640u64 {
                frame[(y * w + x) as usize] = value(x, y, 0.8, 40.0, 4.0);
            }
        }
        let b = dir.join("b.xisf");
        write_xisf(&b, w, h, 1, &frame).unwrap();

        let session = analyze(&[a, b], &dir.join("s.mmm-session")).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        (session, graph)
    }

    /// Moat regression (user report on real Orion data, e.g. canvas
    /// (2376,13912)): when base exclusion acted on *compact* mask components
    /// only, a bright star's reflection halo — flooded into an
    /// extended-structure component — entered the pyramid base band. Near
    /// ownership/mask transitions the per-level masked blend then mixes the
    /// panels' disagreeing halos with level-dependent weights, so the halo's
    /// Laplacian negative lobes no longer cancel: a wide dark annulus
    /// (clearly below background) around the star+halo. The base must
    /// exclude the FULL star mask (compact ∪ structure) as in phase 3/4;
    /// this test pins that: no annulus pixel between the halo edge and 1.5×
    /// the halo radius may fall below the local background, in either
    /// panel's single-owner zone. Teeth (measured red on the compact-only
    /// base): Pyramid-mode B-owned annulus min 0.0361 vs background 0.05
    /// (moat trough 0.0354 at r ≈ 125 px — a 30% deficit); after the fix
    /// both sides sit at 0.0476+, within the ±0.002 noise floor. TwoBand's
    /// per-pixel feather base is a convex combination and never dug a moat;
    /// it is asserted too as a cheap guard.
    #[test]
    fn bright_star_halo_leaves_no_dark_moat() {
        use crate::seam::split_mask_components;

        let dir = tmpdir("halomoat");
        let (session, graph) = halo_moat_panels(&dir);
        let phot = identity_phot(2, 1);

        // Preconditions — the mechanism's trigger must be present: in both
        // panels the star's mask component exceeds every compactness bound
        // and classifies as extended structure.
        let summaries = load_summaries(&session).unwrap();
        let (w8, h8) = (summaries[0].w8, summaries[0].h8);
        let masks: Vec<Vec<bool>> = summaries.iter().map(star_mask).collect();
        let star_cell = (40 * w8 + 40) as usize;
        for (p, mask) in masks.iter().enumerate() {
            let (compact, structure) = split_mask_components(mask, w8, h8);
            assert!(mask[star_cell], "panel {p}: star cell must be masked");
            assert!(
                structure[star_cell] && !compact[star_cell],
                "panel {p}: the halo-flooded component must classify as \
                 structure — the moat mechanism's trigger"
            );
        }

        // The owner map the blend will use (same inputs ⇒ identical),
        // to attribute annulus pixels to single-owner zones.
        let owner = crate::seam::compute_owner_map_masked(
            &summaries,
            &graph,
            &phot,
            None,
            session.canvas,
            256.0,
            &masks,
        );

        // Annulus between the halo edge (both halos below the noise floor,
        // r ≈ 110 px) and 1.5× that radius. Background is 0.05 ± 0.002
        // (deterministic noise); tolerance 2× the noise amplitude.
        let (r_lo, r_hi) = (140.0f64, 210.0f64);
        let (bg, tol) = (0.05f32, 0.004f32);
        for mode in [BlendMode::TwoBand, BlendMode::Pyramid] {
            let params = BlendParams {
                feather_px: 256.0,
                downsample: 1,
                band_rows: 64,
                mode,
                roi: None,
                defect_veto: true,
                flatten: None,
            };
            let mut sink = MemSink::new();
            blend(&session, &phot, None, &graph, &params, &mut sink).unwrap();

            // Output coords == canvas coords (union bbox is the full canvas).
            assert_eq!((sink.w, sink.h), (640, 640));
            // Diagnostic radial profile: min per 10 px bin, r ∈ [80, 250).
            let mut bins = [f32::INFINITY; 17];
            for y in 0..640u64 {
                for x in 0..640u64 {
                    let r = ((x as f64 - 320.0).powi(2) + (y as f64 - 320.0).powi(2)).sqrt();
                    if (80.0..250.0).contains(&r) {
                        let b = ((r - 80.0) / 10.0) as usize;
                        bins[b] = bins[b].min(sink.at(0, x as usize, y as usize));
                    }
                }
            }
            eprintln!("{mode:?} radial min profile (r=80..250, 10px bins): {bins:.4?}");
            let mut mins = [f32::INFINITY; 2];
            for y in 0..640u64 {
                for x in 0..640u64 {
                    let r = ((x as f64 - 320.0).powi(2) + (y as f64 - 320.0).powi(2)).sqrt();
                    if !(r_lo..=r_hi).contains(&r) {
                        continue;
                    }
                    let o = owner.at((x / 8) as u32, (y / 8) as u32);
                    if o < 2 {
                        let v = sink.at(0, x as usize, y as usize);
                        mins[o as usize] = mins[o as usize].min(v);
                    }
                }
            }
            eprintln!(
                "{mode:?}: annulus min A-owned {:.4}, B-owned {:.4} (background {bg}, tol {tol})",
                mins[0], mins[1]
            );
            for (side, min) in ["A", "B"].iter().zip(mins) {
                assert!(
                    min.is_finite(),
                    "{mode:?}: annulus must contain {side}-owned pixels"
                );
                assert!(
                    min >= bg - tol,
                    "{mode:?}: dark moat in the {side}-owned annulus: min {min} \
                     < background {bg} − tol {tol} — the halo's base content is \
                     bleeding Laplacian lobes into the blend"
                );
            }
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unsupported_downsample() {
        let dir = tmpdir("badds");
        let (session, graph) = make_panels(&dir);
        let phot = identity_phot(2, 1);
        let params = BlendParams { feather_px: 16.0, downsample: 4, band_rows: 16, mode: BlendMode::Feather, roi: None, defect_veto: true, flatten: None };
        let mut sink = MemSink::new();
        assert!(blend(&session, &phot, None, &graph, &params, &mut sink).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
