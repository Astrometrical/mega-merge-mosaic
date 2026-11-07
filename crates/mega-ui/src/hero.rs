//! The start screen's hero: a starfield with the selected session's mosaic
//! drifting slowly behind it, crossfading between views, under gradients
//! that keep the text on the left readable.

use std::sync::Arc;

use egui::{Align, Color32, Layout, Mesh, Rect, Shape, Ui, UiBuilder, pos2, vec2};

use crate::icons;
use crate::starfield::Starfield;
use crate::theme::palette::{SURFACE_900, TEXT_EMPHASIS};
use crate::theme::text::{Type, text};
use crate::theme::tokens::{CONTROL_HEIGHT, HERO_CROSSFADE_S, HERO_DWELL_S, KEN_BURNS_ZOOM, space};
use crate::viewer::{PyramidGeometry, TileSource, TiledViewer, View};
use crate::widgets::{ButtonKind, icon_button_of};

/// Seed of the hero's starfield.
const STARFIELD_SEED: u64 = 42;
/// Pan of the drift over a view's time on screen, as a fraction of the view.
const PAN_FRACTION: f64 = 0.02;
/// How long before its fade starts the next view begins loading.
const PRELOAD_S: f64 = 2.0;
/// The longest a swap waits for the incoming view's first tiles.
const HOLD_MAX_S: f64 = 2.0;

/// One image the hero can show: a label and the tiles to draw.
pub struct HeroView {
    /// Short name shown at the image's bottom right (e.g. a group name).
    pub label: String,
    /// The mosaic's tiles.
    pub source: Arc<dyn TileSource>,
}

/// A control the user used, applied after the frame.
#[derive(Clone, Copy)]
enum Action {
    TogglePause,
    Next,
}

/// The hero backdrop. See the module docs.
///
/// Time is injected: [`HeroStage::show`] takes `now` in seconds and the
/// stage keeps no clock of its own.
pub struct HeroStage {
    views: Vec<HeroView>,
    geoms: Vec<PyramidGeometry>,
    /// One viewer per view, built only while the view is (about to be) on
    /// screen.
    viewers: Vec<Option<TiledViewer>>,
    current: usize,
    /// When the current view's fade-in began (the clock of the cycle).
    since: f64,
    paused: bool,
    paused_at: f64,
    reduced_motion: bool,
    message: Option<String>,
    starfield: Starfield,
    loader_threads: usize,
    synchronous: bool,
    crossfading: bool,
    label_idx: Option<usize>,
    /// A view the user asked for with Next, shown once it has drawn tiles.
    swap_to: Option<usize>,
    swap_at: f64,
}

impl HeroStage {
    /// A stage whose viewers load tiles on `loader_threads` background
    /// threads.
    pub fn new(loader_threads: usize) -> Self {
        Self {
            views: Vec::new(),
            geoms: Vec::new(),
            viewers: Vec::new(),
            current: 0,
            since: 0.0,
            paused: false,
            paused_at: 0.0,
            reduced_motion: false,
            message: None,
            starfield: Starfield::new(STARFIELD_SEED),
            loader_threads,
            synchronous: false,
            crossfading: false,
            label_idx: None,
            swap_to: None,
            swap_at: 0.0,
        }
    }

    /// Test-only: viewers fetch synchronously on the UI thread.
    pub fn new_synchronous() -> Self {
        Self {
            synchronous: true,
            ..Self::new(0)
        }
    }

    /// Replace the views and show `start` (clamped) at once, from `now`.
    pub fn set_views(&mut self, views: Vec<HeroView>, start: usize, now: f64) {
        self.geoms = views.iter().map(|v| v.source.geometry()).collect();
        self.viewers = views.iter().map(|_| None).collect();
        self.current = start.min(views.len().saturating_sub(1));
        self.views = views;
        self.since = now;
        self.paused_at = now;
        self.crossfading = false;
        self.swap_to = None;
        self.label_idx = (!self.views.is_empty()).then_some(self.current);
    }

    /// The empty-state line drawn over the starfield (shown when there are
    /// no views).
    pub fn set_message(&mut self, message: Option<String>) {
        self.message = message;
    }

    /// With reduced motion there is no drift and views cut instead of
    /// crossfading.
    pub fn set_reduced_motion(&mut self, on: bool) {
        self.reduced_motion = on;
    }

    /// Label of the view on screen (the one fading in, past the halfway
    /// point of a crossfade).
    pub fn current_label(&self) -> Option<&str> {
        self.label_idx
            .and_then(|i| self.views.get(i))
            .map(|v| v.label.as_str())
    }

