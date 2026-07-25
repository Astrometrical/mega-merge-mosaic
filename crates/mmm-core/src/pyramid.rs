//! Laplacian pyramids on the L8 cell grid, for the Pyramid blend mode's
//! star-free base ([`crate::blend::BlendMode::Pyramid`]).
//!
//! All planes live on the small L8 grid (`w8 × h8` f32 cells), so a full
//! Burt–Adelson multiband blend costs almost nothing and needs no out-of-core
//! plumbing. A [`CellPyramid`] holds Laplacian levels 0..n (finest first) plus
//! the Gaussian residual as the last level; level ℓ+1 has dimensions
//! `(⌈w/2⌉, ⌈h/2⌉)` of level ℓ (non-power-of-two grids round up). Smoothing is
//! the classic 5-tap Gaussian `[1,4,6,4,1]/16` with replicated borders;
//! upsampling is bilinear (coarse cell `j` sits at fine cell `2j`), and
//! `collapse(build(p)) == p` holds *exactly* up to f32 rounding because each
//! Laplacian level stores the true difference against the same upsample used
//! to reconstruct.
//!
//! [`build_masked`] builds the pyramid under a validity plane via normalized
//! convolution: invalid cells (outside a panel's coverage reach) contribute
//! nothing at any level — coarse levels extrapolate from valid data instead
//! of pulling in the zeros surrounding a sparse panel — while `valid == 1`
//! cells still reconstruct exactly. [`blend_pyramids`] combines panels per
//! level with their ownership-mask Gaussian pyramids ([`mask_pyramid`]),
//! renormalizing the masks per cell over the contributing panels, so each
//! frequency band transitions over a distance proportional to its scale.

/// Threshold below which an accumulated mask/validity weight counts as zero.
const EPS_WEIGHT: f32 = 1e-6;

/// Minimum downsampled-validity fraction for a cell to carry ownership-mask
/// weight at a pyramid level ([`mask_pyramid`]). Below it, the panel's data
/// pyramid holds the 0.0 sentinel or an extrapolation from a vanishing sliver
/// of its coverage — blending either with real mask weight drags the base
/// toward the wrong value far outside the panel (the user-visible dark
/// streak in a neighbour's single-coverage zone). The validity chain decays
/// over ~2 cells of each level's grid beyond the panel's coverage, so this
/// clamp bounds a panel's mask support to its geometric coverage dilated by
/// at most that level's transition width.
const MASK_SUPPORT_MIN: f32 = 1e-3;

/// A pyramid of L8-grid planes: Laplacian levels 0..n (finest first) followed
/// by the Gaussian residual as the last level. [`mask_pyramid`] reuses the
/// container for Gaussian (non-Laplacian) mask levels of the same shape.
pub struct CellPyramid {
    pub levels: Vec<Vec<f32>>,
    pub w8: u32,
    pub h8: u32,
}

impl CellPyramid {
    /// Dimensions of level `l`: level 0 is `(w8, h8)`, each next level rounds
    /// halves up (so a 1-cell axis stays 1).
    pub fn level_dims(&self, l: usize) -> (usize, usize) {
        level_dims(self.w8 as usize, self.h8 as usize, l)
    }
}

/// Dimensions of pyramid level `l` for a level-0 grid of `w × h`.
fn level_dims(mut w: usize, mut h: usize, l: usize) -> (usize, usize) {
    for _ in 0..l {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
    }
    (w, h)
}

/// Number of base-pyramid Laplacian levels for a feather length:
/// `ceil(log2(feather/8))` clamped to `[2, 6]` — levels at 8, 16, 32, … px up
/// to the feather scale (feather 256 → 5).
pub fn n_levels_for_feather(feather_px: f32) -> u32 {
    let ratio = feather_px / crate::summary::BLOCK as f32;
    (ratio.log2().ceil() as i32).clamp(2, 6) as u32
}

