use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mmm_core::job::JobCtl;
use mmm_core::mosaic::FRAME_FILE;
use mmm_core::preview::{Preview, build_preview};
use mmm_core::reference::ReferenceFrame;

use super::*;
use crate::camera::Camera;
use crate::compositions::{Composition, save_compositions};
use crate::jobs::tests::analyzed;
use crate::start::tests::{step_until, temp_dir};
use crate::stretch_panel::{PLAN_STRETCH_KEY, save_stretch_at};
use crate::testutil::two_group_session;

struct State {
    view: PlanView,
    session: OpenSession,
    previews: BTreeMap<PathBuf, Arc<Preview>>,
    generation: u64,
    complete: bool,
    look: CanvasLook,
}

impl State {
    fn new(session: OpenSession) -> Self {
        State {
            view: PlanView::new(),
            session,
            previews: BTreeMap::new(),
            generation: 1,
            complete: false,
            look: CanvasLook::default(),
        }
    }
}

/// The view's canvas, with the selected panel's details beside it.
fn harness(session: OpenSession) -> Harness<'static, State> {
    harness_of(State::new(session))
}

/// [`harness`] from `state` (its first frame is drawn here).
fn harness_of(state: State) -> Harness<'static, State> {
    Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let feed = PlanFeed {
                    previews: &s.previews,
                    generation: s.generation,
                    complete: s.complete,
                    checks: None,
                };
                egui::Panel::right("details").show(ui, |ui| {
                    s.view.selection_ui(ui, &s.session);
                });
                egui::CentralPanel::default().show(ui, |ui| {
                    s.view.show(ui, &s.session, &feed, &s.look);
                });
            },
            state,
        )
}

/// The panels of `group`, in configuration order.
pub(crate) fn panels(s: &OpenSession, group: &str) -> Vec<PathBuf> {
    group_members(s, &[]).remove(group).unwrap_or_default()
}

/// Small previews of `paths` in the session's (analysed) frame.
pub(crate) fn previews_of(s: &OpenSession, paths: &[PathBuf]) -> BTreeMap<PathBuf, Arc<Preview>> {
    let frame = ReferenceFrame::load(&s.mosaic.dir.join(FRAME_FILE)).unwrap();
    previews_in(&frame, paths)
}

/// Small previews of `paths` in `frame`.
fn previews_in(frame: &ReferenceFrame, paths: &[PathBuf]) -> BTreeMap<PathBuf, Arc<Preview>> {
    paths
        .iter()
        .map(|p| {
            let preview = build_preview(p, frame, 64, &JobCtl::none()).unwrap();
            (p.clone(), Arc::new(preview))
        })
        .collect()
}

fn textured(h: &Harness<'_, State>) -> usize {
    textured_of(h.state())
}

/// Items drawn with their preview.
fn textured_of(s: &State) -> usize {
    s.view
        .items()
        .iter()
        .filter(|i| !i.layers.is_empty())
        .count()
}

/// Dash segments of not-yet-loaded outlines in the last frame.
fn dashes(h: &Harness<'_, State>) -> usize {
    h.output()
        .shapes
        .iter()
        .filter(|cs| {
            matches!(&cs.shape, egui::Shape::LineSegment { stroke, .. }
                if stroke.color == mega_ui::theme::palette::ACCENT_500)
        })
        .count()
}

/// The camera the canvas drew its last frame with.
fn shown(h: &Harness<'_, State>) -> Option<Camera> {
    let canvas = &h.state().view.canvas;
    let (_, now) = canvas.last_frame()?;
    Some(Camera::of(canvas.view(), now))
}

