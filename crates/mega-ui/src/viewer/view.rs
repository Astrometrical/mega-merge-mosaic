//! Viewport maths: springs, view transform and pyramid tile selection.

use egui::{Pos2, Rect, Vec2};

/// Spring stiffness constant `k` of every pan and zoom animation.
///
/// A [`Spring`] covers the fraction `(1 - e^(-k s)) / (1 - e^(-k))` of its
/// travel at `s = t / ANIMATION_TIME` (OpenSeadragon's formula). Each drag
/// event restarts the spring from where it is, so while dragging the view
/// follows the pointer like a first-order lag with time constant
/// `ANIMATION_TIME * (1 - e^(-k)) / k`. OpenSeadragon's defaults (`k` 6.5,
/// 1.2 s) give 0.18 s and felt stiff; `k` 4.0 with 1.2 s gives a softer,
/// more even ease (half-way at 0.20 s rather than 0.13 s) and a 0.29 s
/// follow lag: smooth, slightly behind the pointer. Raise `k` (or lower
/// [`ANIMATION_TIME`]) for a snappier feel.
pub const SPRING_STIFFNESS: f64 = 4.0;
/// Seconds a spring takes to reach its target (OpenSeadragon's default);
/// see [`SPRING_STIFFNESS`].
pub const ANIMATION_TIME: f64 = 1.2;
/// Maximum zoom, in screen pixels per image pixel.
pub const MAX_ZOOM: f64 = 8.0;

/// Geometry of an image pyramid: level 0 is full size, level `l` is
/// `ceil(w / 2^l) x ceil(h / 2^l)`, with levels added until both dimensions
/// fit in a single tile. Mirrors mmm-core's `TileGeometry` rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PyramidGeometry {
    /// Level-0 width in pixels.
    pub width: u64,
    /// Level-0 height in pixels.
    pub height: u64,
    /// Tile edge length in pixels.
    pub tile: u32,
    /// Number of pyramid levels (at least 1).
    pub levels: u32,
}

impl PyramidGeometry {
    /// Build a geometry, deriving the level count from the tile size.
    pub fn new(width: u64, height: u64, tile: u32) -> Self {
        let t = u64::from(tile.max(1));
        let (mut w, mut h, mut levels) = (width.max(1), height.max(1), 1);
        while w > t || h > t {
            w = w.div_ceil(2);
            h = h.div_ceil(2);
            levels += 1;
        }
        Self {
            width,
            height,
            tile,
            levels,
        }
    }

    /// Pixel dimensions of `level`.
    pub fn level_dims(&self, level: u32) -> (u64, u64) {
        let d = 1u64 << level;
        (self.width.div_ceil(d), self.height.div_ceil(d))
    }

    /// Tile grid size `(columns, rows)` of `level`.
    pub fn tiles(&self, level: u32) -> (u32, u32) {
        let (w, h) = self.level_dims(level);
        let t = u64::from(self.tile.max(1));
        (w.div_ceil(t) as u32, h.div_ceil(t) as u32)
    }
}

/// Identifies one tile of a pyramid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// Pyramid level (0 = full resolution).
    pub level: u32,
    /// Tile column.
    pub tx: u32,
    /// Tile row.
    pub ty: u32,
}

/// An exponentially eased scalar (OpenSeadragon's spring model).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Value when the current animation began.
    pub start: f64,
    /// Value the spring is heading to.
    pub target: f64,
    /// Time (seconds) the current animation began.
    pub start_time: f64,
}

impl Spring {
    /// A settled spring at `v`.
    pub fn new(v: f64) -> Self {
        Self {
            start: v,
            target: v,
            start_time: 0.0,
        }
    }

    /// Value at time `now`.
    pub fn value(&self, now: f64) -> f64 {
        let s = ((now - self.start_time) / ANIMATION_TIME).clamp(0.0, 1.0);
        let f = (1.0 - (-SPRING_STIFFNESS * s).exp()) / (1.0 - (-SPRING_STIFFNESS).exp());
        self.start + (self.target - self.start) * f
    }

    /// Animate from the current value to `target`, starting at `now`.
    pub fn spring_to(&mut self, target: f64, now: f64) {
        self.start = self.value(now);
        self.target = target;
        self.start_time = now;
    }

