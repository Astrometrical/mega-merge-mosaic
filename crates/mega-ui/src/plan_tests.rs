use egui::{ColorImage, Pos2, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;

const SCREEN: Vec2 = Vec2::new(1024.0, 768.0);

struct State {
    canvas: PlanCanvas,
    extent: [f64; 4],
    items: Vec<PlacedItem>,
    log: Vec<PlanResponse>,
    /// Outlines and labels shown.
    outlines: bool,
}

fn harness(extent: [f64; 4], items: Vec<PlacedItem>) -> Harness<'static, State> {
    // Short steps so two clicks land inside egui's double-click window.
    Harness::builder()
        .with_size(SCREEN)
        .with_step_dt(0.01)
        .with_max_steps(400)
        .build_ui_state(
            |ui, s: &mut State| {
                let r = s
                    .canvas
                    .show(ui, s.extent, &s.items, s.outlines, s.outlines);
                s.log.push(r);
            },
            State {
                canvas: PlanCanvas::new(),
                extent,
                items,
                log: Vec::new(),
                outlines: true,
            },
        )
}

fn item(id: u64, rect: [f64; 4]) -> PlacedItem {
    PlacedItem {
        id,
        rect,
        layers: Vec::new(),
        outline: palette::ACCENT_500,
        label: format!("{id}"),
        marker: None,
        selected: false,
        highlighted: false,
    }
}

/// Screen position of canvas point `p` in the last frame.
fn to_screen(h: &Harness<'_, State>, p: (f64, f64)) -> Pos2 {
    let (rect, now) = h.state().canvas.last_frame().expect("shown");
    h.state().canvas.view().image_to_screen(p, rect, now)
}

