//! What the tiled viewer and the plan canvas share for drawing over their
//! content: [`Overlay`] rectangles, image [`Patch`]es, the in-progress
//! rectangle of [`Tool::DrawRect`] (with its pointer, [`DRAW_CURSOR`]),
//! [`Polyline`]s and [`CanvasBadge`]s (counts that click), and
//! the one function that paints them all (`paint_layers`); and a press held
//! on either canvas ([`Hold`]).

use egui::{
    Align2, Color32, Pos2, Rect, Shape, Stroke, StrokeKind, TextStyle, TextureHandle, Vec2,
};

use crate::icons;
use crate::theme::tokens::{BADGE_HEIGHT, BADGE_PAD, LINE_DASH, space};
use crate::theme::{Tone, palette};

/// Stroke width of an overlay rectangle, in points.
const OVERLAY_STROKE: f32 = 1.5;
/// Dash and gap lengths of a dashed overlay, in points.
const OVERLAY_DASH: (f32, f32) = (6.0, 4.0);
/// Offset of an overlay's label from its top-left corner, in points (a
/// [`crate::widgets::CanvasLabel`] puts its name there too).
pub const LABEL_OFFSET: Vec2 = Vec2::new(space::XS, 2.0);

/// The pointer over a canvas while [`Tool::DrawRect`] is armed, so the
/// next drag is seen to draw rather than pan.
pub const DRAW_CURSOR: egui::CursorIcon = egui::CursorIcon::Crosshair;

/// What a primary-button drag does on a canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Drag pans (the default).
    #[default]
    Navigate,
    /// Drag draws a rectangle, reported on release as the canvas
    /// response's `drawn_rect`; Esc cancels. The tool returns to
    /// [`Tool::Navigate`] after a rect is drawn or cancelled. While it is
    /// armed the pointer over the canvas is [`DRAW_CURSOR`].
    DrawRect,
}

/// Show [`DRAW_CURSOR`] while `tool` is [`Tool::DrawRect`] and the pointer
/// is over the canvas of `response` (or drawing on it). Call after the
/// canvas's input (so a rect just drawn, which disarms the tool, shows the
/// plain pointer) and before the controls drawn over it, which may set
/// their own.
pub(crate) fn show_tool_cursor(ui: &egui::Ui, response: &egui::Response, tool: Tool) {
    if tool == Tool::DrawRect && (pointer_over(ui, response).is_some() || response.dragged()) {
        ui.ctx().set_cursor_icon(DRAW_CURSOR);
    }
}

/// A rectangle drawn over a canvas, in canvas coordinates (the viewer's
/// level-0 image pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    /// `[x0, y0, x1, y1]` in canvas coordinates.
    pub rect: [f64; 4],
    /// Stroke and label colour.
    pub color: Color32,
    /// Text drawn at the top-left corner (none if empty).
    pub label: String,
    /// Dashed rather than solid outline.
    pub dashed: bool,
}

/// A polyline drawn over a canvas (a seam, say), in canvas coordinates,
/// painted with the [`Overlay`]s and clipped to the viewport.
#[derive(Clone, Debug, PartialEq)]
pub struct Polyline {
    /// The vertices, `[x, y]` in canvas coordinates.
    pub points: Vec<[f64; 2]>,
    /// Stroke colour.
    pub color: Color32,
    /// Stroke width in points (a [`crate::theme::tokens::LINE_WIDTH`]).
    pub width: f32,
    /// Dashed rather than solid.
    pub dashed: bool,
}

/// A count of something at a canvas point: a pill with the tone's icon and
/// the count, drawn after the overlays at a constant size on screen
/// (however far the canvas is zoomed), centred on `at`. Each badge is a
/// button, reachable by keyboard and clickable; the canvas's response
/// reports it as `badge_clicked`. The icon always comes with the colour.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasBadge {
    /// Caller's identifier, reported back as `badge_clicked`.
    pub id: u64,
    /// Where the badge sits, `[x, y]` in canvas coordinates.
    pub at: [f64; 2],
    /// Its tone: the icon, and the colour of the icon and the outline.
    pub tone: Tone,
    /// The number shown beside the icon.
    pub count: u32,
    /// Its accessible name, which says what it counts and where ("2
    /// problems here"); also its hover text, unless it has `hover`.
    pub description: String,
    /// What resting the pointer on it shows, when that says more than the
    /// description (what it counts, one per line); also its accessible
    /// description. `None`: the description shows.
    pub hover: Option<String>,
}

