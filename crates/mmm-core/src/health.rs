//! Mosaic health metrics as plain data, for a UI to display.
//!
//! Wraps the seam diagnostics of [`crate::diag`] with the same outlier rule
//! `mmm report` prints (a seam step more than 3× the median is flagged) and
//! the spread of the photometric corrections across panels; and draws the
//! seams themselves as [`SeamLine`]s, the owner map's boundaries in canvas
//! px ([`seam_lines`], [`health_with_lines`]).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::diag::{SeamDelta, load_owner_map, median, seam_deltas};
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::seam::OwnerMap;
use crate::session::Session;
use crate::summary::{BLOCK, L8Summary};
use crate::surfaces::Surfaces;

/// One overlap edge's corrected seam step.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeamStep {
    /// Lower panel index of the edge.
    pub a: usize,
    /// Higher panel index of the edge.
    pub b: usize,
    /// Mean over boundary cells of the channel-max corrected step.
    pub delta: f64,
    /// Boundary cells measured.
    pub cells: usize,
    /// Whether `delta` exceeds 3× the median step (the `mmm report` rule).
    pub flagged: bool,
}

/// Mosaic health: seam steps and the spread of photometric corrections.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Health {
    /// Measured seam steps, ordered by `(a, b)`.
    pub seams: Vec<SeamStep>,
    /// Median seam step; `0` when there are no measurable edges. Equals
    /// `MosaicSummary::seam_delta_median`: `seam_deltas` only creates an
    /// entry once a cell is measured (`n >= 1`), so dropping `n == 0` edges
    /// here never changes the set the summary takes its median over.
    pub seam_delta_median: f64,
    /// Largest seam step; `0` when there are none.
    pub seam_delta_max: f64,
    /// `(min, max)` panel gain per channel.
    pub gain_range: Vec<(f64, f64)>,
    /// `(min, max)` panel offset per channel.
    pub offset_range: Vec<(f64, f64)>,
}

/// Turn raw per-edge deltas into ordered, flagged steps. Edges with no
/// measured cells are omitted. The flag threshold is 3× the upper median
/// (element `len / 2` of the sorted deltas), exactly as `mmm report` does,
/// and requires a positive median.
pub fn flag_steps(deltas: &HashMap<(usize, usize), SeamDelta>) -> Vec<SeamStep> {
    let mut v: Vec<f64> = deltas
        .values()
        .filter(|d| d.n > 0)
        .map(|d| d.delta)
        .collect();
    v.sort_by(f64::total_cmp);
    let med = if v.is_empty() { 0.0 } else { v[v.len() / 2] };
    let mut steps: Vec<SeamStep> = deltas
        .iter()
        .filter(|(_, d)| d.n > 0)
        .map(|(&(a, b), d)| SeamStep {
            a,
            b,
            delta: d.delta,
            cells: d.n,
            flagged: med > 0.0 && d.delta > 3.0 * med,
        })
        .collect();
    steps.sort_by_key(|s| (s.a, s.b));
    steps
}

fn ranges(per_channel: &[Vec<f64>]) -> Vec<(f64, f64)> {
    per_channel
        .iter()
        .map(|v| {
            v.iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| {
                    (lo.min(x), hi.max(x))
                })
        })
        .map(|(lo, hi)| if lo > hi { (0.0, 0.0) } else { (lo, hi) })
        .collect()
}

/// Health measured on an already loaded owner map and its summaries.
fn measure(
    session: &Session,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    summaries: &[L8Summary],
    owner: &OwnerMap,
) -> Health {
    let deltas = seam_deltas(summaries, owner, phot, surfaces, session.canvas);
    let seams = flag_steps(&deltas);
    let values: Vec<f64> = seams.iter().map(|s| s.delta).collect();
    Health {
        seam_delta_max: values.iter().copied().fold(0.0, f64::max),
        seam_delta_median: median(values),
        seams,
        gain_range: ranges(&phot.gains),
        offset_range: ranges(&phot.offsets),
    }
}

/// Compute the mosaic's health from an analysed session. `feather_px` should
/// match the blend's.
pub fn health(
    session: &Session,
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    feather_px: f32,
) -> Result<Health> {
    let (summaries, owner) = load_owner_map(session, graph, phot, surfaces, feather_px)?;
    Ok(measure(session, phot, surfaces, &summaries, &owner))
}

