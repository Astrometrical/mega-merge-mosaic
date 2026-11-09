//! Plan canvas: low-resolution images placed in a shared frame, over the
//! viewer's spring pan and zoom.
//!
//! Each [`PlacedItem`] is a rect in canvas coordinates with any number of
//! image [`Layer`]s stretched over it. Drag pans, the scroll wheel zooms about
//! the cursor, a click reports the topmost item under the pointer and a
//! double-click zooms to it. `+`, `−`, `Home` and `Zoom to selection` buttons
//! sit top-left. A selected item and a highlighted one (pointed at
//! elsewhere, e.g. its row in a list) are outlined whether or not outlines
//! are shown, the selection over the highlight.

use egui::{
    Align2, Color32, Event, FontId, MouseWheelUnit, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Ui,
};

use crate::canvas_layers::{
    CanvasBadge, DrawStep, Hold, Overlay, Patch, Polyline, Pressed, Tool, drawing_rect,
    follow_draw, follow_hold, paint_layers, pointer_over, show_badges, show_tool_cursor,
};
use crate::theme::palette;
use crate::viewer::{OverlayButton, View, drag_delta, overlay_buttons};

/// Zoom factor per scroll-wheel notch (as in the tiled viewer).
const SCROLL_ZOOM_PER_NOTCH: f64 = 1.2;
/// Width of the selection outline, in points.
const SELECTED_STROKE: f32 = 2.5;
/// Width of an ordinary outline, in points.
const OUTLINE_STROKE: f32 = 1.0;
/// Margin left around an item zoomed to ([`PlanCanvas::zoom_to`], **Zoom to
/// selection**), as a fraction of its size on each side.
pub const ZOOM_MARGIN: f64 = 0.05;

/// `r` (`[x0, y0, x1, y1]`) grown by [`ZOOM_MARGIN`] of its size on each
/// side: what zooming to it frames, so its edges (and what sits on them)
/// stay in view.
pub fn with_zoom_margin(r: [f64; 4]) -> [f64; 4] {
    let (mx, my) = ((r[2] - r[0]) * ZOOM_MARGIN, (r[3] - r[1]) * ZOOM_MARGIN);
    [r[0] - mx, r[1] - my, r[2] + mx, r[3] + my]
}
/// Dash and gap of the outline of an item with no layers yet, in points.
const DASH: (f32, f32) = (6.0, 4.0);
/// Strength of a highlighted item's outline: the selection outline
/// ([`SELECTED_STROKE`] in the light accent) at part opacity.
const HIGHLIGHT_STRENGTH: f32 = 0.6;

/// One image drawn in a [`PlacedItem`], stretched over the item's rect.
///
/// An opaque layer is drawn with `tint` as its vertex colour, covering what
/// is below. An **additive** layer instead adds `tint × image` onto what is
/// below (e.g. one display channel of a colour composition). It uses egui's
/// premultiplied-alpha blending: the vertex colour keeps `tint`'s RGB with
/// alpha 0 ([`Color32::from_rgb_additive`]). The backends multiply the vertex
/// colour by the texel (`egui_glow` `fragment.glsl`:
/// `v_rgba_in_gamma * texture_in_gamma`), then blend with source factor
/// `ONE` and destination factor `ONE_MINUS_SRC_ALPHA` (`egui_glow`
/// `painter.rs` `blend_func_separate(ONE, ONE_MINUS_SRC_ALPHA, ..)`;
/// `egui-wgpu` `renderer.rs` `BlendFactor::One` / `OneMinusSrcAlpha`, egui
/// 0.36). With an opaque texel the source alpha is 0, so the destination is
/// kept and the colour is added. The sum is formed in the framebuffer's
/// (gamma) space and saturates at white.
pub struct Layer {
    /// The image.
    pub texture: egui::TextureHandle,
    /// Colour the image is multiplied by.
    pub tint: Color32,
    /// Add onto what is below instead of covering it.
    pub additive: bool,
}

impl Layer {
    /// Vertex colour the layer is painted with.
    fn vertex_color(&self) -> Color32 {
        if self.additive {
            Color32::from_rgb_additive(self.tint.r(), self.tint.g(), self.tint.b())
        } else {
            self.tint
        }
    }
}

/// An item placed on a [`PlanCanvas`].
pub struct PlacedItem {
    /// Caller's identifier, reported back in [`PlanResponse`].
    pub id: u64,
    /// Placement `[x0, y0, x1, y1]` in canvas coordinates.
    pub rect: [f64; 4],
    /// Images, drawn in order. With none (not loaded yet) the outline is
    /// drawn dashed.
    pub layers: Vec<Layer>,
    /// Outline colour (when outlines are shown).
    pub outline: Color32,
    /// Label drawn at the item's centre (when labels are shown).
    pub label: String,
    /// Drawn with a 2.5 px accent outline, whether or not outlines are shown;
    /// the target of "Zoom to selection".
    pub selected: bool,
    /// Highlighted (e.g. its row is hovered in a list beside the canvas):
    /// drawn with the selection outline at part strength, whether or not
    /// outlines are shown, under the selection outline of a selected item.
    pub highlighted: bool,
}