/// The primary button held down on a canvas (pressed there with the
/// navigate tool, and not released yet), as the canvas reports it each
/// frame. A press held still stays a hold however long it lasts; once the
/// pointer has moved further than a click may, it is a drag (it pans) for
/// the rest of the press. A press on a control drawn over the canvas (its
/// own zoom buttons, say) is none, nor is anything while the window does
/// not have the focus (a press it interrupts is over).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hold {
    /// Where the button went down, in canvas coordinates (through the view
    /// of that frame).
    pub origin: (f64, f64),
    /// The pointer has moved past the click distance since: a drag.
    pub moved: bool,
}

/// A press followed by [`follow_hold`] across frames: its hold, or `None`
/// for a press that is no hold (it went to a control on top of the
/// canvas, or the window lost the focus).
pub(crate) type Pressed = Option<Option<Hold>>;

/// Follow a press held on `response` (a canvas with the navigate tool):
/// `pressed` keeps it across frames. `to_canvas` maps a screen point to
/// canvas coordinates. The hold this frame, if any.
pub(crate) fn follow_hold(
    ui: &egui::Ui,
    response: &egui::Response,
    pressed: &mut Pressed,
    to_canvas: &dyn Fn(Pos2) -> (f64, f64),
) -> Option<Hold> {
    let (down, origin, latest, focused) = ui.input(|i| {
        (
            i.pointer.primary_down(),
            i.pointer.press_origin(),
            i.pointer.latest_pos(),
            i.focused,
        )
    });
    let Some(origin) = origin.filter(|_| down && response.is_pointer_button_down_on()) else {
        *pressed = None;
        return None;
    };
    if !focused {
        *pressed = Some(None);
        return None;
    }
    let held = pressed.get_or_insert_with(|| {
        (!control_on_top(ui.ctx(), response.id)).then(|| Hold {
            origin: to_canvas(origin),
            moved: false,
        })
    });
    let h = held.as_mut()?;
    let max = ui.ctx().options(|o| o.input_options.max_click_dist);
    h.moved |= latest.is_some_and(|p| p.distance(origin) > max);
    Some(*h)
}

/// Whether a widget sensing clicks other than `canvas` is under the pointer
/// on top of it (egui then gives the press's click to that widget, its drag
/// to the canvas).
fn control_on_top(ctx: &egui::Context, canvas: egui::Id) -> bool {
    let hovered: Vec<egui::Id> = ctx.interaction_snapshot(|s| s.hovered.iter().copied().collect());
    hovered.into_iter().any(|id| {
        id != canvas
            && ctx
                .read_response(id)
                .is_some_and(|r| r.sense.senses_click())
    })
}

/// Where the pointer is over the canvas of `response`, even under a
/// widget drawn over it (a canvas label's panel takes the presses there,
/// not the wheel or the readout); none while another layer covers it.
pub(crate) fn pointer_over(ui: &egui::Ui, response: &egui::Response) -> Option<Pos2> {
    if !response.contains_pointer() {
        return None;
    }
    ui.input(|i| i.pointer.hover_pos())
        .filter(|p| response.rect.contains(*p))
}

/// An image drawn over a canvas: `texture` is stretched over `rect` and
/// only the part inside `clip` is drawn. Painted over the canvas content
/// and under overlays.
#[derive(Clone)]
pub struct Patch {
    /// Canvas pixels `[x0, y0, x1, y1]` the whole texture covers.
    pub rect: [f64; 4],
    /// Canvas pixels `[x0, y0, x1, y1]`; nothing outside is drawn.
    pub clip: [f64; 4],
    /// The image.
    pub texture: TextureHandle,
}

