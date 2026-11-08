//! The start screen's hero: the selected session's mosaic as a full-bleed
//! backdrop, drifting slowly and crossfading between views. A starfield
//! stands in for it until its first image is on screen (and when there is
//! none to show).
//!
//! Two feathers of page colour keep what is laid over the image readable:
//! one along the left, holding over the page's content column then easing
//! to clear (never to black: the image shows through it), and a small one
//! along the top, where the view's label and the slideshow controls sit at
//! the right. Nothing else darkens the image.
//!
//! Each view fills the stage (a cover fit, as the web's hero does),
//! whatever the mosaic's aspect: a portrait mosaic in a landscape window is
//! cropped top and bottom rather than shown as a narrow strip with hard
//! edges. The drift (zoom from the cover fit by [`KEN_BURNS_ZOOM`], pan by
//! 2 % of the view) stays inside the image, so no image edge ever shows.

use std::sync::Arc;

use egui::{Align, Layout, Mesh, Rect, Shape, Ui, UiBuilder, Vec2, pos2, vec2};

use crate::icons;
use crate::starfield::Starfield;
use crate::theme::palette::{SURFACE_900, TEXT_EMPHASIS};
use crate::theme::text::{Type, text};
use crate::theme::tokens::{CONTROL_HEIGHT, HERO_CROSSFADE_S, HERO_DWELL_S, KEN_BURNS_ZOOM, space};
use crate::viewer::{PyramidGeometry, TileSource, TiledViewer};
use crate::widgets::{ButtonKind, icon_button_of};

/// Seed of the hero's starfield.
const STARFIELD_SEED: u64 = 42;
/// Pan of the drift at its zoomed-in end, as a fraction of the view.
///
/// The pan follows the zoom (none at the cover fit), and at most half of
/// the margin the zoom opens on each side, so the view never leaves the
/// image.
const PAN_FRACTION: f64 = 0.02;
const _: () = assert!(PAN_FRACTION <= (KEN_BURNS_ZOOM - 1.0) / 2.0);
/// How long before its fade starts the next view begins loading.
const PRELOAD_S: f64 = 2.0;
/// The longest a swap waits for the incoming view's first tiles.
const HOLD_MAX_S: f64 = 2.0;

/// One image the hero can show: a label and the tiles to draw.
pub struct HeroView {
    /// Short name shown at the top right (e.g. a group name).
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
        let progress = if self.reduced_motion {
            0.0
        } else {
            (age / (HERO_DWELL_S + HERO_CROSSFADE_S)).clamp(0.0, 1.0)
        };
        // Alternate direction per view: even zoom in, odd zoom out.
        let forward = i.is_multiple_of(2);
        let along = if forward { progress } else { 1.0 - progress };
        let dir = if forward { 1.0 } else { -1.0 };
        let (zoom, centre) = drift_camera(&self.geoms[i], rect.size(), along, dir);

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

    /// Draw the hero over all of `ui.max_rect()`: the views (or, until one
    /// is on screen, the starfield), the feathers, the label and the
    /// controls. `column_right` is the right edge of the page's content
    /// column along the left (the start screen's session block and shelf
    /// heading), over which the left feather holds. `now` is in seconds.
    pub fn show(&mut self, ui: &mut Ui, column_right: f32, now: f64) {
        let full = ui.max_rect();
        if self.views.is_empty() || !self.first_image_drawn() {
            self.starfield.paint(ui.painter(), full);
        }

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

        if n > 0 && full.width() > 0.0 && full.height() > 0.0 {
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
            self.draw_view(ui, full, cur, out_opacity, elapsed);
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
                self.draw_view(ui, full, nx, opacity, age);
            }
        }

        if n == 0 {
            if let Some(msg) = &self.message {
                // Centred in the stage to the right of the content column.
                let left = column_right.clamp(full.left(), full.right());
                let centre = pos2((left + full.right()) / 2.0, full.center().y);
                let rect = Rect::from_center_size(centre, vec2(480.0, 24.0));
                ui.put(rect, egui::Label::new(text(Type::Body, msg.clone())));
            }
        } else {
            let painter = ui.painter();
            painter.add(Shape::mesh(left_feather(full, column_right)));
            painter.add(Shape::mesh(top_feather(full)));
            self.controls(ui, full, now);
        }

