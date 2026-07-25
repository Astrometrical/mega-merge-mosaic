//! Overlap graph + chamfer distance maps on L8 coverage grids.
//!
//! For each panel pair, the AND of *full-coverage* cells (`cov == 1.0`) in the
//! L8 grid yields a shared-cell count and bbox; pairs with at least
//! [`MIN_EDGE_CELLS`] shared cells form graph edges (smaller counts are
//! spurious corner touches). Distance maps are two-pass 8-neighbour chamfer
//! transforms (weights 1, √2) giving each cell its distance, in L8 cell units,
//! to the nearest cell with `cov < 1.0` — the basis for feather weights.

use std::path::Path;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::summary::L8Summary;
use crate::{Error, Result};

/// Minimum shared full-coverage cells for a pair to become an edge; smaller
/// overlaps are spurious corner touches.
pub const MIN_EDGE_CELLS: u64 = 16;

/// One overlapping panel pair (`a < b`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlapEdge {
    /// Lower panel id of the pair.
    pub a: usize,
    /// Higher panel id of the pair.
    pub b: usize,
    /// L8-grid bbox of the shared full-coverage cells: `[x0, y0, x1, y1]`, exclusive.
    pub bbox8: [u32; 4],
    /// Number of L8 cells fully covered by both panels.
    pub n_cells: u64,
}

/// Which panels overlap which, from the analyze stage's L8 summaries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverlapGraph {
    /// All overlapping pairs, ordered `(a, b)` with `a < b`.
    pub edges: Vec<OverlapEdge>,
}

impl OverlapGraph {
    /// Build the graph: a cell is shared ⟺ `cov == 1.0` in both panels.
    /// Pairs are prefiltered by each summary's full-coverage bbox, then only
    /// the bbox intersection is scanned. All summaries must share grid dims.
    pub fn build(summaries: &[L8Summary]) -> OverlapGraph {
        for s in summaries {
            assert_eq!(
                (s.w8, s.h8),
                (summaries[0].w8, summaries[0].h8),
                "L8 grids must share canvas dimensions"
            );
        }
        let bboxes: Vec<Option<[u32; 4]>> = summaries.par_iter().map(full_cov_bbox).collect();

        let pairs: Vec<(usize, usize)> = (0..summaries.len())
            .flat_map(|a| (a + 1..summaries.len()).map(move |b| (a, b)))
            .collect();
        let edges = pairs
            .par_iter()
            .filter_map(|&(a, b)| {
                let (ba, bb) = (bboxes[a]?, bboxes[b]?);
                let x0 = ba[0].max(bb[0]);
                let y0 = ba[1].max(bb[1]);
                let x1 = ba[2].min(bb[2]);
                let y1 = ba[3].min(bb[3]);
                if x0 >= x1 || y0 >= y1 {
                    return None;
                }
                let (sa, sb) = (&summaries[a], &summaries[b]);
                let mut n_cells = 0u64;
                let (mut ex0, mut ey0, mut ex1, mut ey1) = (u32::MAX, u32::MAX, 0, 0);
                for y in y0..y1 {
                    for x in x0..x1 {
                        if sa.cov(x, y) == 1.0 && sb.cov(x, y) == 1.0 {
                            n_cells += 1;
                            ex0 = ex0.min(x);
                            ey0 = ey0.min(y);
                            ex1 = ex1.max(x + 1);
                            ey1 = ey1.max(y + 1);
                        }
                    }
                }
                (n_cells >= MIN_EDGE_CELLS).then_some(OverlapEdge {
                    a,
                    b,
                    bbox8: [ex0, ey0, ex1, ey1],
                    n_cells,
                })
            })
            .collect();
        OverlapGraph { edges }
    }