/// Paint `patches`, then `overlays`, then `lines`, then the rectangle being
/// drawn, onto
/// `painter` (which clips to the viewport `viewport`). `to_screen` maps a
/// canvas rect to the screen.
pub(crate) fn paint_layers(
    painter: &egui::Painter,
    viewport: Rect,
    to_screen: &dyn Fn([f64; 4]) -> Rect,
    patches: &[Patch],
    overlays: &[Overlay],
    lines: &[Polyline],
    drawing: Option<[f64; 4]>,
) {
    for p in patches {
        let full = to_screen(p.rect);
        let visible = full.intersect(to_screen(p.clip)).intersect(viewport);
        if full.width() <= 0.0
            || full.height() <= 0.0
            || visible.width() <= 0.0
            || visible.height() <= 0.0
        {
            continue;
        }
        let at = |pt: Pos2| {
            Pos2::new(
                (pt.x - full.left()) / full.width(),
                (pt.y - full.top()) / full.height(),
            )
        };
        let uv = Rect::from_min_max(at(visible.min), at(visible.max));
        painter.image(p.texture.id(), visible, uv, Color32::WHITE);
    }
    let font = TextStyle::Body.resolve(&painter.ctx().global_style());
    for o in overlays {
        let r = to_screen(o.rect);
        if !r.intersects(viewport) {
            continue;
        }
        let stroke = Stroke::new(OVERLAY_STROKE, o.color);
        if o.dashed {
            let ring = [
                r.left_top(),
                r.right_top(),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            painter.extend(Shape::dashed_line(
                &ring,
                stroke,
                OVERLAY_DASH.0,
                OVERLAY_DASH.1,
            ));
        } else {
            painter.rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
        }
        if !o.label.is_empty() {
            painter.text(
                r.left_top() + LABEL_OFFSET,
                Align2::LEFT_TOP,
                &o.label,
                font.clone(),
                o.color,
            );
        }
    }
    paint_lines(painter, viewport, to_screen, lines);
    if let Some(r) = drawing {
        painter.rect_stroke(
            to_screen(r),
            0.0,
            Stroke::new(OVERLAY_STROKE, palette::ACCENT_400),
            StrokeKind::Inside,
        );
    }
}

/// The fixed and the current corner of a rect being drawn.
pub(crate) type Corners = ((f64, f64), (f64, f64));

/// How a `follow_draw` frame ended.
pub(crate) enum DrawStep {
    /// Nothing finished.
    Idle,
    /// Esc: the tool and any rect in progress are dropped.
    Cancelled,
    /// The drag was released: the normalised rect.
    Done([f64; 4]),
}

/// Follow a [`Tool::DrawRect`] drag on `response`. `drawing` holds the fixed
/// and the current corner of the rect in progress; `clamp` maps a screen
/// point to a canvas point clamped to the canvas.
pub(crate) fn follow_draw(
    ui: &egui::Ui,
    response: &egui::Response,
    drawing: &mut Option<Corners>,
    clamp: &dyn Fn(Pos2) -> (f64, f64),
) -> DrawStep {
    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if esc && !ui.ctx().egui_wants_keyboard_input() {
        // egui aborts the drag in progress on Escape too, so the rest of it
        // is not a pan.
        *drawing = None;
        return DrawStep::Cancelled;
    }
    if response.drag_started()
        && let Some(origin) = ui.input(|i| i.pointer.press_origin())
    {
        let a = clamp(origin);
        *drawing = Some((a, a));
    }
    if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
        let at = clamp(p);
        if let Some((_, end)) = drawing {
            *end = at;
        }
    }
    if response.drag_stopped()
        && let Some((a, b)) = drawing.take()
    {
        return DrawStep::Done([a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]);
    }
    DrawStep::Idle
}

/// The corners of a rect in progress as `[x0, y0, x1, y1]`.
pub(crate) fn drawing_rect(d: Option<Corners>) -> Option<[f64; 4]> {
    d.map(|(a, b)| [a.0, a.1, b.0, b.1])
}

/// Paint `lines` (clipped to `viewport`; a dashed line keeps its dash phase
/// from its own start however it is panned).
fn paint_lines(
    painter: &egui::Painter,
    viewport: Rect,
    to_screen: &dyn Fn([f64; 4]) -> Rect,
    lines: &[Polyline],
) {
    for line in lines {
        let pts: Vec<Pos2> = line
            .points
            .iter()
            .map(|p| to_screen([p[0], p[1], p[0], p[1]]).min)
            .collect();
        if pts.len() < 2 {
            continue;
        }
        let stroke = Stroke::new(line.width, line.color);
        let clip = viewport.expand(line.width);
        if !Rect::from_points(&pts).intersects(clip) {
            continue;
        }
        if line.dashed {
            let mut walked = 0.0;
            for pair in pts.windows(2) {
                let len = (pair[1] - pair[0]).length();
                dash_segment(painter, pair[0], pair[1], walked, clip, stroke);
                walked += len;
            }
        } else {
            painter.add(Shape::line(pts, stroke));
        }
    }
}