        self.request_repaints(ui, eff, elapsed);
    }

    /// The label and controls at the top right, a page margin in, inside
    /// the top feather.
    fn controls(&mut self, ui: &mut Ui, full: Rect, now: f64) {
        let top = full.top() + space::XXL;
        let strip = Rect::from_min_max(
            pos2(full.left(), top),
            pos2(full.right() - space::XXL, top + CONTROL_HEIGHT),
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

/// The drift's camera (zoom, centre in image pixels) for an image of
/// `geom` in an area of `screen` points, `along` (0 to 1) the way from
/// the cover fit to [`KEN_BURNS_ZOOM`] times it, panning towards `dir`
/// (+1 right, -1 left).
///
/// The cover fit fills the area (the larger of the two axis fits), so the
/// view never shows past the image across the pan; along it, the pan of
/// [`PAN_FRACTION`] of the view at full zoom grows with the zoom and is
/// at most half the margin the zoom opens (and clamped to it besides).
/// Only a zoom above the viewer's [`MAX_ZOOM`](crate::viewer::MAX_ZOOM)
/// (an image far smaller than its area) can show an edge.
fn drift_camera(geom: &PyramidGeometry, screen: Vec2, along: f64, dir: f64) -> (f64, (f64, f64)) {
    let (w, h) = (geom.width as f64, geom.height as f64);
    let cover = (f64::from(screen.x) / w).max(f64::from(screen.y) / h);
    let cover = if cover.is_finite() && cover > 0.0 {
        cover
    } else {
        1.0
    };
    let zoom = cover * (1.0 + (KEN_BURNS_ZOOM - 1.0) * along);
    let visible_w = f64::from(screen.x) / zoom;
    let margin = ((w - visible_w) / 2.0).max(0.0);
    let pan = (dir * PAN_FRACTION * visible_w * along).clamp(-margin, margin);
    (zoom, (w / 2.0 + pan, h / 2.0))
}

/// The left feather's opacity (of the page colour) from the left edge to
/// the content column's right edge. Below 1, so the image shows through.
const LEFT_FEATHER_OPACITY: f32 = 0.66;
/// How far past the column the left feather eases to clear, as a fraction
/// of the stage's width.
const LEFT_FEATHER_EASE: f32 = 0.2;
/// The top feather's opacity from the top edge down to the controls'
/// bottom. Like the left one it is tuned by eye on real mosaics, which are
/// far from white: over a pure white image the label falls below AA.
const TOP_FEATHER_OPACITY: f32 = 0.33;
/// How far below the controls the top feather eases to clear, in points.
const TOP_FEATHER_EASE: f32 = 196.0;
/// Steps in a feather's ease (a smoothstep, so no band shows where the
/// hold ends).
const EASE_STEPS: usize = 64;

/// A feather of page colour across `full`, from its left edge (or its top
/// edge, when `down`): `opacity` for the first `hold` points, then easing
/// to clear over the next `ease`.
fn feather(full: Rect, down: bool, hold: f32, ease: f32, opacity: f32) -> Mesh {
    let hold = hold.max(0.0);
    let colour = |o: f32| SURFACE_900.gamma_multiply(o);
    let eased = (0..=EASE_STEPS).map(|i| {
        let t = i as f32 / EASE_STEPS as f32;
        let smooth = t * t * (3.0 - 2.0 * t);
        (hold + t * ease, colour(opacity * (1.0 - smooth)))
    });
    let mut m = Mesh::default();
    for (d, c) in std::iter::once((0.0, colour(opacity))).chain(eased) {
        let (a, b) = if down {
            let y = full.top() + d;
            (pos2(full.left(), y), pos2(full.right(), y))
        } else {
            let x = full.left() + d;
            (pos2(x, full.top()), pos2(x, full.bottom()))
        };
        m.colored_vertex(a, c);
        m.colored_vertex(b, c);
    }
    let rows = m.vertices.len() as u32 / 2;
    for r in 0..rows - 1 {
        let i = 2 * r;
        m.add_triangle(i, i + 1, i + 2);
        m.add_triangle(i + 1, i + 2, i + 3);
    }
    m
}

/// The left feather: [`LEFT_FEATHER_OPACITY`] over the content column
/// (up to `column_right`), easing to clear past it.
fn left_feather(full: Rect, column_right: f32) -> Mesh {
    feather(
        full,
        false,
        column_right - full.left(),
        LEFT_FEATHER_EASE * full.width(),
        LEFT_FEATHER_OPACITY,
    )
}

/// The top feather: [`TOP_FEATHER_OPACITY`] down to the controls' bottom,
/// easing to clear below them.
fn top_feather(full: Rect) -> Mesh {
    feather(
        full,
        true,
        space::XXL + CONTROL_HEIGHT,
        TOP_FEATHER_EASE,
        TOP_FEATHER_OPACITY,
    )
}

#[cfg(test)]
#[path = "hero_tests.rs"]
mod tests;