impl PlacedItem {
    fn contains(&self, p: (f64, f64)) -> bool {
        let r = self.rect;
        p.0 >= r[0] && p.0 < r[2] && p.1 >= r[1] && p.1 < r[3]
    }
}

/// What the canvas reports back each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanResponse {
    /// Item clicked this frame (the topmost under the pointer).
    pub clicked: Option<u64>,
    /// Item double-clicked this frame; the canvas zooms to it.
    pub double_clicked: Option<u64>,
    /// Item under the pointer.
    pub hovered: Option<u64>,
    /// A click landed on no item (callers usually deselect).
    pub clicked_empty: bool,
    /// A rectangle the user finished drawing this frame with
    /// [`Tool::DrawRect`]: `[x0, y0, x1, y1]` in canvas coordinates,
    /// normalised (`x0 <= x1`, `y0 <= y1`) and clamped to the extent.
    pub drawn_rect: Option<[f64; 4]>,
    /// The primary button held down on the canvas with [`Tool::Navigate`],
    /// in canvas coordinates.
    pub held: Option<Hold>,
    /// A badge clicked (or activated from the keyboard) this frame: its
    /// [`CanvasBadge::id`].
    pub badge_clicked: Option<u64>,
}

/// Canvas command from a button or the host app, applied in `show`.
#[derive(Clone, Copy, Debug)]
enum Command {
    Home,
    Zoom(f64),
    ZoomTo([f64; 4]),
    ZoomToSelection,
}

/// Canvas of placed items (see the module docs). Items are drawn in slice
/// order, so later items are on top, and hit-testing picks the topmost.
pub struct PlanCanvas {
    view: View,
    /// The extent the view was last fitted to; a new extent re-homes.
    homed_extent: Option<[f64; 4]>,
    /// A view to show in place of fitting an extent: (extent, centre, zoom).
    view_for: Option<([f64; 4], (f64, f64), f64)>,
    commands: Vec<Command>,
    last_frame: Option<(Rect, f64)>,
    overlays: Vec<Overlay>,
    patches: Vec<Patch>,
    lines: Vec<Polyline>,
    badges: Vec<CanvasBadge>,
    tool: Tool,
    /// The rectangle being drawn: its fixed corner and the current one, in
    /// canvas coordinates.
    drawing: Option<((f64, f64), (f64, f64))>,
    /// The press on the canvas, while it lasts ([`follow_hold`]).
    held: Pressed,
    /// The screen rect of the canvas's buttons, as last drawn.
    controls: Option<Rect>,
}

impl Default for PlanCanvas {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanCanvas {
    /// A canvas that fits the extent on its first frame.
    pub fn new() -> Self {
        Self {
            view: View::new(),
            homed_extent: None,
            view_for: None,
            commands: Vec::new(),
            last_frame: None,
            overlays: Vec::new(),
            patches: Vec::new(),
            lines: Vec::new(),
            badges: Vec::new(),
            tool: Tool::Navigate,
            drawing: None,
            held: None,
            controls: None,
        }
    }

    /// Replace the rectangles drawn over the items (canvas coordinates).
    pub fn set_overlays(&mut self, overlays: Vec<Overlay>) {
        self.overlays = overlays;
    }

    /// Replace the images drawn over the items (under the overlays).
    pub fn set_patches(&mut self, patches: Vec<Patch>) {
        self.patches = patches;
    }

    /// Replace the polylines drawn over the items, with the overlays
    /// (canvas coordinates).
    pub fn set_lines(&mut self, lines: Vec<Polyline>) {
        self.lines = lines;
    }

    /// Replace the badges drawn over everything else (canvas coordinates;
    /// constant size on screen). A click or Enter on one is reported as
    /// `badge_clicked`.
    pub fn set_badges(&mut self, badges: Vec<CanvasBadge>) {
        self.badges = badges;
    }

    /// What a drag does from now on (a rectangle being drawn is dropped).
    /// With [`Tool::DrawRect`] a drag draws instead of panning, clicks are
    /// not reported, and the pointer over the canvas is a crosshair
    /// ([`crate::canvas_layers::DRAW_CURSOR`]).
    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.drawing = None;
    }

    /// The current tool.
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// The screen rect the canvas's own buttons (zoom, home, zoom to
    /// selection) took in the last frame drawn. Something drawn over the
    /// canvas can keep clear of it.
    pub fn controls_rect(&self) -> Option<Rect> {
        self.controls
    }

