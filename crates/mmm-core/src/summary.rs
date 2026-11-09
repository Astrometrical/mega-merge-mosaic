//! L8 per-panel summaries: the canvas at 1/8 resolution.
//!
//! Each 8×8 canvas block becomes one cell holding the fraction of covered
//! pixels (all channels nonzero), the per-channel mean over the covered
//! pixels, and the per-channel detail energy — the RMS of `(pixel − cell
//! mean)` over the covered pixels, which flags star/structure cells for the
//! seam optimizer. This single artifact drives overlap detection, photometric
//! fitting, distance/feather maps, seam placement, downsampled previews, and
//! the panel quality statistics (noise, gradient, edge raggedness:
//! [`panel_stats`]).
//!
//! On-disk format v2 (`panels/<id>/summary.bin`, all little-endian):
//! magic `MMM9`, then `w8`, `h8`, `channels` as `u32`, then the coverage plane
//! (`w8*h8` × f32), then the mean planes (`channels` planes of `w8*h8` × f32),
//! then the detail planes (same shape as the mean planes). v1 files (`MMM8`,
//! no detail planes) are rejected — re-run analyze.

use std::path::Path;

use crate::{Error, Result};

/// Canvas pixels per summary cell along each axis.
pub const BLOCK: u32 = 8;

const MAGIC: &[u8; 4] = b"MMM9";
const MAGIC_V1: &[u8; 4] = b"MMM8";

/// One panel's 1/8-resolution summary.
#[derive(Debug, Clone, PartialEq)]
pub struct L8Summary {
    /// Grid width in cells: `ceil(canvas_width / 8)`.
    pub w8: u32,
    /// Grid height in cells: `ceil(canvas_height / 8)`.
    pub h8: u32,
    /// Number of channels (matches the panel).
    pub channels: u32,
    /// len `w8*h8`; fraction of covered pixels in each cell, 0..1.
    pub coverage: Vec<f32>,
    /// len `channels*w8*h8`, planar; mean of covered pixels (0 where none).
    pub mean: Vec<f32>,
    /// len `channels*w8*h8`, planar; RMS of `(pixel − cell mean)` over the
    /// covered pixels of each cell (0 where none) — the detail energy.
    pub detail: Vec<f32>,
}

impl L8Summary {
    /// Allocate an all-zero summary for the given grid.
    pub fn zeroed(w8: u32, h8: u32, channels: u32) -> Self {
        let cells = w8 as usize * h8 as usize;
        Self {
            w8,
            h8,
            channels,
            coverage: vec![0.0; cells],
            mean: vec![0.0; channels as usize * cells],
            detail: vec![0.0; channels as usize * cells],
        }
    }

    /// Mean of covered pixels for channel `c` at cell `(x8, y8)`.
    #[inline]
    pub fn cell(&self, c: u32, x8: u32, y8: u32) -> f32 {
        debug_assert!(c < self.channels && x8 < self.w8 && y8 < self.h8);
        let cells = self.w8 as usize * self.h8 as usize;
        self.mean[c as usize * cells + y8 as usize * self.w8 as usize + x8 as usize]
    }

    /// Detail energy (in-cell RMS) for channel `c` at cell `(x8, y8)`.
    #[inline]
    pub fn det(&self, c: u32, x8: u32, y8: u32) -> f32 {
        debug_assert!(c < self.channels && x8 < self.w8 && y8 < self.h8);
        let cells = self.w8 as usize * self.h8 as usize;
        self.detail[c as usize * cells + y8 as usize * self.w8 as usize + x8 as usize]
    }

    /// Covered-pixel fraction at cell `(x8, y8)`.
    #[inline]
    pub fn cov(&self, x8: u32, y8: u32) -> f32 {
        debug_assert!(x8 < self.w8 && y8 < self.h8);
        self.coverage[y8 as usize * self.w8 as usize + x8 as usize]
    }

