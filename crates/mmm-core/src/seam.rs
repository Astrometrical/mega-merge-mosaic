//! Owner map: which panel owns the detail band at each L8 cell.
//!
//! Start from the argmax feather weight (a Voronoi-like mid-overlap boundary
//! from the chamfer distance maps), then refine each overlap edge with a DP
//! min-cost seam over the L8 band: the seam runs along the band's long axis,
//! one boundary position per row/column moving at most ±1 per step, with a
//! cost that penalizes cutting through photometric disagreement and through
//! high-detail (star/structure) cells. The owner map is shared by all
//! channels so seams cannot introduce colour fringing.

use rayon::prelude::*;

use crate::blend::MIN_WEIGHT;
use crate::overlap::{OverlapGraph, distance_map};
use crate::photometry::Photometry;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::Surfaces;

/// Star-avoidance weight: cost of cutting next to a cell, per unit of
/// corrected detail energy (β in the design).
pub const DETAIL_BETA: f32 = 4.0;

/// Edges whose overlap bbox is smaller than this along *both* axes keep their
/// Voronoi labels (diagonal corner overlaps have no meaningful seam axis).
pub const DP_MIN_LONG: u32 = 64;

/// Cost assigned to boundary cells that are not fully covered by both panels:
/// the seam must stay inside the shared full-coverage band.
const UNSHARED_COST: f32 = 1e12;

/// Panel index per L8 cell; `u16::MAX` = no panel covers the cell.
pub struct OwnerMap {
    pub w8: u32,
    pub h8: u32,
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

/// Compute the shared owner map: Voronoi labels from feather weights, then a
/// per-edge DP seam refinement that avoids stars and photometric mismatch.
pub fn compute_owner_map(
    summaries: &[L8Summary],
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    canvas: (u64, u64, u64),
    feather_px: f32,
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
                let wgt = (d * BLOCK as f32 * inv_feather).clamp(0.0, 1.0).max(MIN_WEIGHT);
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
                .map(|c| phot.gains.get(c).and_then(|t| t.get(p)).copied().unwrap_or(1.0) as f32)
                .collect(),
            offsets: (0..nch)
                .map(|c| phot.offsets.get(c).and_then(|t| t.get(p)).copied().unwrap_or(0.0) as f32)
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
        .map(|e| edge_seam(e, &corrs, &dists, w8))
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
    w8: u32,
) -> Vec<(usize, u16)> {
    let [x0, y0, x1, y1] = e.bbox8;
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx < DP_MIN_LONG && dy < DP_MIN_LONG {
        return Vec::new(); // diagonal corner overlap: keep Voronoi labels
    }
    let along_x = dx >= dy;
    // Long-axis length T, short-axis cell count s_len.
    let (t_len, s_len) = if along_x { (dx as usize, dy as usize) } else { (dy as usize, dx as usize) };
    let cell_at = |t: usize, s: usize| -> (u32, u32) {
        if along_x { (x0 + t as u32, y0 + s as u32) } else { (x0 + s as u32, y0 + t as u32) }
    };

    let (ca, cb) = (&corrs[e.a], &corrs[e.b]);
    let nch = ca.summary.channels as usize;
    // Cost of a *cell*: photometric disagreement + β·detail (star avoidance);
    // cells outside the shared full-coverage band are effectively uncuttable.
    let cell_cost = |t: usize, s: usize| -> f32 {
        let (x, y) = cell_at(t, s);
        if ca.summary.cov(x, y) < 1.0 || cb.summary.cov(x, y) < 1.0 {
            return UNSHARED_COST;
        }
        let diff = (0..nch)
            .map(|c| (ca.corrected(c, x, y) - cb.corrected(c, x, y)).abs())
            .fold(0.0f32, f32::max);
        let det = ca.detail(x, y).max(cb.detail(x, y));
        diff + DETAIL_BETA * det
    };

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
    let (low, high) =
        if low_score >= 0.0 { (e.a as u16, e.b as u16) } else { (e.b as u16, e.a as u16) };

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
                assert!(map.at(x, y) < 2, "band cell ({x},{y}) must stay owned by a/b");
            }
        }

        // The seam still separates the panels somewhere in the band…
        let pairs = boundary_pairs(&map);
        assert!(!pairs.is_empty(), "owner boundary must exist in the overlap band");

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
