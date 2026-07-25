//! Owner map: which panel owns the detail band at each L8 cell.
//!
//! Start from the argmax feather weight (a Voronoi-like mid-overlap boundary
//! from the chamfer distance maps), then refine each overlap edge with a DP
//! min-cost seam over the L8 band: the seam runs along the band's long axis,
//! one boundary position per row/column moving at most ±1 per step, with a
//! cost that penalizes cutting through photometric disagreement, through
//! high-detail (star/structure) cells, and — hard — through cells covered by
//! the connected [`star_mask`], which protects diffraction-spike arms whose
//! individual cells fall below any per-cell threshold. The owner map is
//! shared by all channels so seams cannot introduce colour fringing.

use rayon::prelude::*;

use crate::blend::MIN_WEIGHT;
use crate::overlap::{OverlapGraph, distance_map};
use crate::photometry::Photometry;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::Surfaces;

/// Star-avoidance weight: cost of cutting next to a cell, per unit of
/// corrected detail energy (β in the design).
pub const DETAIL_BETA: f32 = 4.0;

/// Star-mask seed factor: cells whose channel-max detail exceeds this × the
/// panel's median detail seed the connected star mask ([`star_mask`]).
pub const MASK_SEED_FACTOR: f32 = 3.0;

/// Star-mask growth factor: the mask flood-fills (8-connected) from seeds
/// onto cells whose channel-max detail exceeds this × the median — covering
/// diffraction-spike arms attached to star cores, whose cells individually
/// fall below the seed threshold.
pub const MASK_GROW_FACTOR: f32 = 1.5;

/// Compact-component area bound, in L8 cells: a [`star_mask`] connected
/// component with at most this many cells is *compact* (a star core plus its
/// diffraction-spike arms fits comfortably) regardless of its bounding box —
/// spike arms make star components elongated but never large.
pub const COMPACT_MAX_AREA: usize = 40;

/// Compact-component bounding-box bound, in L8 cells: a component whose
/// bounding box does not exceed this along its longer side is compact
/// regardless of area (a dense little clump of saturated cells).
pub const COMPACT_MAX_DIM: u32 = 12;

/// Compact-component thinness bound: a component with
/// `area ≤ this × bbox max dimension` is compact regardless of size. A star
/// core with four ~1-cell-wide spike arms has area ≈ 2 × dim + core — even
/// two merged bright spiked stars measure ≈ 4.6 × dim (observed on the
/// spike-integrity fixture, area 78 over dim 17) — while a mask flooded
/// across extended structure fills its bounding box (area ≈ dim²/2 and up,
/// far above this line for any component the other two rules don't already
/// admit). Without this rule the brightest spiked stars (arms of 6+ cells)
/// exceed both bounds above and would lose the star-lock snap that protects
/// their arms from being averaged across the seam.
pub const COMPACT_THIN_RATIO: usize = 6;

/// DP seam penalty for star-masked cells: this factor × the band's median
/// cell cost, added per cell masked in either panel — large but finite, so a
/// band that is entirely structure still yields a least-bad seam.
pub const MASK_COST_FACTOR: f32 = 100.0;

/// Edges whose overlap bbox is smaller than this along *both* axes keep their
/// Voronoi labels (diagonal corner overlaps have no meaningful seam axis).
pub const DP_MIN_LONG: u32 = 64;

/// Cost assigned to boundary cells that are not fully covered by both panels:
/// the seam must stay inside the shared full-coverage band.
const UNSHARED_COST: f32 = 1e12;

/// Panel index per L8 cell; `u16::MAX` = no panel covers the cell.
pub struct OwnerMap {
    /// Grid width in L8 cells.
    pub w8: u32,
    /// Grid height in L8 cells.
    pub h8: u32,
    /// Owning panel per cell, row-major; `u16::MAX` = no coverage.
    pub owner: Vec<u16>,
}

impl OwnerMap {
    /// Owner at cell `(x8, y8)`.
    #[inline]
    pub fn at(&self, x8: u32, y8: u32) -> u16 {
        self.owner[y8 as usize * self.w8 as usize + x8 as usize]
    }
}