    /// The current view (canvas coordinates are the view's image pixels).
    pub fn view(&self) -> &View {
        &self.view
    }

    /// The view, to move it from outside. The canvas still fits the extent
    /// on its first frame and whenever the extent changes; to show a view in
    /// place of such a fit, use [`PlanCanvas::set_view_for`].
    pub fn view_mut(&mut self) -> &mut View {
        &mut self.view
    }

    /// Show the view at `centre` and `zoom` (canvas coordinates; the zoom is
    /// clamped to the view's limits) from the next frame that draws
    /// `extent`, with no animation, in place of fitting `extent`: that frame
    /// still fits a new extent (which sets the zoom limits), then jumps to
    /// this view before handling input and painting, so the fit is never
    /// shown. If the next frame drawn has another extent, this is forgotten
    /// and that extent is fitted as usual. For a camera shared with another
    /// canvas.
    pub fn set_view_for(&mut self, extent: [f64; 4], centre: (f64, f64), zoom: f64) {
        self.view_for = Some((extent, centre, zoom));
    }

    /// Viewport rect and `ui.input().time` of the last frame shown, for
    /// mapping between screen and canvas with [`PlanCanvas::view`].
    pub fn last_frame(&self) -> Option<(Rect, f64)> {
        self.last_frame
    }

    /// Animate to the home view (the whole extent visible).
    pub fn home(&mut self) {
        self.commands.push(Command::Home);
    }

    /// Animate so `rect` (`[x0, y0, x1, y1]`, canvas coordinates) fills the
    /// canvas, with a small margin.
    pub fn zoom_to(&mut self, rect: [f64; 4]) {
        self.commands.push(Command::ZoomTo(rect));
    }