    /// True once any tile of a view has been painted.
    pub fn first_image_drawn(&self) -> bool {
        self.viewers
            .iter()
            .flatten()
            .any(TiledViewer::has_drawn_tiles)
    }

    /// True while any shown view still has tile requests in flight.
    pub fn is_loading(&self) -> bool {
        self.viewers.iter().flatten().any(TiledViewer::is_loading)
    }

    /// True while two views are drawn at once.
    pub fn is_crossfading(&self) -> bool {
        self.crossfading
    }

    /// True once view `i` has a viewer that has painted tiles.
    fn drawn(&self, i: usize) -> bool {
        self.viewers
            .get(i)
            .and_then(Option::as_ref)
            .is_some_and(TiledViewer::has_drawn_tiles)
    }

    fn next_index(&self) -> usize {
        (self.current + 1) % self.views.len()
    }

    fn make_viewer(&self, i: usize) -> TiledViewer {
        let source = self.views[i].source.clone();
        let mut v = if self.synchronous {
            TiledViewer::new_synchronous(source)
        } else {
            TiledViewer::new(source, self.loader_threads)
        };
        v.set_presentation(true);
        v
    }

    /// Draw viewer `i` in `rect` at `opacity`, `age` seconds into its drift.
    fn draw_view(&mut self, ui: &mut Ui, rect: Rect, i: usize, opacity: f32, age: f64) {
        let fresh = self.viewers[i].is_none();
        if fresh {
            self.viewers[i] = Some(self.make_viewer(i));
        }
        // The fitted camera the drift starts from.
        let mut base = View::new();
        base.fit(&self.geoms[i], rect.size(), 0.0);
        base.finish_animation();
        let (bz, bc) = (base.zoom(0.0), base.centre(0.0));

        let progress = if self.reduced_motion {
            0.0
        } else {
            (age / (HERO_DWELL_S + HERO_CROSSFADE_S)).clamp(0.0, 1.0)
        };
        // Alternate direction per view: even zoom in, odd zoom out.
        let forward = i.is_multiple_of(2);
        let along = if forward { progress } else { 1.0 - progress };
        let zoom = bz * (1.0 + (KEN_BURNS_ZOOM - 1.0) * along);
        let dir = if forward { 1.0 } else { -1.0 };
        let visible_w = f64::from(rect.width()) / zoom;
        let centre = (
            bc.0 + dir * PAN_FRACTION * visible_w * (progress - 0.5),
            bc.1,
        );

        let viewer = self.viewers[i].as_mut().expect("viewer just built");
        viewer.set_opacity(opacity);
        if fresh {
            // Start on the drift camera, not the viewer's home fit.
            viewer.set_camera(rect.size(), centre, zoom);
        }
        viewer.view_mut().set_view(centre, zoom, 0.0);
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            viewer.show(ui);
        });
    }

    /// Draw the hero: the starfield over all of `ui.max_rect()`, the views
    /// in `image_rect`, the gradients, the label and the controls. `now` is
    /// in seconds.
    pub fn show(&mut self, ui: &mut Ui, image_rect: Rect, now: f64) {
        let full = ui.max_rect();
        self.starfield.paint(ui.painter(), full);

        let n = self.views.len();
        let eff = if self.paused { self.paused_at } else { now };
        let total = if self.reduced_motion {
            HERO_DWELL_S
        } else {
            HERO_DWELL_S + HERO_CROSSFADE_S
        };
        if let Some(t) = self.swap_to {
            if t >= n {
                self.swap_to = None;
            } else if self.drawn(t) || now - self.swap_at > HOLD_MAX_S {
                self.current = t;
                self.since = eff;
                self.swap_to = None;
            }
        }
        if n > 1 && self.swap_to.is_none() {
            let mut guard = 0;
            while eff - self.since >= total && guard < n * 64 {
                // Hold the outgoing view until the incoming one has tiles.
                if eff - self.since - total < HOLD_MAX_S && !self.drawn(self.next_index()) {
                    break;
                }
                self.current = self.next_index();
                self.since += HERO_DWELL_S;
                guard += 1;
            }
        }
        let elapsed = eff - self.since;
        let mut fade = 0.0;
        self.crossfading = false;
        if n > 1 && !self.reduced_motion && elapsed >= HERO_DWELL_S {
            fade = ((elapsed - HERO_DWELL_S) / HERO_CROSSFADE_S).clamp(0.0, 1.0);
            self.crossfading = true;
        }
        self.label_idx = (n > 0).then(|| {
            if self.crossfading && fade >= 0.5 && self.drawn(self.next_index()) {
                self.next_index()
            } else {
                self.current
            }
        });

        if n > 0 && image_rect.width() > 0.0 && image_rect.height() > 0.0 {
            let cur = self.current;
            let preload = n > 1 && elapsed >= HERO_DWELL_S - PRELOAD_S;
            // The incoming view: the one asked for, else the next, loading
            // (at opacity 0) before it is shown.
            let incoming = self
                .swap_to
                .or_else(|| preload.then(|| self.next_index()))
                .filter(|&i| i != cur);
            // Drop viewers (and their tiles) that are neither shown nor next.
            for (i, v) in self.viewers.iter_mut().enumerate() {
                if i != cur && Some(i) != incoming {
                    *v = None;
                }
            }
            // Never fade the outgoing view out before the incoming one has
            // tiles to show in its place.
            let incoming_ready = incoming.is_some_and(|i| self.drawn(i));
            let out_opacity = if self.crossfading && incoming_ready {
                1.0 - fade as f32
            } else {
                1.0
            };
            self.draw_view(ui, image_rect, cur, out_opacity, elapsed);
            if let Some(nx) = incoming {
                let age = if self.swap_to.is_some() {
                    0.0
                } else {
                    elapsed - HERO_DWELL_S
                };
                let opacity = if self.swap_to.is_some() {
                    0.0
                } else {
                    fade as f32
                };
                self.draw_view(ui, image_rect, nx, opacity, age);
            }
        }

        paint_feather(ui, full, image_rect);
        paint_gradients(ui, full);

        if n == 0 {
            if let Some(msg) = &self.message {
                let rect = Rect::from_center_size(image_rect.center(), vec2(480.0, 24.0));
                ui.put(rect, egui::Label::new(text(Type::Body, msg.clone())));
            }
        } else {
            self.controls(ui, image_rect, now);
        }

        self.request_repaints(ui, eff, elapsed);
    }

    fn controls(&mut self, ui: &mut Ui, image_rect: Rect, now: f64) {
        let strip = Rect::from_min_max(
            pos2(
                image_rect.left(),
                image_rect.bottom() - space::L - CONTROL_HEIGHT,
            ),
            pos2(
                image_rect.right() - space::L,
                image_rect.bottom() - space::L,
            ),
        );
        let mut action = None;
        let multiple = self.views.len() > 1;
        let paused = self.paused;
        let label = self.current_label().map(str::to_string);
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(strip)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                if multiple {
                    let next = icon_button_of(ButtonKind::Ghost, icons::SKIP_FORWARD, "Next image");
                    if ui.add(next).clicked() {
                        action = Some(Action::Next);
                    }
                    let (glyph, tip) = if paused {
                        (icons::PLAY, "Resume slideshow")
                    } else {
                        (icons::PAUSE, "Pause slideshow")
                    };
                    if ui
                        .add(icon_button_of(ButtonKind::Ghost, glyph, tip))
                        .clicked()
                    {
                        action = Some(Action::TogglePause);
                    }
                }
                if let Some(label) = label {
                    ui.label(text(Type::Small, label).color(TEXT_EMPHASIS));
                }
            },
        );
        match action {
            Some(Action::TogglePause) => {
                if self.paused {
                    self.since += now - self.paused_at;
                } else {
                    self.paused_at = now;
                }
                self.paused = !self.paused;
            }
            Some(Action::Next) => {
                self.swap_to = Some(self.next_index());
                self.swap_at = now;
            }
            None => {}
        }
    }

    /// Repaint only while something moves: drift or crossfade, or (with
    /// reduced motion) at the next cut.
    fn request_repaints(&self, ui: &Ui, eff: f64, elapsed: f64) {
        if self.paused || self.views.is_empty() {
            return;
        }
        let multiple = self.views.len() > 1;
        if self.swap_to.is_some() {
            ui.ctx().request_repaint();
        } else if self.reduced_motion {
            if multiple {
                let wake = if elapsed < HERO_DWELL_S - PRELOAD_S {
                    HERO_DWELL_S - PRELOAD_S
                } else {
                    HERO_DWELL_S
                };
                let wait = (wake - elapsed).max(0.0);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(wait));
            }
        } else if multiple || eff - self.since < HERO_DWELL_S + HERO_CROSSFADE_S {
            ui.ctx().request_repaint();
        }
    }
}