    /// Write the summary to `path` in the on-disk v2 format (module docs).
    pub fn write(&self, path: &Path) -> Result<()> {
        let cells = self.w8 as usize * self.h8 as usize;
        assert_eq!(self.coverage.len(), cells, "coverage plane size mismatch");
        assert_eq!(
            self.mean.len(),
            self.channels as usize * cells,
            "mean planes size mismatch"
        );
        assert_eq!(
            self.detail.len(),
            self.channels as usize * cells,
            "detail planes size mismatch"
        );

        let mut bytes = Vec::with_capacity(
            16 + (self.coverage.len() + self.mean.len() + self.detail.len()) * 4,
        );
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.w8.to_le_bytes());
        bytes.extend_from_slice(&self.h8.to_le_bytes());
        bytes.extend_from_slice(&self.channels.to_le_bytes());
        for &v in &self.coverage {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for &v in &self.mean {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for &v in &self.detail {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
    }

    /// Read a summary written by [`L8Summary::write`], validating magic and
    /// size; v1 (`MMM8`) files are rejected with a re-run-analyze hint.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        if bytes.len() >= 4 && &bytes[0..4] == MAGIC_V1 {
            return Err(Error::format(
                path,
                "summary is format v1 (MMM8) without the detail plane — re-run analyze",
            ));
        }
        if bytes.len() < 16 || &bytes[0..4] != MAGIC {
            return Err(Error::format(path, "not an MMM9 summary file"));
        }
        let w8 = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let h8 = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let channels = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let cells = w8 as usize * h8 as usize;
        let ch_cells = channels as usize * cells;
        let expected = 16 + (cells + 2 * ch_cells) * 4;
        if bytes.len() != expected {
            return Err(Error::format(
                path,
                format!(
                    "summary size {} != expected {expected} for {w8}x{h8}x{channels}",
                    bytes.len()
                ),
            ));
        }
        let read_plane = |off: usize, n: usize| -> Vec<f32> {
            bytes[off..off + n * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect()
        };
        let coverage = read_plane(16, cells);
        let mean = read_plane(16 + cells * 4, ch_cells);
        let detail = read_plane(16 + (cells + ch_cells) * 4, ch_cells);
        Ok(Self {
            w8,
            h8,
            channels,
            coverage,
            mean,
            detail,
        })
    }
}

/// Coverage at or below which a cell counts as empty for the edge measure
/// ([`PanelStats::ragged`]): a stray pixel or two is not part of the panel.
pub const PARTIAL_MIN: f32 = 0.05;

/// Coverage below which a cell counts as only partly covered for the edge
/// measure ([`PanelStats::ragged`]); from [`PARTIAL_MIN`] up to this.
pub const PARTIAL_MAX: f32 = 0.95;

/// One panel's quality statistics, measured on its L8 summary at analysis
/// time by [`panel_stats`] and kept in [`crate::session::PanelMeta`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PanelStats {
    /// Per-channel background noise: the median in-cell RMS over the
    /// background cells. `None` without a fully covered cell.
    pub noise: Option<Vec<f64>>,
    /// Background gradient strength: the largest, over the channels, of the
    /// fitted background plane's rise across the bbox over that channel's
    /// noise. `None` when no channel has positive noise and a well-posed
    /// fit.
    pub gradient: Option<f64>,
    /// Edge raggedness: the share of the inner rim's cells only partly
    /// covered, 0 to 1. `None` when the panel has no inner rim.
    pub ragged: Option<f64>,
}

/// Median of `v` (reordered in place); `0` when empty.
fn median(v: &mut [f64]) -> f64 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    let upper = *v.select_nth_unstable_by(n / 2, f64::total_cmp).1;
    if n % 2 == 1 {
        upper
    } else {
        let lower = v[..n / 2].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        0.5 * (lower + upper)
    }
}

/// Least-squares plane `z ≈ α + β·x + γ·y` over `(x, y, z)` samples;
/// `(β, γ)`, or `None` with fewer than three samples or when they lie on
/// a line.
fn plane_slopes(samples: &[(f64, f64, f64)]) -> Option<(f64, f64)> {
    if samples.len() < 3 {
        return None;
    }
    let n = samples.len() as f64;
    let (mx, my, mz) = samples
        .iter()
        .fold((0.0, 0.0, 0.0), |(a, b, c), &(x, y, z)| {
            (a + x / n, b + y / n, c + z / n)
        });
    let (mut sxx, mut syy, mut sxy, mut sxz, mut syz) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &(x, y, z) in samples {
        let (dx, dy, dz) = (x - mx, y - my, z - mz);
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
        sxz += dx * dz;
        syz += dy * dz;
    }
    let det = sxx * syy - sxy * sxy;
    // det ≥ 0 (Cauchy–Schwarz); it vanishes exactly when the cells are
    // collinear, so allow for rounding relative to its scale.
    if det.is_nan() || det <= 1e-9 * sxx * syy {
        return None;
    }
    Some(((sxz * syy - syz * sxy) / det, (syz * sxx - sxz * sxy) / det))
}