    /// Draw `items` over a canvas spanning `extent` (`[x0, y0, x1, y1]`),
    /// filling `ui`'s available space, and handle input. The view fits the
    /// extent on the first frame and whenever the extent changes.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        extent: [f64; 4],
        items: &[PlacedItem],
        show_outlines: bool,
        show_labels: bool,
    ) -> PlanResponse {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let now = ui.input(|i| i.time);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return PlanResponse::default();
        }
        self.last_frame = Some((rect, now));

        if self.homed_extent != Some(extent) {
            self.view.fit_rect(extent, rect.size(), now);
            self.view.finish_animation();
            self.homed_extent = Some(extent);
        }
        if let Some((for_extent, centre, zoom)) = self.view_for.take()
            && for_extent == extent
        {
            self.view.set_view(centre, zoom, now);
        }
        self.view.update(now);
        for c in std::mem::take(&mut self.commands) {
            match c {
                Command::Home => self.view.fit_rect(extent, rect.size(), now),
                Command::Zoom(f) => self.view.zoom_by(f, rect.center(), rect, now),
                Command::ZoomTo(r) => self.zoom_to_padded(r, rect, now),
                Command::ZoomToSelection => {
                    if let Some(r) = selection_bounds(items) {
                        self.zoom_to_padded(r, rect, now);
                    }
                }
            }
        }

        // Input before hit-testing and painting, so both use this frame's view.
        let drawn_rect = self.handle_draw(ui, &response, rect, extent, now);
        show_tool_cursor(ui, &response, self.tool);
        self.handle_pan_zoom(ui, &response, rect, now);
        let navigating = self.tool == Tool::Navigate && drawn_rect.is_none();
        let held = if navigating {
            let view = &self.view;
            let to_canvas = |p: Pos2| view.screen_to_image(p, rect, now);
            follow_hold(ui, &response, &mut self.held, &to_canvas)
        } else {
            self.held = None;
            None
        };
        let hit = |p: Pos2| {
            let c = self.view.screen_to_image(p, rect, now);
            items.iter().rev().find(|it| it.contains(c)).map(|it| it.id)
        };
        let mut out = PlanResponse {
            hovered: response.hover_pos().and_then(hit),
            drawn_rect,
            held,
            ..PlanResponse::default()
        };
        if navigating
            && response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            out.clicked = hit(p);
            out.clicked_empty = out.clicked.is_none();
        }
        if navigating
            && response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            out.double_clicked = hit(p);
            match out
                .double_clicked
                .and_then(|id| items.iter().find(|it| it.id == id))
            {
                Some(it) => self.zoom_to_padded(it.rect, rect, now),
                None => self.view.zoom_by(2.0, p, rect, now),
            }
        }
        self.paint(
            &ui.painter_at(rect),
            rect,
            now,
            items,
            show_outlines,
            show_labels,
        );
        let badge_clicked = {
            let view = &self.view;
            let screen = |r: [f64; 4]| {
                Rect::from_two_pos(
                    view.image_to_screen((r[0], r[1]), rect, now),
                    view.image_to_screen((r[2], r[3]), rect, now),
                )
            };
            show_badges(ui, rect, &screen, &self.badges)
        };
        out.badge_clicked = badge_clicked;
        let buttons = [
            OverlayButton::new("+", 28.0, "Zoom in"),
            OverlayButton::new("\u{2212}", 28.0, "Zoom out"),
            OverlayButton::new("Home", 60.0, "Fit everything"),
            OverlayButton {
                enabled: items.iter().any(|it| it.selected),
                ..OverlayButton::new("Zoom to selection", 128.0, "Zoom to the selection")
            },
        ];
        let cmds = [
            Command::Zoom(2.0),
            Command::Zoom(0.5),
            Command::Home,
            Command::ZoomToSelection,
        ];
        let (clicked, controls) = overlay_buttons(ui, rect, &buttons);
        self.controls = Some(controls);
        if let Some(i) = clicked {
            self.commands.push(cmds[i]);
        }

        if !self.view.is_settled(now) || !self.commands.is_empty() {
            ui.ctx().request_repaint();
        }
        out
    }

    fn zoom_to_padded(&mut self, r: [f64; 4], screen: Rect, now: f64) {
        self.view
            .zoom_to_rect(with_zoom_margin(r), screen.size(), now);
    }

    /// Follow a [`Tool::DrawRect`] drag; returns the rect once released.
    fn handle_draw(
        &mut self,
        ui: &Ui,
        response: &egui::Response,
        rect: Rect,
        extent: [f64; 4],
        now: f64,
    ) -> Option<[f64; 4]> {
        if self.tool != Tool::DrawRect {
            return None;
        }
        let view = &self.view;
        let clamp = |p: Pos2| {
            let (x, y) = view.screen_to_image(p, rect, now);
            (x.clamp(extent[0], extent[2]), y.clamp(extent[1], extent[3]))
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

    fn handle_pan_zoom(&mut self, ui: &Ui, response: &egui::Response, rect: Rect, now: f64) {
        if response.dragged() && self.tool == Tool::Navigate {
            self.view.pan_by(drag_delta(response), now);
        }
        let Some(p) = pointer_over(ui, response) else {
            return;
        };
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

    /// Images in slice order, then patches, overlays and the rectangle being
    /// drawn, then outlines and labels over them,
    /// then highlighted outlines and selected outlines last (so they are
    /// never hidden; the selection over the highlight).
    fn paint(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        now: f64,
        items: &[PlacedItem],
        show_outlines: bool,
        show_labels: bool,
    ) {
        painter.rect_filled(rect, 0.0, palette::SURFACE_900);
        let screen = |r: [f64; 4]| {
            Rect::from_min_max(
                self.view.image_to_screen((r[0], r[1]), rect, now),
                self.view.image_to_screen((r[2], r[3]), rect, now),
            )
        };
        let full_uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for it in items {
            let r = screen(it.rect);
            if !r.intersects(rect) {
                continue;
            }
            for layer in &it.layers {
                painter.image(layer.texture.id(), r, full_uv, layer.vertex_color());
            }
        }
        paint_layers(
            painter,
            rect,
            &screen,
            &self.patches,
            &self.overlays,
            &self.lines,
            drawing_rect(self.drawing),
        );
        for it in items {
            let r = screen(it.rect);
            if !r.intersects(rect) {
                continue;
            }
            if show_outlines && !it.selected && !it.highlighted {
                let stroke = Stroke::new(OUTLINE_STROKE, it.outline);
                if it.layers.is_empty() {
                    let ring = [
                        r.left_top(),
                        r.right_top(),
                        r.right_bottom(),
                        r.left_bottom(),
                        r.left_top(),
                    ];
                    painter.extend(Shape::dashed_line(&ring, stroke, DASH.0, DASH.1));
                } else {
                    painter.rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
                }
            }
            if show_labels && !it.label.is_empty() {
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    &it.label,
                    FontId::proportional(14.0),
                    palette::TEXT_STRONG,
                );
            }
        }
        let highlight = palette::ACCENT_400.gamma_multiply(HIGHLIGHT_STRENGTH);
        for it in items.iter().filter(|it| it.highlighted) {
            let stroke = Stroke::new(SELECTED_STROKE, highlight);
            painter.rect_stroke(screen(it.rect), 0.0, stroke, StrokeKind::Inside);
        }
        for it in items.iter().filter(|it| it.selected) {
            let stroke = Stroke::new(SELECTED_STROKE, palette::ACCENT_400);
            painter.rect_stroke(screen(it.rect), 0.0, stroke, StrokeKind::Inside);
        }
    }
}

/// Union of the selected items' rects.
fn selection_bounds(items: &[PlacedItem]) -> Option<[f64; 4]> {
    items
        .iter()
        .filter(|it| it.selected)
        .map(|it| it.rect)
        .reduce(|a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        })
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