fn click(h: &mut Harness<'_, State>, p: Pos2) {
    h.hover_at(p);
    h.drag_at(p);
    h.event(egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
}

#[test]
fn click_selects_topmost() {
    let items = vec![
        item(1, [0.0, 0.0, 600.0, 600.0]),
        item(2, [400.0, 400.0, 1000.0, 1000.0]),
    ];
    let mut h = harness([0.0, 0.0, 1000.0, 1000.0], items);
    h.run();

    let cases = [
        ((500.0, 500.0), Some(2)), // both: the later item is on top
        ((100.0, 100.0), Some(1)),
        ((900.0, 900.0), Some(2)),
        ((900.0, 100.0), None),
    ];
    for (p, want) in cases {
        let s = to_screen(&h, p);
        h.state_mut().log.clear();
        click(&mut h, s);
        h.run();
        let clicked: Vec<Option<u64>> = h.state().log.iter().map(|r| r.clicked).collect();
        let empty = h.state().log.iter().any(|r| r.clicked_empty);
        match want {
            Some(id) => {
                assert!(clicked.contains(&Some(id)), "{p:?}: {clicked:?}");
                assert!(!empty, "{p:?}");
            }
            None => {
                assert!(clicked.iter().all(Option::is_none), "{p:?}: {clicked:?}");
                assert!(empty, "{p:?}");
            }
        }
        let hovered = h.state().log.last().unwrap().hovered;
        assert_eq!(hovered, want, "{p:?}");
    }
}

#[test]
fn double_click_zooms_to_item() {
    let target = [600.0, 100.0, 800.0, 300.0];
    let items = vec![item(1, [0.0, 0.0, 2000.0, 1000.0]), item(7, target)];
    let mut h = harness([0.0, 0.0, 2000.0, 1000.0], items);
    h.run();
    let z0 = {
        let (_, now) = h.state().canvas.last_frame().unwrap();
        h.state().canvas.view().zoom(now)
    };
    let p = to_screen(&h, (700.0, 200.0));
    h.state_mut().log.clear();
    click(&mut h, p);
    click(&mut h, p);
    h.run();
    assert!(
        h.state().log.iter().any(|r| r.double_clicked == Some(7)),
        "no double click reported"
    );
    let (rect, now) = h.state().canvas.last_frame().unwrap();
    let view = h.state().canvas.view();
    assert!(view.zoom(now) > z0 * 2.0);
    let a = view.image_to_screen((target[0], target[1]), rect, now);
    let b = view.image_to_screen((target[2], target[3]), rect, now);
    let shown = Rect::from_min_max(a, b);
    // The item is centred, fully visible and fills most of the canvas.
    assert!(rect.contains_rect(shown), "{shown:?} in {rect:?}");
    assert!((shown.center() - rect.center()).length() < 1.0);
    assert!(shown.height() > 0.8 * rect.height(), "{shown:?}");

    // Home fits the extent again.
    h.get_by_label("Home").click();
    h.run();
    let (_, now) = h.state().canvas.last_frame().unwrap();
    assert!((h.state().canvas.view().zoom(now) - z0).abs() < 1e-9);
}

#[test]
fn additive_layer_mesh_has_zero_alpha() {
    let mut h = harness([0.0, 0.0, 1000.0, 1000.0], Vec::new());
    let img = || ColorImage::new([4, 4], vec![Color32::WHITE; 16]);
    let base = h
        .ctx
        .load_texture("base", img(), egui::TextureOptions::LINEAR);
    let add = h
        .ctx
        .load_texture("add", img(), egui::TextureOptions::LINEAR);
    let (base_id, add_id) = (base.id(), add.id());
    let mut it = item(3, [100.0, 100.0, 900.0, 900.0]);
    it.selected = true;
    it.layers = vec![
        Layer {
            texture: base,
            tint: Color32::WHITE,
            additive: false,
        },
        Layer {
            texture: add,
            tint: Color32::from_rgb(200, 50, 0),
            additive: true,
        },
    ];
    h.state_mut().items = vec![it];
    h.run();

    let (mut n_base, mut n_add, mut selected_outline) = (0, 0, false);
    for cs in &h.output().shapes {
        match &cs.shape {
            egui::Shape::Mesh(m) if m.texture_id == add_id => {
                let want = Color32::from_rgba_premultiplied(200, 50, 0, 0);
                assert!(m.vertices.iter().all(|v| v.color == want), "{m:?}");
                n_add += 1;
            }
            egui::Shape::Mesh(m) if m.texture_id == base_id => {
                assert!(m.vertices.iter().all(|v| v.color == Color32::WHITE));
                n_base += 1;
            }
            egui::Shape::Rect(r)
                if r.stroke.color == palette::ACCENT_400 && r.stroke.width == 2.5 =>
            {
                selected_outline = true;
            }
            _ => {}
        }
    }
    assert_eq!((n_base, n_add), (1, 1));
    assert!(
        selected_outline,
        "selected item has no 2.5 px accent outline"
    );
}

/// Where item `label`'s label was drawn last frame.
fn drawn_label(h: &Harness<'_, State>, label: &str) -> Pos2 {
    h.output()
        .shapes
        .iter()
        .find_map(|cs| match &cs.shape {
            egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos),
            _ => None,
        })
        .expect("label drawn")
}

#[test]
fn drag_eases_without_reversal() {
    // Uneven pointer sampling (some frames without an event): the drawn
    // canvas moves every frame, never back, and ends where the pointer took
    // it.
    let items = vec![item(1, [400.0, 400.0, 600.0, 600.0])];
    let mut h = harness([0.0, 0.0, 1000.0, 1000.0], items);
    h.run();
    let p0 = drawn_label(&h, "1");
    let d = Vec2::new(5.0, -3.0);
    let mut p = Pos2::new(300.0, 300.0);
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    let mut track = Vec::new();
    for k in [2.0, 0.0, 1.0, 3.0, 0.0, 0.0, 2.0, 1.0, 0.0, 2.0] {
        if k > 0.0 {
            p += d * k;
            h.hover_at(p);
        }
        h.step();
        track.push(drawn_label(&h, "1"));
    }
    h.drop_at(p);
    h.run();
    track.push(drawn_label(&h, "1"));
    for (i, w) in track.windows(2).enumerate() {
        let along = (w[1] - w[0]).dot(d);
        assert!(along >= -1e-3, "frame {i} moved back: {track:?}");
        if i + 2 < track.len() {
            assert!(along > 1e-3, "frame {i} stood still: {track:?}");
        }
    }
    let moved = drawn_label(&h, "1") - p0;
    assert!((moved - d * 11.0).length() < 0.5, "{moved:?}");
}