/// Per-panel corrected cell values and detail energies for seam costs.
struct CellCorr<'a> {
    summary: &'a L8Summary,
    /// Per-channel gain (photometric).
    gains: Vec<f32>,
    offsets: Vec<f32>,
    surf_terms: Vec<Vec<f64>>,
    canvas: (u64, u64, u64),
}

impl CellCorr<'_> {
    /// Photometrically corrected mean of channel `c` at cell `(x8, y8)`,
    /// surfaces evaluated at the cell center.
    #[inline]
    fn corrected(&self, c: usize, x8: u32, y8: u32) -> f32 {
        let v = self.summary.cell(c as u32, x8, y8) * self.gains[c] + self.offsets[c];
        let t = &self.surf_terms[c];
        if t.is_empty() {
            return v;
        }
        let xn = (x8 as f64 + 0.5) * BLOCK as f64 / self.canvas.0 as f64;
        let yn = (y8 as f64 + 0.5) * BLOCK as f64 / self.canvas.1 as f64;
        let mut s = t[0];
        if t.len() >= 3 {
            s += t[1] * xn + t[2] * yn;
        }
        if t.len() == 6 {
            s += (t[3] * xn + t[4] * yn) * xn + t[5] * yn * yn;
        }
        v + s as f32
    }

    /// Gain-corrected detail energy (channel max) at cell `(x8, y8)`.
    #[inline]
    fn detail(&self, x8: u32, y8: u32) -> f32 {
        (0..self.summary.channels as usize)
            .map(|c| self.summary.det(c as u32, x8, y8) * self.gains[c])
            .fold(0.0f32, f32::max)
    }
}

