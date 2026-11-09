//! The tiled deep-zoom viewer widget.
//!
//! Tiles are fetched from a [`TileSource`] on background loader threads; the
//! UI thread only queues requests, uploads finished tiles (bounded per frame)
//! and draws. Superseded work is dropped as early as possible: the queue
//! holds only the newest wanted set (older requests are never fetched), and
//! a finished tile of a superseded generation never takes upload budget
//! from the current one (at most it replaces an older fallback, and is
//! dropped once over budget). While a fine tile is not loaded, the nearest
//! loaded ancestor's sub-rect stands in for it, so zooming never shows
//! holes.
//!
//! The tile cache grows with the wanted set (1.5x, from 512 up to
//! [`MAX_CAPACITY`](super::cache::MAX_CAPACITY) tiles, about 512 MB of GPU textures at 256-pixel
//! tiles) so a large viewport never evicts tiles it still draws.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use egui::{
    Color32, Event, MouseWheelUnit, Pos2, Rect, Sense, Stroke, TextureHandle, TextureId,
    TextureOptions, Ui, Vec2,
};

use super::cache::{DEFAULT_CAPACITY, TileCache};
use super::{Fetched, PyramidGeometry, TileKey, TileSource, View, pick_level, visible_tiles};
use crate::canvas_layers::{
    DrawStep, Overlay, Patch, Tool, drawing_rect, follow_draw, paint_layers,
};
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
    /// A rectangle the user finished drawing this frame with
    /// [`Tool::DrawRect`]: `[x0, y0, x1, y1]` in image coordinates,
    /// normalised (`x0 <= x1`, `y0 <= y1`) and clamped to the image.
    pub drawn_rect: Option<[f64; 4]>,
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
    /// The UI context, woken after each finished fetch.
    ctx: OnceLock<egui::Context>,
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
    /// Loader threads asked for.
    threads: usize,
    /// Fetch on the calling thread inside `show` (tests only).
    synchronous: bool,
}

fn fetch_guarded(source: &dyn TileSource, key: TileKey) -> Fetched {
    // A panicking source must not strand the request (and the repaint loop).
    catch_unwind(AssertUnwindSafe(|| source.fetch(key))).unwrap_or(Fetched::Missing)
}

