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
        Self::with_tile(w, h, 256)
    }

    fn with_tile(w: u64, h: u64, tile: u32) -> Arc<Self> {
        Arc::new(Self {
            geom: PyramidGeometry::new(w, h, tile),
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
    let mut counts = Vec::new();
    for size in [10_000u64, 1_000_000] {
        let src = TestSource::new(size, size);
        let mut h = harness(TiledViewer::new_synchronous(src.clone()));
        h.run();
        assert!(!h.state().is_loading());
        let (rect, now) = h.state().last_frame().unwrap();
        assert!(h.state().view().is_settled(now));
        let fetched = src.fetched();
        assert!(!fetched.is_empty());
        // Every level from the top down to the drawn one, and no other.
        let levels: HashSet<u32> = fetched.iter().map(|k| k.level).collect();
        let top = src.geom.levels - 1;
        let drawn = *levels.iter().min().unwrap();
        assert_eq!(levels, (drawn..=top).collect::<HashSet<u32>>());
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
        // Drawn-level tiles dominate: each coarser level has about a quarter
        // as many (~4/3 in total), plus rounding at the grid edges.
        let fine = fetched.iter().filter(|k| k.level == drawn).count();
        assert!(fetched.len() <= fine * 2, "{} vs {fine}", fetched.len());
        counts.push(fetched.len());
    }
    // A 10^4 x larger canvas costs no more than a small constant extra.
    assert!(counts[1] <= counts[0] + task6_bound() * 2, "{counts:?}");
}

#[test]
fn texture_filter_by_level() {
    use egui::TextureFilter::{Linear, Nearest};
    let t0 = texture_options(0);
    assert_eq!((t0.magnification, t0.minification), (Nearest, Linear));
    for level in 1..5 {
        let t = texture_options(level);
        assert_eq!((t.magnification, t.minification), (Linear, Linear));
    }
}

#[test]
fn old_missing_does_not_block_ancestor() {
    let src = TestSource::new(20_000, 20_000);
    let mut v = TiledViewer::new_synchronous(src.clone());
    let ctx = egui::Context::default();
    let tex = ctx.load_texture(
        "t",
        ColorImage::new([1, 1], vec![Color32::WHITE]),
        egui::TextureOptions::LINEAR,
    );
    let leaf = TileKey {
        level: 0,
        tx: 4,
        ty: 4,
    };
    let mid = TileKey {
        level: 1,
        tx: 2,
        ty: 2,
    };
    let coarse = TileKey {
        level: 2,
        tx: 1,
        ty: 1,
    };
    v.cache.insert(
        mid,
        1,
        CachedTile {
            texture: None,
            stale: false,
        },
    );
    v.cache.insert(
        coarse,
        1,
        CachedTile {
            texture: Some(tex.clone()),
            stale: false,
        },
    );
    // At generation 1 the Missing tile is current: background.
    assert!(matches!(v.loaded_ancestor(leaf, 1), Some((None, _, _))));
    // At generation 2 it is an old result: draw the valid coarser tile.
    let (found, _, rect) = v.loaded_ancestor(leaf, 2).expect("ancestor");
    assert_eq!(found, Some(tex.id()));
    assert_eq!(rect, v.tile_image_rect(coarse));
}

/// A source whose fetches block until `gate` is released.
struct GatedSource {
    inner: Arc<TestSource>,
    gate: Mutex<()>,
}

impl TileSource for GatedSource {
    fn geometry(&self) -> PyramidGeometry {
        self.inner.geometry()
    }
    fn generation(&self) -> u64 {
        self.inner.generation()
    }
    fn fetch(&self, key: TileKey) -> Fetched {
        let _g = self.gate.lock().unwrap();
        self.inner.fetch(key)
    }
}

#[test]
fn hung_fetch_does_not_spin_repaints() {
    let src = Arc::new(GatedSource {
        inner: TestSource::new(20_000, 20_000),
        gate: Mutex::new(()),
    });
    let gate = src.gate.lock().unwrap();
    let mut h = Harness::builder()
        .with_size(SCREEN)
        .with_max_steps(10)
        .build_ui_state(
            |ui, v: &mut TiledViewer| {
                v.show(ui);
            },
            TiledViewer::new(src.clone(), 1),
        );
    // Settled view, fetch stuck: the frame loop goes idle.
    let steps = h.run();
    assert!(steps <= 3, "{steps}");
    assert!(h.state().is_loading());
    drop(gate);
    // Once released, the loader wakes the UI and tiles arrive.
    for _ in 0..2000 {
        h.step();
        if !h.state().is_loading() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!h.state().is_loading());
}

/// Records which threads fetched.
struct ThreadSource {
    inner: Arc<TestSource>,
    threads: Mutex<HashSet<std::thread::ThreadId>>,
}

impl TileSource for ThreadSource {
    fn geometry(&self) -> PyramidGeometry {
        self.inner.geometry()
    }
    fn generation(&self) -> u64 {
        self.inner.generation()
    }
    fn fetch(&self, key: TileKey) -> Fetched {
        self.threads
            .lock()
            .unwrap()
            .insert(std::thread::current().id());
        self.inner.fetch(key)
    }
}

#[test]
fn zero_loader_threads_still_never_fetch_on_ui_thread() {
    let src = Arc::new(ThreadSource {
        inner: TestSource::new(20_000, 20_000),
        threads: Mutex::new(HashSet::new()),
    });
    let mut h = harness(TiledViewer::new(src.clone(), 0));
    for _ in 0..2000 {
        h.step();
        if !h.state().is_loading() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!h.state().is_loading());
    let threads = src.threads.lock().unwrap();
    assert!(!threads.is_empty());
    assert!(!threads.contains(&std::thread::current().id()));
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
    let mut h = harness(TiledViewer::new_synchronous(src));
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
    let mut h = harness(TiledViewer::new_synchronous(src.clone()));
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
    let mut h = harness(TiledViewer::new_synchronous(src));
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
    let mut h = harness(TiledViewer::new_synchronous(src));
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
    let mut h = harness(TiledViewer::new_synchronous(src));
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
    let mut h = harness(TiledViewer::new_synchronous(src));
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
    let mut h = harness(TiledViewer::new_synchronous(src.clone()));
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
            TiledViewer::new_synchronous(src),
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

/// Viewer plus what it last reported.
struct Drawing {
    viewer: TiledViewer,
    drawn: Vec<[f64; 4]>,
}

fn drawing_harness(viewer: TiledViewer) -> Harness<'static, Drawing> {
    Harness::builder()
        .with_size(SCREEN)
        .with_max_steps(400)
        .build_ui_state(
            |ui, s: &mut Drawing| {
                if let Some(r) = s.viewer.show(ui).drawn_rect {
                    s.drawn.push(r);
                }
            },
            Drawing {
                viewer,
                drawn: Vec::new(),
            },
        )
}

fn drag(h: &mut Harness<'static, Drawing>, from: Pos2, to: Pos2) {
    h.drag_at(from);
    h.step();
    h.hover_at(Pos2::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0));
    h.step();
    h.hover_at(to);
    h.step();
    h.drop_at(to);
    h.run();
}

#[test]
fn drawn_rect_reported_in_image_coords() {
    let src = TestSource::new(20_000, 10_000);
    let mut h = drawing_harness(TiledViewer::new_synchronous(src));
    h.run();
    assert_eq!(h.state().viewer.tool(), Tool::Navigate);
    h.state_mut().viewer.set_tool(Tool::DrawRect);
    h.run();
    let (c0, z0) = centre_zoom(&h.state().viewer);
    let (rect, now) = h.state().viewer.last_frame().unwrap();
    // Dragged up-left: the rect is normalised.
    let (a, b) = (Pos2::new(700.0, 500.0), Pos2::new(300.0, 200.0));
    let ia = h.state().viewer.view().screen_to_image(a, rect, now);
    let ib = h.state().viewer.view().screen_to_image(b, rect, now);
    drag(&mut h, a, b);
    // Drawing never pans or zooms.
    let (c1, z1) = centre_zoom(&h.state().viewer);
    assert_eq!((c0, z0), (c1, z1));
    assert_eq!(h.state().drawn.len(), 1, "one rect per drag");
    let r = h.state().drawn[0];
    let want = [ib.0, ib.1, ia.0, ia.1];
    for i in 0..4 {
        assert!((r[i] - want[i]).abs() < 1e-6, "{r:?} vs {want:?}");
    }
    // The tool returns to Navigate: the next drag pans.
    assert_eq!(h.state().viewer.tool(), Tool::Navigate);
    drag(&mut h, a, b);
    assert_eq!(h.state().drawn.len(), 1);
    assert_ne!(centre_zoom(&h.state().viewer).0, c0);

    // A drag off the image is clamped to it.
    h.state_mut().viewer.home();
    h.run();
    h.state_mut().viewer.set_tool(Tool::DrawRect);
    h.run();
    drag(&mut h, Pos2::new(5.0, 5.0), Pos2::new(500.0, 400.0));
    let r = h.state().drawn[1];
    assert!(
        r[0] == 0.0 && r[1] == 0.0 && r[2] > 0.0 && r[3] > 0.0,
        "{r:?}"
    );

    // Esc cancels a drag in progress, and the tool; the rest of that drag
    // does not pan.
    h.state_mut().viewer.set_tool(Tool::DrawRect);
    h.run();
    let before = centre_zoom(&h.state().viewer);
    h.drag_at(a);
    h.step();
    h.hover_at(Pos2::new(600.0, 400.0));
    h.step();
    h.key_press(egui::Key::Escape);
    h.step();
    // Still held, moving on: this must not pan.
    h.hover_at(Pos2::new(450.0, 300.0));
    h.step();
    h.hover_at(b);
    h.step();
    h.drop_at(b);
    h.run();
    assert_eq!(h.state().drawn.len(), 2, "cancelled drag reported");
    assert_eq!(h.state().viewer.tool(), Tool::Navigate);
    assert_eq!(
        centre_zoom(&h.state().viewer),
        before,
        "cancelled drag panned"
    );
    // The next drag pans again.
    drag(&mut h, a, b);
    assert_ne!(centre_zoom(&h.state().viewer).0, before.0);
}

#[test]
fn overlays_drawn_and_zoom_to_rect() {
    let src = TestSource::new(20_000, 10_000);
    let mut h = drawing_harness(TiledViewer::new_synchronous(src));
    h.state_mut().viewer.set_overlays(vec![Overlay {
        rect: [1000.0, 1000.0, 3000.0, 2000.0],
        color: Color32::RED,
        label: "ROI 1".into(),
        dashed: false,
    }]);
    h.run();
    let labelled = h
        .output()
        .shapes
        .iter()
        .any(|cs| matches!(&cs.shape, egui::Shape::Text(t) if t.galley.text() == "ROI 1"));
    assert!(labelled, "overlay label painted");
    h.state_mut()
        .viewer
        .zoom_to_rect([1000.0, 1000.0, 3000.0, 2000.0]);
    h.run();
    let (c, z) = centre_zoom(&h.state().viewer);
    assert!(
        (c.0 - 2000.0).abs() < 1e-3 && (c.1 - 1500.0).abs() < 1e-3,
        "{c:?}"
    );
    let (rect, _) = h.state().viewer.last_frame().unwrap();
    let want = (f64::from(rect.width()) / 2000.0).min(f64::from(rect.height()) / 1000.0);
    assert!((z - want).abs() < 1e-6, "{z} vs {want}");
}

/// A viewer harness stepping at 60 fps.
fn harness_60fps(viewer: TiledViewer) -> Harness<'static, TiledViewer> {
    Harness::builder()
        .with_size(SCREEN)
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(400)
        .build_ui_state(
            |ui, v: &mut TiledViewer| {
                v.show(ui);
            },
            viewer,
        )
}