/// [`health`] and the [`seam_lines`] it flags, from one owner-map pass
/// (the summaries are loaded and the owner map computed once for both).
/// `feather_px` should match the blend's.
pub fn health_with_lines(
    session: &Session,
    graph: &OverlapGraph,
    phot: &Photometry,
    surfaces: Option<&Surfaces>,
    feather_px: f32,
) -> Result<(Health, Vec<SeamLine>)> {
    let (summaries, owner) = load_owner_map(session, graph, phot, surfaces, feather_px)?;
    let h = measure(session, phot, surfaces, &summaries, &owner);
    let lines = seam_lines(&owner, &h);
    Ok((h, lines))
}

/// Where the merge changes from one panel to another: one connected run of
/// the owner boundary between panels `a` and `b`, as a polyline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SeamLine {
    /// Lower panel index of the pair (session panel id).
    pub a: u16,
    /// Higher panel index of the pair.
    pub b: u16,
    /// The polyline's vertices in canvas px: L8 cell corners × [`BLOCK`],
    /// straight runs merged into one segment. An open line starts at its
    /// lesser end; a closed one repeats its first point last.
    pub points: Vec<[f64; 2]>,
    /// Whether the pair's seam step is flagged ([`SeamStep::flagged`]);
    /// `false` for a pair with no measured step.
    pub flagged: bool,
}

/// An L8 cell corner `(x, y)`.
type Corner = (u32, u32);

/// Whether `b` lies on the straight line through `a` and `c`.
fn collinear(a: Corner, b: Corner, c: Corner) -> bool {
    let d = |p: Corner, q: Corner| (q.0 as i64 - p.0 as i64, q.1 as i64 - p.1 as i64);
    let ((ux, uy), (vx, vy)) = (d(a, b), d(b, c));
    ux * vy == uy * vx
}

/// Drop the interior points of straight runs (and a closed line's start,
/// when it falls mid-run).
fn simplify(points: Vec<Corner>) -> Vec<Corner> {
    let mut out: Vec<Corner> = Vec::with_capacity(points.len());
    for p in points {
        if out.len() >= 2 && collinear(out[out.len() - 2], out[out.len() - 1], p) {
            out.pop();
        }
        out.push(p);
    }
    let n = out.len();
    if n >= 4 && out[0] == out[n - 1] && collinear(out[n - 2], out[0], out[1]) {
        out.pop();
        out.remove(0);
        out.push(out[0]);
    }
    out
}

/// Join unit boundary segments end to end into polylines: open runs are
/// walked from their ends (odd-degree corners), the remaining loops from
/// anywhere. Each segment is used once.
fn chain(segments: &[(Corner, Corner)]) -> Vec<Vec<Corner>> {
    let mut at: HashMap<Corner, Vec<usize>> = HashMap::new();
    for (i, &(p, q)) in segments.iter().enumerate() {
        at.entry(p).or_default().push(i);
        at.entry(q).or_default().push(i);
    }
    let mut ends: Vec<Corner> = at
        .iter()
        .filter(|(_, v)| v.len() % 2 == 1)
        .map(|(&c, _)| c)
        .collect();
    ends.sort_unstable();
    let starts = ends.into_iter().chain(segments.iter().map(|s| s.0));
    let mut used = vec![false; segments.len()];
    let mut lines = Vec::new();
    for start in starts {
        loop {
            let mut cur = start;
            let mut points = vec![start];
            while let Some(&i) = at[&cur].iter().find(|&&i| !used[i]) {
                used[i] = true;
                let (p, q) = segments[i];
                cur = if p == cur { q } else { p };
                points.push(cur);
            }
            if points.len() < 2 {
                break;
            }
            let mut points = simplify(points);
            if points.last() < points.first() {
                points.reverse();
            }
            lines.push(points);
        }
    }
    lines.sort();
    lines
}