/// Centre of item `label`'s number as drawn last frame.
fn label_centre(h: &Harness<'_, State>, label: &str) -> Pos2 {
    h.output()
        .shapes
        .iter()
        .find_map(|cs| match &cs.shape {
            egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos + t.galley.size() / 2.0),
            _ => None,
        })
        .expect("label drawn")
}

/// Where a view at `centre` and `zoom` puts canvas point `p` in the last
/// frame's viewport.
fn through(h: &Harness<'_, State>, centre: (f64, f64), zoom: f64, p: (f64, f64)) -> Pos2 {
    let (rect, _) = h.state().canvas.last_frame().expect("shown");
    rect.center()
        + Vec2::new(
            ((p.0 - centre.0) * zoom) as f32,
            ((p.1 - centre.1) * zoom) as f32,
        )
}

#[test]
fn set_view_for_replaces_the_fit() {
    let items = vec![item(1, [400.0, 400.0, 600.0, 600.0])];
    let first = [0.0, 0.0, 1000.0, 1000.0];
    let mut h = harness(first, items);
    h.run();
    // The extent already fitted: the next frame is painted through the view.
    h.state_mut()
        .canvas
        .set_view_for(first, (300.0, 450.0), 2.0);
    h.step();
    let want = through(&h, (300.0, 450.0), 2.0, (500.0, 500.0));
    assert!((label_centre(&h, "1") - want).length() < 0.5);
    // A new extent: its first frame is painted through the view, never
    // fitted, and the view stays.
    let wide = [0.0, 0.0, 2000.0, 1000.0];
    h.state_mut().extent = wide;
    h.state_mut().canvas.set_view_for(wide, (700.0, 300.0), 1.5);
    h.step();
    let want = through(&h, (700.0, 300.0), 1.5, (500.0, 500.0));
    assert!((label_centre(&h, "1") - want).length() < 0.5);
    h.run();
    assert!((label_centre(&h, "1") - want).length() < 0.5);
    // The zoom limits are the new extent's fit's: half the fit zoom at least.
    let wider = [0.0, 0.0, 4000.0, 1000.0];
    h.state_mut().extent = wider;
    h.state_mut()
        .canvas
        .set_view_for(wider, (2000.0, 500.0), 1e-3);
    h.step();
    let (rect, now) = h.state().canvas.last_frame().unwrap();
    let fit = (f64::from(rect.width()) / 4000.0).min(f64::from(rect.height()) / 1000.0);
    let zoom = h.state().canvas.view().zoom(now);
    assert!((zoom - fit / 2.0).abs() < 1e-9, "{zoom} vs fit {fit}");
    // Given for another extent than the one drawn: forgotten, and the drawn
    // extent is fitted as usual.
    let small = [0.0, 0.0, 500.0, 500.0];
    h.state_mut().extent = small;
    h.state_mut()
        .canvas
        .set_view_for([0.0, 0.0, 1.0, 1.0], (0.5, 0.5), 3.0);
    h.step();
    let (_, now) = h.state().canvas.last_frame().unwrap();
    let c = h.state().canvas.view().centre(now);
    assert!(
        (c.0 - 250.0).abs() < 1e-9 && (c.1 - 250.0).abs() < 1e-9,
        "{c:?}"
    );
    h.state_mut().extent = first;
    h.step();
    let (_, now) = h.state().canvas.last_frame().unwrap();
    let c = h.state().canvas.view().centre(now);
    assert!(
        (c.0 - 500.0).abs() < 1e-9 && (c.1 - 500.0).abs() < 1e-9,
        "{c:?}"
    );
}

/// The outline strokes painted last frame exactly on `rect` (screen), in
/// paint order.
fn strokes_on(h: &Harness<'_, State>, rect: Rect) -> Vec<Stroke> {
    let near = |a: Rect| (a.min - rect.min).length() < 0.5 && (a.max - rect.max).length() < 0.5;
    h.output()
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            Shape::Rect(r) if r.stroke.width > 0.0 && near(r.rect) => Some(r.stroke),
            _ => None,
        })
        .collect()
}