/// The edge measure over the cell window `[x0, x1) × [y0, y1)` (the bbox's
/// cells; every cell outside it is empty). See [`panel_stats`].
fn rim_raggedness(s: &L8Summary, x0: usize, x1: usize, y0: usize, y1: usize) -> Option<f64> {
    let (w, h) = (x1 - x0, y1 - y0);
    let w8 = s.w8 as usize;
    let cov = |x: usize, y: usize| s.coverage[(y0 + y) * w8 + x0 + x];
    let touched: Vec<bool> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| cov(x, y) > PARTIAL_MIN)
        .collect();
    // Whether any 8-neighbour of (x, y) satisfies `f` (off the window is
    // passed as None).
    let any_neighbour = |x: usize, y: usize, f: &dyn Fn(Option<usize>) -> bool| {
        (-1isize..=1).any(|dy| {
            (-1isize..=1).any(|dx| {
                if dx == 0 && dy == 0 {
                    return false;
                }
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                let inside = nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h;
                f(inside.then(|| ny as usize * w + nx as usize))
            })
        })
    };
    // The outermost layer: touched cells with an untouched neighbour.
    let mut outer = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            outer[y * w + x] =
                touched[y * w + x] && any_neighbour(x, y, &|j| !j.is_some_and(|j| touched[j]));
        }
    }
    // The inner rim: touched cells next to the outermost layer.
    let (mut rim, mut partial) = (0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if touched[i] && !outer[i] && any_neighbour(x, y, &|j| j.is_some_and(|j| outer[j])) {
                rim += 1;
                if cov(x, y) < PARTIAL_MAX {
                    partial += 1;
                }
            }
        }
    }
    (rim > 0).then(|| partial as f64 / rim as f64)
}