/// Fraction of the image's width feathered on its left and right edges.
const FEATHER_SIDE: f32 = 0.15;
/// Fraction of the image's height feathered on its bottom edge.
const FEATHER_BOTTOM: f32 = 0.20;
/// Fraction of the image's height feathered on its top edge.
const FEATHER_TOP: f32 = 0.08;

/// Fade the image area's edges into the page: a ramp of the page colour,
/// opaque at the edge and clear inward, on every edge of `image` that is
/// not at the edge of `full` (the window), so no hard line shows wherever
/// the image is placed.
fn paint_feather(ui: &Ui, full: Rect, image: Rect) {
    if image.width() <= 0.0 || image.height() <= 0.0 {
        return;
    }
    let opaque = SURFACE_900;
    let clear = Color32::TRANSPARENT;
    let painter = ui.painter();
    let ramp = |outer: [egui::Pos2; 2], inner: [egui::Pos2; 2]| {
        let mut m = Mesh::default();
        m.colored_vertex(outer[0], opaque);
        m.colored_vertex(outer[1], opaque);
        m.colored_vertex(inner[0], clear);
        m.colored_vertex(inner[1], clear);
        m.add_triangle(0, 1, 2);
        m.add_triangle(1, 2, 3);
        painter.add(Shape::mesh(m));
    };
    let eps = 0.5;
    if image.left() > full.left() + eps {
        let d = image.width() * FEATHER_SIDE;
        ramp(
            [image.left_top(), image.left_bottom()],
            [
                pos2(image.left() + d, image.top()),
                pos2(image.left() + d, image.bottom()),
            ],
        );
    }
    if image.right() < full.right() - eps {
        let d = image.width() * FEATHER_SIDE;
        ramp(
            [image.right_top(), image.right_bottom()],
            [
                pos2(image.right() - d, image.top()),
                pos2(image.right() - d, image.bottom()),
            ],
        );
    }
    if image.bottom() < full.bottom() - eps {
        let d = image.height() * FEATHER_BOTTOM;
        ramp(
            [image.left_bottom(), image.right_bottom()],
            [
                pos2(image.left(), image.bottom() - d),
                pos2(image.right(), image.bottom() - d),
            ],
        );
    }
    if image.top() > full.top() + eps {
        let d = image.height() * FEATHER_TOP;
        ramp(
            [image.left_top(), image.right_top()],
            [
                pos2(image.left(), image.top() + d),
                pos2(image.right(), image.top() + d),
            ],
        );
    }
}