/// Asserts the last frame painted each visible panel's number where `cam`
/// puts the panel's centre: the painted frame itself, not the view after it.
fn assert_painted_with(h: &Harness<'_, State>, cam: Camera) {
    let (rect, _) = h.state().view.canvas.last_frame().expect("drawn");
    let screen = |p: (f64, f64)| {
        rect.center()
            + egui::vec2(
                ((p.0 - cam.centre.0) * cam.zoom) as f32,
                ((p.1 - cam.centre.1) * cam.zoom) as f32,
            )
    };
    let mut checked = 0;
    for it in h.state().view.items() {
        let r = egui::Rect::from_two_pos(
            screen((it.rect[0], it.rect[1])),
            screen((it.rect[2], it.rect[3])),
        );
        if !r.intersects(rect) {
            continue;
        }
        let got = h
            .output()
            .shapes
            .iter()
            .find_map(|cs| match &cs.shape {
                egui::Shape::Text(t) if t.galley.text() == it.label => {
                    Some(t.pos + t.galley.size() / 2.0)
                }
                _ => None,
            })
            .expect("panel number painted");
        let want = r.center();
        assert!(
            (got - want).length() < 0.5,
            "panel {} painted at {got:?}; the camera puts it at {want:?}",
            it.label
        );
        checked += 1;
    }
    assert!(checked > 0, "no panel painted");
}

/// Step until `done` holds, stopping at the frame that made it so.
fn step_to(h: &mut Harness<'_, State>, done: impl Fn(&State) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(h.state()) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the plan"
        );
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn assert_camera(got: Option<Camera>, want: Camera) {
    let got = got.expect("a camera");
    let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(
        near(got.centre.0, want.centre.0)
            && near(got.centre.1, want.centre.1)
            && near(got.zoom, want.zoom),
        "{got:?} != {want:?}"
    );
}

/// Step until the view has settled (at least once, so a change takes
/// effect), and return the mean level of the opaque texels its textures
/// were uploaded with.
fn settle_uploads(h: &mut Harness<'_, State>) -> f64 {
    h.step();
    step_until(h, |s| s.view.is_settled());
    let view = &h.state().view;
    assert!(!view.textures.is_empty(), "no textures");
    let (mut sum, mut n) = (0.0, 0usize);
    for key in view.textures.keys() {
        let image = &view.uploaded_images[key];
        for p in image.pixels.iter().filter(|p| p.a() > 0) {
            sum += f64::from(p.r()) + f64::from(p.g()) + f64::from(p.b());
            n += 3;
        }
    }
    assert!(n > 0, "no opaque texels");
    sum / n as f64
}