/// Measure a panel's quality statistics on its L8 summary, over the cells
/// of its content `bbox` (canvas px, exclusive: [`crate::session::PanelMeta::bbox`])
/// only — never the whole canvas. Analysis calls it on the summary it has
/// just built, so it costs no extra read.
///
/// Per channel, over the bbox's fully covered cells (coverage 1):
/// - **Background cells** are those whose mean is at or below the median
///   of those cells' means: the darker half, which leaves out stars and
///   bright structure.
/// - **Noise** is the median, over the background cells, of the in-cell RMS
///   (`detail`). In a background cell that RMS is the pixel noise measured
///   on its 64 pixels, so the median is a robust per-pixel noise estimate
///   that stars and structure cannot drag up.
/// - **Gradient**: a least-squares plane `mean ≈ α + β·x + γ·y` is fitted to
///   the background cells (cell centres in canvas px). Its rise across the
///   bbox (its max − min over it) is `|β|·width + |γ|·height`; over the
///   channel's noise that is the channel's gradient amplitude. The panel's
///   gradient is the largest amplitude over the channels with positive
///   noise and a well-posed fit.
/// - Cells with a non-finite mean or RMS (NaN pixels) are left out; a
///   channel with no usable cell reads a noise of 0.
///
/// **Raggedness** looks at the cells the panel touches (coverage above
/// [`PARTIAL_MIN`]). The outermost layer is the touched cells with an
/// untouched 8-neighbour; the inner rim is the touched cells next to the
/// outermost layer but not on it. Raggedness is the share of the inner rim
/// that is only partly covered (coverage below [`PARTIAL_MAX`]). A clean
/// edge, straight at any angle, keeps its partly covered cells in the
/// outermost layer (the neighbour diagonally outward of each lies wholly
/// outside), so its inner rim is fully covered and reads 0; a ragged
/// stacking edge, whose coverage thins out over several cells, leaves
/// partial cells further in.
pub fn panel_stats(summary: &L8Summary, bbox: [u64; 4]) -> PanelStats {
    let block = BLOCK as u64;
    let (w8, h8) = (summary.w8 as usize, summary.h8 as usize);
    let x0 = (bbox[0] / block) as usize;
    let y0 = (bbox[1] / block) as usize;
    let x1 = (bbox[2].div_ceil(block) as usize).min(w8);
    let y1 = (bbox[3].div_ceil(block) as usize).min(h8);
    if x1 <= x0 || y1 <= y0 {
        return PanelStats::default();
    }
    let cells = w8 * h8;
    let full: Vec<usize> = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| y * w8 + x))
        .filter(|&i| summary.coverage[i] >= 1.0)
        .collect();

    let (mut noise, mut gradient) = (None, None);
    if !full.is_empty() {
        let (bw, bh) = ((bbox[2] - bbox[0]) as f64, (bbox[3] - bbox[1]) as f64);
        let centre = |i: usize| {
            (
                ((i % w8) as f64 + 0.5) * BLOCK as f64,
                ((i / w8) as f64 + 0.5) * BLOCK as f64,
            )
        };
        let mut per_channel = Vec::with_capacity(summary.channels as usize);
        for c in 0..summary.channels as usize {
            let mean = &summary.mean[c * cells..(c + 1) * cells];
            let detail = &summary.detail[c * cells..(c + 1) * cells];
            // Non-finite pixels (NaN in an input) poison a cell: skip it.
            let usable: Vec<usize> = full
                .iter()
                .copied()
                .filter(|&i| mean[i].is_finite() && detail[i].is_finite())
                .collect();
            let mut means: Vec<f64> = usable.iter().map(|&i| mean[i] as f64).collect();
            let mid = median(&mut means);
            let background: Vec<usize> = usable
                .into_iter()
                .filter(|&i| mean[i] as f64 <= mid)
                .collect();
            let mut rms: Vec<f64> = background.iter().map(|&i| detail[i] as f64).collect();
            // No usable cell reads as no noise: never out of line.
            let n = median(&mut rms);
            per_channel.push(n);
            if n <= 0.0 {
                continue;
            }
            let samples: Vec<(f64, f64, f64)> = background
                .iter()
                .map(|&i| {
                    let (x, y) = centre(i);
                    (x, y, mean[i] as f64)
                })
                .collect();
            if let Some((bx, by)) = plane_slopes(&samples) {
                let amp = (bx.abs() * bw + by.abs() * bh) / n;
                if amp.is_finite() {
                    gradient = Some(gradient.map_or(amp, |g: f64| g.max(amp)));
                }
            }
        }
        noise = Some(per_channel);
    }

    PanelStats {
        noise,
        gradient,
        ragged: rim_raggedness(summary, x0, x1, y0, y1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let mut s = L8Summary::zeroed(3, 2, 2);
        s.coverage = vec![0.0, 0.5, 1.0, 0.25, 0.75, 1.0];
        s.mean = (0..12).map(|i| i as f32 * 0.1).collect();
        s.detail = (0..12).map(|i| i as f32 * 0.01).collect();

        let dir = std::env::temp_dir().join(format!("mmm-summary-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("summary.bin");
        s.write(&path).unwrap();
        let r = L8Summary::read(&path).unwrap();
        assert_eq!(r, s);

        // Accessors address the planar layout correctly.
        assert_eq!(r.cov(1, 0), 0.5);
        assert_eq!(r.cov(2, 1), 1.0);
        assert!((r.cell(0, 1, 1) - 0.4).abs() < 1e-6);
        assert!((r.cell(1, 2, 0) - 0.8).abs() < 1e-6);
        assert!((r.det(0, 1, 1) - 0.04).abs() < 1e-6);
        assert!((r.det(1, 2, 0) - 0.08).abs() < 1e-6);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_magic_and_truncation() {
        let dir = std::env::temp_dir().join(format!("mmm-summary-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let bad = dir.join("bad.bin");
        std::fs::write(&bad, b"NOPE").unwrap();
        assert!(L8Summary::read(&bad).is_err());

        let trunc = dir.join("trunc.bin");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        std::fs::write(&trunc, bytes).unwrap();
        assert!(L8Summary::read(&trunc).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A summary whose coverage is the share of each cell's 64 pixel centres
    /// inside `inside`, on a `w8`×`h8` grid, with flat means; and the
    /// covered pixels' bbox.
    fn covering(w8: u32, h8: u32, inside: impl Fn(f64, f64) -> bool) -> (L8Summary, [u64; 4]) {
        let mut s = L8Summary::zeroed(w8, h8, 1);
        let mut bbox = [u64::MAX, u64::MAX, 0, 0];
        for y8 in 0..h8 {
            for x8 in 0..w8 {
                let mut n = 0;
                for py in 0..BLOCK {
                    for px in 0..BLOCK {
                        let (x, y) = (x8 * BLOCK + px, y8 * BLOCK + py);
                        if inside(x as f64 + 0.5, y as f64 + 0.5) {
                            n += 1;
                            bbox = [
                                bbox[0].min(x as u64),
                                bbox[1].min(y as u64),
                                bbox[2].max(x as u64 + 1),
                                bbox[3].max(y as u64 + 1),
                            ];
                        }
                    }
                }
                let i = (y8 * w8 + x8) as usize;
                s.coverage[i] = n as f32 / 64.0;
                if n > 0 {
                    s.mean[i] = 0.1;
                }
            }
        }
        (s, bbox)
    }

    /// A clean edge, straight at any angle, keeps its partly covered cells
    /// in the outermost layer, so a rotated rectangle never reads as ragged.
    /// A fuzzy edge (coverage thinning out over several cells) does.
    #[test]
    fn clean_edges_at_any_angle_are_not_ragged() {
        for deg in [0.0f64, 1.0, 3.0, 10.0, 22.5, 30.0, 44.0, 45.0] {
            let (sin, cos) = deg.to_radians().sin_cos();
            let (cx, cy) = (200.0, 200.0);
            let (s, bbox) = covering(50, 50, |x, y| {
                let (dx, dy) = (x - cx, y - cy);
                let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
                u.abs() < 131.3 && v.abs() < 97.7
            });
            assert_eq!(panel_stats(&s, bbox).ragged, Some(0.0), "{deg} degrees");
        }
        // Coverage fading over four cells all round.
        let (mut s, bbox) = covering(40, 40, |x, y| {
            (24.0..296.0).contains(&x) && (24.0..296.0).contains(&y)
        });
        for y8 in 3..37u32 {
            for x8 in 3..37u32 {
                let depth = x8.min(y8).min(39 - x8).min(39 - y8) - 3;
                if depth < 4 {
                    s.coverage[(y8 * 40 + x8) as usize] = 0.2 + 0.15 * depth as f32;
                }
            }
        }
        let r = panel_stats(&s, bbox).ragged.unwrap();
        assert!(r > 0.9, "fuzzy edges read {r}");
    }

    /// The definitions on exact values: noise is the background cells' median
    /// RMS (bright star cells change nothing), the gradient the background
    /// plane's rise across the bbox over that noise, per channel, the
    /// largest kept; a NaN cell is ignored.
    #[test]
    fn statistics_follow_their_definitions() {
        let (w8, h8) = (20u32, 10u32);
        let cells = (w8 * h8) as usize;
        let mut s = L8Summary::zeroed(w8, h8, 2);
        s.coverage.fill(1.0);
        for y8 in 0..h8 {
            for x8 in 0..w8 {
                let i = (y8 * w8 + x8) as usize;
                let x = (x8 as f32 + 0.5) * 8.0;
                // Channel 0 rises 1e-4 per px in x; channel 1 is flat.
                s.mean[i] = 0.1 + 1.0e-4 * x;
                s.detail[i] = 0.002;
                s.mean[cells + i] = 0.2;
                s.detail[cells + i] = 0.004;
            }
        }
        // Stars: bright, high-RMS cells (above the median, so not background).
        for i in [25usize, 77, 140] {
            s.mean[i] += 1.0;
            s.detail[i] = 0.5;
            s.mean[cells + i] += 1.0;
            s.detail[cells + i] = 0.5;
        }
        s.mean[3] = f32::NAN;
        let st = panel_stats(&s, [0, 0, 160, 80]);
        let n = st.noise.unwrap();
        assert!(
            (n[0] - 0.002).abs() < 1e-9 && (n[1] - 0.004).abs() < 1e-9,
            "{n:?}"
        );
        // Rise 1e-4 × 160 px over 0.002; channel 1's flat plane reads 0.
        let g = st.gradient.unwrap();
        assert!((g - 8.0).abs() < 1e-3, "{g}");
        // Fully covered to the canvas edge: the inner rim is all covered.
        assert_eq!(st.ragged, Some(0.0));
    }

    /// No coverage, no statistics.
    #[test]
    fn an_empty_panel_has_no_statistics() {
        let s = L8Summary::zeroed(4, 4, 3);
        assert_eq!(panel_stats(&s, [0, 0, 0, 0]), PanelStats::default());
    }

    #[test]
    fn rejects_v1_summaries_with_rerun_hint() {
        let dir = std::env::temp_dir().join(format!("mmm-summary-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A well-formed v1 file: MMM8 magic, no detail planes.
        let (w8, h8, ch) = (2u32, 2u32, 1u32);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC_V1);
        bytes.extend_from_slice(&w8.to_le_bytes());
        bytes.extend_from_slice(&h8.to_le_bytes());
        bytes.extend_from_slice(&ch.to_le_bytes());
        for _ in 0..(4 + 4) {
            bytes.extend_from_slice(&1.0f32.to_le_bytes());
        }
        let path = dir.join("v1.bin");
        std::fs::write(&path, bytes).unwrap();

        let err = L8Summary::read(&path).unwrap_err().to_string();
        assert!(
            err.contains("re-run analyze"),
            "error must tell the user to re-run analyze: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
