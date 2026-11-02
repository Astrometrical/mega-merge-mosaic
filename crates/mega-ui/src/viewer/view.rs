//! Viewport maths: springs, view transform and pyramid tile selection.

use egui::{Pos2, Rect, Vec2};

/// Spring stiffness constant `k` (OpenSeadragon's default).
pub const SPRING_STIFFNESS: f64 = 6.5;
/// Seconds a spring takes to reach its target (OpenSeadragon's default).
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
/// image pixel), each on a spring. Zoom animates in log space.
#[derive(Clone, Debug)]
pub struct View {
    cx: Spring,
    cy: Spring,
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
            cx: Spring::new(0.0),
            cy: Spring::new(0.0),
            ln_zoom: Spring::new(0.0),
            min_zoom: 1e-9,
        }
    }

    /// Animate home: the whole image visible and centred on a `screen`-sized
    /// viewport. Also sets the minimum zoom (half the fit zoom).
    pub fn fit(&mut self, geom: &PyramidGeometry, screen: Vec2, now: f64) {
        let (w, h) = (geom.width as f64, geom.height as f64);
        let z = (f64::from(screen.x) / w).min(f64::from(screen.y) / h);
        self.min_zoom = z / 2.0;
        self.cx.spring_to(w / 2.0, now);
        self.cy.spring_to(h / 2.0, now);
        self.ln_zoom.spring_to(z.ln(), now);
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
        (self.cx.value(now), self.cy.value(now))
    }

    /// True when no spring is still animating.
    pub fn is_settled(&self, now: f64) -> bool {
        self.cx.is_settled(now) && self.cy.is_settled(now) && self.ln_zoom.is_settled(now)
    }

    /// Jump to `centre` and `zoom` (clamped) with no animation.
    pub fn set_view(&mut self, centre: (f64, f64), zoom: f64, _now: f64) {
        self.cx.reset_to(centre.0);
        self.cy.reset_to(centre.1);
        self.ln_zoom.reset_to(self.clamp_zoom(zoom).ln());
    }

    fn clamp_zoom(&self, z: f64) -> f64 {
        z.clamp(self.min_zoom, MAX_ZOOM.max(self.min_zoom))
    }

    /// Multiply the target zoom by `factor`, keeping the image point under
    /// `about_screen` fixed at the target.
    pub fn zoom_by(&mut self, factor: f64, about_screen: Pos2, screen_rect: Rect, now: f64) {
        let old_z = self.zoom_target();
        let new_z = self.clamp_zoom(old_z * factor);
        let off = about_screen - screen_rect.center();
        let (ox, oy) = (f64::from(off.x), f64::from(off.y));
        // Image point under the cursor at the target view.
        let px = self.cx.target + ox / old_z;
        let py = self.cy.target + oy / old_z;
        self.cx.spring_to(px - ox / new_z, now);
        self.cy.spring_to(py - oy / new_z, now);
        self.ln_zoom.spring_to(new_z.ln(), now);
    }

    /// Drag the image by `screen_delta` screen pixels, immediately.
    pub fn pan_by(&mut self, screen_delta: Vec2, now: f64) {
        let z = self.zoom(now);
        let c = self.centre(now);
        self.cx.reset_to(c.0 - f64::from(screen_delta.x) / z);
        self.cy.reset_to(c.1 - f64::from(screen_delta.y) / z);
        let lz = self.ln_zoom.value(now);
        self.ln_zoom.reset_to(lz);
    }

    /// Animate so `image_rect` (`[x0, y0, x1, y1]`) fills a `screen`-sized
    /// viewport.
    pub fn zoom_to_rect(&mut self, image_rect: [f64; 4], screen: Vec2, now: f64) {
        let w = (image_rect[2] - image_rect[0]).max(1e-9);
        let h = (image_rect[3] - image_rect[1]).max(1e-9);
        let z = self.clamp_zoom((f64::from(screen.x) / w).min(f64::from(screen.y) / h));
        self.cx
            .spring_to((image_rect[0] + image_rect[2]) / 2.0, now);
        self.cy
            .spring_to((image_rect[1] + image_rect[3]) / 2.0, now);
        self.ln_zoom.spring_to(z.ln(), now);
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
    fn pan_by_is_immediate() {
        let g = geom();
        let mut v = View::new();
        v.fit(&g, SCREEN, 0.0);
        let p0 = v.screen_to_image(Pos2::new(500.0, 500.0), rect(), 10.0);
        v.pan_by(Vec2::new(100.0, 0.0), 10.0);
        let p1 = v.screen_to_image(Pos2::new(500.0, 500.0), rect(), 10.0);
        let z = v.zoom(10.0);
        assert!((p0.0 - p1.0 - 100.0 / z).abs() < 1e-6);
        assert!((p0.1 - p1.1).abs() < 1e-9);
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