    /// Jump to `v` immediately.
    pub fn reset_to(&mut self, v: f64) {
        self.start = v;
        self.target = v;
    }

    /// True once the animation has finished at `now`.
    pub fn is_settled(&self, now: f64) -> bool {
        self.start == self.target || now - self.start_time >= ANIMATION_TIME
    }
}

/// Viewport onto an image: centre (image pixels) and zoom (screen pixels per
/// image pixel), all eased by [`Spring`]s. Zoom animates in log space.
///
/// The centre is shown in one of two forms, so that the drawn view is always
/// a single smooth function of time:
///
/// - **Anchored** (pointer motion): an image point drawn at a screen offset
///   (from the viewport centre) that is itself a pair of springs, so the
///   centre at time `t` is `anchor - offset(t) / zoom(t)`. A drag moves the
///   offset springs' *targets* by the drag, so the image eases after the
///   pointer (OpenSeadragon's `panBy`). A zoom about a point re-anchors on
///   the image point under it, so that point stays under the cursor
///   throughout the zoom (OpenSeadragon's `zoomBy` with a reference point),
///   and a pan still in flight carries on from there, moving the anchored
///   point with it.
/// - **Gliding** (fit, home, zoom to a rect): the centre springs in image
///   space from where it is to the target, with the same easing progress as
///   the zoom, so it stays between the two throughout. A drag during a glide
///   moves the glide's target.
///
/// Once the zoom has settled, [`View::update`] folds an anchor offset back
/// into the anchor, and a finished glide into a plain centre, without moving
/// the view.
#[derive(Clone, Debug)]
pub struct View {
    /// Image point drawn at the screen offset `(off_x, off_y)`.
    anchor: (f64, f64),
    off_x: Spring,
    off_y: Spring,
    /// Image-space centre springs of a glide in progress; while set, they
    /// define the centre (the anchor is unused).
    glide: Option<(Spring, Spring)>,
    ln_zoom: Spring,
    min_zoom: f64,
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}

impl View {
    /// A view centred on the origin at zoom 1.
    pub fn new() -> Self {
        Self {
            anchor: (0.0, 0.0),
            off_x: Spring::new(0.0),
            off_y: Spring::new(0.0),
            glide: None,
            ln_zoom: Spring::new(0.0),
            min_zoom: 1e-9,
        }
    }

    /// Animate home: the whole image visible and centred on a `screen`-sized
    /// viewport. Also sets the minimum zoom (half the fit zoom). Degenerate
    /// geometry or screen sizes fall back to zoom 1; the fit zoom never
    /// exceeds [`MAX_ZOOM`].
    pub fn fit(&mut self, geom: &PyramidGeometry, screen: Vec2, now: f64) {
        let (w, h) = (geom.width as f64, geom.height as f64);
        self.fit_rect([0.0, 0.0, w, h], screen, now);
    }

    /// Animate home onto an arbitrary image rect `[x0, y0, x1, y1]`: like
    /// [`View::fit`], for content whose extent does not start at the origin.
    pub fn fit_rect(&mut self, image_rect: [f64; 4], screen: Vec2, now: f64) {
        let (w, h) = (image_rect[2] - image_rect[0], image_rect[3] - image_rect[1]);
        let mut z = (f64::from(screen.x) / w).min(f64::from(screen.y) / h);
        if !z.is_finite() || z <= 0.0 {
            z = 1.0;
        }
        let z = z.min(MAX_ZOOM);
        self.min_zoom = z / 2.0;
        let c = (image_rect[0] + w / 2.0, image_rect[1] + h / 2.0);
        self.glide_to(c, z, now);
    }

    /// Glide to centre `c` at zoom `z` (already clamped): the centre and the
    /// zoom spring together from `now`, the centre in image space.
    fn glide_to(&mut self, c: (f64, f64), z: f64, now: f64) {
        let from = self.centre(now);
        if !(from.0.is_finite() && from.1.is_finite() && c.0.is_finite() && c.1.is_finite()) {
            return;
        }
        let spring = |a: f64, b: f64| Spring {
            start: a,
            target: b,
            start_time: now,
        };
        self.glide = Some((spring(from.0, c.0), spring(from.1, c.1)));
        self.ln_zoom.spring_to(z.ln(), now);
    }

