//! Display stretch: per-channel statistics and an automatic midtones stretch.
//!
//! This follows the publicly described auto-stretch rule (clip shadows at a
//! fixed number of robust sigmas below the median, then pick the midtones
//! balance that maps the median to a target background level) and is written
//! independently. It is used for display only: the data stays linear, and the
//! stretch is applied by the viewer, never baked into results.

use serde::{Deserialize, Serialize};

/// Median and raw median absolute deviation of one channel's covered pixels.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ChannelStats {
    /// Median of the covered pixels.
    pub median: f32,
    /// Raw (unscaled) median absolute deviation about the median.
    pub mad: f32,
}

/// Parameters of a display stretch for one channel.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct StretchParams {
    /// Input level mapped to 0.
    pub black: f32,
    /// Input level mapped to 1.
    pub white: f32,
    /// Midtones balance: the normalised input that maps to 0.5.
    pub midtones: f32,
}

/// Shadows clipping point, in robust sigmas relative to the median.
pub const SHADOWS_CLIP: f32 = -2.8;
/// Output level the median is mapped to.
pub const TARGET_BACKGROUND: f32 = 0.25;
/// Scale factor turning a MAD into a Gaussian-equivalent sigma.
pub const MAD_TO_SIGMA: f32 = 1.4826;