/// Screen bounding box of the tiles drawn last frame (the image's drawn
/// rect when every edge tile is at least partly on screen).
fn drawn_image_rect(h: &Harness<'static, TiledViewer>) -> Rect {
    let mut r = Rect::NOTHING;
    for cs in &h.output().shapes {
        if let egui::Shape::Mesh(m) = &cs.shape
            && m.texture_id != egui::TextureId::default()
        {
            for v in &m.vertices {
                r.extend_with(v.pos);
            }
        }
    }
    assert!(r.is_positive(), "no tiles drawn");
    r
}

/// Where image point `p` was drawn last frame, from the drawn tiles.
fn drawn_point(h: &Harness<'static, TiledViewer>, p: (f64, f64)) -> Pos2 {
    let r = drawn_image_rect(h);
    let g = h.state().geom;
    Pos2::new(
        r.min.x + (p.0 / g.width as f64) as f32 * r.width(),
        r.min.y + (p.1 / g.height as f64) as f32 * r.height(),
    )
}

/// Drag from `from` by `step` per input event over `events` events, with
/// `idle` input-free frames after each (a display faster than the pointer),
/// then release and let the view settle. Returns where image point `track`
/// was drawn each frame.
fn drag_track(
    h: &mut Harness<'static, TiledViewer>,
    from: Pos2,
    step: Vec2,
    events: usize,
    idle: usize,
    track: (f64, f64),
) -> Vec<Pos2> {
    let mut out = Vec::new();
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    out.push(drawn_point(h, track));
    let mut p = from;
    for _ in 0..events {
        p += step;
        h.hover_at(p);
        h.step();
        out.push(drawn_point(h, track));
        for _ in 0..idle {
            h.step();
            out.push(drawn_point(h, track));
        }
    }
    h.drop_at(p);
    h.step();
    out.push(drawn_point(h, track));
    for _ in 0..200 {
        h.step();
        out.push(drawn_point(h, track));
        let (_, now) = h.state().last_frame().unwrap();
        if h.state().view().is_settled(now) {
            break;
        }
    }
    out
}