    /// End a glide at `now`: anchor on the centre shown, at zero offset.
    fn end_glide(&mut self, now: f64) {
        if self.glide.is_some() {
            let c = self.centre(now);
            self.glide = None;
            self.anchor = c;
            self.off_x.reset_to(0.0);
            self.off_y.reset_to(0.0);
        }
    }

    /// Re-anchor on image point `p` without moving the view at `now` (a
    /// glide ends there): the offset springs restart from where `p` is
    /// drawn, keeping the travel of a pan still in flight. False (nothing
    /// changed) if `p` is not drawn at a finite position.
    fn reanchor(&mut self, p: (f64, f64), now: f64) -> bool {
        let z = self.zoom(now);
        let c = self.centre(now);
        let at = ((p.0 - c.0) * z, (p.1 - c.1) * z);
        if !(at.0.is_finite() && at.1.is_finite()) {
            return false;
        }
        self.end_glide(now);
        let rest = (
            self.off_x.target - self.off_x.value(now),
            self.off_y.target - self.off_y.value(now),
        );
        self.anchor = p;
        self.off_x = Spring {
            start: at.0,
            target: at.0 + rest.0,
            start_time: now,
        };
        self.off_y = Spring {
            start: at.1,
            target: at.1 + rest.1,
            start_time: now,
        };
        true
    }

    /// Advance to `now`: once the zoom has settled, a finished glide becomes
    /// a plain centre and an anchor offset is folded back into the anchor
    /// (the view does not move; a pan still easing carries on unchanged).
    /// Call once per frame, before input.
    pub fn update(&mut self, now: f64) {
        if !self.ln_zoom.is_settled(now) {
            return;
        }
        if let Some((gx, gy)) = self.glide {
            if gx.is_settled(now) && gy.is_settled(now) {
                self.end_glide(now);
            }
            return;
        }
        if self.off_x.start == 0.0 && self.off_y.start == 0.0 {
            return;
        }
        // offset(t) = start + (target - start) f(t), so shifting the anchor
        // by start / z and the spring by -start leaves the centre unchanged.
        let z = self.zoom(now);
        self.anchor.0 -= self.off_x.start / z;
        self.anchor.1 -= self.off_y.start / z;
        for s in [&mut self.off_x, &mut self.off_y] {
            s.target -= s.start;
            s.start = 0.0;
        }
    }

    /// Jump every spring to its target, ending any animation.
    pub fn finish_animation(&mut self) {
        if let Some((gx, gy)) = self.glide.take() {
            self.anchor = (gx.target, gy.target);
            self.off_x.reset_to(0.0);
            self.off_y.reset_to(0.0);
        }
        self.off_x.reset_to(self.off_x.target);
        self.off_y.reset_to(self.off_y.target);
        self.ln_zoom.reset_to(self.ln_zoom.target);
    }

    /// Current zoom at `now`.
    pub fn zoom(&self, now: f64) -> f64 {
        self.ln_zoom.value(now).exp()
    }

    /// Zoom the view is animating towards.
    pub fn zoom_target(&self) -> f64 {
        self.ln_zoom.target.exp()
    }

    /// Current centre in image pixels at `now`.
    pub fn centre(&self, now: f64) -> (f64, f64) {
        if let Some((gx, gy)) = &self.glide {
            return (gx.value(now), gy.value(now));
        }
        let z = self.zoom(now);
        (
            self.anchor.0 - self.off_x.value(now) / z,
            self.anchor.1 - self.off_y.value(now) / z,
        )
    }

    /// True when no spring is still animating.
    pub fn is_settled(&self, now: f64) -> bool {
        let glide = self
            .glide
            .is_none_or(|(gx, gy)| gx.is_settled(now) && gy.is_settled(now));
        glide
            && self.off_x.is_settled(now)
            && self.off_y.is_settled(now)
            && self.ln_zoom.is_settled(now)
    }

    /// Jump to `centre` and `zoom` (clamped) with no animation.
    pub fn set_view(&mut self, centre: (f64, f64), zoom: f64, _now: f64) {
        self.glide = None;
        self.anchor = centre;
        self.off_x.reset_to(0.0);
        self.off_y.reset_to(0.0);
        self.ln_zoom.reset_to(self.clamp_zoom(zoom).ln());
    }