#[test]
fn a_highlighted_item_is_outlined_under_the_selection() {
    let (a, b, c) = (
        [0.0, 0.0, 300.0, 300.0],
        [350.0, 350.0, 650.0, 650.0],
        [700.0, 700.0, 1000.0, 1000.0],
    );
    let mut items = vec![item(1, a), item(2, b), item(3, c)];
    items[0].highlighted = true;
    items[1].highlighted = true;
    items[1].selected = true;
    let mut h = harness([0.0, 0.0, 1000.0, 1000.0], items);
    // Drawn whether or not outlines are shown, as the selection is.
    h.state_mut().outlines = false;
    h.run();
    let screen = |h: &Harness<'_, State>, r: [f64; 4]| {
        Rect::from_min_max(to_screen(h, (r[0], r[1])), to_screen(h, (r[2], r[3])))
    };
    let highlight = Stroke::new(
        SELECTED_STROKE,
        palette::ACCENT_400.gamma_multiply(HIGHLIGHT_STRENGTH),
    );
    let selection = Stroke::new(SELECTED_STROKE, palette::ACCENT_400);
    assert_eq!(strokes_on(&h, screen(&h, a)), [highlight]);
    // Under the selection: painted first, the selection over it.
    assert_eq!(strokes_on(&h, screen(&h, b)), [highlight, selection]);
    assert!(strokes_on(&h, screen(&h, c)).is_empty(), "outlines hidden");
    // With outlines shown, the highlight stands in for the item's own.
    let mut it = item(4, a);
    it.layers = vec![Layer {
        texture: h.ctx.load_texture(
            "img",
            ColorImage::new([2, 2], vec![Color32::WHITE; 4]),
            egui::TextureOptions::LINEAR,
        ),
        tint: Color32::WHITE,
        additive: false,
    }];
    it.highlighted = true;
    h.state_mut().items = vec![it];
    h.state_mut().outlines = true;
    h.run();
    assert_eq!(strokes_on(&h, screen(&h, a)), [highlight]);
    h.state_mut().items[0].highlighted = false;
    h.run();
    let plain = Stroke::new(OUTLINE_STROKE, palette::ACCENT_500);
    assert_eq!(strokes_on(&h, screen(&h, a)), [plain]);
}

/// Bounding box of a mesh's vertex positions, and of its uvs.
fn mesh_bounds(m: &egui::Mesh) -> (Rect, Rect) {
    let pos = Rect::from_points(&m.vertices.iter().map(|v| v.pos).collect::<Vec<_>>());
    let uv = Rect::from_points(&m.vertices.iter().map(|v| v.uv).collect::<Vec<_>>());
    (pos, uv)
}

fn texture(h: &Harness<'_, State>, name: &str) -> egui::TextureHandle {
    h.ctx.load_texture(
        name,
        ColorImage::new([4, 4], vec![Color32::WHITE; 16]),
        egui::TextureOptions::LINEAR,
    )
}

fn screen_rect(h: &Harness<'_, State>, r: [f64; 4]) -> Rect {
    Rect::from_min_max(to_screen(h, (r[0], r[1])), to_screen(h, (r[2], r[3])))
}

fn near(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() < 0.5 && (a.max - b.max).length() < 0.5
}

