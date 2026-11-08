use std::sync::Arc;

use egui::{Color32, ColorImage, Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::theme::tokens::KEN_BURNS_ZOOM;
use crate::viewer::ImageTileSource;
use crate::viewer::{Fetched, PyramidGeometry, TileKey, TileSource};
use std::sync::atomic::{AtomicBool, Ordering};

const EPS: f64 = 0.01;

struct State {
    stage: HeroStage,
    now: f64,
    /// The drawing area, as of the last frame.
    full: Rect,
}

fn state(stage: HeroStage) -> State {
    State {
        stage,
        now: 0.0,
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
                s.stage.show(ui, column(r), s.now);
            },
            state(stage),
        )
}

/// The right edge of [`harness`]'s content column: a third of the way in.
fn column(full: Rect) -> f32 {
    full.left() + full.width() / 3.0
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

/// A 900 × 500 harness drawing `views` (from `start`) into the fixed area
/// `image` (the stage fills its ui's max rect).
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
                    ui.scope_builder(UiBuilder::new().max_rect(image), |ui| {
                        s.stage.show(ui, image.left(), s.now);
                    });
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

/// How much the stage's page-colour meshes (untextured, near-black
/// vertices) darken the image at `p`: 0 not at all, 1 to the page colour.
fn darkening_at(h: &Harness<'_, State>, p: egui::Pos2) -> f32 {
    let mut clear = 1.0;
    for m in meshes(h) {
        let dark = m
            .vertices
            .iter()
            .all(|v| v.color.r() <= 8 && v.color.g() <= 8 && v.color.b() <= 8);
        if m.texture_id != egui::TextureId::default() || !dark {
            continue;
        }
        for t in m.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.vertices[i as usize]);
            let d = (b.pos.y - c.pos.y) * (a.pos.x - c.pos.x)
                + (c.pos.x - b.pos.x) * (a.pos.y - c.pos.y);
            if d.abs() < 1e-6 {
                continue;
            }
            let wa =
                ((b.pos.y - c.pos.y) * (p.x - c.pos.x) + (c.pos.x - b.pos.x) * (p.y - c.pos.y)) / d;
            let wb =
                ((c.pos.y - a.pos.y) * (p.x - c.pos.x) + (a.pos.x - c.pos.x) * (p.y - c.pos.y)) / d;
            let wc = 1.0 - wa - wb;
            if [wa, wb, wc].iter().all(|&w| w >= -1e-4) {
                let alpha = |v: egui::epaint::Vertex| f32::from(v.color.a()) / 255.0;
                clear *= 1.0 - (wa * alpha(a) + wb * alpha(b) + wc * alpha(c));
                break;
            }
        }
    }
    1.0 - clear
}

/// Whether the starfield was painted: an untextured mesh with bright
/// vertices.
fn starfield_painted(h: &Harness<'_, State>) -> bool {
    meshes(h).iter().any(|m| {
        m.texture_id == egui::TextureId::default() && m.vertices.iter().any(|v| v.color.r() > 64)
    })
}

#[test]
fn starfield_stands_in_until_the_image_is_on_screen() {
    let mut h = harness(Vec::new(), true);
    frames(&mut h, 0.0);
    assert!(starfield_painted(&h), "no views: the starfield shows");
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    assert!(h.state().stage.first_image_drawn());
    assert!(
        !starfield_painted(&h),
        "the image is on screen: no starfield"
    );
}

#[test]
fn label_and_controls_sit_in_the_top_right_feather() {
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    let full = h.state().full;
    let next = h.get_by_label("Next image").rect();
    assert!(
        (next.right() - (full.right() - space::XXL)).abs() < 1.0,
        "{next:?}"
    );
    assert!(
        (next.top() - (full.top() + space::XXL)).abs() < 1.0,
        "{next:?}"
    );
    // The label lies in the feather's hold, corner to corner.
    let label = h.get_by_label("Ha").rect();
    for p in [
        label.left_top(),
        label.right_top(),
        label.left_bottom(),
        label.right_bottom(),
    ] {
        let d = darkening_at(&h, p);
        assert!(
            (d - TOP_FEATHER_OPACITY).abs() < 0.01,
            "label at {p:?}: {d}"
        );
    }
}

/// The first row below the top feather's reach.
fn below_top_feather(full: Rect) -> f32 {
    full.top() + space::XXL + CONTROL_HEIGHT + TOP_FEATHER_EASE + 1.0
}

#[test]
fn left_feather_holds_over_the_column_then_eases_to_clear() {
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    let full = h.state().full;
    let (col, y) = (column(full), below_top_feather(full));
    let at = |x: f32| darkening_at(&h, pos2(x, y));
    let edge = at(full.left() + 1.0);
    // Darkened, but never to black: the image shows through.
    assert!(edge > 0.0 && edge < 1.0, "{edge}");
    assert!((at(col - 1.0) - edge).abs() < 0.01, "holds over the column");
    let ease = LEFT_FEATHER_EASE * full.width();
    let eased: Vec<f32> = (0..=10).map(|i| at(col + ease * i as f32 / 10.0)).collect();
    assert!(eased.windows(2).all(|w| w[1] <= w[0] + 1e-4), "{eased:?}");
    assert_eq!(at(col + ease + 1.0), 0.0);
}

#[test]
fn nothing_else_darkens_the_image() {
    let mut h = harness(two_views(), true);
    frames(&mut h, 0.0);
    let full = h.state().full;
    let x = full.left() + 0.9 * full.width();
    for p in [
        pos2(x, full.bottom() - 2.0),
        pos2(x, below_top_feather(full)),
        pos2(full.right() - 2.0, full.bottom() - 2.0),
    ] {
        assert_eq!(darkening_at(&h, p), 0.0, "at {p:?}");
    }
}