/// Separable 5-tap Gaussian `[1,4,6,4,1]/16`, borders replicated (the kernel
/// keeps unit mass everywhere, so constants — including an all-ones validity
/// plane — are preserved exactly).
fn smooth(plane: &[f32], w: usize, h: usize) -> Vec<f32> {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        let row = &plane[y * w..][..w];
        let out = &mut tmp[y * w..][..w];
        for (x, o) in out.iter_mut().enumerate() {
            let mut s = 0.0f32;
            for (k, &kv) in K.iter().enumerate() {
                let xx = (x as isize + k as isize - 2).clamp(0, w as isize - 1) as usize;
                s += kv * row[xx];
            }
            *o = s;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0f32;
            for (k, &kv) in K.iter().enumerate() {
                let yy = (y as isize + k as isize - 2).clamp(0, h as isize - 1) as usize;
                s += kv * tmp[yy * w + x];
            }
            out[y * w + x] = s;
        }
    }
    out
}

/// Smooth + decimate (keep even indices): the Gaussian-pyramid step.
fn downsample(plane: &[f32], w: usize, h: usize) -> Vec<f32> {
    let sm = smooth(plane, w, h);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![0.0f32; w2 * h2];
    for y2 in 0..h2 {
        for x2 in 0..w2 {
            out[y2 * w2 + x2] = sm[(y2 * 2) * w + x2 * 2];
        }
    }
    out
}

/// Bilinear upsample of a coarse plane back onto the fine grid (coarse cell
/// `j` at fine cell `2j`; edges clamp).
fn upsample(coarse: &[f32], cw: usize, ch: usize, fw: usize, fh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; fw * fh];
    for y in 0..fh {
        let gy = y as f32 * 0.5;
        let y0 = (gy as usize).min(ch - 1);
        let y1 = (y0 + 1).min(ch - 1);
        let fy = gy - y0 as f32;
        for x in 0..fw {
            let gx = x as f32 * 0.5;
            let x0 = (gx as usize).min(cw - 1);
            let x1 = (x0 + 1).min(cw - 1);
            let fx = gx - x0 as f32;
            let top = coarse[y0 * cw + x0] * (1.0 - fx) + coarse[y0 * cw + x1] * fx;
            let bot = coarse[y1 * cw + x0] * (1.0 - fx) + coarse[y1 * cw + x1] * fx;
            out[y * fw + x] = top * (1.0 - fy) + bot * fy;
        }
    }
    out
}

/// Laplacian pyramid of `plane`: `n_levels` difference levels + the Gaussian
/// residual. `collapse(build(p)) == p` within f32 rounding for any size.
pub fn build(plane: &[f32], w8: u32, h8: u32, n_levels: u32) -> CellPyramid {
    build_masked(plane, &vec![1.0f32; plane.len()], w8, h8, n_levels)
}

/// As [`build`], under a validity plane (1 = trusted cell, 0 = excluded):
/// each Gaussian level is the normalized convolution `smooth(d·v)/smooth(v)`
/// chained through the pyramid, so invalid cells never contaminate any level
/// (coarse levels extrapolate from the valid data's edge instead). Where
/// `valid == 1`, reconstruction is still exact; cells whose whole smoothing
/// footprint is invalid get 0.
pub fn build_masked(plane: &[f32], valid: &[f32], w8: u32, h8: u32, n_levels: u32) -> CellPyramid {
    let (w, h) = (w8 as usize, h8 as usize);
    assert_eq!(plane.len(), w * h, "plane must be w8*h8");
    assert_eq!(valid.len(), w * h, "valid must be w8*h8");

    // Premultiplied chain: a = d·v and v run through the same Gaussian
    // pyramid; each level's data is a/v (normalized convolution).
    let mut a: Vec<f32> = plane.iter().zip(valid).map(|(&d, &v)| d * v).collect();
    let mut v = valid.to_vec();
    let ratio = |a: &[f32], v: &[f32]| -> Vec<f32> {
        a.iter()
            .zip(v)
            .map(|(&av, &vv)| if vv > EPS_WEIGHT { av / vv } else { 0.0 })
            .collect()
    };
    let mut gauss = vec![ratio(&a, &v)];
    for l in 0..n_levels as usize {
        let (lw, lh) = level_dims(w, h, l);
        a = downsample(&a, lw, lh);
        v = downsample(&v, lw, lh);
        gauss.push(ratio(&a, &v));
    }

    let mut levels = Vec::with_capacity(n_levels as usize + 1);
    for l in 0..n_levels as usize {
        let (lw, lh) = level_dims(w, h, l);
        let (cw, ch) = level_dims(w, h, l + 1);
        let up = upsample(&gauss[l + 1], cw, ch, lw, lh);
        levels.push(gauss[l].iter().zip(&up).map(|(&g, &u)| g - u).collect());
    }
    levels.push(gauss.pop().expect("gauss chain is non-empty"));
    CellPyramid { levels, w8, h8 }
}