#[test]
fn the_plan_draws_overlays_and_patches() {
    let mut h = harness([0.0, 0.0, 1000.0, 1000.0], Vec::new());
    let tex = texture(&h, "patch");
    let id = tex.id();
    let patch_rect = [100.0, 100.0, 500.0, 500.0];
    let clip = [300.0, 100.0, 800.0, 400.0];
    h.state_mut().canvas.set_patches(vec![Patch {
        rect: patch_rect,
        clip,
        texture: tex,
    }]);
    let roi = [200.0, 600.0, 700.0, 900.0];
    h.state_mut().canvas.set_overlays(vec![Overlay {
        rect: roi,
        color: Color32::RED,
        label: "Region 1".into(),
        dashed: false,
    }]);
    h.run();

    // The patch is clipped: only x 300..500, y 100..400 of it is drawn,
    // with the matching part of the texture.
    let meshes: Vec<_> = h
        .output()
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            Shape::Mesh(m) if m.texture_id == id => Some(mesh_bounds(m)),
            _ => None,
        })
        .collect();
    assert_eq!(meshes.len(), 1, "one patch mesh");
    let (pos, uv) = meshes[0];
    let want = screen_rect(&h, [300.0, 100.0, 500.0, 400.0]);
    assert!(near(pos, want), "{pos:?} vs {want:?}");
    let want_uv = Rect::from_min_max(Pos2::new(0.5, 0.0), Pos2::new(1.0, 0.75));
    assert!(
        (uv.min - want_uv.min).length() < 1e-4 && (uv.max - want_uv.max).length() < 1e-4,
        "{uv:?}"
    );

    // The overlay is stroked at its mapped rect and labelled.
    let strokes = strokes_on(&h, screen_rect(&h, roi));
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].color, Color32::RED);
    assert!(drawn_label_exists(&h, "Region 1"));

    // A patch wholly outside its clip draws nothing.
    let tex2 = texture(&h, "patch2");
    let id2 = tex2.id();
    h.state_mut().canvas.set_patches(vec![Patch {
        rect: patch_rect,
        clip: [600.0, 600.0, 700.0, 700.0],
        texture: tex2,
    }]);
    h.run();
    let drawn = h
        .output()
        .shapes
        .iter()
        .any(|cs| matches!(&cs.shape, Shape::Mesh(m) if m.texture_id == id2));
    assert!(!drawn, "a patch outside its clip drew");
}

fn drawn_label_exists(h: &Harness<'_, State>, label: &str) -> bool {
    h.output()
        .shapes
        .iter()
        .any(|cs| matches!(&cs.shape, Shape::Text(t) if t.galley.text() == label))
}

fn drag_between(h: &mut Harness<'_, State>, from: (f64, f64), to: (f64, f64)) {
    let (from, to) = (to_screen(h, from), to_screen(h, to));
    h.drag_at(from);
    h.step();
    h.hover_at(Pos2::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0));
    h.step();
    h.hover_at(to);
    h.step();
    h.drop_at(to);
    h.run();
}

fn drawn_rects(h: &Harness<'_, State>) -> Vec<[f64; 4]> {
    h.state().log.iter().filter_map(|r| r.drawn_rect).collect()
}

