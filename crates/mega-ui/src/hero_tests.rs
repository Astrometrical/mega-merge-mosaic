use std::sync::Arc;

use egui::{Color32, ColorImage, Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::theme::palette::SURFACE_900;
use crate::theme::palette::TEXT_SUBTLE;
use crate::theme::tokens::KEN_BURNS_ZOOM;
use crate::viewer::ImageTileSource;
use crate::viewer::{Fetched, PyramidGeometry, TileKey, TileSource};
use std::sync::atomic::{AtomicBool, Ordering};

const EPS: f64 = 0.01;

struct State {
    stage: HeroStage,
    now: f64,
    /// How far above the bottom of the drawing area [`harness`] puts the
    /// floor.
    floor_inset: f32,
    /// The drawing area, as of the last frame.
    full: Rect,
}

fn state(stage: HeroStage) -> State {
    State {
        stage,
        now: 0.0,
        floor_inset: 0.0,
        full: Rect::NOTHING,
    }
}

fn view(label: &str, color: Color32) -> HeroView {
    HeroView {
        label: label.to_string(),
        source: Arc::new(ImageTileSource::new(
            ColorImage::new([640, 480], vec![color; 640 * 480]),
            256,
        )),
    }
}

fn two_views() -> Vec<HeroView> {
    vec![
        view("Ha", Color32::from_rgb(200, 40, 40)),
        view("OIII", Color32::from_rgb(40, 120, 200)),
    ]
}

fn harness(views: Vec<HeroView>, reduced: bool) -> Harness<'static, State> {
    let mut stage = HeroStage::new_synchronous();
    stage.set_reduced_motion(reduced);
    stage.set_views(views, 0, 0.0);
    Harness::builder()
        .with_size(Vec2::new(900.0, 500.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let r = ui.max_rect();
                s.full = r;
                let image = Rect::from_min_max(pos2(r.left() + r.width() / 3.0, r.top()), r.max);
                s.stage.show(ui, image, r.bottom() - s.floor_inset, s.now);
            },
            state(stage),
        )
}

fn frames(h: &mut Harness<'_, State>, now: f64) {
    h.state_mut().now = now;
    h.run_steps(3);
}

fn label(h: &Harness<'_, State>) -> Option<String> {
    h.state().stage.current_label().map(str::to_string)
}

#[test]
fn cycles_after_the_dwell() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert!(!h.state().stage.is_crossfading());
    frames(&mut h, 8.0 + EPS);
    assert!(h.state().stage.is_crossfading());
    frames(&mut h, 10.1);
    assert!(!h.state().stage.is_crossfading());
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

#[test]
fn pause_holds_and_next_advances() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    h.get_by_label("Pause slideshow").click();
    frames(&mut h, 1.0);
    frames(&mut h, 20.0);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert!(h.query_by_label("Resume slideshow").is_some());
    h.get_by_label("Next image").click();
    frames(&mut h, 20.0);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

#[test]
fn reduced_motion_cuts() {
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    let zoom = |h: &Harness<'_, State>| {
        let s = &h.state().stage;
        let v = s.viewers[s.current].as_ref().expect("viewer");
        let (_, t) = v.last_frame().expect("shown");
        v.view().zoom(t)
    };
    let z0 = zoom(&h);
    frames(&mut h, 4.0);
    assert_eq!(zoom(&h), z0);
    // The next view is already loaded when the cut comes.
    frames(&mut h, 6.5);
    let next = h.state().stage.next_index();
    assert!(h.state().stage.drawn(next));
    frames(&mut h, 8.0 + EPS);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
    assert!(!h.state().stage.is_crossfading());
}

#[test]
fn one_view_never_cycles() {
    let mut h = harness(vec![view("Ha", Color32::RED)], false);
    frames(&mut h, 0.0);
    frames(&mut h, 30.0);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert!(!h.state().stage.is_crossfading());
}

#[test]
fn message_shows_without_views() {
    let mut h = harness(Vec::new(), false);
    h.state_mut()
        .stage
        .set_message(Some("No panels yet — resume to add them".into()));
    frames(&mut h, 0.0);
    assert!(
        h.query_by_label("No panels yet — resume to add them")
            .is_some()
    );
    assert_eq!(label(&h), None);
}

#[test]
fn first_image_reports_drawn() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    assert!(h.state().stage.first_image_drawn());
}

#[test]
fn drift_zooms_in_over_time() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    let zoom = |h: &Harness<'_, State>| {
        let s = &h.state().stage;
        let v = s.viewers[s.current].as_ref().expect("viewer");
        let (_, t) = v.last_frame().expect("shown");
        v.view().zoom(t)
    };
    let z0 = zoom(&h);
    frames(&mut h, 5.0);
    assert!(zoom(&h) > z0 * 1.01);
}