/// Gaussian pyramid of an ownership-mask plane (levels 0..=n, same shapes as
/// a data pyramid's). Each stored level is smoothed once more, so even level
/// 0 transitions over ~a cell rather than switching hard — the transition
/// width at level ℓ is a few level-ℓ cells, i.e. proportional to 2^ℓ·8 px.
///
/// `valid` is the same validity plane the panel's [`build_masked`] uses
/// (1 = trusted cell). Both the mask chain and every stored level are
/// clamped to zero where the identically-downsampled validity falls below
/// [`MASK_SUPPORT_MIN`]: without the clamp, the per-level smoothing walks
/// the mask support outward without limit — by the coarse levels a panel
/// held blend weight hundreds of px past its geometric coverage, exactly
/// where its data pyramid is the 0.0 sentinel or a baseless extrapolation
/// (the dark-streak bug). With it, a panel's contribution is strictly zero
/// beyond its coverage dilated by ~2 cells of each level's grid, and
/// wherever its mask is nonzero its data pyramid is a genuine normalized
/// convolution of covered cells. Masks of all-ones validity (tests, single
/// dense panel) are unaffected.
pub fn mask_pyramid(mask: &[f32], valid: &[f32], w8: u32, h8: u32, n_levels: u32) -> CellPyramid {
    let (w, h) = (w8 as usize, h8 as usize);
    assert_eq!(mask.len(), w * h, "mask must be w8*h8");
    assert_eq!(valid.len(), w * h, "valid must be w8*h8");
    let clamp = |m: Vec<f32>, v: &[f32]| -> Vec<f32> {
        m.iter()
            .zip(v)
            .map(|(&mv, &vv)| if vv >= MASK_SUPPORT_MIN { mv } else { 0.0 })
            .collect()
    };
    let mut v = valid.to_vec();
    let mut cur = clamp(mask.to_vec(), &v);
    let mut levels = vec![clamp(smooth(&cur, w, h), &v)];
    for l in 0..n_levels as usize {
        let (lw, lh) = level_dims(w, h, l);
        cur = downsample(&cur, lw, lh);
        v = downsample(&v, lw, lh);
        cur = clamp(cur, &v);
        let (cw, ch) = level_dims(w, h, l + 1);
        levels.push(clamp(smooth(&cur, cw, ch), &v));
    }
    CellPyramid { levels, w8, h8 }
}

/// Reconstruct the level-0 plane: residual upsampled and Laplacian levels
/// added back, finest last.
pub fn collapse(p: &CellPyramid) -> Vec<f32> {
    let n = p.levels.len() - 1;
    let mut cur = p.levels[n].clone();
    for l in (0..n).rev() {
        let (lw, lh) = p.level_dims(l);
        let (cw, ch) = p.level_dims(l + 1);
        let up = upsample(&cur, cw, ch, lw, lh);
        cur = p.levels[l].iter().zip(&up).map(|(&d, &u)| d + u).collect();
    }
    cur
}

/// Blend per level and collapse: `out_ℓ = Σ_i m_iℓ·p_iℓ / Σ_i m_iℓ`, where
/// `m_iℓ` is panel i's ownership-mask pyramid ([`mask_pyramid`]) — masks are
/// renormalized per cell over the panels that reach it, so coverage holes in
/// one panel hand its share to the others instead of bleeding zeros. Cells
/// where `Σ m ≈ 0` at some level are numerically undefined; use
/// [`blend_pyramids_guarded`] to know which (the caller falls back to its
/// feather-weighted base there).
pub fn blend_pyramids(panels: &[(CellPyramid, CellPyramid)]) -> Vec<f32> {
    let datas: Vec<&CellPyramid> = panels.iter().map(|(d, _)| d).collect();
    let masks: Vec<&CellPyramid> = panels.iter().map(|(_, m)| m).collect();
    blend_pyramids_guarded(&datas, &masks).0
}

