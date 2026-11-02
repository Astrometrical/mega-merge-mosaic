//! The tiled deep-zoom viewer widget.
//!
//! Tiles are fetched from a [`TileSource`] on background loader threads; the
//! UI thread only queues requests, uploads finished tiles (bounded per frame)
//! and draws. While a fine tile is not loaded, the nearest loaded ancestor's
//! sub-rect stands in for it, so zooming never shows holes.

use std::collections::{HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use egui::{
    Color32, Event, MouseWheelUnit, Pos2, Rect, Sense, Stroke, TextureHandle, TextureId,
    TextureOptions, Ui, Vec2,
};

use super::cache::{DEFAULT_CAPACITY, TileCache};
use super::{Fetched, PyramidGeometry, TileKey, TileSource, View, pick_level, visible_tiles};
use crate::theme::palette;

/// Most tile textures uploaded per frame.
const MAX_UPLOADS_PER_FRAME: usize = 32;
/// Zoom factor per scroll-wheel notch.
const SCROLL_ZOOM_PER_NOTCH: f64 = 1.2;
/// Brightness of a stale tile (spec §5.4: dimmed and hatched).
const STALE_BRIGHTNESS: f32 = 0.55;
/// Screen spacing of the stale hatch lines, in points.
const HATCH_SPACING: f32 = 10.0;

/// What the viewer reports back each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewerResponse {
    /// Image point (level-0 pixels) under the pointer, if hovered.
    pub hovered_image_pos: Option<(f64, f64)>,
}

/// Tint a tile is drawn with: white for `Valid`, ~55% grey for `Stale`,
/// transparent for `Missing` (the background shows).
pub fn tile_tint(f: &Fetched) -> Color32 {
    match f {
        Fetched::Valid(_) => Color32::WHITE,
        Fetched::Stale(_) => stale_tint(),
        Fetched::Missing => Color32::TRANSPARENT,
    }
}

fn stale_tint() -> Color32 {
    Color32::from_gray((255.0 * STALE_BRIGHTNESS).round() as u8)
}

/// A request: a tile at a source generation.
type Request = (TileKey, u64);
/// A finished fetch.
type Loaded = (TileKey, u64, Fetched);

struct Queue {
    items: VecDeque<Request>,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Background fetchers fed by a queue holding only the newest wanted set.
struct Loader {
    shared: Arc<Shared>,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    source: Arc<dyn TileSource>,
    /// Threads running; 0 = fetch synchronously in `show` (tests).
    threads: usize,
}

fn fetch_guarded(source: &dyn TileSource, key: TileKey) -> Fetched {
    // A panicking source must not strand the request (and the repaint loop).
    catch_unwind(AssertUnwindSafe(|| source.fetch(key))).unwrap_or(Fetched::Missing)
}

impl Loader {
    fn new(source: Arc<dyn TileSource>, threads: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
        });
        let (tx, rx) = channel();
        let mut spawned = 0;
        for i in 0..threads {
            let (shared, tx, source) = (shared.clone(), tx.clone(), source.clone());
            let ok = std::thread::Builder::new()
                .name(format!("mega-ui-tile-loader-{i}"))
                .spawn(move || Self::worker(&shared, &tx, &*source))
                .is_ok();
            spawned += usize::from(ok);
        }
        Self {
            shared,
            tx,
            rx,
            source,
            threads: spawned,
        }
    }

    fn worker(shared: &Shared, tx: &Sender<Loaded>, source: &dyn TileSource) {
        loop {
            let (key, generation) = {
                let mut q = shared.lock();
                loop {
                    if q.shutdown {
                        return;
                    }
                    if let Some(r) = q.items.pop_front() {
                        break r;
                    }
                    q = shared.wake.wait(q).unwrap_or_else(PoisonError::into_inner);
                }
            };
            let fetched = fetch_guarded(source, key);
            if tx.send((key, generation, fetched)).is_err() {
                return;
            }
        }
    }

    /// Take back every request not yet picked up by a loader.
    fn take_queue(&self) -> Vec<Request> {
        self.shared.lock().items.drain(..).collect()
    }

    /// Queue `items` (in order) and wake the loaders; with no loader threads,
    /// fetch them now.
    fn set_queue(&self, items: Vec<Request>) {
        if self.threads == 0 {
            for (key, generation) in items {
                let fetched = fetch_guarded(&*self.source, key);
                let _ = self.tx.send((key, generation, fetched));
            }
            return;
        }
        self.shared.lock().items.extend(items);
        self.shared.wake.notify_all();
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        // Loaders exit after their current fetch; never join (the UI thread
        // never waits on I/O).
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
    }
}

