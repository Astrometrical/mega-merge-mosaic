//! Opening a problem (a checks row, a canvas badge, the keyboard): it is
//! framed and selected; the window's selection of a panel, an overlap or a
//! seam; See all checks with Un-accept; the selected panel's overlaps
//! (spec §7.4, §9).

use std::collections::BTreeMap;
use std::path::Path;

use mega_ui::canvas_layers::Polyline;
use mmm_core::checks::CheckTarget;
use mmm_core::health::SeamLine;

use super::marks::{
    analysed, at, badge_button, centre, framed, meet, members, mosaic_ready, plan_ready,
    problem_at, seam,
};
use super::*;
use crate::marks::{Place, selection_ring};

/// A file's name, as the window names panels.
fn name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

/// Two panels in path order (as a place holds them).
fn pair(a: &Path, b: &Path) -> (PathBuf, PathBuf) {
    (a.min(b).to_path_buf(), a.max(b).to_path_buf())
}

/// The bbox of `points`.
fn bbox(points: &[[f64; 2]]) -> [f64; 4] {
    points
        .iter()
        .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |r, p| {
            [
                r[0].min(p[0]),
                r[1].min(p[1]),
                r[2].max(p[0]),
                r[3].max(p[1]),
            ]
        })
}

/// The rects of the selection rings among `lines`.
fn rings(lines: &[Polyline]) -> Vec<[f64; 4]> {
    lines
        .iter()
        .filter(|l| {
            let r = bbox(&l.points);
            **l == selection_ring(r)
        })
        .map(|l| bbox(&l.points))
        .collect()
}

/// Open the checks row titled `title` (its open button, through
/// accessibility: wherever the inspector's scrolling has put it).
fn open_row(h: &mut Harness<'static, State>, title: &str) {
    h.get_by_role_and_label(Role::Button, title)
        .click_accesskit();
    h.run_steps(3);
}

/// Whether the checks row titled `title` is an open button.
fn opens(h: &Harness<'static, State>, title: &str) -> bool {
    h.query_by_role_and_label(Role::Button, title).is_some()
}

/// Click the plan at canvas point `p`.
fn click_plan(h: &mut Harness<'static, State>, p: (f64, f64)) {
    let at = h.state().w.plan().to_screen(p).expect("the plan drawn");
    let (frame, _) = h.state().w.plan().canvas_frame().unwrap();
    assert!(
        frame.contains(at),
        "{p:?} at {at:?} is off the plan {frame:?}"
    );
    h.hover_at(at);
    h.drag_at(at);
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(2);
}

/// A canvas point inside panel `i` of `panels` and no other: near one of
/// its corners.
fn own_point(
    geo: &BTreeMap<PathBuf, (usize, [f64; 4])>,
    panels: &[PathBuf],
    i: usize,
) -> (f64, f64) {
    let r = geo[&panels[i]].1;
    let inside = |p: (f64, f64), r: [f64; 4]| p.0 > r[0] && p.0 < r[2] && p.1 > r[1] && p.1 < r[3];
    [
        (r[0] + 10.0, r[1] + 10.0),
        (r[2] - 10.0, r[1] + 10.0),
        (r[0] + 10.0, r[3] - 10.0),
        (r[2] - 10.0, r[3] - 10.0),
    ]
    .into_iter()
    .find(|&p| {
        panels
            .iter()
            .enumerate()
            .all(|(j, q)| j == i || !inside(p, geo[q].1))
    })
    .expect("a corner of its own")
}

/// Asserts the plan's camera frames canvas rect `r` (whole pixels, as
/// opening a problem rounds it).
fn assert_plan_framed(h: &Harness<'static, State>, r: [f64; 4]) {
    let (frame, _) = h.state().w.plan().canvas_frame().unwrap();
    assert_camera(
        h.state().w.plan().camera(),
        Camera::framing(framed(r), frame.size()),
    );
}

/// The window's selection.
fn selection(h: &Harness<'static, State>) -> Option<Selected> {
    h.state().w.selection().cloned()
}

/// Whether the inspector's Panels section is open.
fn panels_open(h: &Harness<'static, State>) -> bool {
    h.state().w.inspector().is_open(PANELS)
}