/// [`blend_pyramids`] plus a level-0 definedness plane: `false` where the
/// blend hit `Σ masks ≈ 0` at any level feeding that cell.
pub fn blend_pyramids_guarded(
    datas: &[&CellPyramid],
    masks: &[&CellPyramid],
) -> (Vec<f32>, Vec<bool>) {
    assert!(!datas.is_empty(), "pyramid blend needs at least one panel");
    assert_eq!(
        datas.len(),
        masks.len(),
        "one mask pyramid per data pyramid"
    );
    let (w8, h8) = (datas[0].w8, datas[0].h8);
    let n_lv = datas[0].levels.len();
    for (d, m) in datas.iter().zip(masks) {
        assert_eq!(
            (d.w8, d.h8, d.levels.len()),
            (w8, h8, n_lv),
            "pyramid shapes must match"
        );
        assert_eq!(
            (m.w8, m.h8, m.levels.len()),
            (w8, h8, n_lv),
            "pyramid shapes must match"
        );
    }

    // Per level: mask-weighted sum, renormalized per cell.
    let mut blended: Vec<Vec<f32>> = Vec::with_capacity(n_lv);
    let mut defined: Vec<Vec<bool>> = Vec::with_capacity(n_lv);
    for l in 0..n_lv {
        let cells = datas[0].levels[l].len();
        let mut acc = vec![0.0f32; cells];
        let mut wsum = vec![0.0f32; cells];
        for (d, m) in datas.iter().zip(masks) {
            for ((a, s), (&dv, &mv)) in acc
                .iter_mut()
                .zip(&mut wsum)
                .zip(d.levels[l].iter().zip(&m.levels[l]))
            {
                *a += mv * dv;
                *s += mv;
            }
        }
        defined.push(wsum.iter().map(|&s| s > EPS_WEIGHT).collect());
        for (a, &s) in acc.iter_mut().zip(&wsum) {
            *a = if s > EPS_WEIGHT { *a / s } else { 0.0 };
        }
        blended.push(acc);
    }

    // Collapse, propagating undefinedness down from the coarse levels
    // (nearest coarse cell — the guard is a numerical safety net, not a
    // precision feature).
    let (w, h) = (w8 as usize, h8 as usize);
    let n = n_lv - 1;
    let mut cur = blended[n].clone();
    let mut curdef = defined[n].clone();
    for l in (0..n).rev() {
        let (lw, lh) = level_dims(w, h, l);
        let (cw, ch) = level_dims(w, h, l + 1);
        let up = upsample(&cur, cw, ch, lw, lh);
        cur = blended[l].iter().zip(&up).map(|(&d, &u)| d + u).collect();
        let mut def = vec![false; lw * lh];
        for y in 0..lh {
            let cy = (y / 2).min(ch - 1);
            for x in 0..lw {
                let cx = (x / 2).min(cw - 1);
                def[y * lw + x] = defined[l][y * lw + x] && curdef[cy * cw + cx];
            }
        }
        curdef = def;
    }
    (cur, curdef)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random plane in [0, 1).
    fn random_plane(w: usize, h: usize, seed: u64) -> Vec<f32> {
        let mut s = seed | 1;
        (0..w * h)
            .map(|_| {
                s ^= s >> 12;
                s ^= s << 25;
                s ^= s >> 27;
                (s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
            })
            .collect()
    }

    /// Mandatory phase-4 test 1: build→collapse is the identity within 1e-6
    /// on random planes of several sizes, including non-power-of-two.
    #[test]
    fn build_collapse_is_identity() {
        for &(w, h, n) in &[
            (16u32, 16u32, 3u32),
            (13, 9, 2),
            (57, 41, 4),
            (128, 1, 3),
            (1157, 33, 5),
        ] {
            let plane = random_plane(w as usize, h as usize, 7 + w as u64 * h as u64);
            let p = build(&plane, w, h, n);
            assert_eq!(
                p.levels.len(),
                n as usize + 1,
                "n Laplacian levels + residual"
            );
            let back = collapse(&p);
            let max = plane
                .iter()
                .zip(&back)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(max < 1e-6, "{w}x{h} n={n}: max reconstruction error {max}");
        }
    }

    /// Masked build: garbage outside the validity plane must not leak into
    /// any level — reconstruction over valid cells stays exact, and coarse
    /// Gaussian levels near the valid region's edge extrapolate instead of
    /// dipping toward the (huge) invalid values.
    #[test]
    fn masked_build_excludes_invalid_cells() {
        let (w, h) = (48u32, 32u32);
        let (wu, hu) = (w as usize, h as usize);
        // Valid left half holds a gentle plane; invalid right half is poison.
        let mut plane = vec![1e6f32; wu * hu];
        let mut valid = vec![0.0f32; wu * hu];
        for y in 0..hu {
            for x in 0..wu / 2 {
                plane[y * wu + x] = 0.1 + 0.001 * x as f32;
                valid[y * wu + x] = 1.0;
            }
        }
        let p = build_masked(&plane, &valid, w, h, 3);
        let back = collapse(&p);
        for y in 0..hu {
            for x in 0..wu / 2 {
                let (a, b) = (plane[y * wu + x], back[y * wu + x]);
                assert!(
                    (a - b).abs() < 1e-4,
                    "valid cell ({x},{y}): {b} vs {a} — poison leaked in"
                );
            }
        }
        // Every Gaussian-side value the pyramid stores must stay near the
        // valid data's range — nowhere within a factor 1000 of the poison.
        for (l, lv) in p.levels.iter().enumerate() {
            let max = lv.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            assert!(max < 1e3, "level {l} contains poison-scale value {max}");
        }
    }

    /// Blending a single panel with a constant-1 mask reproduces its plane
    /// exactly (the base of the single-coverage reconstruction guarantee).
    #[test]
    fn single_panel_blend_reproduces_plane() {
        let (w, h) = (40u32, 24u32);
        let plane = random_plane(w as usize, h as usize, 99);
        let data = build(&plane, w, h, 3);
        let ones = vec![1.0f32; plane.len()];
        let mask = mask_pyramid(&ones, &ones, w, h, 3);
        let (out, def) = blend_pyramids_guarded(&[&data], &[&mask]);
        assert!(
            def.iter().all(|&d| d),
            "constant mask must be defined everywhere"
        );
        let max = plane
            .iter()
            .zip(&out)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max < 1e-5,
            "single-panel blend must reproduce the plane, max err {max}"
        );
    }

    /// Two constant panels split left/right: the blend transitions from one
    /// value to the other monotonically, hits both plateaus, and the public
    /// pair API agrees with the guarded one.
    #[test]
    fn two_panel_blend_transitions_monotonically() {
        let (w, h) = (64u32, 16u32);
        let (wu, hu) = (w as usize, h as usize);
        let cells = wu * hu;
        let a = vec![0.2f32; cells];
        let b = vec![0.8f32; cells];
        let mask_a: Vec<f32> = (0..cells)
            .map(|i| if i % wu < wu / 2 { 1.0 } else { 0.0 })
            .collect();
        let mask_b: Vec<f32> = mask_a.iter().map(|&m| 1.0 - m).collect();
        let n = 3;
        let ones = vec![1.0f32; cells];
        let pa = (build(&a, w, h, n), mask_pyramid(&mask_a, &ones, w, h, n));
        let pb = (build(&b, w, h, n), mask_pyramid(&mask_b, &ones, w, h, n));
        let out = blend_pyramids(&[pa, pb]);

        // Plateau tolerance 5e-3: the residual level's transition is ±~2·2ⁿ
        // cells wide by design, so its far tails still graze the grid edges.
        let y = hu / 2;
        assert!(
            (out[y * wu] - 0.2).abs() < 5e-3,
            "left plateau: {}",
            out[y * wu]
        );
        assert!(
            (out[y * wu + wu - 1] - 0.8).abs() < 5e-3,
            "right plateau: {}",
            out[y * wu + wu - 1]
        );
        let mut prev = f32::NEG_INFINITY;
        for x in 0..wu {
            let v = out[y * wu + x];
            assert!(
                v >= prev - 1e-4,
                "profile not monotone at x={x}: {v} < {prev}"
            );
            assert!(
                (0.2 - 1e-3..=0.8 + 1e-3).contains(&v),
                "overshoot at x={x}: {v}"
            );
            prev = v;
        }
    }

    /// Guard: where every panel's mask vanishes, the blend is flagged
    /// undefined (the blender falls back to the feathered base there).
    #[test]
    fn zero_mask_sum_is_flagged_undefined() {
        let (w, h) = (32u32, 32u32);
        let cells = (w * h) as usize;
        let plane = random_plane(w as usize, h as usize, 5);
        let data = build(&plane, w, h, 2);
        let mask = mask_pyramid(&vec![0.0f32; cells], &vec![1.0f32; cells], w, h, 2);
        let (out, def) = blend_pyramids_guarded(&[&data], &[&mask]);
        assert!(
            def.iter().all(|&d| !d),
            "zero masks must be undefined everywhere"
        );
        assert!(out.iter().all(|&v| v == 0.0));
    }

    /// Support clamp (dark-streak regression): two panels with different
    /// constant backgrounds, each valid only over its own half (+overlap).
    /// (a) A panel's mask pyramid must be exactly zero at every level beyond
    /// its validity dilated by that level's transition width; (b) deep in
    /// panel A's single-coverage zone the blend equals A's plane exactly;
    /// (c) the blend never dips below the darker panel's level — pre-clamp,
    /// the partner's bled mask weighted its 0.0-sentinel data there, digging
    /// a below-both-backgrounds streak.
    #[test]
    fn partner_mask_and_influence_stop_at_dilated_coverage() {
        let (w, h) = (384u32, 32u32);
        let (wu, hu) = (w as usize, h as usize);
        let cells = wu * hu;
        let n = 3u32;
        // A: bright background, valid x < 224; B: darker, valid x >= 192.
        let (edge_a, edge_b) = (224usize, 192usize);
        let a = vec![0.5f32; cells];
        let b = vec![0.1f32; cells];
        let valid_a: Vec<f32> = (0..cells)
            .map(|i| if i % wu < edge_a { 1.0 } else { 0.0 })
            .collect();
        let valid_b: Vec<f32> = (0..cells)
            .map(|i| if i % wu >= edge_b { 1.0 } else { 0.0 })
            .collect();
        // Ownership splits mid-overlap.
        let mask_a: Vec<f32> = (0..cells)
            .map(|i| if i % wu < 208 { 1.0 } else { 0.0 })
            .collect();
        let mask_b: Vec<f32> = mask_a.iter().map(|&m| 1.0 - m).collect();

        let pa = build_masked(&a, &valid_a, w, h, n);
        let pb = build_masked(&b, &valid_b, w, h, n);
        let ma = mask_pyramid(&mask_a, &valid_a, w, h, n);
        let mb = mask_pyramid(&mask_b, &valid_b, w, h, n);

        // (a) B's mask support stops within ~2 cells of each level's grid
        // (the validity chain's decay) beyond B's validity edge.
        for (l, lv) in mb.levels.iter().enumerate() {
            let (lw, lh) = mb.level_dims(l);
            let scale = 1usize << l;
            let reach = 4 * scale; // 2 chain cells + smoothing, in fine cells
            for y in 0..lh {
                for x in 0..lw {
                    let fine_x = x * scale;
                    if fine_x + reach < edge_b {
                        assert_eq!(
                            lv[y * lw + x],
                            0.0,
                            "level {l}: B mask nonzero at fine x {fine_x}, \
                             {} cells past its validity",
                            edge_b - fine_x
                        );
                    }
                }
            }
        }

        // (b) + (c): blend both, compare against A alone.
        let (out, def) = blend_pyramids_guarded(&[&pa, &pb], &[&ma, &mb]);
        let solo = collapse(&pa);
        let y = hu / 2;
        for x in 0..edge_b - 64 {
            let (o, s) = (out[y * wu + x], solo[y * wu + x]);
            assert!(
                (o - s).abs() < 1e-6,
                "deep single coverage x={x}: blend {o} != A alone {s}"
            );
        }
        for x in 0..wu {
            if !def[y * wu + x] {
                continue;
            }
            let o = out[y * wu + x];
            assert!(o > 0.1 - 1e-3, "below-both-panels dip at x={x}: {o}");
        }
    }

    #[test]
    fn n_levels_matches_feather_scale() {
        assert_eq!(n_levels_for_feather(256.0), 5);
        assert_eq!(n_levels_for_feather(24.0), 2);
        assert_eq!(n_levels_for_feather(64.0), 3);
        assert_eq!(n_levels_for_feather(8.0), 2, "clamped low");
        assert_eq!(n_levels_for_feather(1.0e9), 6, "clamped high");
    }
}