/// A loaded tile: its texture (none for `Missing`) and whether it is stale.
struct CachedTile {
    texture: Option<TextureHandle>,
    stale: bool,
}

/// Viewer command from a button or the host app, applied in `show`.
#[derive(Clone, Copy, Debug)]
enum Command {
    Home,
    Zoom(f64),
}

/// Deep-zoom viewer over a [`TileSource`]: drag pans, the scroll wheel zooms
/// about the cursor, double-click zooms in 2x, and `+` / `−` / `Home`
/// buttons sit top-left.
pub struct TiledViewer {
    source: Arc<dyn TileSource>,
    geom: PyramidGeometry,
    view: View,
    loader: Loader,
    cache: TileCache<CachedTile>,
    /// Requests queued, in flight or awaiting upload.
    requested: HashSet<Request>,
    /// Snap to the home view on the next frame (first frame, new geometry).
    needs_home: bool,
    commands: Vec<Command>,
    pixels_per_point: f32,
    last_frame: Option<(Rect, f64)>,
}

impl TiledViewer {
    /// A viewer over `source` with `loader_threads` background loaders. With
    /// 0 loader threads tiles are fetched synchronously inside
    /// [`TiledViewer::show`], which is deterministic but blocks the UI thread:
    /// meant for tests only.
    pub fn new(source: Arc<dyn TileSource>, loader_threads: usize) -> Self {
        Self {
            geom: source.geometry(),
            loader: Loader::new(source.clone(), loader_threads),
            source,
            view: View::new(),
            cache: TileCache::new(DEFAULT_CAPACITY),
            requested: HashSet::new(),
            needs_home: true,
            commands: Vec::new(),
            pixels_per_point: 1.0,
            last_frame: None,
        }
    }

    /// Replace the source. The view is kept when the geometry is unchanged
    /// and reset to home otherwise; cached tiles are dropped.
    pub fn set_source(&mut self, source: Arc<dyn TileSource>) {
        let geom = source.geometry();
        if geom != self.geom {
            self.needs_home = true;
        }
        self.geom = geom;
        self.loader = Loader::new(source.clone(), self.loader.threads);
        self.source = source;
        self.cache.clear();
        self.requested.clear();
    }

    /// The current view.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// True while tile requests are queued, in flight or awaiting upload.
    pub fn is_loading(&self) -> bool {
        !self.requested.is_empty()
    }

    /// Animate to the home view (whole image visible).
    pub fn home(&mut self) {
        self.commands.push(Command::Home);
    }

    /// Zoom in 2x about the viewport centre.
    pub fn zoom_in(&mut self) {
        self.commands.push(Command::Zoom(2.0));
    }

    /// Zoom out 2x about the viewport centre.
    pub fn zoom_out(&mut self) {
        self.commands.push(Command::Zoom(0.5));
    }

    /// Viewport rect and `ui.input().time` of the last frame shown, for
    /// mapping between screen and image with [`TiledViewer::view`].
    pub fn last_frame(&self) -> Option<(Rect, f64)> {
        self.last_frame
    }

    /// The level drawn and its tiles visible in `rect` at `now`.
    fn fine_tiles(&self, rect: Rect, now: f64) -> (u32, Vec<TileKey>) {
        let zoom = self.view.zoom(now) * f64::from(self.pixels_per_point);
        let level = pick_level(zoom, self.geom.levels);
        let r = self.view.visible_image_rect(rect, now);
        (level, visible_tiles(&self.geom, level, r))
    }

    /// Tiles requested for `rect` at `now`, coarse to fine: the coarsest
    /// level (an always-available fallback) then the level drawn.
    pub(crate) fn wanted_tiles(&self, rect: Rect, now: f64) -> Vec<TileKey> {
        let (level, fine) = self.fine_tiles(rect, now);
        let top = self.geom.levels.saturating_sub(1);
        if level == top {
            return fine;
        }
        let r = self.view.visible_image_rect(rect, now);
        let mut out = visible_tiles(&self.geom, top, r);
        out.extend(fine);
        out
    }