#[test]
fn cut_keeps_the_outgoing_view_until_the_incoming_has_tiles() {
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    // Jump past the cut with nothing preloaded: the old view holds for the
    // frame in which the new viewer is built, then the cut happens.
    h.state_mut().now = 8.0 + EPS;
    h.step();
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    h.run_steps(2);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
    assert!(h.state().stage.drawn(1));
}

#[test]
fn next_waits_for_tiles_then_swaps() {
    let mut h = harness(two_views(), false);
    frames(&mut h, 0.0);
    h.get_by_label("Next image").click();
    h.state_mut().now = 1.0;
    h.step();
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    h.run_steps(3);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

#[test]
fn an_odd_view_starts_on_its_drift_camera() {
    let zoom_of = |start: usize| {
        let mut h = harness(two_views(), false);
        h.state_mut().stage.set_views(two_views(), start, 0.0);
        frames(&mut h, 0.0);
        let s = &h.state().stage;
        let v = s.viewers[s.current].as_ref().expect("viewer");
        let (_, t) = v.last_frame().expect("shown");
        v.view().zoom(t)
    };
    let ratio = zoom_of(1) / zoom_of(0);
    assert!((ratio - KEN_BURNS_ZOOM).abs() < 1e-6, "{ratio}");
}

/// A one-colour view of `size` pixels.
fn sized(label: &str, size: [usize; 2]) -> HeroView {
    HeroView {
        label: label.to_string(),
        source: Arc::new(ImageTileSource::new(
            ColorImage::new(size, vec![Color32::GRAY; size[0] * size[1]]),
            256,
        )),
    }
}

/// A 900 × 500 harness drawing `views` (from `start`) into the fixed image
/// area `image`.
fn harness_in(
    views: Vec<HeroView>,
    start: usize,
    reduced: bool,
    image: Rect,
) -> Harness<'static, State> {
    let mut stage = HeroStage::new_synchronous();
    stage.set_reduced_motion(reduced);
    stage.set_views(views, start, 0.0);
    Harness::builder()
        .with_size(Vec2::new(900.0, 500.0))
        .build_ui_state(
            move |ui, s: &mut State| {
                if crate::testutil::themed(ui) {
                    s.stage.show(ui, image, image.bottom(), s.now);
                }
            },
            state(stage),
        )
}

/// The image area of [`harness_in`] tests: the right two thirds of the
/// window (600 × 500).
fn area() -> Rect {
    Rect::from_min_max(pos2(300.0, 0.0), pos2(900.0, 500.0))
}

/// Every drawn view's visible image rect lies within its image: the image
/// covers the whole area, so none of its edges shows.
fn assert_covered(h: &Harness<'_, State>, when: &str) {
    let s = &h.state().stage;
    let mut drawn = 0;
    for (i, v) in s.viewers.iter().enumerate() {
        let Some((rect, t)) = v.as_ref().and_then(TiledViewer::last_frame) else {
            continue;
        };
        drawn += 1;
        let v = v.as_ref().expect("viewer");
        let (w, ht) = (s.geoms[i].width as f64, s.geoms[i].height as f64);
        let [x0, y0, x1, y1] = v.view().visible_image_rect(rect, t);
        let eps = 1e-6 * w.max(ht);
        assert!(
            x0 >= -eps && y0 >= -eps && x1 <= w + eps && y1 <= ht + eps,
            "{when}, view {i}: shows [{x0:.2}, {y0:.2}, {x1:.2}, {y1:.2}] of {w} × {ht}"
        );
    }
    assert!(drawn > 0, "{when}: nothing drawn");
}

#[test]
fn portrait_image_covers_a_landscape_area() {
    // A portrait mosaic (9255 × 18310 has this aspect) in a landscape area.
    for reduced in [false, true] {
        // Even views zoom in, odd ones zoom out.
        for start in [0, 1] {
            let views = vec![sized("Ha", [250, 500]), sized("OIII", [250, 500])];
            let mut h = harness_in(views, start, reduced, area());
            // Through the dwell and the crossfade into the next view.
            for step in 0..=24 {
                let t = f64::from(step) * 0.5;
                frames(&mut h, t);
                assert_covered(&h, &format!("reduced {reduced}, start {start}, {t} s"));
            }
        }
    }
}

#[test]
fn matching_aspect_keeps_its_drift() {
    // The image has the area's aspect: the cover zoom is the fit zoom, and
    // the drift zooms by KEN_BURNS_ZOOM from it as before, within the image.
    let views = vec![sized("Ha", [1200, 1000]), sized("OIII", [1200, 1000])];
    let mut h = harness_in(views, 0, false, area());
    let zoom = |h: &Harness<'_, State>| {
        let s = &h.state().stage;
        let v = s.viewers[s.current].as_ref().expect("viewer");
        let (_, t) = v.last_frame().expect("shown");
        v.view().zoom(t)
    };
    frames(&mut h, 0.0);
    assert!((zoom(&h) - 0.5).abs() < 1e-9, "{}", zoom(&h));
    assert_covered(&h, "start");
    frames(&mut h, 5.0);
    let halfway = 1.0 + (KEN_BURNS_ZOOM - 1.0) / 2.0;
    assert!((zoom(&h) / 0.5 - halfway).abs() < 1e-9, "{}", zoom(&h));
    assert_covered(&h, "halfway");
}