/// Panics if the drawn track ever moves against `dir` (a back-and-forth).
fn assert_no_reversal(track: &[Pos2], dir: Vec2) {
    for (i, w) in track.windows(2).enumerate() {
        let d = (w[1] - w[0]).dot(dir);
        assert!(
            d >= -1e-3,
            "frame {i}: drawn image moved back by {d}: {track:?}"
        );
    }
}

#[test]
fn drag_motion_never_reverses() {
    let src = TestSource::new(10_000, 10_000);
    let mut h = harness_60fps(TiledViewer::new_synchronous(src));
    h.run();
    let step = Vec2::new(6.0, 3.0);
    let track = drag_track(
        &mut h,
        Pos2::new(400.0, 300.0),
        step,
        20,
        0,
        (5_000.0, 5_000.0),
    );
    assert_no_reversal(&track, step);
    // The dragged point ends where the pointer took it.
    let moved = *track.last().unwrap() - track[0];
    assert!((moved - step * 20.0).length() < 0.5, "{moved:?}");
}

#[test]
fn drag_after_scroll_zoom_never_reverses() {
    let src = TestSource::new(10_000, 10_000);
    let mut h = harness_60fps(TiledViewer::new_synchronous(src));
    h.run();
    let cursor = Pos2::new(300.0, 300.0);
    let (rect, now) = h.state().last_frame().unwrap();
    let under = h.state().view().screen_to_image(cursor, rect, now);
    h.hover_at(cursor);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: Vec2::new(0.0, -1.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    // Drag while the zoom is still animating, then again once settled.
    h.step();
    h.step();
    let step = Vec2::new(5.0, 2.0);
    let track = drag_track(&mut h, cursor, step, 20, 0, under);
    assert_no_reversal(&track, step);
    let moved = *track.last().unwrap() - cursor;
    assert!((moved - step * 20.0).length() < 0.5, "{moved:?}");
    let from = cursor + step * 20.0;
    let track = drag_track(&mut h, from, -step, 20, 0, under);
    assert_no_reversal(&track, -step);
    assert!((*track.last().unwrap() - cursor).length() < 0.5);
}

#[test]
fn drag_motion_smooth_when_pointer_slower_than_display() {
    // The display runs at 60 fps but pointer events come every 3rd frame:
    // the image must keep moving between events, not stop and jump.
    let src = TestSource::new(10_000, 10_000);
    let mut h = harness_60fps(TiledViewer::new_synchronous(src));
    h.run();
    let step = Vec2::new(9.0, 0.0);
    let track = drag_track(
        &mut h,
        Pos2::new(400.0, 300.0),
        step,
        12,
        2,
        (5_000.0, 5_000.0),
    );
    assert_no_reversal(&track, step);
    // Every frame of the drag (after the first event) moves the image.
    let drag_frames = &track[1..1 + 12 * 3];
    for (i, w) in drag_frames.windows(2).enumerate() {
        assert!((w[1].x - w[0].x) > 1e-3, "frame {i} stood still: {track:?}");
    }
    let moved = *track.last().unwrap() - track[0];
    assert!((moved - step * 12.0).length() < 0.5, "{moved:?}");
}

#[test]
fn irregular_pointer_deltas_are_smoothed() {
    // Uneven input sampling: per-frame pointer moves of 0, 2d, 0, 2d, d, 0,
    // 3d, ... The drawn image must move every frame, never back, with no
    // frame-to-frame jump much above the average step.
    let src = TestSource::new(10_000, 10_000);
    let mut h = harness_60fps(TiledViewer::new_synchronous(src));
    h.run();
    let start = Pos2::new(400.0, 300.0);
    let (rect, now) = h.state().last_frame().unwrap();
    let grabbed = h.state().view().screen_to_image(start, rect, now);
    let d = Vec2::new(4.0, 2.0);
    let pattern = [0.0, 2.0, 0.0, 2.0, 1.0, 0.0, 3.0, 0.0, 1.0, 1.0, 0.0, 2.0];
    let mut p = start;
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    // Engage the drag (past egui's threshold) and the spring.
    p += d * 2.0;
    h.hover_at(p);
    h.step();
    h.step();
    let mut track = vec![drawn_point(&h, grabbed)];
    for _ in 0..3 {
        for &k in &pattern {
            if k > 0.0 {
                p += d * k;
                h.hover_at(p);
            }
            h.step();
            track.push(drawn_point(&h, grabbed));
        }
    }
    assert_no_reversal(&track, d);
    let steps: Vec<f32> = track.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    // Unsmoothed, the steps would jump between 0 and 3|d| (13.4 px) from
    // one frame to the next; eased, they change by a fraction of |d|.
    for (i, s) in steps.iter().enumerate() {
        assert!(*s > 1e-3, "frame {i} stood still: {steps:?}");
    }
    for (i, w) in steps.windows(2).enumerate() {
        let change = (w[1] - w[0]).abs();
        assert!(
            change < 1.0,
            "frame {i}: step changed by {change}: {steps:?}"
        );
    }
    h.drop_at(p);
    h.run();
    // The grabbed point ends under the pointer.
    assert!((drawn_point(&h, grabbed) - p).length() < 0.5);
}

/// Drawn-level tiles not yet cached at `generation`.
fn fine_not_current(h: &mut Harness<'static, TiledViewer>, generation: u64) -> usize {
    let (rect, now) = h.state().last_frame().unwrap();
    let (_, fine) = h.state().fine_tiles(rect, now);
    let v = h.state_mut();
    fine.iter()
        .filter(|&&k| !v.cache.has_current(k, generation))
        .count()
}

#[test]
fn superseded_results_never_delay_current_tiles() {
    // Small tiles: far more wanted tiles than one frame's upload budget,
    // like a 1080p viewer at 256-pixel tiles.
    let src = TestSource::with_tile(20_000, 20_000, 128);
    let mut h = harness(TiledViewer::new_synchronous(src.clone()));
    h.run();
    let (rect, now) = h.state().last_frame().unwrap();
    let wanted = h.state().wanted_tiles(rect, now).len();
    assert!(wanted > 2 * MAX_UPLOADS_PER_FRAME, "{wanted}");
    assert!(wanted < DEFAULT_CAPACITY / 2, "{wanted}"); // no eviction churn
    // A slider drag: a new generation every frame.
    for _ in 0..30 {
        src.bump();
        h.step();
    }
    // Once it stops, the drawn tiles reach the final generation within
    // about one upload cycle of the wanted set, not a backlog of every
    // superseded generation's results.
    let last = src.generation();
    let cycle = wanted.div_ceil(MAX_UPLOADS_PER_FRAME) + 2;
    let mut frames = 0;
    while fine_not_current(&mut h, last) > 0 {
        assert!(frames < 10 * cycle, "not settled after {frames} frames");
        h.step();
        frames += 1;
    }
    assert!(frames <= cycle, "{frames} frames > {cycle}");
}

#[test]
fn rapid_generations_bound_fetches() {
    // Loaders stuck in a slow fetch while the generation changes many
    // times: superseded requests are dropped unfetched.
    let src = Arc::new(GatedSource {
        inner: TestSource::new(20_000, 20_000),
        gate: Mutex::new(()),
    });
    let threads = 2;
    let gate = src.gate.lock().unwrap();
    let mut h = harness(TiledViewer::new(src.clone(), threads));
    h.step();
    let (rect, now) = h.state().last_frame().unwrap();
    let wanted = h.state().wanted_tiles(rect, now).len();
    for _ in 0..50 {
        src.inner.bump();
        h.step();
    }
    drop(gate);
    let last = src.generation();
    for _ in 0..4000 {
        h.step();
        if !h.state().is_loading() && fine_not_current(&mut h, last) == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(fine_not_current(&mut h, last), 0);
    // The first generation's in-flight fetches plus the final set.
    let fetched = src.inner.fetched().len();
    assert!(
        fetched <= wanted + threads,
        "{fetched} > {wanted} + {threads}"
    );
}
