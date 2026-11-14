//! What the recorder draws over the app: caption card, time-lapse badge,
//! click ripples and a cursor. Ported from the tour spike.

use egui::epaint::{Mesh, PathShape};
use egui::{Color32, Id, LayerId, Order, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2};
use egui_kittest::TestRenderer;
use egui_kittest::wgpu::WgpuTestRenderer;
use mega_ui::theme::palette;
use mega_ui::theme::text::Type;
use mega_ui::theme::tokens::{radius, space};

/// Seconds a caption takes to fade in.
const FADE_SECS: f32 = 0.3;
/// Seconds a click ripple lives.
const RIPPLE_SECS: f64 = 0.45;

/// The caption on screen: its text and how long it has been showing.
pub(crate) struct ShownCaption {
    /// The caption text.
    pub text: String,
    /// Seconds since the caption appeared (in output time).
    pub age: f32,
}

/// What the recorder draws over the app, apart from the caption.
#[derive(Default)]
pub(crate) struct Overlay {
    /// Cursor tip, if shown.
    pub cursor: Option<Pos2>,
    /// Whether the primary button is down (shrinks the cursor).
    pub pressed: bool,
    /// Click ripples: (position, egui time of the click).
    pub ripples: Vec<(Pos2, f64)>,
    /// Time-lapse factor shown while waiting on processing.
    pub fast: Option<u32>,
}

/// Paint the overlay and the caption on a debug layer above the app.
pub(crate) fn paint_overlay(ctx: &egui::Context, ov: &Overlay, caption: Option<&ShownCaption>) {
    let now = ctx.input(|i| i.time);
    let painter = ctx.layer_painter(LayerId::new(Order::Debug, Id::new("tour-overlay")));
    let screen = ctx.content_rect();
    let mut above = screen.bottom() - space::XXL;
    if let Some(cap) = caption {
        let k = (cap.age / FADE_SECS).clamp(0.0, 1.0);
        let font = Type::Title
            .text_style()
            .resolve(&ctx.style_of(egui::Theme::Dark));
        let galley = painter.layout(
            cap.text.clone(),
            font,
            palette::TEXT_STRONG,
            screen.width() * 0.7,
        );
        let pad = vec2(space::XL, space::M);
        let size = galley.size() + 2.0 * pad;
        let rise = (1.0 - k) * space::M;
        let rect =
            Rect::from_center_size(pos2(screen.center().x, above - size.y / 2.0 + rise), size);
        painter.rect(
            rect,
            radius::CARD,
            palette::SURFACE_800.gamma_multiply(0.94 * k),
            Stroke::new(1.0, palette::BORDER_STRONG.gamma_multiply(k)),
            StrokeKind::Inside,
        );
        painter.galley(
            rect.min + pad,
            galley,
            palette::TEXT_STRONG.gamma_multiply(k),
        );
        above = rect.top() - space::S;
    }
    if let Some(x) = ov.fast {
        let font = Type::Body
            .text_style()
            .resolve(&ctx.style_of(egui::Theme::Dark));
        let galley = painter.layout_no_wrap(format!("▶▶  sped up {x}×"), font, palette::ON_ACCENT);
        let pad = vec2(space::M, space::XS);
        let size = galley.size() + 2.0 * pad;
        let rect = Rect::from_center_size(pos2(screen.center().x, above - size.y / 2.0), size);
        painter.rect_filled(rect, radius::CONTROL, palette::ACCENT_700);
        painter.galley(rect.min + pad, galley, palette::ON_ACCENT);
    }
    for (p, t0) in &ov.ripples {
        let k = ((now - t0) / RIPPLE_SECS) as f32;
        if (0.0..1.0).contains(&k) {
            painter.circle_stroke(
                *p,
                6.0 + 22.0 * k,
                Stroke::new(
                    0.5 + 2.5 * (1.0 - k),
                    palette::ACCENT_400.gamma_multiply(1.0 - k),
                ),
            );
        }
    }
    if let Some(p) = ov.cursor {
        paint_cursor(&painter, p, if ov.pressed { 0.88 } else { 1.0 });
    }
}

/// A classic arrow pointer with its tip at `tip`.
fn paint_cursor(painter: &egui::Painter, tip: Pos2, scale: f32) {
    const PTS: [(f32, f32); 7] = [
        (0.0, 0.0),
        (0.0, 17.0),
        (4.5, 13.0),
        (7.5, 20.0),
        (10.0, 19.0),
        (7.0, 12.0),
        (12.0, 12.0),
    ];
    const TRIS: [[u32; 3]; 5] = [[0, 1, 2], [0, 2, 5], [0, 5, 6], [2, 3, 4], [2, 4, 5]];
    let s = 1.35 * scale;
    let at =
        |o: Vec2| -> Vec<Pos2> { PTS.iter().map(|&(x, y)| tip + o + vec2(x, y) * s).collect() };
    let mesh = |pts: &[Pos2], c: Color32| {
        let mut m = Mesh::default();
        for p in pts {
            m.colored_vertex(*p, c);
        }
        for [a, b, c] in TRIS {
            m.add_triangle(a, b, c);
        }
        m
    };
    painter.add(mesh(&at(vec2(1.5, 2.5)), Color32::from_black_alpha(80)));
    let pts = at(Vec2::ZERO);
    painter.add(mesh(&pts, Color32::WHITE));
    painter.add(PathShape::closed_line(
        pts,
        Stroke::new(1.3, Color32::BLACK),
    ));
}

/// kittest's wgpu renderer without the triangle it paints at the pointer
/// (the recorder draws its own cursor).
pub(crate) struct NoTestCursor(pub WgpuTestRenderer);

impl TestRenderer for NoTestCursor {
    fn handle_delta(&mut self, delta: &mut egui::TexturesDelta) {
        self.0.handle_delta(delta);
    }

    fn render(
        &mut self,
        ctx: &egui::Context,
        output: &egui::FullOutput,
    ) -> Result<image::RgbaImage, String> {
        let mut output = output.clone();
        if ctx.input(|i| i.pointer.hover_pos()).is_some() {
            output.shapes.pop();
        }
        self.0.render(ctx, &output)
    }
}