/// A source that serves no tiles until `open` is set.
struct Gated {
    inner: ImageTileSource,
    open: Arc<AtomicBool>,
}

impl TileSource for Gated {
    fn geometry(&self) -> PyramidGeometry {
        self.inner.geometry()
    }
    fn generation(&self) -> u64 {
        u64::from(self.open.load(Ordering::SeqCst))
    }
    fn fetch(&self, key: TileKey) -> Fetched {
        if self.open.load(Ordering::SeqCst) {
            self.inner.fetch(key)
        } else {
            Fetched::Missing
        }
    }
}

#[test]
fn crossfade_keeps_the_outgoing_view_while_the_incoming_has_no_tiles() {
    let open = Arc::new(AtomicBool::new(false));
    let slow = HeroView {
        label: "OIII".into(),
        source: Arc::new(Gated {
            inner: ImageTileSource::new(
                ColorImage::new([640, 480], vec![Color32::BLUE; 640 * 480]),
                256,
            ),
            open: open.clone(),
        }),
    };
    let mut h = harness(vec![view("Ha", Color32::RED), slow], false);
    frames(&mut h, 0.0);
    frames(&mut h, 6.5);
    let outgoing_opacity = |h: &Harness<'_, State>| {
        let s = &h.state().stage;
        s.viewers[s.current].as_ref().expect("viewer").opacity()
    };
    // Past dwell + crossfade, still no incoming tiles.
    frames(&mut h, 10.5);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    assert_eq!(outgoing_opacity(&h), 1.0);
    // Tiles arrive: the swap happens.
    open.store(true, Ordering::SeqCst);
    frames(&mut h, 10.6);
    frames(&mut h, 10.7);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

#[test]
fn crossfade_swaps_after_the_hold_when_tiles_never_come() {
    let open = Arc::new(AtomicBool::new(false));
    let slow = HeroView {
        label: "OIII".into(),
        source: Arc::new(Gated {
            inner: ImageTileSource::new(
                ColorImage::new([640, 480], vec![Color32::BLUE; 640 * 480]),
                256,
            ),
            open,
        }),
    };
    let mut h = harness(vec![view("Ha", Color32::RED), slow], false);
    frames(&mut h, 0.0);
    frames(&mut h, 6.5);
    frames(&mut h, 10.5);
    assert_eq!(label(&h).as_deref(), Some("Ha"));
    frames(&mut h, 12.5);
    assert_eq!(label(&h).as_deref(), Some("OIII"));
}

