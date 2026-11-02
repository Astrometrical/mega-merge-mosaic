//! Mosaic health metrics as plain data, for a UI to display.
//!
//! Wraps the seam diagnostics of [`crate::diag`] with the same outlier rule
//! `mmm report` prints (a seam step more than 3× the median is flagged) and
//! the spread of the photometric corrections across panels.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::diag::{SeamDelta, load_owner_map, median, seam_deltas};
use crate::overlap::OverlapGraph;
use crate::photometry::Photometry;
use crate::session::Session;
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
    let deltas = seam_deltas(&summaries, &owner, phot, surfaces, session.canvas);
    let seams = flag_steps(&deltas);
    let values: Vec<f64> = seams.iter().map(|s| s.delta).collect();
    Ok(Health {
        seam_delta_max: values.iter().copied().fold(0.0, f64::max),
        seam_delta_median: median(values),
        seams,
        gain_range: ranges(&phot.gains),
        offset_range: ranges(&phot.offsets),
    })
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