    /// Persist as JSON (conventionally `analysis/overlap_graph.json`).
    pub fn save(&self, p: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::format(p, format!("serialize overlap graph: {e}")))?;
        std::fs::write(p, json).map_err(|e| Error::io(p, e))
    }

    /// Load a graph previously written by [`OverlapGraph::save`]. A missing
    /// file gets a hint: the analyze stage writes it.
    pub fn load(p: &Path) -> Result<OverlapGraph> {
        let json = std::fs::read_to_string(p).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::format(p, "overlap graph missing — re-run `mmm analyze`")
            } else {
                Error::io(p, e)
            }
        })?;
        serde_json::from_str(&json).map_err(|e| Error::format(p, format!("bad overlap graph: {e}")))
    }

    /// Connected components over panels `0..n_panels`; each component sorted
    /// ascending, components ordered by their smallest member. Panels without
    /// edges form singleton components.
    pub fn components(&self, n_panels: usize) -> Vec<Vec<usize>> {
        let mut parent: Vec<usize> = (0..n_panels).collect();
        fn find(parent: &mut [usize], i: usize) -> usize {
            let mut root = i;
            while parent[root] != root {
                root = parent[root];
            }
            let mut cur = i;
            while parent[cur] != root {
                let next = parent[cur];
                parent[cur] = root;
                cur = next;
            }
            root
        }
        for e in &self.edges {
            let (ra, rb) = (find(&mut parent, e.a), find(&mut parent, e.b));
            if ra != rb {
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
        let mut comps: Vec<Vec<usize>> = Vec::new();
        let mut comp_of_root = vec![usize::MAX; n_panels];
        for i in 0..n_panels {
            let root = find(&mut parent, i);
            if comp_of_root[root] == usize::MAX {
                comp_of_root[root] = comps.len();
                comps.push(Vec::new());
            }
            comps[comp_of_root[root]].push(i);
        }
        comps
    }
}

/// Bbox `[x0, y0, x1, y1]` (exclusive) of cells with `cov == 1.0`, or `None`.
fn full_cov_bbox(s: &L8Summary) -> Option<[u32; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..s.h8 {
        for x in 0..s.w8 {
            if s.cov(x, y) == 1.0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    (x1 > 0).then_some([x0, y0, x1, y1])
}

/// Two-pass 8-neighbour chamfer distance transform (weights 1, √2), in L8 cell
/// units, to the nearest cell with `cov < 1.0`. Returns `w8*h8` values; cells
/// with `cov < 1.0` are 0. If every cell is fully covered the distances are
/// `f32::INFINITY`.
pub fn distance_map(cov: &[f32], w8: u32, h8: u32) -> Vec<f32> {
    let (w, h) = (w8 as usize, h8 as usize);
    assert_eq!(cov.len(), w * h, "coverage plane size mismatch");
    const SQ2: f32 = std::f32::consts::SQRT_2;

    let mut d: Vec<f32> = cov
        .iter()
        .map(|&c| if c < 1.0 { 0.0 } else { f32::INFINITY })
        .collect();

    // Forward pass: left, up-left, up, up-right.
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                v = v.min(d[i - w] + 1.0);
                if x > 0 {
                    v = v.min(d[i - w - 1] + SQ2);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + SQ2);
                }
            }
            d[i] = v;
        }
    }
    // Backward pass: right, down-right, down, down-left.
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + 1.0);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + 1.0);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + SQ2);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + SQ2);
                }
            }
            d[i] = v;
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::L8Summary;

    /// Summary with `cov = 1.0` inside `x8 ∈ [x0,x1), y8 ∈ [y0,y1)`, 0 outside.
    fn cov_rect(w8: u32, h8: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> L8Summary {
        let mut s = L8Summary::zeroed(w8, h8, 1);
        for y in y0..y1 {
            for x in x0..x1 {
                s.coverage[(y * w8 + x) as usize] = 1.0;
            }
        }
        s
    }

    #[test]
    fn build_finds_known_overlap_with_exact_count_and_bbox() {
        // A: x∈[5,25), y∈[10,35); B: x∈[15,40), y∈[5,30)
        // → shared full-coverage region x∈[15,25), y∈[10,30) = 10×20 cells.
        let a = cov_rect(40, 40, 5, 10, 25, 35);
        let b = cov_rect(40, 40, 15, 5, 40, 30);
        let g = OverlapGraph::build(&[a, b]);
        assert_eq!(g.edges.len(), 1);
        let e = &g.edges[0];
        assert_eq!((e.a, e.b), (0, 1));
        assert_eq!(e.n_cells, 200);
        assert_eq!(e.bbox8, [15, 10, 25, 30]);
    }

    #[test]
    fn partial_coverage_cells_do_not_count_as_shared() {
        let mut a = cov_rect(40, 40, 5, 10, 25, 35);
        let b = cov_rect(40, 40, 15, 5, 40, 30);
        // Knock three cells inside the overlap down to partial coverage.
        for &(x, y) in &[(15u32, 10u32), (20, 20), (24, 29)] {
            a.coverage[(y * 40 + x) as usize] = 0.99;
        }
        let g = OverlapGraph::build(&[a, b]);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].n_cells, 197);
    }

    #[test]
    fn small_overlaps_are_dropped() {
        // 3×5 = 15 shared cells < 16 → no edge.
        let a = cov_rect(40, 40, 0, 0, 20, 20);
        let b = cov_rect(40, 40, 17, 15, 40, 40);
        assert!(OverlapGraph::build(&[a, b]).edges.is_empty());

        // 4×4 = 16 shared cells → exactly at threshold, kept.
        let a = cov_rect(40, 40, 0, 0, 20, 20);
        let b = cov_rect(40, 40, 16, 16, 40, 40);
        let g = OverlapGraph::build(&[a, b]);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].n_cells, 16);
        assert_eq!(g.edges[0].bbox8, [16, 16, 20, 20]);
    }

    #[test]
    fn disjoint_panels_produce_no_edge() {
        let a = cov_rect(40, 40, 0, 0, 15, 15);
        let b = cov_rect(40, 40, 20, 20, 40, 40);
        assert!(OverlapGraph::build(&[a, b]).edges.is_empty());
    }

    #[test]
    fn components_group_connected_panels_and_keep_singletons() {
        // Chain 0–1–2 (0 and 2 disjoint), panel 3 isolated.
        let p0 = cov_rect(60, 20, 0, 0, 20, 20);
        let p1 = cov_rect(60, 20, 15, 0, 35, 20);
        let p2 = cov_rect(60, 20, 30, 0, 50, 20);
        let p3 = cov_rect(60, 20, 55, 0, 60, 5); // touches nothing
        let g = OverlapGraph::build(&[p0, p1, p2, p3]);
        assert_eq!(g.edges.len(), 2);
        assert_eq!((g.edges[0].a, g.edges[0].b), (0, 1));
        assert_eq!((g.edges[1].a, g.edges[1].b), (1, 2));

        let comps = g.components(4);
        assert_eq!(comps, vec![vec![0, 1, 2], vec![3]]);
    }

    #[test]
    fn graph_round_trips_through_json() {
        let a = cov_rect(40, 40, 5, 10, 25, 35);
        let b = cov_rect(40, 40, 15, 5, 40, 30);
        let g = OverlapGraph::build(&[a, b]);

        let dir = std::env::temp_dir().join(format!("mmm-overlap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overlap_graph.json");
        g.save(&path).unwrap();
        let r = OverlapGraph::load(&path).unwrap();
        assert_eq!(r.edges.len(), 1);
        assert_eq!((r.edges[0].a, r.edges[0].b), (0, 1));
        assert_eq!(r.edges[0].n_cells, 200);
        assert_eq!(r.edges[0].bbox8, [15, 10, 25, 30]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn distance_map_around_a_hole() {
        // 8×8 grid fully covered except a hole at (4,3).
        let (w8, h8) = (8u32, 8u32);
        let mut cov = vec![1.0f32; 64];
        cov[3 * 8 + 4] = 0.0;
        let d = distance_map(&cov, w8, h8);
        let at = |x: usize, y: usize| d[y * 8 + x];
        let sq2 = std::f32::consts::SQRT_2;

        assert_eq!(at(4, 3), 0.0);
        // 4-neighbours of the hole: 1 (checks both pass directions).
        assert_eq!(at(3, 3), 1.0);
        assert_eq!(at(5, 3), 1.0);
        assert_eq!(at(4, 2), 1.0);
        assert_eq!(at(4, 4), 1.0);
        // Diagonal neighbours: √2.
        assert!((at(3, 2) - sq2).abs() < 1e-6);
        assert!((at(5, 4) - sq2).abs() < 1e-6);
        // Chamfer metric: |dx−dy| + √2·min(dx,dy).
        assert!((at(0, 0) - (1.0 + 3.0 * sq2)).abs() < 1e-6); // dx=4, dy=3
        assert!((at(7, 7) - (1.0 + 3.0 * sq2)).abs() < 1e-6); // dx=3, dy=4
        assert!((at(4, 7) - 4.0).abs() < 1e-6); // straight down
    }

    #[test]
    fn distance_map_boundary_cells_are_zero() {
        // Top row partially covered → distance 0 there, y elsewhere.
        let (w8, h8) = (8u32, 8u32);
        let mut cov = vec![1.0f32; 64];
        cov[..8].fill(0.5);
        let d = distance_map(&cov, w8, h8);
        for x in 0..8usize {
            assert_eq!(d[x], 0.0, "boundary cell ({x},0)");
            for y in 1..8usize {
                assert!((d[y * 8 + x] - y as f32).abs() < 1e-6, "cell ({x},{y})");
            }
        }
    }
}
