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

    /// True while two views are drawn at once.
    pub fn is_crossfading(&self) -> bool {
        self.crossfading
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
        if self.viewers[i].is_none() {
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

        let reduced = self.reduced_motion;
        let viewer = self.viewers[i].as_mut().expect("viewer just built");
        viewer.set_opacity(opacity);
        if !reduced {
            viewer.view_mut().set_view(centre, zoom, 0.0);
        }
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
        if n > 1 {
            let mut guard = 0;
            while eff - self.since >= total && guard < n * 64 {
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
            if self.crossfading && fade >= 0.5 {
                self.next_index()
            } else {
                self.current
            }
        });

        if n > 0 && image_rect.width() > 0.0 && image_rect.height() > 0.0 {
            let cur = self.current;
            let next = (n > 1).then(|| self.next_index());
            // Drop viewers (and their tiles) that are neither shown nor next.
            let preload = n > 1 && !self.reduced_motion && elapsed >= HERO_DWELL_S - PRELOAD_S;
            for (i, v) in self.viewers.iter_mut().enumerate() {
                if i != cur && !(preload && Some(i) == next) {
                    *v = None;
                }
            }
            let out_opacity = if self.crossfading {
                1.0 - fade as f32
            } else {
                1.0
            };
            self.draw_view(ui, image_rect, cur, out_opacity, elapsed);
            if preload && let Some(nx) = next {
                let age = elapsed - HERO_DWELL_S;
                self.draw_view(ui, image_rect, nx, fade as f32, age);
            }
        }

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
                let at = if self.paused { self.paused_at } else { now };
                self.current = self.next_index();
                self.since = at;
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
        if self.reduced_motion {
            if multiple {
                let wait = (HERO_DWELL_S - elapsed).max(0.0);
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(wait));
            }
        } else if multiple || eff - self.since < HERO_DWELL_S + HERO_CROSSFADE_S {
            ui.ctx().request_repaint();
        }
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