/// The dashes of the segment `a`-`b`, which starts `walked` points into its
/// polyline, that fall inside `clip`.
fn dash_segment(
    painter: &egui::Painter,
    a: Pos2,
    b: Pos2,
    walked: f32,
    clip: Rect,
    stroke: Stroke,
) {
    let len = (b - a).length();
    let Some((t0, t1)) = clip_params(a, b, clip) else {
        return;
    };
    if len <= 0.0 {
        return;
    }
    let (dash, gap) = LINE_DASH;
    let period = dash + gap;
    let (s0, s1) = (t0 * len, t1 * len);
    let mut start = ((walked + s0) / period).floor() * period - walked;
    while start < s1 {
        let (from, to) = (start.max(s0), (start + dash).min(s1));
        if to > from {
            painter.add(Shape::line_segment(
                [a.lerp(b, from / len), a.lerp(b, to / len)],
                stroke,
            ));
        }
        start += period;
    }
}

/// The part of the segment `a`-`b`, as parameters in `0..=1`, inside
/// `clip` (Liang-Barsky), if any.
fn clip_params(a: Pos2, b: Pos2, clip: Rect) -> Option<(f32, f32)> {
    let d = b - a;
    let (mut t0, mut t1) = (0.0_f32, 1.0_f32);
    for (p, q) in [
        (-d.x, a.x - clip.min.x),
        (d.x, clip.max.x - a.x),
        (-d.y, a.y - clip.min.y),
        (d.y, clip.max.y - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 < t1).then_some((t0, t1))
}

/// Show `badges` over the canvas filling `viewport`: each is a pill at its
/// canvas point, centred, at its constant size, and a button of its own
/// (above the canvas's own press, so a press on one never starts a pan),
/// with focus ring and accessible description. `to_screen` maps a canvas
/// rect to the screen. Returns the id of the badge clicked (or activated
/// with Enter or Space) this frame. Call after the canvas has painted and
/// before the controls drawn over it.
pub(crate) fn show_badges(
    ui: &mut egui::Ui,
    viewport: Rect,
    to_screen: &dyn Fn([f64; 4]) -> Rect,
    badges: &[CanvasBadge],
) -> Option<u64> {
    let painter = ui.painter_at(viewport);
    let font = TextStyle::Small.resolve(ui.style());
    let mut clicked = None;
    for badge in badges {
        let at = to_screen([badge.at[0], badge.at[1], badge.at[0], badge.at[1]]).min;
        let icon = painter.layout_no_wrap(
            badge.tone.icon().unwrap_or(icons::INFO).to_string(),
            icons::font(font.size),
            badge.tone.color(),
        );
        let count =
            painter.layout_no_wrap(badge.count.to_string(), font.clone(), palette::TEXT_STRONG);
        let width =
            (icon.size().x + space::XS + count.size().x + 2.0 * BADGE_PAD).max(BADGE_HEIGHT);
        let pill = Rect::from_center_size(at, Vec2::new(width, BADGE_HEIGHT));
        let visible = pill.intersect(viewport);
        if visible.width() <= 0.0 || visible.height() <= 0.0 {
            continue;
        }
        let id = ui.id().with(("mega-ui-canvas-badge", badge.id));
        // Click and drag, so a drag that starts on a badge is the badge's
        // and does not pan the canvas under it.
        let response = ui.interact(visible, id, egui::Sense::click_and_drag());
        let corner = (BADGE_HEIGHT / 2.0) as u8;
        let outline = if response.hovered() || response.has_focus() {
            palette::TEXT_STRONG
        } else {
            badge.tone.color()
        };
        painter.rect(
            pill,
            egui::CornerRadius::same(corner),
            palette::SURFACE_900,
            Stroke::new(OVERLAY_STROKE, outline),
            StrokeKind::Inside,
        );
        let left = pill.left() + BADGE_PAD;
        painter.galley(
            Pos2::new(left, pill.center().y - icon.size().y / 2.0),
            icon.clone(),
            badge.tone.color(),
        );
        painter.galley(
            Pos2::new(
                left + icon.size().x + space::XS,
                pill.center().y - count.size().y / 2.0,
            ),
            count,
            palette::TEXT_STRONG,
        );
        crate::widgets::button::focus_ring_with(&painter, &response, corner);
        let label = badge.description.clone();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
        });
        if let Some(more) = &badge.hover {
            ui.ctx().accesskit_node_builder(response.id, |node| {
                node.set_description(more.clone());
            });
        }
        let hover = badge.hover.as_ref().unwrap_or(&badge.description);
        let response = response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(hover);
        if response.clicked() {
            clicked = Some(badge.id);
        }
    }
    clicked
}
