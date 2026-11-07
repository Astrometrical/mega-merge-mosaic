use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mmm_core::checks::{Check, CheckCode, Severity};
use mmm_core::job::JobCtl;
use mmm_core::mosaic::FRAME_FILE;
use mmm_core::preview::{Preview, build_preview};
use mmm_core::reference::ReferenceFrame;

use super::*;
use crate::checks::ChecksReport;
use crate::compositions::{Composition, save_compositions};
use crate::jobs::tests::analyzed;
use crate::session::OpenSession;
use crate::start::tests::{step_until, temp_dir};
use crate::testutil::two_group_session;

struct State {
    tab: PlanTab,
    session: OpenSession,
    previews: BTreeMap<PathBuf, Arc<Preview>>,
    generation: u64,
    complete: bool,
    report: Option<ChecksReport>,
    resumed: usize,
    accepted: Vec<Check>,
}

fn harness(session: OpenSession) -> Harness<'static, State> {
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
                    previews_busy: false,
                    checks: s.report.as_ref(),
                };
                let out = s.tab.ui(ui, &mut s.session, &feed);
                s.resumed += usize::from(out.resume_previews);
                s.accepted.extend(out.accept);
            },
            State {
                tab: PlanTab::new(),
                session,
                previews: BTreeMap::new(),
                generation: 1,
                complete: false,
                report: None,
                resumed: 0,
                accepted: Vec::new(),
            },
        )
}

/// The panels of `group`, in configuration order.
fn panels(s: &OpenSession, group: &str) -> Vec<PathBuf> {
    group_members(s, &[]).remove(group).unwrap_or_default()
}

/// Small previews of `paths` in the session's frame.
fn previews_of(s: &OpenSession, paths: &[PathBuf]) -> BTreeMap<PathBuf, Arc<Preview>> {
    let frame = ReferenceFrame::load(&s.mosaic.dir.join(FRAME_FILE)).unwrap();
    paths
        .iter()
        .map(|p| {
            let preview = build_preview(p, &frame, 64, &JobCtl::none()).unwrap();
            (p.clone(), Arc::new(preview))
        })
        .collect()
}

fn textured(h: &Harness<'_, State>) -> usize {
    h.state()
        .tab
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

/// Screen position of the centre of `path`'s item.
fn centre_of(h: &Harness<'_, State>, path: &PathBuf) -> egui::Pos2 {
    let it = h
        .state()
        .tab
        .items()
        .iter()
        .find(|i| &i.path == path)
        .expect("item drawn")
        .rect;
    let c = ((it[0] + it[2]) / 2.0, (it[1] + it[3]) / 2.0);
    let (rect, now) = h.state().tab.canvas.last_frame().unwrap();
    h.state().tab.canvas.view().image_to_screen(c, rect, now)
}

fn click(h: &mut Harness<'_, State>, p: egui::Pos2) {
    h.hover_at(p);
    h.drag_at(p);
    h.event(egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.step();
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
        s.tab.items().len() == 4
            && s.tab
                .items()
                .iter()
                .filter(|i| !i.layers.is_empty())
                .count()
                == 2
    });
    assert_eq!(h.state().tab.view(), Some(&ViewKind::Group("Ha".into())));
    let items = h.state().tab.items().to_vec();
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
    assert!(h.state().tab.stretch().is_some());
    // The extent is the frame: the view does not move as the rest arrive.
    let before = {
        let (_, now) = h.state().tab.canvas.last_frame().unwrap();
        h.state().tab.canvas.view().zoom(now)
    };
    let rest = previews_of(&h.state().session, &ha[2..]);
    h.state_mut().previews.extend(rest);
    h.state_mut().complete = true;
    step_until(&mut h, |s| s.tab.is_settled());
    assert_eq!(textured(&h), 4);
    assert_eq!(dashes(&h), 0, "every panel drawn with its preview");
    let (_, now) = h.state().tab.canvas.last_frame().unwrap();
    assert_eq!(h.state().tab.canvas.view().zoom(now), before);
    assert_eq!(h.state().tab.frame_canvas(), Some(crate::testutil::CANVAS));
    // Hidden for a while: the textures go, and come back when shown.
    h.state_mut()
        .tab
        .release_if_hidden(std::time::Instant::now() + RELEASE_AFTER * 2);
    assert!(h.state().tab.textures.is_empty());
    h.step();
    step_until(&mut h, |s| s.tab.is_settled());
    assert_eq!(textured(&h), 4);
    // Shown recently: kept.
    h.state_mut()
        .tab
        .release_if_hidden(std::time::Instant::now());
    assert_eq!(h.state().tab.textures.len(), 4);
    let _ = std::fs::remove_dir_all(&root);
}

fn check(code: CheckCode, severity: Severity, panels: Vec<PathBuf>, message: &str) -> Check {
    Check {
        code,
        severity,
        panels,
        groups: Vec::new(),
        message: message.into(),
        remedy: None,
    }
}