/// The seam lines of an owner map: for each pair of panels owning
/// neighbouring L8 cells, the cell edges between them joined end to end
/// into polylines in canvas px (a pair meeting in several places has
/// several lines), each carrying its pair's flag from `health`. Edges
/// against unowned cells (panel rims) are not seams. Ordered by pair, then
/// by first point. Works on the L8 owner grid, never at full resolution.
pub fn seam_lines(owner: &OwnerMap, health: &Health) -> Vec<SeamLine> {
    let (w, h) = (owner.w8, owner.h8);
    let mut segments: BTreeMap<(u16, u16), Vec<(Corner, Corner)>> = BTreeMap::new();
    for y in 0..h {
        for x in 0..w {
            let o = owner.at(x, y);
            if o == u16::MAX {
                continue;
            }
            let mut edge = |n: u16, seg: (Corner, Corner)| {
                if n != u16::MAX && n != o {
                    segments.entry((o.min(n), o.max(n))).or_default().push(seg);
                }
            };
            if x + 1 < w {
                edge(owner.at(x + 1, y), ((x + 1, y), (x + 1, y + 1)));
            }
            if y + 1 < h {
                edge(owner.at(x, y + 1), ((x, y + 1), (x + 1, y + 1)));
            }
        }
    }
    let px = |c: Corner| [c.0 as f64 * BLOCK as f64, c.1 as f64 * BLOCK as f64];
    let mut out = Vec::new();
    for ((a, b), segs) in segments {
        let flagged = health
            .seams
            .iter()
            .any(|s| (s.a, s.b) == (a as usize, b as usize) && s.flagged);
        for points in chain(&segs) {
            out.push(SeamLine {
                a,
                b,
                points: points.into_iter().map(px).collect(),
                flagged,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze;
    use crate::diag::write_session_reports;
    use crate::synth::write_xisf;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-health-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Three overlapping panels in a row (levels 0.2 / 0.4 / 0.3), analysed.
    fn analysed(tag: &str) -> (Session, OverlapGraph) {
        let dir = tmpdir(tag);
        let (w, h) = (160u64, 48u64);
        let mut paths = Vec::new();
        for (i, (x0, x1, v)) in [(8u64, 64u64, 0.2f32), (48, 112, 0.4), (96, 152, 0.3)]
            .into_iter()
            .enumerate()
        {
            let mut frame = vec![0f32; (w * h) as usize];
            for y in 8..40 {
                for x in x0..x1 {
                    frame[(y * w + x) as usize] = v;
                }
            }
            let p = dir.join(format!("p{i}.xisf"));
            write_xisf(&p, w, h, 1, &frame).unwrap();
            paths.push(p);
        }
        let session = analyze(&paths, &dir.join("s.mmm-session")).unwrap();
        let graph = OverlapGraph::load(&session.overlap_graph_path()).unwrap();
        (session, graph)
    }

    fn phot(gains: Vec<f64>, offsets: Vec<f64>) -> Photometry {
        Photometry {
            edge_fits: vec![],
            gains: vec![gains],
            offsets: vec![offsets],
        }
    }

    #[test]
    fn health_matches_report_rule() {
        let (session, graph) = analysed("rule");
        let ph = phot(vec![1.0; 3], vec![0.0; 3]);
        let h = health(&session, &graph, &ph, None, 16.0).unwrap();
        let s = write_session_reports(&session, &graph, &ph, None, 16.0, false).unwrap();
        assert!(!h.seams.is_empty());
        assert_eq!(h.seam_delta_median, s.seam_delta_median);
        assert_eq!(h.seam_delta_max, s.seam_delta_max);
        assert!(
            h.seams
                .windows(2)
                .all(|w| (w[0].a, w[0].b) < (w[1].a, w[1].b))
        );
    }

    #[test]
    fn gain_range_spans_panels() {
        let (session, graph) = analysed("range");
        let gains = vec![0.9, 1.1, 1.0];
        let offsets = vec![-0.01, 0.0, 0.02];
        let ph = phot(gains.clone(), offsets.clone());
        let h = health(&session, &graph, &ph, None, 16.0).unwrap();
        assert_eq!(h.gain_range.len(), 1);
        let (lo, hi) = h.gain_range[0];
        assert!(gains.iter().all(|g| lo <= *g && *g <= hi));
        assert_eq!((lo, hi), (0.9, 1.1));
        assert_eq!(h.offset_range[0], (-0.01, 0.02));
    }

    /// A line's points in a canonical direction (first point the smaller),
    /// so tests need not care which end a walk started from.
    fn canonical(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
        let mut p = points.to_vec();
        if p.last() < p.first() {
            p.reverse();
        }
        p
    }

    fn step(a: usize, b: usize, flagged: bool) -> SeamStep {
        SeamStep {
            a,
            b,
            delta: 0.0,
            cells: 1,
            flagged,
        }
    }

    fn health_with(seams: Vec<SeamStep>) -> Health {
        Health {
            seams,
            seam_delta_median: 0.0,
            seam_delta_max: 0.0,
            gain_range: vec![],
            offset_range: vec![],
        }
    }

    /// Seam lines run along the cell edges where ownership changes between
    /// two panels, joined into polylines in canvas px (cells × 8), straight
    /// runs merged, each carrying its seam's flag. Panel rims (an owned
    /// cell beside an unowned one) are not seams.
    #[test]
    fn seam_lines_follow_the_owner_boundary() {
        //   0 0 0 1 1 1
        //   0 0 0 1 1 1
        //   0 0 2 2 1 1
        //   . 0 2 2 1 1
        const M: u16 = u16::MAX;
        let owner = OwnerMap {
            w8: 6,
            h8: 4,
            owner: vec![
                0, 0, 0, 1, 1, 1, //
                0, 0, 0, 1, 1, 1, //
                0, 0, 2, 2, 1, 1, //
                M, 0, 2, 2, 1, 1,
            ],
        };
        // (0, 1) flagged; (0, 2) measured clean; (1, 2) never measured.
        let h = health_with(vec![step(0, 1, true), step(0, 2, false)]);
        let lines = seam_lines(&owner, &h);
        let got: Vec<(u16, u16, bool, Vec<[f64; 2]>)> = lines
            .iter()
            .map(|l| (l.a, l.b, l.flagged, canonical(&l.points)))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 1, true, vec![[24.0, 0.0], [24.0, 16.0]]),
                (0, 2, false, vec![[16.0, 32.0], [16.0, 16.0], [24.0, 16.0]]),
                (1, 2, false, vec![[24.0, 16.0], [32.0, 16.0], [32.0, 32.0]]),
            ]
        );

        // A pair meeting in two places has two lines.
        let owner = OwnerMap {
            w8: 3,
            h8: 2,
            owner: vec![0, 1, 0, 0, 1, 0],
        };
        let lines = seam_lines(&owner, &health_with(vec![]));
        let got: Vec<Vec<[f64; 2]>> = lines.iter().map(|l| canonical(&l.points)).collect();
        assert_eq!(
            got,
            vec![
                vec![[8.0, 0.0], [8.0, 16.0]],
                vec![[16.0, 0.0], [16.0, 16.0]],
            ]
        );
        assert!(lines.iter().all(|l| (l.a, l.b) == (0, 1) && !l.flagged));

        // A closed boundary (an island owned by another panel) is one loop.
        let owner = OwnerMap {
            w8: 3,
            h8: 3,
            owner: vec![0, 0, 0, 0, 1, 0, 0, 0, 0],
        };
        let lines = seam_lines(&owner, &health_with(vec![]));
        assert_eq!(lines.len(), 1);
        let p = &lines[0].points;
        assert_eq!(p.len(), 5, "four corners and back: {p:?}");
        assert_eq!(p.first(), p.last());
    }

    /// `health_with_lines` measures exactly what `health` does, from the
    /// same owner map, and its lines name the measured seams.
    #[test]
    fn health_with_lines_matches_health() {
        let (session, graph) = analysed("lines");
        let ph = phot(vec![1.0; 3], vec![0.0; 3]);
        let h = health(&session, &graph, &ph, None, 16.0).unwrap();
        let (h2, lines) = health_with_lines(&session, &graph, &ph, None, 16.0).unwrap();
        assert_eq!(h, h2);
        assert!(!lines.is_empty());
        for l in &lines {
            assert!(l.a < l.b);
            assert!(l.points.len() >= 2);
            let s = h
                .seams
                .iter()
                .find(|s| (s.a, s.b) == (l.a as usize, l.b as usize));
            assert_eq!(l.flagged, s.is_some_and(|s| s.flagged));
        }
        // Panels 0 | 1 meet in the 48..64 overlap, 1 | 2 in 96..112: the
        // lines lie there (canvas px).
        for (pair, lo, hi) in [((0, 1), 48.0, 64.0), ((1, 2), 96.0, 112.0)] {
            let l = lines
                .iter()
                .find(|l| (l.a, l.b) == pair)
                .expect("seam line");
            assert!(
                l.points.iter().all(|p| (lo..=hi).contains(&p[0])),
                "{pair:?}: {:?}",
                l.points
            );
        }
    }

    #[test]
    fn outlier_edge_flagged() {
        let d = |delta| SeamDelta { n: 10, delta };
        let mut m = HashMap::new();
        m.insert((0, 1), d(0.01));
        m.insert((1, 2), d(0.012));
        m.insert((2, 3), d(0.011));
        m.insert((3, 4), d(0.5));
        m.insert((4, 5), SeamDelta { n: 0, delta: 9.0 });
        let steps = flag_steps(&m);
        assert_eq!(steps.len(), 4, "n == 0 edge omitted");
        let flagged: Vec<(usize, usize)> = steps
            .iter()
            .filter(|s| s.flagged)
            .map(|s| (s.a, s.b))
            .collect();
        assert_eq!(flagged, vec![(3, 4)]);
    }
}