/// Every mesh painted in the last frame.
fn meshes(h: &Harness<'_, State>) -> Vec<egui::Mesh> {
    fn walk(shape: &egui::Shape, out: &mut Vec<egui::Mesh>) {
        match shape {
            egui::Shape::Mesh(m) => out.push((**m).clone()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// Whether a feather ramp lies on `edge`: a mesh whose vertices there are
/// opaque page colour and whose others are transparent.
fn has_ramp(meshes: &[egui::Mesh], edge: impl Fn(egui::Pos2) -> bool) -> bool {
    meshes.iter().any(|m| {
        let on: Vec<_> = m.vertices.iter().filter(|v| edge(v.pos)).collect();
        let off = m.vertices.len() - on.len();
        m.vertices.len() == 4
            && on.len() == 2
            && off == 2
            && on.iter().all(|v| v.color == SURFACE_900)
            && m.vertices
                .iter()
                .filter(|v| !edge(v.pos))
                .all(|v| v.color == Color32::TRANSPARENT)
    })
}

#[test]
fn image_edges_off_the_window_edge_are_feathered() {
    // The harness image runs from a third of the width to the right and
    // bottom edges of the window: only its left edge is feathered.
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    let m = meshes(&h);
    let ramps = |m: &[egui::Mesh]| -> Vec<(egui::Pos2, egui::Pos2)> {
        m.iter()
            .filter(|m| m.vertices.len() == 4)
            .filter(|m| m.vertices.iter().filter(|v| v.color == SURFACE_900).count() == 2)
            .filter(|m| {
                m.vertices
                    .iter()
                    .filter(|v| v.color == Color32::TRANSPARENT)
                    .count()
                    == 2
            })
            .map(|m| (m.vertices[0].pos, m.vertices[2].pos))
            .collect()
    };
    let r = ramps(&m);
    assert_eq!(r.len(), 1, "{r:?}");
    // Opaque at the image's left edge, clear to its right.
    assert!(r[0].1.x > r[0].0.x);

    // An image inset from the bottom and right is feathered there too.
    let mut stage = HeroStage::new_synchronous();
    stage.set_reduced_motion(true);
    stage.set_views(two_views(), 0, 0.0);
    let mut h = Harness::builder()
        .with_size(Vec2::new(900.0, 500.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let image = Rect::from_min_max(pos2(300.0, -10.0), pos2(850.0, 400.0));
                s.stage.show(ui, image, image.bottom(), 0.0);
            },
            state(stage),
        );
    h.run_steps(3);
    let m = meshes(&h);
    assert!(has_ramp(&m, |p| (p.y - 400.0).abs() < 0.5));
    assert!(has_ramp(&m, |p| (p.x - 850.0).abs() < 0.5));
    // The top overhangs the window: no ramp there.
    assert!(!has_ramp(&m, |p| (p.y + 10.0).abs() < 0.5));
}

/// A [`harness`] whose floor (the top of a shelf laid over the image's
/// foot) is `inset` points above the bottom.
fn harness_over(views: Vec<HeroView>, inset: f32) -> Harness<'static, State> {
    let mut h = harness(views, true);
    h.state_mut().floor_inset = inset;
    h
}

#[test]
fn label_and_controls_sit_just_above_the_floor() {
    let mut h = harness_over(two_views(), 150.0);
    frames(&mut h, 0.0);
    let full = h.state().full;
    let floor = full.bottom() - 150.0;
    let next = h.get_by_label("Next image").rect();
    let label = h.get_by_label("Ha").rect();
    for (what, r) in [("Next", next), ("label", label)] {
        assert!(
            r.bottom() <= floor - space::S,
            "{what} {r:?} reaches the floor {floor}"
        );
        assert!(
            r.top() >= floor - 2.0 * CONTROL_HEIGHT,
            "{what} {r:?} is far above the floor {floor}"
        );
    }
    // Their right edge keeps the page margin, like the content below.
    assert!(
        (next.right() - (full.right() - space::XXL)).abs() < 1.0,
        "{next:?}"
    );
}

/// The painted bottom scrim: the six-vertex mesh, rows at the bottom, the
/// floor and the top of its rise, opaque [`FLOOR_SCRIM`] at the bottom.
fn scrim(h: &Harness<'_, State>) -> egui::Mesh {
    let dark = SURFACE_900.gamma_multiply(FLOOR_SCRIM);
    meshes(h)
        .into_iter()
        .find(|m| m.vertices.len() == 6 && m.vertices[0].color == dark)
        .expect("a bottom scrim")
}

/// The colour `scrim` leaves at height `y` over a white image (the worst
/// case for light text): premultiplied blending, as egui's renderers do.
fn over_white(scrim: &egui::Mesh, y: f32) -> Color32 {
    let rows: Vec<_> = scrim.vertices.iter().step_by(2).collect();
    let src = rows
        .windows(2)
        .find_map(|w| {
            let (a, b) = (w[0], w[1]);
            let (lo, hi) = (a.pos.y.min(b.pos.y), a.pos.y.max(b.pos.y));
            (lo <= y && y <= hi && hi > lo).then(|| {
                let t = (y - a.pos.y) / (b.pos.y - a.pos.y);
                a.color.lerp_to_gamma(b.color, t)
            })
        })
        .unwrap_or(Color32::TRANSPARENT);
    let v = |c: u8| (f32::from(c) + 255.0 * (1.0 - f32::from(src.a()) / 255.0)).round() as u8;
    Color32::from_rgb(v(src.r()), v(src.g()), v(src.b()))
}

#[test]
fn text_on_and_above_the_floor_stays_legible_over_any_image() {
    let mut h = harness_over(two_views(), 150.0);
    frames(&mut h, 0.0);
    let full = h.state().full;
    let floor = full.bottom() - 150.0;
    let s = scrim(&h);
    // The shelf's faintest text (overlines) anywhere from the floor down.
    for y in [floor, floor + 20.0, full.bottom() - 1.0] {
        let bg = over_white(&s, y);
        let ratio = crate::theme::contrast_ratio(TEXT_SUBTLE, bg);
        assert!(ratio >= 4.5, "at {y}: {ratio} on {bg:?}");
    }
    // The hero's label, just above the floor, at its top edge.
    let top = h.get_by_label("Ha").rect().top();
    let bg = over_white(&s, top);
    let ratio = crate::theme::contrast_ratio(TEXT_EMPHASIS, bg);
    assert!(ratio >= 4.5, "label top {top}: {ratio} on {bg:?}");
    // High up the image, nothing is darkened.
    assert_eq!(over_white(&s, full.top() + 1.0), Color32::WHITE);
}