#[test]
fn selection_filters_checks() {
    let (root, s, _r) = analyzed("plan-select");
    let ha = panels(&s, "Ha");
    let previews = previews_of(&s, &ha);
    let mut h = harness(s);
    h.state_mut().previews = previews;
    h.state_mut().complete = true;
    h.state_mut().report = Some(ChecksReport {
        checks: vec![
            check(
                CheckCode::FileChanged,
                Severity::Problem,
                vec![ha[0].clone()],
                "First panel changed",
            ),
            check(
                CheckCode::SolutionPresent,
                Severity::Note,
                vec![ha[1].clone()],
                "Second panel unsolved",
            ),
            check(
                CheckCode::GroupCountMismatch,
                Severity::Note,
                Vec::new(),
                "Groups differ",
            ),
        ],
        errors: Vec::new(),
    });
    step_until(&mut h, |s| s.tab.is_settled());
    // No selection: every check.
    h.get_by_label("First panel changed");
    h.get_by_label("Second panel unsolved");
    h.get_by_label("Groups differ");
    // The flagged panel is outlined in Hα, the others in the accent.
    for it in h.state().tab.items() {
        let want = if it.path == ha[0] {
            mega_ui::theme::palette::HA_500
        } else {
            mega_ui::theme::palette::ACCENT_500
        };
        assert_eq!(it.outline, want, "{:?}", it.path);
    }
    let p = centre_of(&h, &ha[0]);
    click(&mut h, p);
    step_until(&mut h, |s| s.tab.selected() == Some(ha[0].as_path()));
    h.get_by_label("Checks for this panel");
    h.get_by_label("First panel changed");
    assert!(h.query_by_label("Second panel unsolved").is_none());
    assert!(h.query_by_label("Groups differ").is_none());
    // The side panel: name, header facts (read on a worker) and overlaps.
    h.get_by_label("Ha_0.xisf");
    step_until(&mut h, |s| s.tab.info.contains_key(&ha[0]));
    h.get_by_label("600 × 420 (0.3 MP)");
    h.get_by_label("None (registered)");
    // New inputs (a generation): the header is read again.
    h.state_mut().generation = 2;
    h.step();
    assert!(!h.state().tab.info.contains_key(&ha[0]));
    step_until(&mut h, |s| s.tab.info.contains_key(&ha[0]));
    assert!(
        h.query_all_by_label("Ha_1.xisf").next().is_some(),
        "overlaps listed"
    );
    // Accept goes to the shell.
    // The side panel scrolls; bring Accept into view as a user would.
    h.get_by_label("Accept").scroll_to_me();
    h.run();
    h.get_by_label("Accept").click();
    h.run();
    assert_eq!(h.state().accepted.len(), 1);
    assert_eq!(h.state().accepted[0].message, "First panel changed");
    // Show all clears the filter.
    h.get_by_label("Show all").click();
    h.run();
    h.get_by_label("Second panel unsolved");
    h.get_by_label("Groups differ");
    // Esc deselects.
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().tab.selected(), None);
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
    h.state_mut().tab.select_view(ViewKind::Composition(0));
    step_until(&mut h, |s| s.tab.is_settled());
    let items = h.state().tab.items().to_vec();
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
    assert_eq!(h.state().tab.stretch().map(<[_]>::len), Some(3));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn copy_path_puts_path_on_clipboard() {
    let root = temp_dir("plan-copy");
    let s = two_group_session(&root);
    let path = s.mosaic.config.panels[0].path.clone();
    let mut h = harness(s);
    h.run();
    h.state_mut().tab.select(Some(path.clone()));
    h.run();
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
fn group_selector_hidden_when_grouping_off() {
    let root = temp_dir("plan-grouping");
    let mut s = two_group_session(&root);
    let mut h = harness(two_group_session(&temp_dir("plan-grouping-on")));
    h.run();
    h.get_by_label("View");
    let on_root = h.state().session.mosaic.dir.parent().unwrap().to_path_buf();
    drop(h);
    s.mosaic.config.grouping.enabled = false;
    s.groups = s.mosaic.groups().ok();
    let mut h = harness(s);
    h.run();
    assert!(h.query_by_label("View").is_none());
    assert!(h.query_by_label("+ composition").is_none());
    assert_eq!(
        h.state().tab.view(),
        Some(&ViewKind::Group(mmm_core::mosaic::SINGLE_GROUP.into()))
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&on_root);
}

#[test]
fn previews_paused_offers_resume() {
    let root = temp_dir("plan-resume");
    let mut h = harness(two_group_session(&root));
    h.run();
    h.get_by_label("Previews paused —");
    h.get_by_label("Resume").click();
    h.run();
    assert!(h.state().resumed >= 1);
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
    };
    let mut h = harness(s);
    // No previews, no analysis: dashed outlines at the WCS footprints.
    step_until(&mut h, |s| s.tab.items().len() == 2);
    assert_eq!(h.state().tab.unplaced, 0);
    for it in h.state().tab.items() {
        assert!(it.layers.is_empty());
        let want = mmm_core::preview::footprint(&it.path, &frame)
            .unwrap()
            .unwrap();
        assert_eq!(it.rect, want);
    }
    let r: Vec<[f64; 4]> = h.state().tab.items().iter().map(|i| i.rect).collect();
    // The second panel's reference pixel is (60, 30) px further left/up.
    assert!((r[1][0] - r[0][0] - 60.0).abs() < 1.0, "{r:?}");
    assert!((r[1][1] - r[0][1] - 30.0).abs() < 1.0, "{r:?}");
    assert!(dashes(&h) > 0);
    assert!(h.query_by_label_contains("not placed yet").is_none());
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
            linked,
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
    // A stretch slider drag: a new epoch per step, each resending every
    // conversion. With every job queued before the converters start (and
    // the epoch at the last step), superseded jobs are skipped unconverted:
    // exactly one epoch's worth is done.
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