    fn clamp_zoom(&self, z: f64) -> f64 {
        z.clamp(self.min_zoom, MAX_ZOOM.max(self.min_zoom))
    }

    /// Multiply the target zoom by `factor`, keeping the image point under
    /// `about_screen` fixed there throughout the animation (it moves only
    /// with a pan still in flight). A no-op at a zoom limit.
    pub fn zoom_by(&mut self, factor: f64, about_screen: Pos2, screen_rect: Rect, now: f64) {
        let new_z = self.clamp_zoom(self.zoom_target() * factor);
        if !new_z.is_finite() || new_z <= 0.0 {
            return;
        }
        if (new_z.ln() - self.ln_zoom.target).abs() < 1e-12 {
            return; // at a zoom limit: leave any pan in flight alone
        }
        let p = self.screen_to_image(about_screen, screen_rect, now);
        if self.reanchor(p, now) {
            self.ln_zoom.spring_to(new_z.ln(), now);
        }
    }

    /// Drag the image by `screen_delta` screen pixels. The view eases after
    /// the pointer: the drag accumulates into the offset springs' targets,
    /// so a stream of drag events becomes one smooth motion. An in-flight
    /// zoom keeps running, its anchored point carried along by the drag; a
    /// glide's target moves by the drag (at the target zoom).
    pub fn pan_by(&mut self, screen_delta: Vec2, now: f64) {
        let (dx, dy) = (f64::from(screen_delta.x), f64::from(screen_delta.y));
        let z = self.zoom_target();
        if let Some((gx, gy)) = &mut self.glide {
            if dx != 0.0 {
                gx.spring_to(gx.target - dx / z, now);
            }
            if dy != 0.0 {
                gy.spring_to(gy.target - dy / z, now);
            }
            return;
        }
        if dx != 0.0 {
            self.off_x.spring_to(self.off_x.target + dx, now);
        }
        if dy != 0.0 {
            self.off_y.spring_to(self.off_y.target + dy, now);
        }
    }

    /// Animate so `image_rect` (`[x0, y0, x1, y1]`) fills a `screen`-sized
    /// viewport.
    pub fn zoom_to_rect(&mut self, image_rect: [f64; 4], screen: Vec2, now: f64) {
        let w = (image_rect[2] - image_rect[0]).max(1e-9);
        let h = (image_rect[3] - image_rect[1]).max(1e-9);
        let z = self.clamp_zoom((f64::from(screen.x) / w).min(f64::from(screen.y) / h));
        let c = (
            (image_rect[0] + image_rect[2]) / 2.0,
            (image_rect[1] + image_rect[3]) / 2.0,
        );
        self.glide_to(c, z, now);
    }

    /// Image point -> screen position, for the view at `now`.
    pub fn image_to_screen(&self, p: (f64, f64), screen_rect: Rect, now: f64) -> Pos2 {
        let z = self.zoom(now);
        let c = self.centre(now);
        let mid = screen_rect.center();
        Pos2::new(
            mid.x + ((p.0 - c.0) * z) as f32,
            mid.y + ((p.1 - c.1) * z) as f32,
        )
    }

    /// Screen position -> image point, for the view at `now`.
    pub fn screen_to_image(&self, s: Pos2, screen_rect: Rect, now: f64) -> (f64, f64) {
        let z = self.zoom(now);
        let c = self.centre(now);
        let off = s - screen_rect.center();
        (c.0 + f64::from(off.x) / z, c.1 + f64::from(off.y) / z)
    }

    /// Image rect `[x0, y0, x1, y1]` visible in `screen_rect` at `now`.
    pub fn visible_image_rect(&self, screen_rect: Rect, now: f64) -> [f64; 4] {
        let a = self.screen_to_image(screen_rect.min, screen_rect, now);
        let b = self.screen_to_image(screen_rect.max, screen_rect, now);
        [a.0, a.1, b.0, b.1]
    }
}

/// Pyramid level to draw at `zoom`: `floor(log2(1 / zoom))` clamped to
/// `[0, levels - 1]`.
pub fn pick_level(zoom: f64, levels: u32) -> u32 {
    let l = (1.0 / zoom).log2().floor();
    if l.is_nan() || l < 0.0 {
        return 0;
    }
    (l as u32).min(levels.saturating_sub(1))
}