/// Per-panel L8 star/structure mask: seeds = cells with channel-max detail
/// above [`MASK_SEED_FACTOR`] × the panel's median detail (over fully
/// covered cells); grown by 8-connectivity flood fill onto cells with detail
/// above [`MASK_GROW_FACTOR`] × median. Covers diffraction-spike arms
/// connected to star cores — elongated detail a per-cell threshold alone
/// misses, which is how a seam once kinked a spike. Returns `w8*h8` flags.
pub fn star_mask(summary: &L8Summary) -> Vec<bool> {
    let (w, h) = (summary.w8 as usize, summary.h8 as usize);
    let cells = w * h;
    let nch = summary.channels as usize;
    let chdet = |i: usize| -> f32 {
        (0..nch)
            .map(|c| summary.detail[c * cells + i])
            .fold(0.0, f32::max)
    };

    let mut det_cov: Vec<f32> = (0..cells)
        .filter(|&i| summary.coverage[i] >= 1.0)
        .map(chdet)
        .collect();
    if det_cov.is_empty() {
        return vec![false; cells];
    }
    let mid = det_cov.len() / 2;
    let median = *det_cov.select_nth_unstable_by(mid, f32::total_cmp).1;

    let mut mask = vec![false; cells];
    let mut stack: Vec<usize> = Vec::new();
    for (i, m) in mask.iter_mut().enumerate() {
        if summary.coverage[i] > 0.0 && chdet(i) > MASK_SEED_FACTOR * median {
            *m = true;
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
            for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                let j = yy * w + xx;
                if !mask[j] && summary.coverage[j] > 0.0 && chdet(j) > MASK_GROW_FACTOR * median {
                    mask[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    mask
}

/// Split a connected [`star_mask`] into its *compact* part (star cores plus
/// attached diffraction-spike arms — the misregistration-sensitive point
/// sources that must be taken whole from one panel) and its *extended
/// structure* part (bright nebular signal the flood fill crossed because
/// everything connects to star seeds through it — e.g. the whole M42 core).
///
/// A connected component (8-connectivity, matching the flood fill) is compact
/// iff its area is ≤ [`COMPACT_MAX_AREA`] cells, OR its bounding box is
/// ≤ [`COMPACT_MAX_DIM`] cells along its longer side, OR it is thin —
/// area ≤ [`COMPACT_THIN_RATIO`] × its bbox max dimension, which keeps the
/// brightest spiked stars (long cross-shaped skeletons) compact while filled
/// floods stay structure. Everything else is structure. Consumers differ
/// deliberately:
/// - only the detail-transition star-lock acts on the **compact** part —
///   snapping the transition across extended structure imprints the full
///   inter-panel mismatch as a hard L8-quantized step (the user-reported
///   M42 staircase), so structure ramps instead, over the widened
///   `blend::WIDE_RAMP_PX` half-width;
/// - the DP seam cost, the defect-veto exemption, AND the base-band
///   exclusion keep using the **full** mask. The base in particular must
///   exclude structure components too: a bright star's reflection halo
///   floods its component past every compactness bound, and letting the
///   star+halo cell means into the pyramid base blends the panels'
///   disagreeing halos with level-dependent mask weights — the halo's
///   Laplacian negative lobes stop cancelling, imprinting a wide dark moat
///   around the star (a user-reported regression when base exclusion was
///   briefly compact-only; pinned by the
///   `bright_star_halo_leaves_no_dark_moat` blend test).
///
/// Returns `(compact, structure)`, each `w8*h8` flags; their union is the
/// input mask and they are disjoint.
pub fn split_mask_components(mask: &[bool], w8: u32, h8: u32) -> (Vec<bool>, Vec<bool>) {
    let (w, h) = (w8 as usize, h8 as usize);
    debug_assert_eq!(mask.len(), w * h);
    let mut compact = vec![false; mask.len()];
    let mut structure = vec![false; mask.len()];
    let mut seen = vec![false; mask.len()];
    let mut comp: Vec<usize> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        comp.clear();
        stack.push(start);
        seen[start] = true;
        let (mut x0, mut y0, mut x1, mut y1) = (start % w, start / w, start % w, start / w);
        while let Some(i) = stack.pop() {
            comp.push(i);
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let j = yy * w + xx;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        let max_dim = (x1 - x0 + 1).max(y1 - y0 + 1);
        let is_compact = comp.len() <= COMPACT_MAX_AREA
            || max_dim <= COMPACT_MAX_DIM as usize
            || comp.len() <= COMPACT_THIN_RATIO * max_dim;
        let out = if is_compact {
            &mut compact
        } else {
            &mut structure
        };
        for &i in &comp {
            out[i] = true;
        }
    }
    (compact, structure)
}

/// Compute the shared owner map: Voronoi labels from feather weights, then a
/// per-edge DP seam refinement that avoids photometric mismatch and — via
/// [`star_mask`] — connected star/spike structure.
pub fn compute_owner_map(
    summaries: &[L8Summary],
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    canvas: (u64, u64, u64),
    feather_px: f32,
) -> OwnerMap {
    let masks: Vec<Vec<bool>> = summaries.par_iter().map(star_mask).collect();
    compute_owner_map_masked(summaries, graph, phot, surfaces, canvas, feather_px, &masks)
}

/// As [`compute_owner_map`], with the per-panel star masks supplied by the
/// caller — the blender computes them once and shares them with the
/// star-lock and base-exclusion stages; tests pass all-false masks to prove
/// the mask's protection bites.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_owner_map_masked(
    summaries: &[L8Summary],
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    canvas: (u64, u64, u64),
    feather_px: f32,
    masks: &[Vec<bool>],
) -> OwnerMap {
    assert!(!summaries.is_empty(), "owner map needs at least one panel");
    let (w8, h8) = (summaries[0].w8, summaries[0].h8);
    let (w, h) = (w8 as usize, h8 as usize);
    let nch = canvas.2 as usize;

    let dists: Vec<Vec<f32>> = summaries
        .par_iter()
        .map(|s| {
            let mut d = distance_map(&s.coverage, s.w8, s.h8);
            for v in &mut d {
                if !v.is_finite() {
                    *v = 1e30;
                }
            }
            d
        })
        .collect();

    // Voronoi stage: per cell, argmax feather weight over covering panels
    // (ties broken by raw distance, then lower panel index).
    let inv_feather = 1.0 / feather_px;
    let mut owner = vec![u16::MAX; w * h];
    owner.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let mut best: Option<(f32, f32, usize)> = None;
            for (p, s) in summaries.iter().enumerate() {
                if s.coverage[i] <= 0.0 {
                    continue;
                }
                let d = dists[p][i];
                let wgt = (d * BLOCK as f32 * inv_feather)
                    .clamp(0.0, 1.0)
                    .max(MIN_WEIGHT);
                if best.is_none_or(|(bw, bd, _)| wgt > bw || (wgt == bw && d > bd)) {
                    best = Some((wgt, d, p));
                }
            }
            if let Some((_, _, p)) = best {
                *out = p as u16;
            }
        }
    });

    // Seam refinement stage: per-edge DP over the overlap band.
    let corrs: Vec<CellCorr> = summaries
        .iter()
        .enumerate()
        .map(|(p, s)| CellCorr {
            summary: s,
            gains: (0..nch)
                .map(|c| {
                    phot.gains
                        .get(c)
                        .and_then(|t| t.get(p))
                        .copied()
                        .unwrap_or(1.0) as f32
                })
                .collect(),
            offsets: (0..nch)
                .map(|c| {
                    phot.offsets
                        .get(c)
                        .and_then(|t| t.get(p))
                        .copied()
                        .unwrap_or(0.0) as f32
                })
                .collect(),
            surf_terms: (0..nch)
                .map(|c| {
                    surfaces
                        .and_then(|s| s.coeffs.get(c))
                        .and_then(|t| t.get(p))
                        .cloned()
                        .unwrap_or_default()
                })
                .collect(),
            canvas,
        })
        .collect();

    let relabels: Vec<Vec<(usize, u16)>> = graph
        .edges
        .par_iter()
        .map(|e| edge_seam(e, &corrs, &dists, masks, w8))
        .collect();
    for ops in relabels {
        for (i, p) in ops {
            owner[i] = p;
        }
    }

    OwnerMap { w8, h8, owner }
}