#[test]
fn the_plan_draw_tool_returns_a_rect() {
    let mut h = harness(
        [0.0, 0.0, 1000.0, 1000.0],
        vec![item(1, [0.0, 0.0, 1000.0, 1000.0])],
    );
    h.run();
    assert_eq!(h.state().canvas.tool(), Tool::Navigate);
    h.state_mut().canvas.set_tool(Tool::DrawRect);
    h.run();
    let view0 = {
        let (_, now) = h.state().canvas.last_frame().unwrap();
        (
            h.state().canvas.view().centre(now),
            h.state().canvas.view().zoom(now),
        )
    };
    // Dragged up-left: normalised, in canvas coordinates; neither panned
    // nor clicked.
    let (a, b) = ((600.0, 500.0), (200.0, 300.0));
    drag_between(&mut h, a, b);
    let rects = drawn_rects(&h);
    assert_eq!(rects.len(), 1, "one rect per drag");
    let want = [200.0, 300.0, 600.0, 500.0];
    for i in 0..4 {
        assert!((rects[0][i] - want[i]).abs() < 1.0, "{:?}", rects[0]);
    }
    assert!(
        h.state()
            .log
            .iter()
            .all(|r| r.clicked.is_none() && !r.clicked_empty)
    );
    let (_, now) = h.state().canvas.last_frame().unwrap();
    assert_eq!(
        (
            h.state().canvas.view().centre(now),
            h.state().canvas.view().zoom(now)
        ),
        view0,
        "drawing panned"
    );
    // Back to Navigate: the next drag pans.
    assert_eq!(h.state().canvas.tool(), Tool::Navigate);
    drag_between(&mut h, a, b);
    assert_eq!(drawn_rects(&h).len(), 1);
    let (_, now) = h.state().canvas.last_frame().unwrap();
    assert_ne!(h.state().canvas.view().centre(now), view0.0);

    // Clamped to the extent.
    h.state_mut().canvas.home();
    h.run();
    h.state_mut().canvas.set_tool(Tool::DrawRect);
    h.run();
    drag_between(&mut h, (-100.0, 500.0), (400.0, 700.0));
    let r = drawn_rects(&h)[1];
    assert!(r[0] == 0.0 && (r[2] - 400.0).abs() < 1.0, "{r:?}");

    // Esc cancels the tool and the rest of the drag.
    h.state_mut().canvas.set_tool(Tool::DrawRect);
    h.run();
    let before = {
        let (_, now) = h.state().canvas.last_frame().unwrap();
        (
            h.state().canvas.view().centre(now),
            h.state().canvas.view().zoom(now),
        )
    };
    h.drag_at(to_screen(&h, a));
    h.step();
    h.hover_at(to_screen(&h, (400.0, 400.0)));
    h.step();
    h.key_press(egui::Key::Escape);
    h.step();
    h.hover_at(to_screen(&h, b));
    h.step();
    h.drop_at(to_screen(&h, b));
    h.run();
    assert_eq!(drawn_rects(&h).len(), 2, "cancelled drag reported");
    let (_, now) = h.state().canvas.last_frame().unwrap();
    assert_eq!(
        (
            h.state().canvas.view().centre(now),
            h.state().canvas.view().zoom(now)
        ),
        before,
        "cancelled drag panned"
    );
    assert_eq!(h.state().canvas.tool(), Tool::Navigate);
}

#[test]
fn overlay_button_labels_stay_on_one_line_in_a_narrow_canvas() {
    let mut h = Harness::builder()
        .with_size(Vec2::new(640.0, 280.0))
        .build_ui_state(
            |ui, canvas: &mut PlanCanvas| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                canvas.show(ui, [0.0, 0.0, 1000.0, 440.0], &[], true, true);
            },
            PlanCanvas::new(),
        );
    h.run();
    for label in ["Home", "Zoom to selection"] {
        let rows = h
            .output()
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                Shape::Text(t) if t.galley.text() == label => Some(t.galley.rows.len()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{label} not drawn"));
        assert_eq!(rows, 1, "{label} wrapped");
    }
}

#[test]
fn a_held_press_is_reported_until_released() {
    let mut h = harness(
        [0.0, 0.0, 1000.0, 1000.0],
        vec![item(1, [0.0, 0.0, 1000.0, 1000.0])],
    );
    h.run();
    let held = |h: &Harness<'_, State>| h.state().log.last().and_then(|r| r.held);
    assert_eq!(held(&h), None);
    let at = to_screen(&h, (300.0, 400.0));
    h.hover_at(at);
    h.step();
    h.drag_at(at);
    h.step();
    let got = held(&h).expect("held");
    assert!((got.origin.0 - 300.0).abs() < 1e-3 && (got.origin.1 - 400.0).abs() < 1e-3);
    assert!(!got.moved);
    // Past a click's time, still: held, not moved.
    h.run_steps(100);
    assert_eq!(held(&h).map(|h| h.moved), Some(false));
    // Past the click distance: moved (and panned), for the rest of the press.
    h.hover_at(at + Vec2::new(0.0, 60.0));
    h.step();
    assert_eq!(held(&h).map(|h| h.moved), Some(true));
    h.hover_at(at);
    h.step();
    assert_eq!(held(&h).map(|h| h.moved), Some(true));
    h.drop_at(at);
    h.step();
    assert_eq!(held(&h), None);
    // The draw tool's press is no hold.
    h.state_mut().canvas.set_tool(Tool::DrawRect);
    h.run();
    h.hover_at(at);
    h.drag_at(at);
    h.step();
    assert_eq!(held(&h), None);
    h.drop_at(at);
    h.run();
}