    /// Draw the viewer filling `ui`'s available space and handle input.
    pub fn show(&mut self, ui: &mut Ui) -> ViewerResponse {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let now = ui.input(|i| i.time);
        self.pixels_per_point = ui.ctx().pixels_per_point();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return ViewerResponse::default();
        }
        self.last_frame = Some((rect, now));

        if self.needs_home {
            self.view.fit(&self.geom, rect.size(), now);
            self.view.finish_animation();
            self.needs_home = false;
        }
        for c in std::mem::take(&mut self.commands) {
            match c {
                Command::Home => self.view.fit(&self.geom, rect.size(), now),
                Command::Zoom(f) => self.view.zoom_by(f, rect.center(), rect, now),
            }
        }
        self.handle_input(ui, &response, rect, now);

        let generation = self.source.generation();
        let wanted = self.wanted_tiles(rect, now);
        self.request(&wanted, generation);
        self.receive(ui.ctx());
        self.paint(&ui.painter_at(rect), rect, now);
        self.overlay_buttons(ui, rect);

        if !self.view.is_settled(now) || self.is_loading() || !self.commands.is_empty() {
            ui.ctx().request_repaint();
        }
        ViewerResponse {
            hovered_image_pos: response
                .hover_pos()
                .map(|p| self.view.screen_to_image(p, rect, now)),
        }
    }

    fn handle_input(&mut self, ui: &Ui, response: &egui::Response, rect: Rect, now: f64) {
        if response.dragged() {
            self.view.pan_by(response.drag_delta(), now);
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.view.zoom_by(2.0, p, rect, now);
        }
        if let Some(p) = response.hover_pos() {
            let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
            let notches: f32 = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        Event::MouseWheel { unit, delta, .. } => Some(match unit {
                            MouseWheelUnit::Line => delta.y,
                            MouseWheelUnit::Point => delta.y / line.max(1.0),
                            MouseWheelUnit::Page => delta.y * rect.height() / line.max(1.0),
                        }),
                        _ => None,
                    })
                    .sum()
            });
            if notches != 0.0 {
                let factor = SCROLL_ZOOM_PER_NOTCH.powf(f64::from(notches));
                self.view.zoom_by(factor, p, rect, now);
            }
        }
    }

    /// Replace the loader queue with the wanted tiles not yet loaded at
    /// `generation` (requests no longer wanted are dropped unfetched).
    fn request(&mut self, wanted: &[TileKey], generation: u64) {
        for r in self.loader.take_queue() {
            self.requested.remove(&r);
        }
        let new: Vec<Request> = wanted
            .iter()
            .map(|&k| (k, generation))
            .filter(|r| !self.cache.has_current(r.0, generation) && !self.requested.contains(r))
            .collect();
        self.requested.extend(new.iter().copied());
        self.loader.set_queue(new);
    }

    /// Upload finished tiles, at most [`MAX_UPLOADS_PER_FRAME`] textures.
    fn receive(&mut self, ctx: &egui::Context) {
        let mut uploads = 0;
        while uploads < MAX_UPLOADS_PER_FRAME {
            let Ok((key, generation, fetched)) = self.loader.rx.try_recv() else {
                break;
            };
            self.requested.remove(&(key, generation));
            let mut upload = |img: egui::ColorImage| {
                uploads += 1;
                let name = format!("tile-{}-{}-{}", key.level, key.tx, key.ty);
                Some(ctx.load_texture(name, img, texture_options()))
            };
            let tile = match fetched {
                Fetched::Valid(img) => CachedTile {
                    texture: upload(img),
                    stale: false,
                },
                Fetched::Stale(img) => CachedTile {
                    texture: upload(img),
                    stale: true,
                },
                Fetched::Missing => CachedTile {
                    texture: None,
                    stale: false,
                },
            };
            self.cache.insert(key, generation, tile);
        }
    }

    /// Image rect (level-0 pixels) covered by `key`.
    fn tile_image_rect(&self, key: TileKey) -> [f64; 4] {
        let scale = (1u64 << key.level) as f64;
        let t = u64::from(self.geom.tile.max(1));
        let (lw, lh) = self.geom.level_dims(key.level);
        let (x0, y0) = (u64::from(key.tx) * t, u64::from(key.ty) * t);
        let (x1, y1) = ((x0 + t).min(lw), (y0 + t).min(lh));
        [
            x0 as f64 * scale,
            y0 as f64 * scale,
            x1 as f64 * scale,
            y1 as f64 * scale,
        ]
    }

    /// Nearest loaded tile at or above `key`: its texture (if not `Missing`),
    /// stale flag and image rect.
    fn loaded_ancestor(&mut self, key: TileKey) -> Option<(Option<TextureId>, bool, [f64; 4])> {
        let mut k = key;
        loop {
            if let Some((_, tile)) = self.cache.get(k) {
                let found = (tile.texture.as_ref().map(TextureHandle::id), tile.stale);
                return Some((found.0, found.1, self.tile_image_rect(k)));
            }
            if k.level + 1 >= self.geom.levels {
                return None;
            }
            k = TileKey {
                level: k.level + 1,
                tx: k.tx / 2,
                ty: k.ty / 2,
            };
        }
    }

    /// Coarse to fine: each visible tile of the drawn level shows itself or
    /// the matching sub-rect of its nearest loaded ancestor.
    fn paint(&mut self, painter: &egui::Painter, rect: Rect, now: f64) {
        painter.rect_filled(rect, 0.0, palette::SURFACE_900);
        let (_, fine) = self.fine_tiles(rect, now);
        for key in fine {
            let Some((Some(tex), stale, a)) = self.loaded_ancestor(key) else {
                continue; // nothing loaded, or Missing: background shows
            };
            let c = self.tile_image_rect(key);
            let screen = Rect::from_min_max(
                self.view.image_to_screen((c[0], c[1]), rect, now),
                self.view.image_to_screen((c[2], c[3]), rect, now),
            );
            let (aw, ah) = (a[2] - a[0], a[3] - a[1]);
            let uv = Rect::from_min_max(
                Pos2::new(((c[0] - a[0]) / aw) as f32, ((c[1] - a[1]) / ah) as f32),
                Pos2::new(((c[2] - a[0]) / aw) as f32, ((c[3] - a[1]) / ah) as f32),
            );
            let tint = if stale { stale_tint() } else { Color32::WHITE };
            painter.image(tex, screen, uv, tint);
            if stale {
                hatch(&painter.with_clip_rect(screen.intersect(rect)), screen);
            }
        }
    }

    fn overlay_buttons(&mut self, ui: &mut Ui, rect: Rect) {
        let mut x = rect.min.x + 8.0;
        let y = rect.min.y + 8.0;
        let buttons = [
            ("+", 28.0, Command::Zoom(2.0), "Zoom in"),
            ("\u{2212}", 28.0, Command::Zoom(0.5), "Zoom out"),
            ("Home", 52.0, Command::Home, "Fit the whole image"),
        ];
        for (label, w, cmd, tip) in buttons {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 28.0));
            if ui
                .put(r, egui::Button::new(label))
                .on_hover_text(tip)
                .clicked()
            {
                self.commands.push(cmd);
            }
            x += w + 4.0;
        }
    }
}

/// Texture sampling: smooth when minified, crisp pixels when magnified.
fn texture_options() -> TextureOptions {
    TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..TextureOptions::LINEAR
    }
}

/// Diagonal hatch over `r` in the border colour, on a screen-aligned grid so
/// neighbouring tiles' lines meet.
fn hatch(painter: &egui::Painter, r: Rect) {
    let stroke = Stroke::new(1.0, palette::BORDER);
    let start = ((r.min.x + r.min.y) / HATCH_SPACING).ceil() * HATCH_SPACING;
    let mut c = start;
    while c <= r.max.x + r.max.y {
        painter.line_segment(
            [
                Pos2::new(c - r.max.y, r.max.y),
                Pos2::new(c - r.min.y, r.min.y),
            ],
            stroke,
        );
        c += HATCH_SPACING;
    }
}

#[cfg(test)]
#[path = "widget_tests.rs"]
mod tests;