#[test]
fn camera_round_trips() {
    let root = temp_dir("plan-view-camera");
    let s = two_group_session(&root);
    let all: Vec<PathBuf> = s
        .mosaic
        .config
        .panels
        .iter()
        .map(|p| p.path.clone())
        .collect();
    let frame = mmm_core::reference::derive(&all, s.mosaic.config.analyze.input).unwrap();
    let ha = panels(&s, "Ha");
    let mut state = State::new(s);
    // Two previews and no frame on disk yet: the canvas fits the two
    // previews, an extent that changes once the frame is read.
    state.previews = previews_in(&frame, &ha[..2]);
    // Set before the canvas first draws.
    let early = Camera {
        centre: (250.0, 180.0),
        zoom: 1.25,
    };
    state.view.set_camera(early);
    assert_camera(state.view.camera(), early);
    let mut h = harness_of(state);
    // The first frame the canvas draws is painted through the camera: the
    // canvas never shows its fit of the previews' extent.
    step_to(&mut h, |s| s.view.canvas.last_frame().is_some());
    assert_eq!(h.state().view.frame_canvas(), None, "the previews' extent");
    assert_painted_with(&h, early);
    step_until(&mut h, |s| textured_of(s) == 2);
    assert_camera(h.state().view.camera(), early);
    assert_camera(shown(&h), early);
    assert_painted_with(&h, early);
    // The frame arrives: the frame that draws its extent is painted through
    // the camera too.
    let dir = h.state().session.mosaic.dir.clone();
    frame
        .save(&mmm_core::preview::preview_frame_path(&dir))
        .unwrap();
    h.state_mut().generation = 2;
    step_to(&mut h, |s| s.view.frame_canvas().is_some());
    assert_painted_with(&h, early);
    assert_camera(h.state().view.camera(), early);
    assert_camera(shown(&h), early);
    // Set once drawn: painted from the next frame on.
    let later = Camera {
        centre: (420.0, 90.0),
        zoom: 2.5,
    };
    h.state_mut().view.set_camera(later);
    assert_camera(h.state().view.camera(), later);
    h.step();
    assert_painted_with(&h, later);
    assert_camera(h.state().view.camera(), later);
    assert_camera(shown(&h), later);
    // And it stays.
    h.run();
    assert_camera(shown(&h), later);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn strong_stretch_brightens_the_plan() {
    let (root, s, _r) = analyzed("plan-view-strong");
    let ha = panels(&s, "Ha");
    let previews = previews_of(&s, &ha);
    let mut h = harness(s);
    h.state_mut().previews = previews;
    h.state_mut().complete = true;
    let normal = settle_uploads(&mut h);
    let normal_stretch = h.state().view.stretch_for_view().map(<[_]>::to_vec);
    assert!(normal_stretch.is_some());
    h.state_mut().look.stretch.strong = true;
    let strong = settle_uploads(&mut h);
    assert_ne!(
        h.state().view.stretch_for_view().map(<[_]>::to_vec),
        normal_stretch
    );
    assert_eq!(textured(&h), 4);
    assert!(
        strong > normal + 1.0,
        "normal {normal:.1}, strong {strong:.1}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn old_manual_plan_stretch_is_ignored() {
    let (root, mut s, _r) = analyzed("plan-view-old-stretch");
    let manual = StretchParams {
        black: 0.5,
        white: 0.6,
        midtones: 0.9,
    };
    save_stretch_at(
        &mut s.mosaic.config.ui,
        PLAN_STRETCH_KEY,
        "Ha",
        Some(&[manual; 3]),
    );
    let ha = panels(&s, "Ha");
    let previews = previews_of(&s, &ha);
    let sets = vec![(
        ha.iter().map(|p| Arc::clone(&previews[p])).collect(),
        Mode::Native,
    )];
    let auto = view_auto_stretch(sets, DisplayStretch::default()).expect("a stretch");
    assert_ne!(auto[0], manual);
    let mut h = harness(s);
    h.state_mut().previews = previews;
    h.state_mut().complete = true;
    step_until(&mut h, |s| s.view.is_settled());
    assert_eq!(h.state().view.stretch_for_view(), Some(&auto[..]));
    assert_eq!(textured(&h), 4);
    // Ignored, not deleted.
    assert!(
        h.state().session.mosaic.config.ui[PLAN_STRETCH_KEY]
            .get("Ha")
            .is_some()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plan_draws_partial_previews() {
    let (root, s, _r) = analyzed("plan-partial");
    let ha = panels(&s, "Ha");
    assert_eq!(ha.len(), 4);
    let previews = previews_of(&s, &ha[..2]);
    let mut h = harness(s);
    h.state_mut().previews = previews;
    step_until(&mut h, |s| {
        s.view.items().len() == 4
            && s.view
                .items()
                .iter()
                .filter(|i| !i.layers.is_empty())
                .count()
                == 2
    });
    assert_eq!(h.state().view.view(), Some(&ViewKind::Group("Ha".into())));
    let items = h.state().view.items().to_vec();
    for it in &items {
        let arrived = ha[..2].contains(&it.path);
        assert_eq!(!it.layers.is_empty(), arrived, "{:?}", it.path);
        if arrived {
            // A group view's layer is opaque.
            assert!(it.layers.iter().all(|(_, additive)| !additive));
        }
    }
    // The two still loading are dashed outlines (line segments).
    assert!(dashes(&h) > 0, "no dashed outlines");
    // The stretch came from the previews' statistics.
    assert!(h.state().view.stretch_for_view().is_some());
    // The extent is the frame: the view does not move as the rest arrive.
    let before = {
        let (_, now) = h.state().view.canvas.last_frame().unwrap();
        h.state().view.canvas.view().zoom(now)
    };
    let rest = previews_of(&h.state().session, &ha[2..]);
    h.state_mut().previews.extend(rest);
    h.state_mut().complete = true;
    step_until(&mut h, |s| s.view.is_settled());
    assert_eq!(textured(&h), 4);
    assert_eq!(dashes(&h), 0, "every panel drawn with its preview");
    let (_, now) = h.state().view.canvas.last_frame().unwrap();
    assert_eq!(h.state().view.canvas.view().zoom(now), before);
    assert_eq!(h.state().view.frame_canvas(), Some(crate::testutil::CANVAS));
    // Hidden for a while: the textures go, and come back when shown.
    h.state_mut()
        .view
        .release_if_hidden(std::time::Instant::now() + RELEASE_AFTER * 2);
    assert!(h.state().view.textures.is_empty());
    h.step();
    step_until(&mut h, |s| s.view.is_settled());
    assert_eq!(textured(&h), 4);
    // Shown recently: kept.
    h.state_mut()
        .view
        .release_if_hidden(std::time::Instant::now());
    assert_eq!(h.state().view.textures.len(), 4);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn composition_view_uses_additive_layers() {
    let (root, mut s, _r) = analyzed("plan-composition");
    let mut hoo = Composition::new("HOO");
    hoo.channels = [
        Some(("Ha".into(), 1.0)),
        Some(("OIII".into(), 1.0)),
        Some(("OIII".into(), 0.5)),
    ];
    save_compositions(&mut s.mosaic.config.ui, &[hoo]);
    let all: Vec<PathBuf> = s
        .mosaic
        .config
        .panels
        .iter()
        .map(|p| p.path.clone())
        .collect();
    let previews = previews_of(&s, &all);
    let (ha, oiii) = (panels(&s, "Ha"), panels(&s, "OIII"));
    let mut h = harness(s);
    h.state_mut().previews = previews;
    h.state_mut().complete = true;
    h.state_mut().view.select_view(ViewKind::Composition(0));
    step_until(&mut h, |s| s.view.is_settled());
    let items = h.state().view.items().to_vec();
    assert_eq!(items.len(), 8);
    for it in &items {
        if ha.contains(&it.path) {
            assert_eq!(it.layers, vec![(Color32::from_rgb(255, 0, 0), true)]);
        } else {
            assert!(oiii.contains(&it.path));
            assert_eq!(
                it.layers,
                vec![
                    (Color32::from_rgb(0, 255, 0), true),
                    (Color32::from_rgb(0, 0, 128), true),
                ]
            );
        }
    }
    // The meshes are drawn additively (alpha-0 vertex colours).
    let additive = h
        .output()
        .shapes
        .iter()
        .filter(|cs| match &cs.shape {
            egui::Shape::Mesh(m) => {
                m.texture_id != egui::TextureId::default()
                    && m.vertices.iter().all(|v| v.color.a() == 0)
            }
            _ => false,
        })
        .count();
    assert_eq!(additive, 12, "4 red + 4 green + 4 blue layers");
    // Numbers run across the view: unique.
    let mut labels: Vec<u32> = items.iter().map(|i| i.label.parse().unwrap()).collect();
    labels.sort_unstable();
    assert_eq!(labels, (1..=8).collect::<Vec<_>>());
    // A three-channel stretch, keyed apart from any group's.
    assert_eq!(h.state().view.stretch_for_view().map(<[_]>::len), Some(3));
    assert_eq!(h.state().view.view_key(), Some("comp:HOO"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn header_facts_are_read_again_for_new_inputs() {
    let (root, s, _r) = analyzed("plan-view-reread");
    let path = s.mosaic.config.panels[0].path.clone();
    let mut h = harness(s);
    h.run();
    h.state_mut().view.select(Some(path.clone()));
    step_until(&mut h, |s| s.view.has_info(&path));
    h.get_by_label("600 × 420 (0.3 MP)");
    // New inputs (a previews generation): the header is read again.
    h.state_mut().generation = 2;
    h.step();
    assert!(!h.state().view.has_info(&path));
    step_until(&mut h, |s| s.view.has_info(&path));
    h.get_by_label("600 × 420 (0.3 MP)");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn copy_path_puts_path_on_clipboard() {
    let root = temp_dir("plan-copy");
    let s = two_group_session(&root);
    let path = s.mosaic.config.panels[0].path.clone();
    let mut h = harness(s);
    h.run();
    h.state_mut().view.select(Some(path.clone()));
    h.run();
    assert_eq!(h.state().view.selected(), Some(path.as_path()));
    h.get_by_label("Copy path").click();
    let mut copied = Vec::new();
    for _ in 0..3 {
        h.step();
        for c in &h.output().platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = c {
                copied.push(t.clone());
            }
        }
    }
    assert_eq!(copied, vec![path.display().to_string()]);
    h.get_by_label("Show in folder");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn preview_image_stretches_and_keeps_no_data_clear() {
    let lin = StretchParams {
        black: 0.0,
        white: 1.0,
        midtones: 0.5,
    };
    let p = Preview {
        path: PathBuf::from("x"),
        rect: [0.0, 0.0, 2.0, 1.0],
        width: 2,
        height: 1,
        channels: 3,
        // R plane, G plane, B plane: pixel 0 has data, pixel 1 none.
        data: vec![1.0, 0.0, 0.5, 0.0, 0.0, 0.0],
    };
    let rgb = preview_image(&p, Mode::Native, &[lin; 3]);
    assert_eq!(rgb.pixels[0], Color32::from_rgb(255, 128, 0));
    assert_eq!(rgb.pixels[1], Color32::TRANSPARENT);
    let mono = preview_image(&p, Mode::Mono, &[lin]);
    assert_eq!(mono.pixels[0], Color32::from_gray(128));
    assert_eq!(mono.pixels[1], Color32::TRANSPARENT);
}

#[test]
fn show_in_folder_uses_the_platform_opener() {
    let path = PathBuf::from("/data/panels/Ha_0.xisf");
    let c = show_in_folder_command(&path);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        assert_eq!(c.get_program(), "xdg-open");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, [std::ffi::OsStr::new("/data/panels")]);
    }
    #[cfg(target_os = "macos")]
    {
        assert_eq!(c.get_program(), "open");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args[0], "-R");
    }
    #[cfg(target_os = "windows")]
    assert_eq!(c.get_program(), "explorer");
}

#[test]
fn plan_places_solved_panels_before_previews() {
    use mmm_core::synth::{SynthWcs, write_xisf_solved};
    let root = temp_dir("plan-solved");
    let (w, h) = (200u64, 150u64);
    let paths: Vec<PathBuf> = [[100.0, 75.0], [40.0, 45.0]]
        .iter()
        .enumerate()
        .map(|(k, refimg)| {
            let path = root.join(format!("raw_{k}.xisf"));
            let wcs = SynthWcs {
                crval: [83.8, -5.4],
                refimg: *refimg,
                cd: [[-1e-3, 0.0], [0.0, 1e-3]],
            };
            write_xisf_solved(&path, w, h, 1, &vec![0.1f32; (w * h) as usize], &wcs).unwrap();
            path
        })
        .collect();
    let cfg = mmm_core::mosaic::MosaicConfig::new(paths.clone());
    let input = cfg.analyze.input;
    let mosaic = mmm_core::mosaic::Mosaic::create(&root.join("s.mmm-session"), cfg).unwrap();
    // The frame as a previews run derives it (header-only), saved as the
    // previews' frame (Plan's; nothing has been analysed).
    let frame = mmm_core::reference::derive(&paths, input).unwrap();
    frame
        .save(&mmm_core::preview::preview_frame_path(&mosaic.dir))
        .unwrap();
    let groups = mosaic.groups().ok();
    let s = OpenSession {
        mosaic,
        frame: None,
        groups,
        lock: None,
        summary: None,
        merged: false,
    };
    let mut h = harness(s);
    // No previews, no analysis: dashed outlines at the WCS footprints.
    step_until(&mut h, |s| s.view.items().len() == 2);
    assert_eq!(h.state().view.unplaced(), 0);
    for it in h.state().view.items() {
        assert!(it.layers.is_empty());
        let want = mmm_core::preview::footprint(&it.path, &frame)
            .unwrap()
            .unwrap();
        assert_eq!(it.rect, want);
    }
    let r: Vec<[f64; 4]> = h.state().view.items().iter().map(|i| i.rect).collect();
    // The second panel's reference pixel is (60, 30) px further left/up.
    assert!((r[1][0] - r[0][0] - 60.0).abs() < 1.0, "{r:?}");
    assert!((r[1][1] - r[0][1] - 30.0).abs() < 1.0, "{r:?}");
    assert!(dashes(&h) > 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn linked_auto_stretch_ignores_empty_channels() {
    let p = Arc::new(Preview {
        path: PathBuf::from("x"),
        rect: [0.0, 0.0, 4.0, 1.0],
        width: 4,
        height: 1,
        channels: 1,
        data: vec![0.1, 0.12, 0.11, 0.3],
    });
    let sets = |linked| {
        view_auto_stretch(
            vec![
                (vec![Arc::clone(&p)], Mode::Mono),
                (Vec::new(), Mode::Mono),
                (Vec::new(), Mode::Mono),
            ],
            DisplayStretch {
                linked,
                strong: false,
            },
        )
        .unwrap()
    };
    let unlinked = sets(false);
    let linked = sets(true);
    // Linked: the one channel with data sets the shared stretch.
    assert_eq!(linked, vec![unlinked[0]; 3]);
    assert_ne!(unlinked[0], unlinked[1]);
}

#[test]
fn converter_drops_superseded_epochs() {
    // A stretch change per step: a new epoch each time, each resending
    // every conversion. With every job queued before the converters start
    // (and the epoch at the last step), superseded jobs are skipped
    // unconverted: exactly one epoch's worth is done.
    let ctx = egui::Context::default();
    let side = 64u32;
    let preview = Arc::new(Preview {
        path: PathBuf::from("p.xisf"),
        rect: [0.0, 0.0, f64::from(side), f64::from(side)],
        width: side,
        height: side,
        channels: 3,
        data: vec![0.25; 3 * (side * side) as usize],
    });
    let jobs = 16usize;
    let epochs = 200u64;
    let (tx, queue) = std::sync::mpsc::channel();
    for epoch in 1..=epochs {
        for i in 0..jobs {
            let job = ConvertJob {
                epoch,
                key: TexKey {
                    path: PathBuf::from(format!("{i}")),
                    slot: None,
                },
                preview: Arc::clone(&preview),
                mode: Mode::Native,
                stretch: vec![crate::tile_source::LINEAR; 3],
            };
            tx.send(job).unwrap();
        }
    }
    let epoch = Arc::new(std::sync::atomic::AtomicU64::new(epochs));
    let conv = Converter::start_on(&ctx, tx, queue, epoch).expect("converter");
    let mut done = Vec::new();
    while done.len() < jobs {
        let (e, _, _) = conv
            .rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("conversions finish");
        done.push(e);
    }
    assert!(done.iter().all(|&e| e == epochs), "{done:?}");
    // Nothing else is converted.
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(conv.rx.try_recv().is_err());
}