/// Open the Panels section, as its heading clicked does (scrolling
/// nowhere).
fn open_panels(h: &mut Harness<'static, State>) {
    h.state_mut().w.inspector_mut().open.insert(PANELS.into());
    h.run_steps(2);
}

/// A seam line of group `geo`'s panels `a` and `b` through `points`.
fn seam_line(
    geo: &BTreeMap<PathBuf, (usize, [f64; 4])>,
    a: &PathBuf,
    b: &PathBuf,
    points: Vec<[f64; 2]>,
) -> SeamLine {
    let (x, y) = (geo[a].0 as u16, geo[b].0 as u16);
    SeamLine {
        a: x.min(y),
        b: x.max(y),
        points,
        flagged: true,
    }
}

#[test]
fn opening_a_problem_frames_and_selects_it() {
    let (root, s, jobs) = merged("window-open-problem");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let rect = |i: usize| geo[&ha[i]].1;
    // A seam line between panels 0 and 2, inside their overlap.
    let o02 = meet(rect(0), rect(2));
    let line = vec![[o02[0] + 8.0, o02[1] + 6.0], [o02[2] - 8.0, o02[1] + 14.0]];
    let checks = vec![
        problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[3].clone()),
            &["Ha"],
            "Panel 3 alone",
        ),
        problem_at(
            CheckCode::ThinOverlap,
            CheckTarget::Overlap(ha[1].clone(), ha[0].clone()),
            &["Ha"],
            "Thin between 0 and 1",
        ),
        problem_at(
            CheckCode::ResidualSeam,
            seam("Ha", &ha[2], &ha[0]),
            &["Ha"],
            "A seam shows between 0 and 2",
        ),
        // A note at the overlap: listed with it (every severity).
        at(
            CheckCode::DuplicatePanels,
            Severity::Note,
            CheckTarget::Overlap(ha[0].clone(), ha[1].clone()),
            &["Ha"],
            "Nearly the same sky",
        ),
    ];
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks,
        errors: Vec::new(),
        seams: BTreeMap::from([(
            "Ha".to_string(),
            vec![seam_line(&geo, &ha[0], &ha[2], line.clone())],
        )]),
    });
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    step_to(&mut h, plan_ready);
    h.run_steps(2);

    // A panel's problem: framed, the panel selected on the plan. Opened
    // from a list, the inspector keeps its place: Panels stays closed.
    open_row(&mut h, "Panel 3 alone");
    assert_plan_framed(&h, rect(3));
    assert_eq!(h.state().w.plan().selected(), Some(ha[3].as_path()));
    assert_eq!(selection(&h), Some(Selected::Panel(ha[3].clone())));
    assert!(!panels_open(&h), "a list's open reveals nothing");
    // Open, Panels details it.
    open_panels(&mut h);
    h.get_by_role_and_label(Role::Heading, &name(&ha[3]));
    assert!(
        rings(h.state().w.plan().lines()).is_empty(),
        "a panel has no ring"
    );

    // An overlap's: framed and ringed, its checks under its heading, no
    // panel selected.
    open_row(&mut h, "Thin between 0 and 1");
    let o01 = meet(rect(0), rect(1));
    assert_plan_framed(&h, o01);
    let (lo, hi) = pair(&ha[0], &ha[1]);
    assert_eq!(
        selection(&h),
        Some(Selected::Overlap(lo.clone(), hi.clone()))
    );
    assert_eq!(h.state().w.plan().selected(), None);
    assert_eq!(rings(h.state().w.plan().lines()), [o01]);
    h.get_by_role_and_label(
        Role::Heading,
        &format!("Overlap {} – {}", name(&lo), name(&hi)),
    );
    // Every check there: the problem (in Problems too) and the note; not
    // the others.
    assert_eq!(h.query_all_by_label("Thin between 0 and 1").count(), 2);
    h.get_by_label("Nearly the same sky");
    assert_eq!(h.query_all_by_label("Panel 3 alone").count(), 1);

    // A seam's: framed by its lines and ringed there.
    open_row(&mut h, "A seam shows between 0 and 2");
    let lines = bbox(&line);
    assert_plan_framed(&h, lines);
    let (lo, hi) = pair(&ha[0], &ha[2]);
    assert_eq!(
        selection(&h),
        Some(Selected::Seam {
            group: "Ha".into(),
            a: lo.clone(),
            b: hi.clone(),
        })
    );
    assert_eq!(rings(h.state().w.plan().lines()), [lines]);
    h.get_by_role_and_label(
        Role::Heading,
        &format!("Seam {} – {}", name(&lo), name(&hi)),
    );
    assert_eq!(
        h.query_all_by_label("A seam shows between 0 and 2").count(),
        2
    );

    // The mosaic rings it too, and frames what is opened there.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, mosaic_ready);
    h.run_steps(2);
    assert_eq!(rings(h.state().w.mosaic().lines()), [lines]);
    open_row(&mut h, "Thin between 0 and 1");
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    assert_camera(
        h.state().w.mosaic().camera(),
        Camera::framing(framed(o01), frame.size()),
    );
    assert_eq!(rings(h.state().w.mosaic().lines()), [o01]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_opened_panel_keeps_its_badge_in_view_and_is_ringed_on_the_mosaic() {
    use mega_ui::theme::tokens::BADGE_HEIGHT;
    let (root, s, jobs) = merged("window-open-panel-margin");
    let ha = members(&s, "Ha");
    let r1 = analysed(&s, "Ha")[&ha[1]].1;
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[1].clone()),
            &["Ha"],
            "Panel 1 alone",
        )],
        errors: Vec::new(),
        seams: Default::default(),
    });
    let mut h = harness(state);
    step_to(&mut h, mosaic_ready);
    h.run_steps(2);
    open_row(&mut h, "Panel 1 alone");
    // Framed with a margin: its badge, at its top-right corner, is well
    // inside the view, not on its edge.
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    let cam = h.state().w.mosaic().camera().unwrap();
    assert_camera(Some(cam), Camera::framing(framed(r1), frame.size()));
    let badge = screen(frame, cam, (r1[2], r1[1]));
    assert!(
        frame.shrink(BADGE_HEIGHT / 2.0).contains(badge),
        "the badge at {badge:?} in {frame:?}"
    );
    // The mosaic rings the panel selected at its bbox.
    assert_eq!(selection(&h), Some(Selected::Panel(ha[1].clone())));
    assert_eq!(rings(h.state().w.mosaic().lines()), [r1]);
    // The plan outlines it instead (no ring).
    h.get_by_label("Plan").click();
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    assert_eq!(h.state().w.plan().selected(), Some(ha[1].as_path()));
    assert!(rings(h.state().w.plan().lines()).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_selection_gives_way_to_a_panel_an_empty_click_and_esc() {
    let (root, s, jobs) = analyzed("window-selection-clears");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![problem_at(
            CheckCode::ThinOverlap,
            CheckTarget::Overlap(ha[0].clone(), ha[1].clone()),
            &["Ha"],
            "Thin between 0 and 1",
        )],
        errors: Vec::new(),
        seams: Default::default(),
    });
    let mut h = harness(state);
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    let overlap = |h: &Harness<'static, State>| {
        matches!(selection(h), Some(Selected::Overlap(..)))
            && rings(h.state().w.plan().lines()).len() == 1
    };
    // A panel clicked on the plan is selected instead.
    open_row(&mut h, "Thin between 0 and 1");
    assert!(overlap(&h));
    h.state_mut().w.frame([0, 0, 600, 420]);
    h.run_steps(3);
    click_plan(&mut h, own_point(&geo, &ha, 2));
    assert_eq!(selection(&h), Some(Selected::Panel(ha[2].clone())));
    assert_eq!(h.state().w.plan().selected(), Some(ha[2].as_path()));
    assert!(rings(h.state().w.plan().lines()).is_empty());
    // A click on no panel selects nothing.
    open_row(&mut h, "Thin between 0 and 1");
    assert!(overlap(&h));
    assert_eq!(h.state().w.plan().selected(), None, "no panel with it");
    h.state_mut().w.frame([0, 0, 1200, 840]);
    h.run_steps(3);
    click_plan(&mut h, (1000.0, 700.0));
    assert_eq!(selection(&h), None);
    assert!(rings(h.state().w.plan().lines()).is_empty());
    // Nor after Esc.
    open_row(&mut h, "Thin between 0 and 1");
    assert!(overlap(&h));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(selection(&h), None);
    assert!(rings(h.state().w.plan().lines()).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_badge_click_opens_its_problem() {
    let (root, s, jobs) = merged("window-badge-opens");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let rect = |i: usize| geo[&ha[i]].1;
    let o01 = meet(rect(0), rect(1));
    let line = vec![[o01[0] + 6.0, o01[1] + 4.0], [o01[0] + 6.0, o01[3] - 4.0]];
    // The pair's badge holds its seam's problem first (the Problems list's
    // order), then its overlap's.
    let checks = vec![
        problem_at(
            CheckCode::ResidualSeam,
            seam("Ha", &ha[1], &ha[0]),
            &["Ha"],
            "A seam shows between 0 and 1",
        ),
        problem_at(
            CheckCode::ThinOverlap,
            CheckTarget::Overlap(ha[0].clone(), ha[1].clone()),
            &["Ha"],
            "Thin between 0 and 1",
        ),
        problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[2].clone()),
            &["Ha"],
            "Panel 2 alone",
        ),
    ];
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks,
        errors: Vec::new(),
        seams: BTreeMap::from([(
            "Ha".to_string(),
            vec![seam_line(&geo, &ha[0], &ha[1], line.clone())],
        )]),
    });
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    step_to(&mut h, |st| {
        plan_ready(st) && st.w.plan().badges().len() == 2
    });
    h.run_steps(2);
    let (lo, hi) = pair(&ha[0], &ha[1]);
    let the_seam = Selected::Seam {
        group: "Ha".into(),
        a: lo.clone(),
        b: hi.clone(),
    };
    // The pair's badge opens its first problem, the seam: framed by its
    // lines, selected and ringed.
    let c = centre(o01);
    let at = h.state().w.plan().to_screen((c[0], c[1])).unwrap();
    assert!(!panels_open(&h));
    badge_button(&h, at).click();
    h.run_steps(3);
    assert_plan_framed(&h, bbox(&line));
    assert_eq!(selection(&h), Some(the_seam.clone()));
    assert_eq!(rings(h.state().w.plan().lines()), [bbox(&line)]);
    // A badge reveals Panels: the seam's heading, then every check of the
    // pair (its seam's and its overlap's, as the badge counts them).
    assert!(panels_open(&h), "a badge reveals Panels");
    h.get_by_role_and_label(
        Role::Heading,
        &format!("Seam {} – {}", name(&lo), name(&hi)),
    );
    for title in ["A seam shows between 0 and 1", "Thin between 0 and 1"] {
        assert_eq!(
            h.query_all_by_role_and_label(Role::Button, title).count(),
            2,
            "{title} in Problems and the pair's checks"
        );
    }
    assert_eq!(h.query_all_by_label("Panel 2 alone").count(), 1);
    // A panel's badge opens its panel's problem: the panel selected.
    h.state_mut().w.frame([0, 0, 600, 420]);
    h.run_steps(3);
    let r2 = rect(2);
    let at = h.state().w.plan().to_screen((r2[2], r2[1])).unwrap();
    badge_button(&h, at).click();
    h.run_steps(3);
    assert_plan_framed(&h, r2);
    assert_eq!(selection(&h), Some(Selected::Panel(ha[2].clone())));
    assert_eq!(h.state().w.plan().selected(), Some(ha[2].as_path()));
    // On the mosaic too.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        mosaic_ready(st) && st.w.mosaic().badges().len() == 2
    });
    h.state_mut().w.frame([0, 0, 600, 420]);
    h.run_steps(3);
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    let cam = h.state().w.mosaic().camera().unwrap();
    badge_button(&h, screen(frame, cam, (c[0], c[1]))).click();
    h.run_steps(3);
    assert_camera(
        h.state().w.mosaic().camera(),
        Camera::framing(framed(bbox(&line)), frame.size()),
    );
    assert_eq!(selection(&h), Some(the_seam));
    assert_eq!(rings(h.state().w.mosaic().lines()), [bbox(&line)]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_stale_problem_target_is_harmless() {
    let (root, s, jobs) = merged("window-stale-problem");
    let ha = members(&s, "Ha");
    let alone = problem_at(
        CheckCode::NoOverlap,
        CheckTarget::Panel(ha[3].clone()),
        &["Ha"],
        "Panel 3 alone",
    );
    let thin = problem_at(
        CheckCode::ThinOverlap,
        CheckTarget::Overlap(ha[1].clone(), ha[3].clone()),
        &["Ha"],
        "Thin between 1 and 3",
    );
    // Targets that never resolve: a group the session does not have, a
    // panel it does not have, a group-wide problem with no area.
    let other_group = problem_at(
        CheckCode::ResidualSeam,
        seam("SII", &ha[0], &ha[1]),
        &["SII"],
        "A seam shows in SII",
    );
    let other_panel = problem_at(
        CheckCode::NoOverlap,
        CheckTarget::Panel("gone.xisf".into()),
        &["Ha"],
        "gone.xisf alone",
    );
    let nowhere = problem_at(
        CheckCode::MixedChannels,
        CheckTarget::Group("Ha".into()),
        &["Ha"],
        "Mixed channels",
    );
    // A coverage hole of a group the session no longer has (renamed or
    // removed since the checks ran): its area is no place now.
    let mut old_hole = problem_at(
        CheckCode::CoverageHole,
        CheckTarget::Group("SII".into()),
        &["SII"],
        "A hole in SII",
    );
    old_hole.rect = Some([20, 30, 60, 50]);
    let never = [&other_group, &other_panel, &nowhere, &old_hole];
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: [&alone, &thin].into_iter().chain(never).cloned().collect(),
        errors: Vec::new(),
        seams: Default::default(),
    });
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    step_to(&mut h, |st| {
        plan_ready(st) && !st.w.plan().badges().is_empty()
    });
    h.run_steps(2);
    // Rows whose target does not resolve lead nowhere.
    for title in [
        "A seam shows in SII",
        "gone.xisf alone",
        "Mixed channels",
        "A hole in SII",
    ] {
        h.get_by_label(title);
        assert!(!opens(&h, title), "{title} opens");
    }
    assert!(opens(&h, "Panel 3 alone"));
    // Opening `checks` (and the badge `badge`, if any) as a row or a badge
    // would, then a few frames.
    let toasts = |h: &Harness<'static, State>| h.state().toasts.len();
    let open = |h: &mut Harness<'static, State>, checks_: &[&Check], badge: Option<u64>| {
        let State {
            w, session, checks, ..
        } = h.state_mut();
        if let Some(id) = badge {
            w.open_badge(session, checks.report(), id);
        }
        for c in checks_ {
            w.open_problem(session, checks.report(), c, true);
        }
        h.run_steps(3);
    };
    // On a plan drawn in another frame, a target that resolves says why in
    // a toast (and nothing more); one that does not says nothing.
    let analysed_frame = h.state().session.frame.clone();
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(2);
    let before = toasts(&h);
    open(&mut h, &never, None);
    assert_eq!(toasts(&h), before, "no toast");
    open(&mut h, &[&alone], None);
    let said: Vec<&str> = h.state().toasts[before..]
        .iter()
        .map(|t| t.toast.title.as_str())
        .collect();
    assert_eq!(said, [FRAME_BEHIND]);
    assert_eq!(selection(&h), None, "nothing more");
    h.state_mut().session.frame = analysed_frame;
    h.run_steps(2);
    // Opened, the overlap is selected.
    open_row(&mut h, "Thin between 1 and 3");
    let (lo, hi) = pair(&ha[1], &ha[3]);
    assert_eq!(selection(&h), Some(Selected::Overlap(lo, hi)));
    let badge = h
        .state()
        .w
        .marks
        .iter()
        .find(|m| m.place == Place::Panel(ha[3].clone()))
        .map(|m| m.id)
        .expect("panel 3's badge");
    // Panel 3 leaves the session: the overlap's selection and ring go.
    edit_panels(&mut h, |p| p.retain(|e| e.path != ha[3]));
    h.run_steps(3);
    assert_eq!(selection(&h), None);
    assert!(rings(h.state().w.plan().lines()).is_empty());
    assert!(!opens(&h, "Panel 3 alone"), "its row leads nowhere now");
    // Opening its problems (a row, its badge, kept from before) or those
    // that never resolved moves nothing, selects nothing, says nothing.
    let (cam, before) = (h.state().w.plan().camera(), toasts(&h));
    let all: Vec<&Check> = [&alone, &thin].into_iter().chain(never).collect();
    open(&mut h, &all, Some(badge));
    assert_camera(h.state().w.plan().camera(), cam.unwrap());
    assert_eq!(selection(&h), None);
    assert_eq!(h.state().w.plan().selected(), None);
    assert_eq!(toasts(&h), before, "no toast");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn see_all_groups_and_unaccepts() {
    let (root, mut s, jobs) = analyzed("window-see-all");
    let ha = members(&s, "Ha");
    let accepted = problem_at(
        CheckCode::ThinOverlap,
        CheckTarget::Overlap(ha[0].clone(), ha[1].clone()),
        &["Ha"],
        "Thin, accepted",
    );
    s.mosaic.config.accepted_checks.extend(accepted.keys());
    let checks = vec![
        accepted.clone(),
        problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[2].clone()),
            &["Ha"],
            "Panel 2 alone",
        ),
        at(
            CheckCode::EmptyArea,
            Severity::Note,
            CheckTarget::Panel(ha[1].clone()),
            &["Ha"],
            "Panel 1 is half empty",
        ),
        at(
            CheckCode::DistortionModel,
            Severity::Passed,
            CheckTarget::Panel(ha[0].clone()),
            &[],
            "No distortion model: accepted for pre-aligned panels",
        ),
    ];
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks,
        errors: vec!["Cannot read the analysis of group OIII.".into()],
        seams: Default::default(),
    });
    let mut h = harness(state);
    h.run_steps(3);
    let heading = |h: &Harness<'static, State>, name: &str| {
        h.query_by_role_and_label(Role::Heading, name).is_some()
    };
    // Problems: the problems only (the accepted one says so), ungrouped,
    // nothing to un-accept.
    h.get_by_label("Panel 2 alone");
    h.get_by_label("Thin, accepted");
    h.get_by_label("Accepted");
    h.get_by_label("Cannot read the analysis of group OIII.");
    assert!(h.query_by_label("Panel 1 is half empty").is_none());
    assert!(!heading(&h, "Notes"));
    assert!(h.query_by_label("Un-accept").is_none());
    assert!(h.query_by_label("‹ Problems").is_none());
    // See all: every check, grouped by severity; an accepted problem can be
    // un-accepted.
    h.get_by_label("See all checks ›").click();
    h.run_steps(2);
    for name in ["Problems", "Notes", "Passed"] {
        assert!(heading(&h, name), "{name} heading");
    }
    h.get_by_label("Panel 1 is half empty");
    h.get_by_label("1 check passed");
    h.get_by_label("Cannot read the analysis of group OIII.");
    assert!(h.query_by_label("See all checks ›").is_none());
    h.get_by_label("Un-accept").click_accesskit();
    h.run_steps(2);
    assert_eq!(h.state().unaccepted, [accepted]);
    assert!(h.state().accepted.is_empty());
    // ‹ Problems returns.
    h.get_by_label("‹ Problems").click_accesskit();
    h.run_steps(2);
    assert!(h.query_by_label("Panel 1 is half empty").is_none());
    assert!(!heading(&h, "Notes"));
    h.get_by_label("See all checks ›");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn overlap_rows_go_to_the_neighbour() {
    let (root, s, jobs) = analyzed("window-overlap-rows");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let rect = |i: usize| geo[&ha[i]].1;
    let mut h = harness(State::new(s, jobs));
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    let r0 = rect(0);
    click_plan(&mut h, own_point(&geo, &ha, 0));
    assert_eq!(h.state().w.plan().selected(), Some(ha[0].as_path()));
    assert!(h.state().w.plan().layout().overlaps[&ha[0]].contains(&ha[1]));
    // Its overlaps are rows that say what they do: one selects the
    // neighbour and frames the overlap (not the neighbour's whole bbox).
    let neighbour = name(&ha[1]);
    let row = h.get_by_role_and_label(Role::Button, &neighbour);
    let said = format!("Select {neighbour} and go to the overlap");
    assert_eq!(
        row.accesskit_node().description().as_deref(),
        Some(said.as_str())
    );
    row.click_accesskit();
    h.run_steps(3);
    assert_eq!(h.state().w.plan().selected(), Some(ha[1].as_path()));
    assert_eq!(selection(&h), Some(Selected::Panel(ha[1].clone())));
    assert_plan_framed(&h, meet(r0, rect(1)));
    // Its details now, the first panel among its overlaps.
    h.get_by_role_and_label(Role::Heading, &name(&ha[1]));
    h.get_by_role_and_label(Role::Button, &name(&ha[0]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn keyboard_opens_a_problem() {
    let (root, s, jobs) = analyzed("window-keyboard-opens");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[1].clone()),
            &["Ha"],
            "Panel 1 alone",
        )],
        errors: Vec::new(),
        seams: Default::default(),
    });
    let mut h = harness(state);
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "Panel 1 alone")
        .focus();
    h.run_steps(2);
    assert!(
        h.get_by_role_and_label(Role::Button, "Panel 1 alone")
            .accesskit_node()
            .is_focused()
    );
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_plan_framed(&h, geo[&ha[1]].1);
    assert_eq!(selection(&h), Some(Selected::Panel(ha[1].clone())));
    assert_eq!(h.state().w.plan().selected(), Some(ha[1].as_path()));
    assert!(!panels_open(&h), "a list's open reveals nothing");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn opening_shows_the_problems_group() {
    let (root, s, jobs) = analyzed("window-open-group");
    let oiii = members(&s, "OIII");
    let geo = analysed(&s, "OIII");
    let o01 = meet(geo[&oiii[0]].1, geo[&oiii[1]].1);
    let mut hole = problem_at(
        CheckCode::CoverageHole,
        CheckTarget::Group("OIII".into()),
        &["OIII"],
        "A hole in OIII",
    );
    hole.rect = Some([20, 30, 60, 50]);
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![
            problem_at(
                CheckCode::ThinOverlap,
                CheckTarget::Overlap(oiii[0].clone(), oiii[1].clone()),
                &["OIII"],
                "Thin in OIII",
            ),
            hole,
        ],
        errors: Vec::new(),
        seams: Default::default(),
    });
    let mut h = harness(state);
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    let ha = ViewKind::Group("Ha".into());
    let shows = |h: &Harness<'static, State>| h.state().w.view().cloned();
    assert_eq!(shows(&h), Some(ha.clone()));
    // An OIII overlap opened while Ha shows: OIII shows, framed and ringed.
    open_row(&mut h, "Thin in OIII");
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    assert_eq!(shows(&h), Some(ViewKind::Group("OIII".into())));
    assert_plan_framed(&h, o01);
    let (lo, hi) = pair(&oiii[0], &oiii[1]);
    assert_eq!(selection(&h), Some(Selected::Overlap(lo, hi)));
    assert_eq!(rings(h.state().w.plan().lines()), [o01]);
    // Only a view showing it rings it, as its badge.
    h.state_mut().w.select_view(ha.clone());
    h.run_steps(3);
    assert!(rings(h.state().w.plan().lines()).is_empty(), "not on Ha");
    assert!(selection(&h).is_some(), "still selected");
    h.state_mut().w.select_view(ViewKind::Group("OIII".into()));
    h.run_steps(3);
    assert_eq!(rings(h.state().w.plan().lines()), [o01]);
    // Nor a plan drawn in another frame than the analysed one.
    let analysed_frame = h.state().session.frame.clone();
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    assert!(rings(h.state().w.plan().lines()).is_empty(), "frame behind");
    h.state_mut().session.frame = analysed_frame;
    h.run_steps(3);
    assert_eq!(rings(h.state().w.plan().lines()), [o01]);
    // A hole in OIII opened while Ha shows: OIII shows, the hole framed,
    // nothing selected.
    h.state_mut().w.select_view(ha);
    h.run_steps(3);
    open_row(&mut h, "A hole in OIII");
    assert_eq!(shows(&h), Some(ViewKind::Group("OIII".into())));
    assert_plan_framed(&h, [20.0, 30.0, 60.0, 50.0]);
    assert_eq!(selection(&h), None);
    let _ = std::fs::remove_dir_all(&root);
}