/// The darkening gradients, as vertex-coloured meshes over `rect`.
fn paint_gradients(ui: &Ui, rect: Rect) {
    let dark = |alpha: f32| SURFACE_900.gamma_multiply(alpha);
    let clear = Color32::TRANSPARENT;
    let painter = ui.painter();

    // Left to right: 0.92 falling to 0 at 60 % of the width.
    let x60 = rect.left() + rect.width() * 0.6;
    let mut m = Mesh::default();
    for (x, c) in [(rect.left(), dark(0.92)), (x60, clear)] {
        m.colored_vertex(pos2(x, rect.top()), c);
        m.colored_vertex(pos2(x, rect.bottom()), c);
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 2, 3);
    painter.add(Shape::mesh(m));

    // Bottom to top: 0.97 falling to 0 at 45 % of the height.
    let y45 = rect.bottom() - rect.height() * 0.45;
    let mut m = Mesh::default();
    for (y, c) in [(rect.bottom(), dark(0.97)), (y45, clear)] {
        m.colored_vertex(pos2(rect.left(), y), c);
        m.colored_vertex(pos2(rect.right(), y), c);
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 2, 3);
    painter.add(Shape::mesh(m));

    // Vignette: clear in the middle, 0.6 around the edges.
    let mut m = Mesh::default();
    m.colored_vertex(rect.center(), clear);
    let ring = [
        rect.left_top(),
        rect.center_top(),
        rect.right_top(),
        rect.right_center(),
        rect.right_bottom(),
        rect.center_bottom(),
        rect.left_bottom(),
        rect.left_center(),
    ];
    for p in ring {
        m.colored_vertex(p, dark(0.6));
    }
    for i in 0..ring.len() as u32 {
        m.add_triangle(0, 1 + i, 1 + (i + 1) % ring.len() as u32);
    }
    painter.add(Shape::mesh(m));
}

#[cfg(test)]
#[path = "hero_tests.rs"]
mod tests;