/// DP seam for one edge; returns `(cell index, new owner)` relabel ops.
/// Bands smaller than [`DP_MIN_LONG`] along both axes keep Voronoi labels.
fn edge_seam(
    e: &crate::overlap::OverlapEdge,
    corrs: &[CellCorr],
    dists: &[Vec<f32>],
    masks: &[Vec<bool>],
    w8: u32,
) -> Vec<(usize, u16)> {
    let [x0, y0, x1, y1] = e.bbox8;
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx < DP_MIN_LONG && dy < DP_MIN_LONG {
        return Vec::new(); // diagonal corner overlap: keep Voronoi labels
    }
    let along_x = dx >= dy;
    // Long-axis length T, short-axis cell count s_len.
    let (t_len, s_len) = if along_x {
        (dx as usize, dy as usize)
    } else {
        (dy as usize, dx as usize)
    };
    let cell_at = |t: usize, s: usize| -> (u32, u32) {
        if along_x {
            (x0 + t as u32, y0 + s as u32)
        } else {
            (x0 + s as u32, y0 + t as u32)
        }
    };

    let (ca, cb) = (&corrs[e.a], &corrs[e.b]);
    let nch = ca.summary.channels as usize;
    // Cost of a *cell*: photometric disagreement + β·detail, plus a large
    // finite penalty ([`MASK_COST_FACTOR`] × the band's median cost) on cells
    // star-masked in either panel — the seam crosses connected star/spike
    // structure only when the whole band is structure, and then takes the
    // least-bad line. Cells outside the shared full-coverage band are
    // effectively uncuttable.
    let mut cost = vec![0.0f32; t_len * s_len];
    let mut masked = vec![false; t_len * s_len];
    for t in 0..t_len {
        for s in 0..s_len {
            let (x, y) = cell_at(t, s);
            let k = t * s_len + s;
            if ca.summary.cov(x, y) < 1.0 || cb.summary.cov(x, y) < 1.0 {
                cost[k] = UNSHARED_COST;
                continue;
            }
            let diff = (0..nch)
                .map(|c| (ca.corrected(c, x, y) - cb.corrected(c, x, y)).abs())
                .fold(0.0f32, f32::max);
            let det = ca.detail(x, y).max(cb.detail(x, y));
            cost[k] = diff + DETAIL_BETA * det;
            let i = y as usize * w8 as usize + x as usize;
            masked[k] = masks[e.a][i] || masks[e.b][i];
        }
    }
    let mut shared: Vec<f32> = cost
        .iter()
        .copied()
        .filter(|&c| c < UNSHARED_COST)
        .collect();
    if !shared.is_empty() {
        let mid = shared.len() / 2;
        let median = *shared.select_nth_unstable_by(mid, f32::total_cmp).1;
        let penalty = MASK_COST_FACTOR * median;
        for (c, &m) in cost.iter_mut().zip(&masked) {
            if m && *c < UNSHARED_COST {
                *c += penalty;
            }
        }
    }
    let cell_cost = |t: usize, s: usize| -> f32 { cost[t * s_len + s] };

    // Boundary states s ∈ 0..=s_len: the seam runs between cells s−1 and s
    // (s = 0 or s_len give the whole band to one side). Cutting is charged
    // the cost of both adjacent cells so the seam cannot hug a star.
    let n_states = s_len + 1;
    let bcost = |t: usize, s: usize| -> f32 {
        let mut c = 0.0;
        if s > 0 {
            c += cell_cost(t, s - 1);
        }
        if s < s_len {
            c += cell_cost(t, s);
        }
        c
    };

    let mut dp = vec![0.0f32; t_len * n_states];
    let mut from = vec![0u16; t_len * n_states];
    for (s, d) in dp[..n_states].iter_mut().enumerate() {
        *d = bcost(0, s);
    }
    for t in 1..t_len {
        for s in 0..n_states {
            let (mut best, mut arg) = (dp[(t - 1) * n_states + s], s);
            if s > 0 && dp[(t - 1) * n_states + s - 1] < best {
                best = dp[(t - 1) * n_states + s - 1];
                arg = s - 1;
            }
            if s + 1 < n_states && dp[(t - 1) * n_states + s + 1] < best {
                best = dp[(t - 1) * n_states + s + 1];
                arg = s + 1;
            }
            dp[t * n_states + s] = best + bcost(t, s);
            from[t * n_states + s] = arg as u16;
        }
    }
    // Backtrack the min-cost path.
    let mut path = vec![0usize; t_len];
    let last = (0..n_states)
        .min_by(|&a, &b| dp[(t_len - 1) * n_states + a].total_cmp(&dp[(t_len - 1) * n_states + b]))
        .unwrap();
    path[t_len - 1] = last;
    for t in (1..t_len).rev() {
        path[t - 1] = from[t * n_states + path[t]] as usize;
    }

    // Which panel owns the low-s side: the panel deeper in coverage (larger
    // chamfer distance) at the band's low edge.
    let mut low_score = 0.0f64;
    for t in 0..t_len {
        let (x, y) = cell_at(t, 0);
        let i = y as usize * w8 as usize + x as usize;
        low_score += (dists[e.a][i] - dists[e.b][i]) as f64;
    }
    let (low, high) = if low_score >= 0.0 {
        (e.a as u16, e.b as u16)
    } else {
        (e.b as u16, e.a as u16)
    };

    // Relabel shared full-coverage cells on each side of the seam.
    let mut ops = Vec::new();
    for (t, &b) in path.iter().enumerate() {
        for s in 0..s_len {
            let (x, y) = cell_at(t, s);
            if ca.summary.cov(x, y) < 1.0 || cb.summary.cov(x, y) < 1.0 {
                continue;
            }
            let i = y as usize * w8 as usize + x as usize;
            ops.push((i, if s < b { low } else { high }));
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlap::OverlapGraph;

    /// Two single-channel panels on a 32×80-cell grid (canvas 256×640):
    /// A covers x8 ∈ [0,20), B covers x8 ∈ [12,32), overlap band [12,20)
    /// spanning the full height (80 ≥ DP_MIN_LONG along y).
    fn band_summaries(star: Option<(u32, u32)>) -> Vec<L8Summary> {
        let (w8, h8) = (32u32, 80u32);
        let mk = |x_lo: u32, x_hi: u32| -> L8Summary {
            let mut s = L8Summary::zeroed(w8, h8, 1);
            for y in 0..h8 {
                for x in x_lo..x_hi {
                    let i = (y * w8 + x) as usize;
                    s.coverage[i] = 1.0;
                    s.mean[i] = 0.1;
                    s.detail[i] = 0.001;
                }
            }
            if let Some((sx, sy)) = star {
                // A bright star: high detail energy in a 2-cell radius.
                for y in sy.saturating_sub(2)..=(sy + 2).min(h8 - 1) {
                    for x in sx.saturating_sub(2).max(x_lo)..=(sx + 2).min(x_hi - 1) {
                        s.detail[(y * w8 + x) as usize] = 0.5;
                    }
                }
            }
            s
        };
        vec![mk(0, 20), mk(12, 32)]
    }

    fn identity_phot(n: usize) -> Photometry {
        Photometry {
            edge_fits: vec![],
            gains: vec![vec![1.0; n]],
            offsets: vec![vec![0.0; n]],
        }
    }

    /// All 4-neighbour owner boundary pairs `((x,y),(x2,y2))`.
    fn boundary_pairs(map: &OwnerMap) -> Vec<((u32, u32), (u32, u32))> {
        let mut pairs = Vec::new();
        for y in 0..map.h8 {
            for x in 0..map.w8 {
                let o = map.at(x, y);
                if o == u16::MAX {
                    continue;
                }
                if x + 1 < map.w8 && map.at(x + 1, y) != u16::MAX && map.at(x + 1, y) != o {
                    pairs.push(((x, y), (x + 1, y)));
                }
                if y + 1 < map.h8 && map.at(x, y + 1) != u16::MAX && map.at(x, y + 1) != o {
                    pairs.push(((x, y), (x, y + 1)));
                }
            }
        }
        pairs
    }

    #[test]
    fn voronoi_owner_splits_band_at_midline() {
        let summaries = band_summaries(None);
        let graph = OverlapGraph::build(&summaries);
        assert_eq!(graph.edges.len(), 1);
        let map = compute_owner_map(
            &summaries,
            &OverlapGraph::default(), // no edges: pure Voronoi
            &identity_phot(2),
            None,
            (256, 640, 1),
            256.0,
        );
        // A-only and B-only cells keep their panels; uncovered cells have none.
        assert_eq!(map.at(5, 40), 0);
        assert_eq!(map.at(25, 40), 1);
        // Mid-band boundary: equidistant at x ≈ 15.5.
        assert_eq!(map.at(14, 40), 0);
        assert_eq!(map.at(17, 40), 1);
    }

    /// Mandatory phase-3E test 1 (mask connectivity): a star core (seed) with
    /// four spike arms — one diagonal, to exercise 8-connectivity — whose
    /// cells sit between the grow and seed thresholds. The connected mask
    /// must cover core and full arms (≥ 2× the extent of a seed-only mask);
    /// background cells stay unmasked.
    #[test]
    fn star_mask_grows_over_connected_spike_arms() {
        let (w8, h8) = (48u32, 48u32);
        let mut s = L8Summary::zeroed(w8, h8, 1);
        for i in 0..(48 * 48) {
            s.coverage[i] = 1.0;
            s.detail[i] = 0.01; // background detail; median = 0.01
        }
        let idx = |x: i64, y: i64| (y * 48 + x) as usize;
        let (cx, cy) = (24i64, 24i64);
        s.detail[idx(cx, cy)] = 0.2; // core: 20× median → seed
        let arm = 8i64;
        for t in 1..=arm {
            // 2× median: below the 3× seed threshold, above the 1.5× grow one.
            s.detail[idx(cx + t, cy)] = 0.02; // +x arm
            s.detail[idx(cx - t, cy)] = 0.02; // −x arm
            s.detail[idx(cx, cy + t)] = 0.02; // +y arm
            s.detail[idx(cx + t, cy - t)] = 0.02; // diagonal arm (8-conn only)
        }

        let mask = star_mask(&s);
        // Seed-only mask (what the raw 3× threshold would protect): core only.
        let seed_only: Vec<bool> = s
            .detail
            .iter()
            .map(|&d| d > MASK_SEED_FACTOR * 0.01)
            .collect();
        assert_eq!(
            seed_only.iter().filter(|&&m| m).count(),
            1,
            "only the core seeds"
        );

        // The connected mask covers core and every arm cell — and nothing else.
        assert!(mask[idx(cx, cy)], "core must be masked");
        for t in 1..=arm {
            for (x, y) in [(cx + t, cy), (cx - t, cy), (cx, cy + t), (cx + t, cy - t)] {
                assert!(mask[idx(x, y)], "arm cell ({x},{y}) must be masked");
            }
        }
        assert_eq!(
            mask.iter().filter(|&&m| m).count(),
            1 + 4 * arm as usize,
            "background cells must stay unmasked"
        );

        // Extent along the +x arm: 1 + arm cells vs the seed-only single cell.
        let extent = |m: &[bool]| (0..=arm).take_while(|&t| m[idx(cx + t, cy)]).count();
        assert!(
            extent(&mask) >= 2 * extent(&seed_only),
            "mask arm extent {} must be ≥ 2× seed-only extent {}",
            extent(&mask),
            extent(&seed_only)
        );
    }

    /// Two panels as in [`band_summaries`], plus a spike: a seed core at
    /// (12, 40) with an arm along +x to (17, 40) whose detail (2× median)
    /// is below the seed threshold but inside the mask's growth range, and a
    /// mildly mismatched background corridor at x ∈ {18, 19} (photometric
    /// diff 0.005) — cheap enough that only the mask penalty pushes the seam
    /// into it.
    fn spike_summaries() -> Vec<L8Summary> {
        let mut ss = band_summaries(None);
        for s in &mut ss {
            s.detail[(40 * 32 + 12) as usize] = 0.5; // core seed
            for x in 13..18u32 {
                s.detail[(40 * 32 + x) as usize] = 0.002; // arm: grow range
            }
        }
        for y in 0..80u32 {
            for x in 18..20u32 {
                ss[1].mean[(y * 32 + x) as usize] = 0.105; // corridor diff
            }
        }
        ss
    }

    /// Mandatory phase-3E test 2: the owner boundary never crosses masked
    /// cells when a clear background corridor exists — and, teeth: with the
    /// masks disabled the mildly cheaper straight seam does cross the arm.
    #[test]
    fn seam_avoids_masked_spike_arm_via_background_corridor() {
        let summaries = spike_summaries();
        let graph = OverlapGraph::build(&summaries);
        assert_eq!(graph.edges.len(), 1);
        let masks: Vec<Vec<bool>> = summaries.iter().map(star_mask).collect();
        // Sanity: core + arm are masked in both panels, corridor is not.
        for x in 12..18usize {
            assert!(
                masks[0][40 * 32 + x] && masks[1][40 * 32 + x],
                "arm cell x={x}"
            );
        }
        for x in 18..20usize {
            assert!(
                !masks[0][40 * 32 + x] && !masks[1][40 * 32 + x],
                "corridor x={x}"
            );
        }

        let map = compute_owner_map(
            &summaries,
            &graph,
            &identity_phot(2),
            None,
            (256, 640, 1),
            256.0,
        );
        for ((x, y), (x2, y2)) in boundary_pairs(&map) {
            for (bx, by) in [(x, y), (x2, y2)] {
                let i = by as usize * 32 + bx as usize;
                assert!(
                    !masks[0][i] && !masks[1][i],
                    "owner boundary crosses masked cell ({bx},{by})"
                );
            }
        }

        // Teeth: all-false masks (mask disabled) → the seam cuts the arm.
        let no_masks: Vec<Vec<bool>> = summaries
            .iter()
            .map(|s| vec![false; (s.w8 * s.h8) as usize])
            .collect();
        let unmasked = compute_owner_map_masked(
            &summaries,
            &graph,
            &identity_phot(2),
            None,
            (256, 640, 1),
            256.0,
            &no_masks,
        );
        let crossed = boundary_pairs(&unmasked).iter().any(|&((x, y), (x2, y2))| {
            [(x, y), (x2, y2)]
                .iter()
                .any(|&(bx, by)| masks[0][by as usize * 32 + bx as usize])
        });
        assert!(
            crossed,
            "without the mask the cheap straight seam must cross the arm — \
             otherwise this test has no teeth"
        );
    }

    /// Mandatory M42-staircase-fix test 1 (component classification): a
    /// 3-cell star, a star with four spike arms (~20 cells, bbox 11×11) and a
    /// 200-cell blob (10×20 — over both compactness bounds) on one mask →
    /// the first two classify compact, the blob classifies structure; the
    /// two parts partition the mask exactly.
    #[test]
    fn mask_components_split_by_compactness() {
        let (w8, h8) = (64u32, 64u32);
        let mut mask = vec![false; (w8 * h8) as usize];
        let idx = |x: u32, y: u32| (y * w8 + x) as usize;

        // 3-cell star at (5,5)..(7,5).
        for x in 5..8 {
            mask[idx(x, 5)] = true;
        }
        // Star + 4 spike arms at (30,10): core + arms of 5 cells each
        // (one arm diagonal — 8-connectivity), 21 cells, bbox 11×11.
        mask[idx(30, 10)] = true;
        for t in 1..=5u32 {
            mask[idx(30 + t, 10)] = true;
            mask[idx(30 - t, 10)] = true;
            mask[idx(30, 10 + t)] = true;
            mask[idx(30 + t, 10 - t)] = true;
        }
        // 200-cell blob: 10 wide × 20 tall at (10,30).
        for y in 30..50 {
            for x in 10..20 {
                mask[idx(x, y)] = true;
            }
        }
        // Very bright spiked star: 3×3 core at (46,45) + 4 straight arms of
        // 14 cells — area 65 over a 29-cell bbox, past both the area and dim
        // bounds; only the thinness rule (65 ≤ 6×29) keeps it compact.
        for y in 44..47u32 {
            for x in 45..48u32 {
                mask[idx(x, y)] = true;
            }
        }
        for t in 2..=15u32 {
            mask[idx(46 + t, 45)] = true;
            mask[idx(46 - t, 45)] = true;
            mask[idx(46, 45 + t)] = true;
            mask[idx(46, 45 - t)] = true;
        }

        let (compact, structure) = split_mask_components(&mask, w8, h8);
        for x in 5..8u32 {
            assert!(compact[idx(x, 5)], "3-cell star must be compact");
        }
        assert!(compact[idx(30, 10)], "spiked star core must be compact");
        for t in 1..=5u32 {
            for (x, y) in [(30 + t, 10), (30 - t, 10), (30, 10 + t), (30 + t, 10 - t)] {
                assert!(
                    compact[idx(x, y)],
                    "spike arm cell ({x},{y}) must be compact"
                );
            }
        }
        for y in 30..50u32 {
            for x in 10..20u32 {
                assert!(
                    structure[idx(x, y)],
                    "blob cell ({x},{y}) must be structure"
                );
                assert!(
                    !compact[idx(x, y)],
                    "blob cell ({x},{y}) must not be compact"
                );
            }
        }
        assert!(
            compact[idx(46, 45)] && compact[idx(61, 45)] && compact[idx(46, 60)],
            "long-armed spiked star must be compact via the thinness rule"
        );
        // Partition: compact ∪ structure = mask, compact ∩ structure = ∅.
        for i in 0..mask.len() {
            assert_eq!(
                compact[i] || structure[i],
                mask[i],
                "union must equal the mask"
            );
            assert!(!(compact[i] && structure[i]), "parts must be disjoint");
        }
    }

    /// Mandatory test 1: with a bright star mid-band, the seam must route
    /// around it — no owner boundary within 2 cells of the star.
    #[test]
    fn seam_routes_around_bright_star() {
        let star = (16u32, 40u32);
        let summaries = band_summaries(Some(star));
        let graph = OverlapGraph::build(&summaries);
        let map = compute_owner_map(
            &summaries,
            &graph,
            &identity_phot(2),
            None,
            (256, 640, 1),
            256.0,
        );

        // Every covered cell still has an owner from the overlapping pair.
        for y in 0..80 {
            for x in 12..20 {
                assert!(
                    map.at(x, y) < 2,
                    "band cell ({x},{y}) must stay owned by a/b"
                );
            }
        }

        // The seam still separates the panels somewhere in the band…
        let pairs = boundary_pairs(&map);
        assert!(
            !pairs.is_empty(),
            "owner boundary must exist in the overlap band"
        );

        // …but never within 2 cells (Chebyshev) of the star.
        for ((x, y), (x2, y2)) in pairs {
            for (bx, by) in [(x, y), (x2, y2)] {
                let d = (bx.abs_diff(star.0)).max(by.abs_diff(star.1));
                assert!(
                    d > 2,
                    "owner boundary cell ({bx},{by}) is within 2 cells of the star at {star:?}"
                );
            }
        }
    }
}