/// Tiles of `level` intersecting `image_rect` (`[x0, y0, x1, y1]` in level-0
/// pixels), clamped to the grid.
pub fn visible_tiles(geom: &PyramidGeometry, level: u32, image_rect: [f64; 4]) -> Vec<TileKey> {
    let ts = f64::from(geom.tile.max(1)) * (1u64 << level) as f64;
    let (cols, rows) = geom.tiles(level);
    let x0 = image_rect[0].max(0.0);
    let y0 = image_rect[1].max(0.0);
    let x1 = image_rect[2].min(geom.width as f64);
    let y1 = image_rect[3].min(geom.height as f64);
    if x1 <= x0 || y1 <= y0 || cols == 0 || rows == 0 {
        return Vec::new();
    }
    let last = |v: f64, n: u32| ((v / ts).ceil() as i64 - 1).clamp(0, i64::from(n) - 1) as u32;
    let first = |v: f64, n: u32| ((v / ts).floor() as i64).clamp(0, i64::from(n) - 1) as u32;
    let (tx0, tx1) = (first(x0, cols), last(x1, cols));
    let (ty0, ty1) = (first(y0, rows), last(y1, rows));
    let mut out = Vec::new();
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            out.push(TileKey { level, tx, ty });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Vec2 = Vec2::new(1920.0, 1080.0);

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, SCREEN)
    }

    fn geom() -> PyramidGeometry {
        PyramidGeometry::new(70_000, 48_000, 256)
    }

    #[test]
    fn spring_reaches_target_after_animation_time() {
        let mut s = Spring::new(2.0);
        s.spring_to(10.0, 5.0);
        assert!((s.value(5.0) - 2.0).abs() < 1e-12);
        assert!(!s.is_settled(5.5));
        assert!((s.value(5.0 + ANIMATION_TIME) - 10.0).abs() < 1e-12);
        assert!((s.value(100.0) - 10.0).abs() < 1e-12);
        assert!(s.is_settled(5.0 + ANIMATION_TIME));
    }

    #[test]
    fn spring_monotonic() {
        let mut s = Spring::new(0.0);
        s.spring_to(1.0, 0.0);
        let mut prev = s.value(0.0);
        for i in 1..=120 {
            let v = s.value(i as f64 * 0.01);
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn fit_centres_and_contains_image() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let now = 10.0;
        let tl = v.image_to_screen((0.0, 0.0), rect(), now);
        let br = v.image_to_screen((70_000.0, 48_000.0), rect(), now);
        assert!(tl.x >= -0.01 && tl.y >= -0.01);
        assert!(br.x <= 1920.01 && br.y <= 1080.01);
        let mid = v.image_to_screen((35_000.0, 24_000.0), rect(), now);
        assert!((mid.x - 960.0).abs() < 0.01 && (mid.y - 540.0).abs() < 0.01);
        // Touches the limiting dimension.
        assert!((br.y - tl.y - 1080.0).abs() < 0.01);
    }

    #[test]
    fn zoom_keeps_cursor_point_fixed() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let cursor = Pos2::new(1400.0, 300.0);
        let before = v.screen_to_image(cursor, rect(), 10.0);
        v.zoom_by(4.0, cursor, rect(), 10.0);
        let after = v.screen_to_image(cursor, rect(), 20.0);
        assert!((before.0 - after.0).abs() < 1e-6, "{before:?} {after:?}");
        assert!((before.1 - after.1).abs() < 1e-6);
        assert!(v.zoom_target() > v.zoom(10.0));
    }

    #[test]
    fn zoom_is_clamped() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let fit = v.zoom_target();
        v.zoom_by(1e9, Pos2::new(10.0, 10.0), rect(), 10.0);
        assert!((v.zoom_target() - MAX_ZOOM).abs() < 1e-9);
        v.zoom_by(1e-12, Pos2::new(10.0, 10.0), rect(), 10.0);
        assert!((v.zoom_target() - fit / 2.0).abs() < 1e-12);
    }

    #[test]
    fn pan_by_springs_after_the_pointer() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        v.finish_animation();
        let p0 = v.screen_to_image(Pos2::new(500.0, 500.0), rect(), 10.0);
        v.pan_by(Vec2::new(100.0, 0.0), 10.0);
        // Nothing moves at the event; the view then eases monotonically.
        let at = |v: &View, t: f64| v.screen_to_image(Pos2::new(500.0, 500.0), rect(), t);
        assert!((at(&v, 10.0).0 - p0.0).abs() < 1e-9);
        assert!(!v.is_settled(10.1));
        let mut prev = p0.0;
        for i in 1..=12 {
            let x = at(&v, 10.0 + f64::from(i) * 0.1).0;
            assert!(x < prev, "reversed at step {i}");
            prev = x;
        }
        let z = v.zoom(10.0);
        let p1 = at(&v, 10.0 + ANIMATION_TIME);
        assert!((p0.0 - p1.0 - 100.0 / z).abs() < 1e-6);
        assert!((p0.1 - p1.1).abs() < 1e-9);
        // Further drags accumulate into the target, from where it is.
        v.pan_by(Vec2::new(50.0, 0.0), 10.1);
        v.pan_by(Vec2::new(50.0, 0.0), 10.2);
        let p2 = at(&v, 20.0);
        assert!((p0.0 - p2.0 - 200.0 / z).abs() < 1e-6);
    }

    #[test]
    fn fit_from_deep_far_pan_stays_between_start_and_target() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        v.finish_animation();
        // Deep in, far from the fit centre (zoom 8, a corner of the image).
        v.set_view((500.0, 500.0), MAX_ZOOM, 1.0);
        let from = v.centre(1.0);
        v.fit(&g, SCREEN, 1.0);
        let to = (35_000.0, 24_000.0);
        for i in 0..=48 {
            let t = 1.0 + f64::from(i) * ANIMATION_TIME / 48.0;
            v.update(t);
            let c = v.centre(t);
            for k in 0..2 {
                let (a, b, x) = [(from.0, to.0, c.0), (from.1, to.1, c.1)][k];
                assert!(
                    x >= a.min(b) - 1e-6 && x <= a.max(b) + 1e-6,
                    "t={t}: centre {c:?} outside {from:?}..{to:?}"
                );
            }
        }
        let t = 1.0 + ANIMATION_TIME;
        assert_eq!(v.centre(t), to);
        v.update(t + 1.0);
        assert_eq!(v.centre(t + 1.0), to);
        assert!(v.is_settled(t));
    }

    #[test]
    fn drag_during_fit_moves_its_target() {
        let g = geom();
        let mut v = View::new();
        v.set_view((500.0, 500.0), 1.0, 0.0);
        v.fit(&g, SCREEN, 0.0);
        v.pan_by(Vec2::new(100.0, 0.0), 0.3);
        let z = v.zoom_target();
        let c = v.centre(10.0);
        assert!((c.0 - (35_000.0 - 100.0 / z)).abs() < 1e-6, "{c:?}");
        assert!((c.1 - 24_000.0).abs() < 1e-6);
        // A zoom mid-glide takes over at the view shown.
        let mut v = View::new();
        v.set_view((500.0, 500.0), 1.0, 0.0);
        v.fit(&g, SCREEN, 0.0);
        let p = Pos2::new(700.0, 200.0);
        let under = v.screen_to_image(p, rect(), 0.3);
        v.zoom_by(1.2, p, rect(), 0.3);
        let at = v.screen_to_image(p, rect(), 0.3);
        assert!((at.0 - under.0).abs() < 1e-6 && (at.1 - under.1).abs() < 1e-6);
        let later = v.screen_to_image(p, rect(), 10.0);
        assert!((later.0 - under.0).abs() < 1e-6 && (later.1 - under.1).abs() < 1e-6);
    }

    #[test]
    fn zoom_at_limit_keeps_pan_easing() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        v.set_view((1000.0, 1000.0), MAX_ZOOM, 0.0);
        v.pan_by(Vec2::new(100.0, 0.0), 1.0);
        let before = v.clone();
        v.zoom_by(1.2, Pos2::new(10.0, 10.0), rect(), 1.1);
        for t in [1.1, 1.5, 3.0] {
            assert_eq!(v.centre(t), before.centre(t));
            assert_eq!(v.zoom(t), before.zoom(t));
        }
    }

    #[test]
    fn update_folds_anchor_without_moving() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let cursor = Pos2::new(1400.0, 300.0);
        v.zoom_by(4.0, cursor, rect(), 10.0);
        v.pan_by(Vec2::new(30.0, 10.0), 10.5);
        // Mid-zoom, update changes nothing.
        let before = v.centre(10.6);
        v.update(10.6);
        assert_eq!(v.centre(10.6), before);
        // Zoom settled, pan still easing: folded, the view unchanged.
        let t = 10.0 + ANIMATION_TIME + 0.01;
        let (c, later) = (v.centre(t), v.centre(30.0));
        v.update(t);
        assert_eq!((v.off_x.start, v.off_y.start), (0.0, 0.0));
        for (a, b) in [(c, v.centre(t)), (later, v.centre(30.0))] {
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
        }
    }

    #[test]
    fn cursor_anchored_mid_animation() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let cursor = Pos2::new(1400.0, 300.0);
        let before = v.screen_to_image(cursor, rect(), 10.0);
        v.zoom_by(4.0, cursor, rect(), 10.0);
        for i in 0..=12 {
            let t = 10.0 + f64::from(i) * 0.1;
            let p = v.screen_to_image(cursor, rect(), t);
            assert!(
                (p.0 - before.0).abs() < 1e-6 && (p.1 - before.1).abs() < 1e-6,
                "t={t}: {p:?} vs {before:?}"
            );
        }
        // A second zoom mid-animation, about another point, re-anchors there.
        let c2 = Pos2::new(300.0, 900.0);
        let b2 = v.screen_to_image(c2, rect(), 10.3);
        v.zoom_by(0.5, c2, rect(), 10.3);
        for i in 0..=12 {
            let t = 10.3 + f64::from(i) * 0.1;
            let p = v.screen_to_image(c2, rect(), t);
            assert!((p.0 - b2.0).abs() < 1e-6 && (p.1 - b2.1).abs() < 1e-6);
        }
    }

    #[test]
    fn pan_keeps_zoom_animation() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let cursor = Pos2::new(1400.0, 300.0);
        v.zoom_by(4.0, cursor, rect(), 10.0);
        let target = v.zoom_target();
        v.pan_by(Vec2::new(50.0, -20.0), 10.2);
        assert!(!v.is_settled(10.2), "pan cancelled the zoom animation");
        assert!((v.zoom_target() - target).abs() < 1e-12);
        // The zoom still lands on its target...
        assert!((v.zoom(20.0) - target).abs() < 1e-9);
        // The zoom's anchored point moves with the drag, never back, and
        // lands under the dragged pointer.
        let under = v.screen_to_image(cursor, rect(), 10.0);
        let mut prev = v.image_to_screen(under, rect(), 10.2);
        for i in 1..=20 {
            let s = v.image_to_screen(under, rect(), 10.2 + f64::from(i) * 0.1);
            assert!(s.x >= prev.x - 1e-3 && s.y <= prev.y + 1e-3, "step {i}");
            prev = s;
        }
        assert!((prev - (cursor + Vec2::new(50.0, -20.0))).length() < 1e-3);
    }

    #[test]
    fn fit_handles_degenerate() {
        let finite = |v: &View| {
            let c = v.centre(10.0);
            v.zoom(10.0).is_finite() && v.zoom(10.0) > 0.0 && c.0.is_finite() && c.1.is_finite()
        };
        let mut v = View::new();
        v.fit(&PyramidGeometry::new(0, 0, 256), SCREEN, 0.0);
        assert!(finite(&v));
        let mut v = View::new();
        v.fit(&geom(), Vec2::ZERO, 0.0);
        assert!(finite(&v));
        // Zooming afterwards stays finite too.
        v.zoom_by(2.0, Pos2::new(5.0, 5.0), Rect::ZERO, 1.0);
        assert!(finite(&v));
        // A tiny image fits no closer than MAX_ZOOM.
        let mut v = View::new();
        v.fit(&PyramidGeometry::new(10, 10, 256), SCREEN, 0.0);
        assert!(v.zoom_target() <= MAX_ZOOM);
    }

    #[test]
    fn fit_rect_centres_offset_extent() {
        let mut v = View::new();
        v.fit_rect([-500.0, 1000.0, 1500.0, 2000.0], SCREEN, 0.0);
        let a = v.image_to_screen((-500.0, 1000.0), rect(), 10.0);
        let b = v.image_to_screen((1500.0, 2000.0), rect(), 10.0);
        assert!(
            (a.x - 0.0).abs() < 0.01 && (b.x - 1920.0).abs() < 0.01,
            "{a:?} {b:?}"
        );
        assert!(((a.y + b.y) / 2.0 - 540.0).abs() < 0.01);
    }

    #[test]
    fn zoom_to_rect_fills_screen() {
        let mut v = View::new();
        v.fit(&geom(), SCREEN, 0.0);
        v.zoom_to_rect([1000.0, 1000.0, 3000.0, 2000.0], SCREEN, 10.0);
        let a = v.image_to_screen((1000.0, 1000.0), rect(), 20.0);
        let b = v.image_to_screen((3000.0, 2000.0), rect(), 20.0);
        assert!(a.x >= -0.01 && a.y >= -0.01 && b.x <= 1920.01 && b.y <= 1080.01);
        assert!(((b.x - a.x) - 1920.0).abs() < 0.01 || ((b.y - a.y) - 1080.0).abs() < 0.01);
    }

    #[test]
    fn pyramid_geometry_rules() {
        let g = PyramidGeometry::new(1000, 600, 256);
        assert_eq!(g.level_dims(0), (1000, 600));
        assert_eq!(g.level_dims(1), (500, 300));
        assert_eq!(g.level_dims(2), (250, 150));
        assert_eq!(g.levels, 3);
        assert_eq!(g.tiles(0), (4, 3));
        assert_eq!(g.level_dims(3), (125, 75));
        assert_eq!(PyramidGeometry::new(256, 256, 256).levels, 1);
        assert_eq!(PyramidGeometry::new(257, 10, 256).levels, 2);
    }

    #[test]
    fn pick_level_examples() {
        assert_eq!(pick_level(1.0, 10), 0);
        assert_eq!(pick_level(8.0, 10), 0);
        assert_eq!(pick_level(0.5, 10), 1);
        assert_eq!(pick_level(0.24, 10), 2);
        assert_eq!(pick_level(1e-9, 10), 9);
    }

    #[test]
    fn visible_tiles_clamped_to_grid() {
        let g = PyramidGeometry::new(1000, 600, 256);
        let t = visible_tiles(&g, 0, [-500.0, -500.0, 300.0, 100.0]);
        assert_eq!(t.len(), 2);
        assert!(t.contains(&TileKey {
            level: 0,
            tx: 0,
            ty: 0
        }));
        assert!(t.contains(&TileKey {
            level: 0,
            tx: 1,
            ty: 0
        }));
        assert!(visible_tiles(&g, 0, [2000.0, 0.0, 3000.0, 10.0]).is_empty());
        // Level 1 tiles cover 512 image px.
        let t1 = visible_tiles(&g, 1, [0.0, 0.0, 1000.0, 600.0]);
        assert_eq!(t1.len(), 2 * 2);
    }

    #[test]
    fn visible_tiles_bounded_by_screen() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let fit = v.zoom_target();
        // With floor(log2(1/zoom)) a level-l tile spans 128..=256 screen px,
        // so the per-axis tile count is bounded by 2x the 256-px figure.
        let bound =
            ((2.0 * 1920.0f64 / 256.0 + 2.0) * (2.0 * 1080.0f64 / 256.0 + 2.0)).floor() as usize;
        for centre in [(35_000.0, 24_000.0), (0.0, 0.0), (69_999.0, 47_999.0)] {
            for i in 0..=100 {
                let z = fit * (MAX_ZOOM / fit).powf(i as f64 / 100.0);
                let mut v2 = View::new();
                v2.fit(&g, SCREEN, 0.0);
                v2.set_view(centre, z, 100.0);
                let level = pick_level(z, g.levels);
                let r = v2.visible_image_rect(rect(), 100.0);
                let n = visible_tiles(&g, level, r).len();
                assert!(n <= bound, "zoom {z} level {level}: {n} > {bound}");
            }
        }
        let _ = v;
    }
}