fn median_of(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Per-channel median and MAD of planar data (`channels` planes of `pixels`
/// samples each), ignoring no-data pixels (all channels zero). With no covered
/// pixels every channel's stats are zero.
pub fn channel_stats(planar: &[f32], pixels: usize, channels: usize) -> Vec<ChannelStats> {
    let covered: Vec<usize> = (0..pixels)
        .filter(|&i| (0..channels).any(|c| planar[c * pixels + i] != 0.0))
        .collect();
    (0..channels)
        .map(|c| {
            if covered.is_empty() {
                return ChannelStats {
                    median: 0.0,
                    mad: 0.0,
                };
            }
            let plane = &planar[c * pixels..(c + 1) * pixels];
            let mut vals: Vec<f32> = covered.iter().map(|&i| plane[i]).collect();
            let median = median_of(&mut vals);
            for v in vals.iter_mut() {
                *v = (*v - median).abs();
            }
            ChannelStats {
                median,
                mad: median_of(&mut vals),
            }
        })
        .collect()
}

/// The midtones transfer function: 0 at `x = 0`, 1 at `x = 1`, 0.5 at
/// `x = m`. Degenerate input (zero denominator) yields 0.
pub fn mtf(m: f32, x: f32) -> f32 {
    let d = (2.0 * m - 1.0) * x - m;
    if d == 0.0 {
        return 0.0;
    }
    ((m - 1.0) * x) / d
}

fn params_for(median: f32, mad: f32, clip: f32, target: f32) -> StretchParams {
    let black = if mad == 0.0 {
        0.0
    } else {
        (median + clip * MAD_TO_SIGMA * mad).clamp(0.0, 1.0)
    };
    let white = 1.0;
    let norm = (median - black) / (white - black);
    let midtones = if norm.is_finite() && norm > 0.0 {
        mtf(target, norm.min(1.0))
    } else {
        0.5
    };
    StretchParams {
        black,
        white,
        midtones,
    }
}

/// How hard the automatic stretch pulls the background up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strength {
    /// The standard stretch ([`SHADOWS_CLIP`], [`TARGET_BACKGROUND`]).
    #[default]
    Normal,
    /// A stronger stretch that exaggerates faint gradients
    /// ([`STRONG_SHADOWS_CLIP`], [`STRONG_TARGET_BACKGROUND`]).
    Strong,
}

/// Shadows clipping point of the strong stretch, in robust sigmas.
pub const STRONG_SHADOWS_CLIP: f32 = -1.25;
/// Output level the median is mapped to by the strong stretch.
pub const STRONG_TARGET_BACKGROUND: f32 = 0.45;

/// Automatic display stretch, one [`StretchParams`] per channel. When `linked`
/// every channel gets the same parameters, computed from the mean of the
/// channels' medians and MADs.
pub fn auto_stretch(stats: &[ChannelStats], linked: bool) -> Vec<StretchParams> {
    auto_stretch_with(stats, linked, Strength::Normal)
}

/// [`auto_stretch`] with a chosen [`Strength`].
pub fn auto_stretch_with(
    stats: &[ChannelStats],
    linked: bool,
    strength: Strength,
) -> Vec<StretchParams> {
    if stats.is_empty() {
        return Vec::new();
    }
    let (clip, target) = match strength {
        Strength::Normal => (SHADOWS_CLIP, TARGET_BACKGROUND),
        Strength::Strong => (STRONG_SHADOWS_CLIP, STRONG_TARGET_BACKGROUND),
    };
    if linked {
        let n = stats.len() as f32;
        let median = stats.iter().map(|s| s.median).sum::<f32>() / n;
        let mad = stats.iter().map(|s| s.mad).sum::<f32>() / n;
        vec![params_for(median, mad, clip, target); stats.len()]
    } else {
        stats
            .iter()
            .map(|s| params_for(s.median, s.mad, clip, target))
            .collect()
    }
}

/// Apply a display stretch to one sample; the result is in `[0, 1]`.
pub fn apply(p: &StretchParams, x: f32) -> f32 {
    let range = p.white - p.black;
    let n = if range > 0.0 {
        ((x - p.black) / range).clamp(0.0, 1.0)
    } else {
        // Degenerate range: a hard threshold at `white`.
        if x >= p.white { 1.0 } else { 0.0 }
    };
    mtf(p.midtones, n).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_lifts_the_background() {
        let s = [ChannelStats {
            median: 0.1,
            mad: 0.01,
        }];
        let normal = auto_stretch_with(&s, false, Strength::Normal)[0];
        let strong = auto_stretch_with(&s, false, Strength::Strong)[0];
        assert!((apply(&normal, 0.1) - 0.25).abs() < 1e-4);
        assert!((apply(&strong, 0.1) - 0.45).abs() < 1e-4);
    }

    #[test]
    fn strong_clips_shadows_nearer_the_median() {
        let s = [ChannelStats {
            median: 0.1,
            mad: 0.01,
        }];
        let normal = auto_stretch_with(&s, false, Strength::Normal)[0];
        let strong = auto_stretch_with(&s, false, Strength::Strong)[0];
        assert!(strong.black > normal.black);
    }

    #[test]
    fn auto_stretch_is_normal() {
        let s = [
            ChannelStats {
                median: 0.1,
                mad: 0.01,
            },
            ChannelStats {
                median: 0.2,
                mad: 0.02,
            },
        ];
        for linked in [false, true] {
            assert_eq!(
                auto_stretch(&s, linked),
                auto_stretch_with(&s, linked, Strength::Normal)
            );
        }
    }

    #[test]
    fn mtf_fixed_points() {
        assert_eq!(mtf(0.25, 0.0), 0.0);
        assert!((mtf(0.25, 1.0) - 1.0).abs() < 1e-6);
        assert!((mtf(0.25, 0.25) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn stats_ignore_no_data() {
        // 3 channels, 4 pixels; pixels 2 and 3 are all-zero (no data).
        let px = 4;
        let planar = vec![
            1.0, 3.0, 0.0, 0.0, //
            2.0, 4.0, 0.0, 0.0, //
            0.0, 5.0, 0.0, 0.0, // zero in one channel only at pixel 0 -> covered
        ];
        let s = channel_stats(&planar, px, 3);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].median, 2.0);
        assert_eq!(s[0].mad, 1.0);
        assert_eq!(s[1].median, 3.0);
        assert_eq!(s[2].median, 2.5);
        let empty = channel_stats(&[0.0; 8], 4, 2);
        assert!(empty.iter().all(|c| c.median == 0.0 && c.mad == 0.0));
    }

    #[test]
    fn auto_puts_median_at_target() {
        let s = [ChannelStats {
            median: 0.1,
            mad: 0.01,
        }];
        let p = auto_stretch(&s, false)[0];
        assert!((apply(&p, 0.1) - TARGET_BACKGROUND).abs() < 1e-4);
        assert!(p.black > 0.0 && p.black < 0.1);
    }

    #[test]
    fn unlinked_differs_per_channel_linked_does_not() {
        let s = [
            ChannelStats {
                median: 0.1,
                mad: 0.01,
            },
            ChannelStats {
                median: 0.2,
                mad: 0.02,
            },
        ];
        let u = auto_stretch(&s, false);
        assert_ne!(u[0], u[1]);
        let l = auto_stretch(&s, true);
        assert_eq!(l[0], l[1]);
        let mean = params_for(0.15, 0.015, SHADOWS_CLIP, TARGET_BACKGROUND);
        assert_eq!(l[0], mean);
    }

    #[test]
    fn flat_data_is_safe() {
        for (median, mad) in [(0.0, 0.0), (0.3, 0.0), (1.0, 0.0), (0.0, 0.5), (0.05, 5.0)] {
            let p = auto_stretch(&[ChannelStats { median, mad }], false)[0];
            assert!(p.black.is_finite() && p.white.is_finite() && p.midtones.is_finite());
            for x in [0.0, 0.1, 0.5, 1.0, 2.0] {
                let y = apply(&p, x);
                assert!(y.is_finite() && (0.0..=1.0).contains(&y), "{p:?} {x} {y}");
            }
        }
        let degenerate = StretchParams {
            black: 0.5,
            white: 0.5,
            midtones: 0.5,
        };
        assert!(apply(&degenerate, 0.7).is_finite());
    }
}