impl Loader {
    fn new(source: Arc<dyn TileSource>, threads: usize, synchronous: bool) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                shutdown: false,
            }),
            wake: Condvar::new(),
            ctx: OnceLock::new(),
        });
        let (tx, rx) = channel();
        let spawn = if synchronous { 0 } else { threads };
        for i in 0..spawn {
            let (shared, tx, source) = (shared.clone(), tx.clone(), source.clone());
            if let Err(e) = std::thread::Builder::new()
                .name(format!("mega-ui-tile-loader-{i}"))
                .spawn(move || Self::worker(&shared, &tx, &*source))
            {
                // Requests stay queued for the threads that did start (if
                // none did, tiles never load); never fall back to fetching on
                // the UI thread.
                tracing::error!("tile loader thread {i} failed to start: {e}");
            }
        }
        Self {
            shared,
            tx,
            rx,
            source,
            threads,
            synchronous,
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
            if let Some(ctx) = shared.ctx.get() {
                ctx.request_repaint();
            }
        }
    }

    /// Take back every request not yet picked up by a loader.
    fn take_queue(&self) -> Vec<Request> {
        self.shared.lock().items.drain(..).collect()
    }

    /// Remember the UI context so loaders can wake it.
    fn set_ctx(&self, ctx: &egui::Context) {
        self.shared.ctx.get_or_init(|| ctx.clone());
    }

    /// Queue `items` (in order) and wake the loaders; in synchronous mode,
    /// fetch them now.
    fn set_queue(&self, items: Vec<Request>) {
        if self.synchronous {
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
    ZoomTo([f64; 4]),
}

/// Deep-zoom viewer over a [`TileSource`]: drag pans (or draws a rectangle
/// with [`Tool::DrawRect`]), the scroll wheel zooms about the cursor,
/// double-click zooms in 2x, and `+` / `−` / `Home` buttons sit top-left.
/// [`Patch`] images and [`Overlay`] rectangles are drawn over the image.
pub struct TiledViewer {
    source: Arc<dyn TileSource>,
    geom: PyramidGeometry,
    view: View,
    loader: Loader,
    cache: TileCache<CachedTile>,
    /// Requests queued, in flight or awaiting upload.
    requested: HashSet<Request>,
    /// Finished fetches not uploaded yet: the newest per tile.
    arrived: HashMap<TileKey, (u64, Fetched)>,
    /// Snap to the home view on the next frame (first frame, new geometry).
    needs_home: bool,
    commands: Vec<Command>,
    pixels_per_point: f32,
    last_frame: Option<(Rect, f64)>,
    tool: Tool,
    /// The rectangle being drawn: its fixed corner and the current one, in
    /// image coordinates.
    drawing: Option<((f64, f64), (f64, f64))>,
    overlays: Vec<Overlay>,
    patches: Vec<Patch>,
    /// See [`TiledViewer::set_presentation`].
    presentation: bool,
    /// See [`TiledViewer::set_opacity`].
    opacity: f32,
    /// Set once any tile has been painted.
    drawn_any: bool,
}

impl TiledViewer {
    /// A viewer over `source` with `loader_threads` background loaders (at
    /// least one). Fetches never run on the UI thread.
    pub fn new(source: Arc<dyn TileSource>, loader_threads: usize) -> Self {
        Self::build(source, loader_threads.max(1), false)
    }

    /// Test-only: a viewer that fetches synchronously inside
    /// [`TiledViewer::show`], with no loader threads. Deterministic, but it
    /// blocks the UI thread on every fetch; never use it in an app.
    pub fn new_synchronous(source: Arc<dyn TileSource>) -> Self {
        Self::build(source, 0, true)
    }

    fn build(source: Arc<dyn TileSource>, threads: usize, synchronous: bool) -> Self {
        Self {
            geom: source.geometry(),
            loader: Loader::new(source.clone(), threads, synchronous),
            source,
            view: View::new(),
            cache: TileCache::new(DEFAULT_CAPACITY),
            requested: HashSet::new(),
            arrived: HashMap::new(),
            needs_home: true,
            commands: Vec::new(),
            pixels_per_point: 1.0,
            last_frame: None,
            tool: Tool::Navigate,
            drawing: None,
            overlays: Vec::new(),
            patches: Vec::new(),
            presentation: false,
            opacity: 1.0,
            drawn_any: false,
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
        self.loader = Loader::new(source.clone(), self.loader.threads, self.loader.synchronous);
        self.source = source;
        self.cache.clear();
        self.requested.clear();
        self.arrived.clear();
    }

    /// The current view.
    pub fn view(&self) -> &View {
        &self.view
    }

    /// Presentation mode: the viewer only senses hover (no pan, zoom,
    /// double-click or drawing), shows no overlay buttons, fills no
    /// background and draws stale tiles at full brightness. For a camera
    /// driven by code through [`TiledViewer::view_mut`].
    pub fn set_presentation(&mut self, on: bool) {
        self.presentation = on;
        self.drawing = None;
    }

    /// Multiply every tile's tint alpha by `opacity` (0 to 1), for
    /// crossfades.
    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    /// The view, for a camera driven by code.
    pub fn view_mut(&mut self) -> &mut View {
        &mut self.view
    }

    /// Start at `centre` and `zoom` in a viewport of `screen` size instead
    /// of the home fit: the next [`TiledViewer::show`] draws this camera.
    /// View commands queued before it ([`TiledViewer::home`], the zooms,
    /// [`TiledViewer::zoom_to_rect`]) are dropped, so nothing moves the
    /// view away from it. Home still fits the whole image.
    pub fn set_camera(&mut self, screen: Vec2, centre: (f64, f64), zoom: f64) {
        self.view.fit(&self.geom, screen, 0.0);
        self.view.finish_animation();
        self.view.set_view(centre, zoom, 0.0);
        self.needs_home = false;
        self.commands.clear();
    }

    /// The tile opacity set with [`TiledViewer::set_opacity`].
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// True once any tile has been painted.
    pub fn has_drawn_tiles(&self) -> bool {
        self.drawn_any
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

    /// Animate so `rect` (`[x0, y0, x1, y1]`, image coordinates) fills the
    /// viewport.
    pub fn zoom_to_rect(&mut self, rect: [f64; 4]) {
        self.commands.push(Command::ZoomTo(rect));
    }

    /// What a drag does from now on (a rectangle being drawn is dropped).
    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.drawing = None;
    }

    /// The current tool.
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// Replace the rectangles drawn over the image.
    pub fn set_overlays(&mut self, overlays: Vec<Overlay>) {
        self.overlays = overlays;
    }

    /// Replace the images drawn over the tiles (under the overlays).
    pub fn set_patches(&mut self, patches: Vec<Patch>) {
        self.patches = patches;
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

    /// Tiles requested for `rect` at `now`, coarse to fine: every level from
    /// the top down to the one drawn (progressive refinement; each coarser
    /// level adds about a quarter of the drawn level's count).
    pub(crate) fn wanted_tiles(&self, rect: Rect, now: f64) -> Vec<TileKey> {
        let (level, fine) = self.fine_tiles(rect, now);
        let top = self.geom.levels.saturating_sub(1);
        let r = self.view.visible_image_rect(rect, now);
        let mut out = Vec::new();
        for l in (level + 1..=top).rev() {
            out.extend(visible_tiles(&self.geom, l, r));
        }
        out.extend(fine);
        out
    }

    /// Draw the viewer filling `ui`'s available space and handle input.
    pub fn show(&mut self, ui: &mut Ui) -> ViewerResponse {
        let sense = if self.presentation {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        };
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), sense);
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
        self.view.update(now);
        for c in std::mem::take(&mut self.commands) {
            match c {
                Command::Home => self.view.fit(&self.geom, rect.size(), now),
                Command::Zoom(f) => self.view.zoom_by(f, rect.center(), rect, now),
                Command::ZoomTo(r) => self.view.zoom_to_rect(r, rect.size(), now),
            }
        }
        let drawn_rect = if self.presentation {
            None
        } else {
            self.handle_input(ui, &response, rect, now)
        };

        let generation = self.source.generation();
        let wanted = self.wanted_tiles(rect, now);
        self.cache
            .ensure_capacity(DEFAULT_CAPACITY.max(wanted.len() * 3 / 2));
        self.loader.set_ctx(ui.ctx());
        self.request(&wanted, generation);
        let backlog = self.receive(ui.ctx(), generation);
        self.paint(&ui.painter_at(rect), rect, now, generation);
        self.paint_overlays(&ui.painter_at(rect), rect, now);
        if !self.presentation {
            self.overlay_buttons(ui, rect);
        }

        // Loaders wake the UI when a tile arrives; repaint here only for
        // motion, pending commands and an upload backlog.
        if !self.view.is_settled(now) || !self.commands.is_empty() || backlog {
            ui.ctx().request_repaint();
        }
        ViewerResponse {
            hovered_image_pos: response
                .hover_pos()
                .map(|p| self.view.screen_to_image(p, rect, now)),
            drawn_rect,
        }
    }

    /// Follow a [`Tool::DrawRect`] drag; returns the rect once released.
    fn handle_draw(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        rect: Rect,
        now: f64,
    ) -> Option<[f64; 4]> {
        let geom = self.geom;
        let view = &self.view;
        let clamp = |p: Pos2| {
            let (x, y) = view.screen_to_image(p, rect, now);
            (
                x.clamp(0.0, geom.width as f64),
                y.clamp(0.0, geom.height as f64),
            )
        };
        match follow_draw(ui, response, &mut self.drawing, &clamp) {
            DrawStep::Idle => None,
            DrawStep::Cancelled => {
                self.set_tool(Tool::Navigate);
                None
            }
            DrawStep::Done(r) => {
                self.tool = Tool::Navigate;
                Some(r)
            }
        }
    }

    fn handle_input(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        rect: Rect,
        now: f64,
    ) -> Option<[f64; 4]> {
        let mut drawn = None;
        match self.tool {
            Tool::DrawRect => drawn = self.handle_draw(ui, response, rect, now),
            Tool::Navigate if response.dragged() => {
                self.view.pan_by(drag_delta(response), now);
            }
            Tool::Navigate => {}
        }
        if self.tool == Tool::Navigate
            && drawn.is_none()
            && response.double_clicked()
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
        drawn
    }

    /// The patches, the overlays, and the rectangle being drawn.
    fn paint_overlays(&self, painter: &egui::Painter, rect: Rect, now: f64) {
        let screen = |r: [f64; 4]| {
            Rect::from_two_pos(
                self.view.image_to_screen((r[0], r[1]), rect, now),
                self.view.image_to_screen((r[2], r[3]), rect, now),
            )
        };
        paint_layers(
            painter,
            rect,
            &screen,
            &self.patches,
            &self.overlays,
            drawing_rect(self.drawing),
        );
    }

    /// Fetch and upload order of `key`: tiles with nothing cached first,
    /// coarse to fine (coverage), then refreshes of cached tiles, finest
    /// first (a cached fine tile is what shows; its ancestors are only
    /// fallbacks), longest stale first within a level (so a long drag
    /// refreshes every tile in turn, not the same top rows each frame).
    fn priority(&self, key: TileKey) -> (bool, i64, u64) {
        let level = i64::from(key.level);
        match self.cache.generation_of(key) {
            None => (false, -level, 0),
            Some(g) => (true, level, g),
        }
    }

    /// Replace the loader queue with the wanted tiles not yet loaded at
    /// `generation` (requests no longer wanted, or of an older generation,
    /// are dropped unfetched), in [`Self::priority`] order.
    fn request(&mut self, wanted: &[TileKey], generation: u64) {
        for r in self.loader.take_queue() {
            self.requested.remove(&r);
        }
        let mut new: Vec<Request> = wanted
            .iter()
            .map(|&k| (k, generation))
            .filter(|r| !self.cache.has_current(r.0, generation) && !self.requested.contains(r))
            .collect();
        new.sort_by_key(|r| self.priority(r.0)); // stable: keeps the grid order
        self.requested.extend(new.iter().copied());
        self.loader.set_queue(new);
    }

    /// Take every finished fetch and upload at most
    /// [`MAX_UPLOADS_PER_FRAME`] textures: the current `generation` first
    /// (in [`Self::priority`] order), then superseded results newer than
    /// what is cached (a better fallback). Current results over budget wait
    /// for the next frame; superseded ones over budget are dropped, so they
    /// never pile up ahead of current work. Returns true if results wait.
    fn receive(&mut self, ctx: &egui::Context, generation: u64) -> bool {
        // Draining is cheap (no upload): keep only the newest per tile.
        while let Ok((key, g, fetched)) = self.loader.rx.try_recv() {
            if self.arrived.get(&key).is_some_and(|(old, _)| *old > g) {
                self.requested.remove(&(key, g));
                continue;
            }
            if let Some((old, _)) = self.arrived.insert(key, (g, fetched))
                && old != g
            {
                self.requested.remove(&(key, old));
            }
        }
        let mut order: Vec<Request> = self.arrived.iter().map(|(&k, &(g, _))| (k, g)).collect();
        order.sort_by_key(|&(k, g)| (g != generation, Reverse(g), self.priority(k), k));
        let mut uploads = 0;
        for (key, g) in order {
            let costs = !matches!(self.arrived.get(&key), Some((_, Fetched::Missing)));
            let fits = !costs || uploads < MAX_UPLOADS_PER_FRAME;
            if !fits && g == generation {
                continue; // current: next frame
            }
            let Some((_, fetched)) = self.arrived.remove(&key) else {
                continue;
            };
            self.requested.remove(&(key, g));
            let newer = self.cache.generation_of(key).is_none_or(|c| c < g);
            if !fits || !newer {
                continue; // superseded and over budget, or no better
            }
            let mut upload = |img: egui::ColorImage| {
                uploads += 1;
                let name = format!("tile-{}-{}-{}", key.level, key.tx, key.ty);
                Some(ctx.load_texture(name, img, texture_options(key.level)))
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
            self.cache.insert(key, g, tile);
        }
        !self.arrived.is_empty()
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

    /// Nearest loaded tile at or above `key`: its texture (`None` for a
    /// current `Missing`), stale flag and image rect. A `Missing` from an
    /// older generation is skipped so it never hides a coarser image.
    pub(crate) fn loaded_ancestor(
        &mut self,
        key: TileKey,
        generation: u64,
    ) -> Option<(Option<TextureId>, bool, [f64; 4])> {
        let mut k = key;
        loop {
            if let Some((g, tile)) = self.cache.get(k) {
                let found = (tile.texture.as_ref().map(TextureHandle::id), tile.stale);
                if found.0.is_some() || g == generation {
                    return Some((found.0, found.1, self.tile_image_rect(k)));
                }
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
    fn paint(&mut self, painter: &egui::Painter, rect: Rect, now: f64, generation: u64) {
        if !self.presentation {
            painter.rect_filled(rect, 0.0, palette::SURFACE_900);
        }
        let (_, fine) = self.fine_tiles(rect, now);
        for key in fine {
            let Some((Some(tex), stale, a)) = self.loaded_ancestor(key, generation) else {
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
            let dim = stale && !self.presentation;
            let tint = if dim { stale_tint() } else { Color32::WHITE };
            painter.image(tex, screen, uv, tint.gamma_multiply(self.opacity));
            self.drawn_any = true;
            if dim {
                hatch(&painter.with_clip_rect(screen.intersect(rect)), screen);
            }
        }
    }

    fn overlay_buttons(&mut self, ui: &mut Ui, rect: Rect) {
        let buttons = [
            OverlayButton::new("+", 28.0, "Zoom in"),
            OverlayButton::new("\u{2212}", 28.0, "Zoom out"),
            OverlayButton::new("Home", 60.0, "Fit the whole image"),
        ];
        let cmds = [Command::Zoom(2.0), Command::Zoom(0.5), Command::Home];
        if let Some(i) = overlay_buttons(ui, rect, &buttons) {
            self.commands.push(cmds[i]);
        }
    }
}

/// Screen motion of a drag this frame. On the frame egui recognises the
/// drag (after its movement threshold) this is the whole motion since the
/// press, so the grabbed point stays under the pointer.
pub(crate) fn drag_delta(response: &egui::Response) -> Vec2 {
    if response.drag_started() {
        response.total_drag_delta().unwrap_or_default()
    } else {
        response.drag_delta()
    }
}

/// One button of an overlay row (see [`overlay_buttons`]).
pub(crate) struct OverlayButton<'a> {
    pub label: &'a str,
    pub width: f32,
    pub tip: &'a str,
    pub enabled: bool,
}

impl<'a> OverlayButton<'a> {
    /// An enabled button.
    pub fn new(label: &'a str, width: f32, tip: &'a str) -> Self {
        Self {
            label,
            width,
            tip,
            enabled: true,
        }
    }
}

/// Draw a row of 28-px-high buttons over the top-left corner of `rect`;
/// returns the index of the one clicked this frame.
pub(crate) fn overlay_buttons(ui: &mut Ui, rect: Rect, buttons: &[OverlayButton]) -> Option<usize> {
    let mut x = rect.min.x + 8.0;
    let y = rect.min.y + 8.0;
    let mut clicked = None;
    for (i, b) in buttons.iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(b.width, 28.0));
        let resp = ui
            .scope_builder(
                egui::UiBuilder::new()
                    .max_rect(r)
                    .layout(egui::Layout::centered_and_justified(
                        egui::Direction::TopDown,
                    )),
                |ui| {
                    ui.add_enabled(
                        b.enabled,
                        egui::Button::new(b.label).wrap_mode(egui::TextWrapMode::Extend),
                    )
                },
            )
            .inner;
        if resp.on_hover_text(b.tip).clicked() {
            clicked = Some(i);
        }
        x += b.width + 4.0;
    }
    clicked
}

/// Texture sampling for a tile of `level`. Only level 0 is ever magnified as
/// the drawn level (zoom > 1), so it gets crisp `Nearest` magnification;
/// every other level is magnified only as a stretched ancestor fallback (or
/// minified when drawn) and is sampled `Linear`.
pub(crate) fn texture_options(level: u32) -> TextureOptions {
    if level == 0 {
        TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            ..TextureOptions::LINEAR
        }
    } else {
        TextureOptions::LINEAR
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
