use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use egui::{ColorImage, Pos2, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::viewer::PyramidGeometry;

const SCREEN: Vec2 = Vec2::new(1024.0, 768.0);

#[derive(Clone, Copy)]
enum Kind {
    Stale,
    Missing,
}

/// Procedural source: flat colours from the key, optional per-key
/// overrides, and a log of every fetch.
struct TestSource {
    geom: PyramidGeometry,
    generation: AtomicU64,
    all: Mutex<Option<Kind>>,
    overrides: Mutex<HashMap<TileKey, Kind>>,
    fetched: Mutex<Vec<TileKey>>,
}

impl TestSource {
    fn new(w: u64, h: u64) -> Arc<Self> {
        Arc::new(Self {
            geom: PyramidGeometry::new(w, h, 256),
            generation: AtomicU64::new(1),
            all: Mutex::new(None),
            overrides: Mutex::new(HashMap::new()),
            fetched: Mutex::new(Vec::new()),
        })
    }

    fn fetched(&self) -> Vec<TileKey> {
        self.fetched.lock().unwrap().clone()
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl TileSource for TestSource {
    fn geometry(&self) -> PyramidGeometry {
        self.geom
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn fetch(&self, key: TileKey) -> Fetched {
        self.fetched.lock().unwrap().push(key);
        let c = Color32::from_rgb(
            (key.tx * 37) as u8,
            (key.ty * 53) as u8,
            (key.level * 29) as u8,
        );
        let img = ColorImage::new([8, 8], vec![c; 64]);
        let kind = self
            .overrides
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .or(*self.all.lock().unwrap());
        match kind {
            None => Fetched::Valid(img),
            Some(Kind::Stale) => Fetched::Stale(img),
            Some(Kind::Missing) => Fetched::Missing,
        }
    }
}

fn harness(viewer: TiledViewer) -> Harness<'static, TiledViewer> {
    Harness::builder()
        .with_size(SCREEN)
        .with_max_steps(400)
        .build_ui_state(
            |ui, v: &mut TiledViewer| {
                v.show(ui);
            },
            viewer,
        )
}

fn centre_zoom(v: &TiledViewer) -> ((f64, f64), f64) {
    let (_, now) = v.last_frame().expect("shown");
    (v.view().centre(now), v.view().zoom(now))
}

/// The Task 6 per-level bound for a 1024 x 768 screen.
fn task6_bound() -> usize {
    ((2.0 * 1024.0f64 / 256.0 + 2.0) * (2.0 * 768.0f64 / 256.0 + 2.0)).floor() as usize
}

#[test]
fn requests_independent_of_canvas_size() {
    for size in [10_000u64, 1_000_000] {
        let src = TestSource::new(size, size);
        let mut h = harness(TiledViewer::new(src.clone(), 0));
        h.run();
        assert!(!h.state().is_loading());
        let (rect, now) = h.state().last_frame().unwrap();
        assert!(h.state().view().is_settled(now));
        let fetched = src.fetched();
        let levels: HashSet<u32> = fetched.iter().map(|k| k.level).collect();
        assert!(!fetched.is_empty());
        assert!(levels.len() <= 2, "{levels:?}");
        let bound = task6_bound() * levels.len();
        assert!(
            fetched.len() <= bound,
            "{size}: {} > {bound}",
            fetched.len()
        );
        // Each tile fetched once, and everything shown was asked for.
        let unique: HashSet<TileKey> = fetched.iter().copied().collect();
        assert_eq!(unique.len(), fetched.len());
        let wanted: HashSet<TileKey> = h.state().wanted_tiles(rect, now).into_iter().collect();
        assert_eq!(unique, wanted);
    }
}

#[test]
fn stale_tiles_drawn_dimmed() {
    let img = ColorImage::new([1, 1], vec![Color32::RED]);
    let stale = tile_tint(&Fetched::Stale(img.clone()));
    assert_eq!(stale.a(), 255);
    for c in [stale.r(), stale.g(), stale.b()] {
        assert!((i32::from(c) - 140).abs() <= 2, "{stale:?}");
    }
    assert_eq!(tile_tint(&Fetched::Valid(img)), Color32::WHITE);
    assert_eq!(tile_tint(&Fetched::Missing), Color32::TRANSPARENT);

    // Through the widget: an all-stale source paints dimmed tile meshes and
    // the border-colour hatch.
    let src = TestSource::new(5000, 5000);
    *src.all.lock().unwrap() = Some(Kind::Stale);
    let mut h = harness(TiledViewer::new(src, 0));
    h.run();
    let mut dimmed = 0;
    let mut hatch = 0;
    for cs in &h.output().shapes {
        match &cs.shape {
            egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() => {
                assert!(m.vertices.iter().all(|v| v.color == stale));
                dimmed += 1;
            }
            egui::Shape::LineSegment { stroke, .. }
                if stroke.color == crate::theme::palette::BORDER =>
            {
                hatch += 1;
            }
            _ => {}
        }
    }
    assert!(dimmed > 0 && hatch > 0, "dimmed {dimmed} hatch {hatch}");
}

#[test]
fn generation_change_refetches_visible_only() {
    let src = TestSource::new(20_000, 20_000);
    let mut h = harness(TiledViewer::new(src.clone(), 0));
    h.run();
    let home: HashSet<TileKey> = src.fetched().into_iter().collect();
    h.state_mut().zoom_in();
    h.run();
    h.state_mut().zoom_in();
    h.run();
    let before = src.fetched().len();
    src.bump();
    h.run();
    assert!(!h.state().is_loading());
    let refetched: Vec<TileKey> = src.fetched()[before..].to_vec();
    let unique: HashSet<TileKey> = refetched.iter().copied().collect();
    assert_eq!(unique.len(), refetched.len(), "duplicate refetch");
    let (rect, now) = h.state().last_frame().unwrap();
    let visible: HashSet<TileKey> = h.state().wanted_tiles(rect, now).into_iter().collect();
    assert_eq!(unique, visible);
    // Some cached home tiles are now off-screen and were not refetched.
    assert!(home.difference(&visible).count() > 0);
}

#[test]
fn home_button_fits() {
    let src = TestSource::new(20_000, 10_000);
    let mut h = harness(TiledViewer::new(src, 0));
    h.run();
    let (c0, z0) = centre_zoom(h.state());
    assert!((c0.0 - 10_000.0).abs() < 1e-6 && (c0.1 - 5_000.0).abs() < 1e-6);

    // The minus sign must render with egui's default fonts.
    assert!(
        h.ctx
            .fonts_mut(|f| f.has_glyph(&egui::FontId::default(), '\u{2212}'))
    );
    h.get_by_label("+").click();
    h.run();
    let (_, z1) = centre_zoom(h.state());
    assert!((z1 / z0 - 2.0).abs() < 1e-6, "{z1} {z0}");
    h.get_by_label("\u{2212}").click();
    h.run();
    let (_, z2) = centre_zoom(h.state());
    assert!((z2 / z0 - 1.0).abs() < 1e-6);

    h.state_mut().zoom_in();
    h.run();
    h.drag_at(Pos2::new(600.0, 400.0));
    h.step();
    h.hover_at(Pos2::new(700.0, 450.0));
    h.step();
    h.drop_at(Pos2::new(700.0, 450.0));
    h.run();
    let (c3, z3) = centre_zoom(h.state());
    assert!((c3.0 - c0.0).abs() > 1.0 && (z3 - z0).abs() > 1e-9);

    h.get_by_label("Home").click();
    h.run();
    let (c4, z4) = centre_zoom(h.state());
    assert!((c4.0 - c0.0).abs() < 1e-6 && (c4.1 - c0.1).abs() < 1e-6);
    assert!((z4 - z0).abs() < 1e-9);
}

#[test]
fn drag_pans() {
    let src = TestSource::new(20_000, 10_000);
    let mut h = harness(TiledViewer::new(src, 0));
    h.run();
    let (c0, z0) = centre_zoom(h.state());
    h.drag_at(Pos2::new(600.0, 400.0));
    h.step();
    h.hover_at(Pos2::new(700.0, 450.0));
    h.step();
    h.drop_at(Pos2::new(700.0, 450.0));
    h.run();
    let (c1, z1) = centre_zoom(h.state());
    assert!((z1 - z0).abs() < 1e-12);
    assert!((c0.0 - c1.0 - 100.0 / z0).abs() < 1e-3, "{c0:?} {c1:?}");
    assert!((c0.1 - c1.1 - 50.0 / z0).abs() < 1e-3);
}

#[test]
fn scroll_zooms_about_cursor() {
    let src = TestSource::new(20_000, 10_000);
    let mut h = harness(TiledViewer::new(src, 0));
    h.run();
    let (rect, now) = h.state().last_frame().unwrap();
    let cursor = Pos2::new(300.0, 200.0);
    let before = h.state().view().screen_to_image(cursor, rect, now);
    let (_, z0) = centre_zoom(h.state());
    h.hover_at(cursor);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: Vec2::new(0.0, 1.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    let (rect, now) = h.state().last_frame().unwrap();
    let (_, z1) = centre_zoom(h.state());
    assert!((z1 / z0 - 1.2).abs() < 1e-6, "{z1} {z0}");
    let after = h.state().view().screen_to_image(cursor, rect, now);
    assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
}

#[test]
fn threaded_loader_fills_visible_tiles() {
    let src = TestSource::new(20_000, 20_000);
    let mut h = harness(TiledViewer::new(src.clone(), 2));
    // Bounded wait: the loaders are real threads.
    for _ in 0..2000 {
        h.step();
        if !h.state().is_loading() && !src.fetched().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!h.state().is_loading());
    let (rect, now) = h.state().last_frame().unwrap();
    let wanted: HashSet<TileKey> = h.state().wanted_tiles(rect, now).into_iter().collect();
    let fetched: HashSet<TileKey> = src.fetched().into_iter().collect();
    assert_eq!(wanted, fetched);
}

#[test]
fn set_source_rehomes_on_new_geometry() {
    let src = TestSource::new(20_000, 10_000);
    let mut h = harness(TiledViewer::new(src, 0));
    h.run();
    let other = TestSource::new(4_000, 4_000);
    h.state_mut().set_source(other.clone());
    h.run();
    let (c, _) = centre_zoom(h.state());
    assert!((c.0 - 2_000.0).abs() < 1e-6 && (c.1 - 2_000.0).abs() < 1e-6);
    assert!(!other.fetched().is_empty());
}

#[test]
fn missing_tiles_draw_background() {
    let src = TestSource::new(5000, 5000);
    *src.all.lock().unwrap() = Some(Kind::Missing);
    let mut h = harness(TiledViewer::new(src.clone(), 0));
    h.run();
    assert!(!h.state().is_loading());
    let textured = h.output().shapes.iter().any(|cs| {
        matches!(&cs.shape, egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default())
    });
    assert!(!textured);
    // Missing is a result: it is cached, not refetched every frame.
    let n = src.fetched().len();
    h.run_steps(3);
    assert_eq!(src.fetched().len(), n);
}

#[test]
fn double_click_zooms_in_about_point() {
    let src = TestSource::new(20_000, 10_000);
    // Short steps so two clicks land inside egui's double-click window.
    let mut h = Harness::builder()
        .with_size(SCREEN)
        .with_step_dt(0.01)
        .with_max_steps(400)
        .build_ui_state(
            |ui, v: &mut TiledViewer| {
                v.show(ui);
            },
            TiledViewer::new(src, 0),
        );
    h.run();
    let (rect, now) = h.state().last_frame().unwrap();
    let p = Pos2::new(700.0, 300.0);
    let before = h.state().view().screen_to_image(p, rect, now);
    let (_, z0) = centre_zoom(h.state());
    h.hover_at(p);
    for _ in 0..2 {
        h.drag_at(p);
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run();
    let (rect, now) = h.state().last_frame().unwrap();
    let (_, z1) = centre_zoom(h.state());
    assert!((z1 / z0 - 2.0).abs() < 1e-6, "{z1} {z0}");
    let after = h.state().view().screen_to_image(p, rect, now);
    assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
}
