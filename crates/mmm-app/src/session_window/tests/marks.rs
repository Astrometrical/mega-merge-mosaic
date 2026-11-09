//! Problems badged where they are, panel outlines and seam lines on both
//! canvases, the Seams toggle, and the chips' problem counts (spec §7.2–§7.4).

use std::collections::BTreeMap;

use egui::Color32;
use egui::epaint::ColorMode;
use mega_ui::canvas_layers::CanvasBadge;
use mega_ui::theme::tokens::{LINE_WIDTH, LINE_WIDTH_STRONG};
use mega_ui::theme::{Tone, palette};
use mmm_core::checks::CheckTarget;
use mmm_core::health::SeamLine;
use mmm_core::session::Session;

use super::*;

/// The panels of `group` in `s`, in configuration order.
fn members(s: &OpenSession, group: &str) -> Vec<PathBuf> {
    group_members(s, &[group.to_string()])
        .remove(group)
        .expect("the group")
}

/// Each panel of `group`'s id and content bbox in the group's analysis
/// (read from its `session.json`, not through the window).
fn analysed(s: &OpenSession, group: &str) -> BTreeMap<PathBuf, (usize, [f64; 4])> {
    let session = Session::open(&s.mosaic.group_session_dir(group)).unwrap();
    session
        .panels
        .iter()
        .map(|p| {
            let b = p.bbox.map(|v| v as f64);
            (
                p.source.clone().unwrap_or_else(|| p.path.clone()),
                (p.id, b),
            )
        })
        .collect()
}

/// The intersection of two rects.
fn meet(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

/// A rect's centre.
fn centre(r: [f64; 4]) -> [f64; 2] {
    [(r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0]
}

/// A check of `severity` at `target`.
fn at(
    code: CheckCode,
    severity: Severity,
    target: CheckTarget,
    groups: &[&str],
    message: &str,
) -> Check {
    let panels = match &target {
        CheckTarget::Panel(p) => vec![p.clone()],
        CheckTarget::Overlap(a, b) | CheckTarget::Seam { a, b, .. } => vec![a.clone(), b.clone()],
        CheckTarget::Session | CheckTarget::Group(_) => Vec::new(),
    };
    Check {
        code,
        severity,
        panels,
        groups: groups.iter().map(|g| g.to_string()).collect(),
        message: message.into(),
        remedy: None,
        target,
        rect: None,
    }
}

/// A problem at `target`.
fn problem_at(code: CheckCode, target: CheckTarget, groups: &[&str], message: &str) -> Check {
    at(code, Severity::Problem, target, groups, message)
}

/// The seam of group `group` between `a` and `b`.
fn seam(group: &str, a: &std::path::Path, b: &std::path::Path) -> CheckTarget {
    CheckTarget::Seam {
        group: group.into(),
        a: a.to_path_buf(),
        b: b.to_path_buf(),
    }
}

/// `badges` in id order.
fn by_id(badges: &[CanvasBadge]) -> Vec<CanvasBadge> {
    let mut v = badges.to_vec();
    v.sort_by_key(|b| b.id);
    v
}

/// The badge at canvas point `p` (within a hundredth of a pixel), if any.
fn badge_at(badges: &[CanvasBadge], p: [f64; 2]) -> Option<&CanvasBadge> {
    badges
        .iter()
        .find(|b| (b.at[0] - p[0]).abs() < 0.01 && (b.at[1] - p[1]).abs() < 0.01)
}

/// Whether the last frame stroked a line in `color` at `width` inside
/// `canvas` (a polyline, or one of its dashes).
fn stroked(h: &Harness<'_, State>, canvas: egui::Rect, color: Color32, width: f32) -> bool {
    fn hit(s: &egui::Shape, color: Color32, width: f32) -> bool {
        match s {
            egui::Shape::Path(p) => {
                matches!(&p.stroke.color, ColorMode::Solid(c) if *c == color)
                    && (p.stroke.width - width).abs() < 1e-3
            }
            egui::Shape::LineSegment { stroke, .. } => {
                stroke.color == color && (stroke.width - width).abs() < 1e-3
            }
            egui::Shape::Vec(v) => v.iter().any(|s| hit(s, color, width)),
            _ => false,
        }
    }
    h.output()
        .shapes
        .iter()
        .any(|cs| canvas.expand(1.0).contains_rect(cs.clip_rect) && hit(&cs.shape, color, width))
}

/// The plan has drawn every panel of its view, its layout in.
fn plan_ready(st: &State) -> bool {
    plan_placed(st) && !st.w.plan().layout().rects.is_empty()
}

/// The mosaic is drawn, with the layout in.
fn mosaic_ready(st: &State) -> bool {
    st.w.mode() == CanvasMode::Mosaic
        && st.w.mosaic().viewer_frame().is_some()
        && !st.w.plan().layout().rects.is_empty()
}

#[test]
fn badges_match_on_plan_and_mosaic() {
    let (root, mut s, jobs) = merged("window-badges");
    let ha = members(&s, "Ha");
    let oiii = members(&s, "OIII");
    let geo = analysed(&s, "Ha");
    let rect = |p: &PathBuf| geo[p].1;
    // A seam line inside the overlap of panels 1 and 3, off its centre.
    let o13 = meet(rect(&ha[1]), rect(&ha[3]));
    let line = vec![
        [o13[0] + 10.0, o13[1] + 5.0],
        [o13[0] + 10.0, o13[1] + 25.0],
    ];
    let (i1, i3) = (geo[&ha[1]].0, geo[&ha[3]].0);
    let seam_line = SeamLine {
        a: i1.min(i3) as u16,
        b: i1.max(i3) as u16,
        points: line.clone(),
        flagged: true,
    };
    let mut hole = problem_at(
        CheckCode::CoverageHole,
        CheckTarget::Group("Ha".into()),
        &["Ha"],
        "A hole",
    );
    hole.rect = Some([20, 30, 60, 50]);
    let accepted = problem_at(
        CheckCode::FileMissing,
        CheckTarget::Panel(ha[3].clone()),
        &[],
        "Accepted",
    );
    s.mosaic.config.accepted_checks.extend(accepted.keys());
    let checks = vec![
        // Two problems on panel 0 (one naming no group): one badge, "2".
        problem_at(
            CheckCode::FileChanged,
            CheckTarget::Panel(ha[0].clone()),
            &[],
            "Panel 0 changed",
        ),
        problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[0].clone()),
            &["Ha"],
            "Panel 0 alone",
        ),
        problem_at(
            CheckCode::ThinOverlap,
            CheckTarget::Overlap(ha[1].clone(), ha[0].clone()),
            &["Ha"],
            "Thin",
        ),
        problem_at(
            CheckCode::ResidualSeam,
            seam("Ha", &ha[3], &ha[1]),
            &["Ha"],
            "A seam shows",
        ),
        hole,
        // No badge: a note, an accepted problem, a problem of the other
        // group and one of the whole group (it counts on the chip).
        at(
            CheckCode::EmptyArea,
            Severity::Note,
            CheckTarget::Panel(ha[2].clone()),
            &["Ha"],
            "Empty",
        ),
        accepted,
        problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(oiii[0].clone()),
            &["OIII"],
            "OIII alone",
        ),
        problem_at(
            CheckCode::MixedChannels,
            CheckTarget::Group("Ha".into()),
            &["Ha"],
            "Mixed",
        ),
    ];
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks,
        errors: Vec::new(),
        seams: BTreeMap::from([("Ha".to_string(), vec![seam_line])]),
    });
    state.w.set_mode(CanvasMode::Plan);
    let mut h = harness(state);
    step_to(&mut h, |st| {
        plan_ready(st) && !st.w.plan().badges().is_empty()
    });
    h.run_steps(2);
    let plan = by_id(h.state().w.plan().badges());
    assert_eq!(plan.len(), 4, "one badge per place: {plan:?}");
    // Each at its place: a panel's at its outline's top-right, the others
    // at their rect's centre (a seam's: its lines' bbox).
    let r0 = rect(&ha[0]);
    let want = [
        ([r0[2], r0[1]], 2),
        (centre(meet(r0, rect(&ha[1]))), 1),
        ([o13[0] + 10.0, o13[1] + 15.0], 1),
        ([40.0, 40.0], 1),
    ];
    for (p, n) in want {
        let b = badge_at(&plan, p).unwrap_or_else(|| panic!("a badge at {p:?}: {plan:?}"));
        assert_eq!(b.count, n);
        assert_eq!(b.tone, Tone::Problem, "the problem icon, not colour alone");
        let said = if n == 1 {
            "1 problem here".to_string()
        } else {
            format!("{n} problems here")
        };
        assert_eq!(b.description, said);
    }
    // Read to a screen reader as buttons.
    let buttons = |h: &Harness<'static, State>, label: &str| {
        h.query_all(By::new().role(Role::Button).label(label))
            .count()
    };
    assert_eq!(buttons(&h, "2 problems here"), 1);
    assert_eq!(buttons(&h, "1 problem here"), 3);
    // The mosaic badges the same places, the same way.
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        mosaic_ready(st) && !st.w.mosaic().badges().is_empty()
    });
    h.run_steps(2);
    assert_eq!(by_id(h.state().w.mosaic().badges()), plan);
    assert_eq!(buttons(&h, "2 problems here"), 1);
    assert_eq!(buttons(&h, "1 problem here"), 3);
    // A badge clicked goes to its place: the panel's bbox is framed.
    h.get(By::new().role(Role::Button).label("2 problems here"))
        .click();
    h.run_steps(3);
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    let rect0 = [r0[0].floor(), r0[1].floor(), r0[2].ceil(), r0[3].ceil()];
    assert_camera(
        h.state().w.mosaic().camera(),
        Camera::framing(rect0, frame.size()),
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn outlines_show_over_the_mosaic() {
    let (root, s, jobs) = merged("window-mosaic-outlines");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![problem_at(
            CheckCode::NoOverlap,
            CheckTarget::Panel(ha[2].clone()),
            &["Ha"],
            "Panel 2 alone",
        )],
        errors: Vec::new(),
        seams: Default::default(),
    });
    let mut h = harness(state);
    step_to(&mut h, |st| {
        mosaic_ready(st) && !st.w.mosaic().outlines().is_empty()
    });
    h.run_steps(2);
    assert!(toggle_on(&h, "Outlines"));
    // Each panel of the view at its analysed bbox, numbered as on the
    // plan; the one with a problem in the problem colour.
    let outlines = h.state().w.mosaic().outlines().to_vec();
    assert_eq!(outlines.len(), ha.len());
    for (i, (o, p)) in outlines.iter().zip(&ha).enumerate() {
        assert_eq!(o.rect, geo[p].1, "{p:?}");
        assert_eq!(o.label, (i + 1).to_string());
        let want = if i == 2 {
            palette::HA_500
        } else {
            palette::ACCENT_500
        };
        assert_eq!(o.color, want, "{p:?}");
    }
    // Painted: each number just inside its panel's top-left corner.
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    let cam = h.state().w.mosaic().camera().unwrap();
    let numbers: Vec<(String, egui::Pos2)> = ha
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let r = geo[p].1;
            let at = screen(frame, cam, (r[0], r[1])) + egui::vec2(4.0, 2.0);
            ((i + 1).to_string(), at)
        })
        .collect();
    let numbered = |h: &Harness<'static, State>, label: &str, at: egui::Pos2| {
        h.output().shapes.iter().any(|cs| match &cs.shape {
            egui::Shape::Text(t) => t.galley.text() == label && (t.pos - at).length() < 1.0,
            _ => false,
        })
    };
    for (label, at) in &numbers {
        assert!(numbered(&h, label, *at), "panel {label} numbered at {at:?}");
    }
    // Outlines off: none over the mosaic (the badge stays).
    h.get(toggle("Outlines")).click();
    h.run_steps(2);
    assert!(h.state().w.mosaic().outlines().is_empty());
    for (label, at) in &numbers {
        assert!(!numbered(&h, label, *at), "panel {label} still numbered");
    }
    assert_eq!(h.state().w.mosaic().badges().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_seams_toggle_draws_flagged_seams_in_warning() {
    let (root, s, jobs) = merged("window-seams");
    let ha = members(&s, "Ha");
    let geo = analysed(&s, "Ha");
    let id = |p: &PathBuf| geo[p].0 as u16;
    let line = |a: &PathBuf, b: &PathBuf| {
        let o = meet(geo[a].1, geo[b].1);
        SeamLine {
            a: id(a).min(id(b)),
            b: id(a).max(id(b)),
            points: vec![[o[0] + 4.0, o[1] + 4.0], [o[2] - 4.0, o[1] + 4.0]],
            flagged: true,
        }
    };
    // The seam of panels 0 and 1 shows after the merge (a problem); that of
    // 2 and 3 only disagrees (a note: no warning); OIII's are not shown.
    let shows = problem_at(
        CheckCode::ResidualSeam,
        seam("Ha", &ha[0], &ha[1]),
        &["Ha"],
        "A seam shows",
    );
    let oiii = members(&s, "OIII");
    let report = ChecksReport {
        checks: vec![
            shows.clone(),
            at(
                CheckCode::LevelMatching,
                Severity::Note,
                seam("Ha", &ha[2], &ha[3]),
                &["Ha"],
                "A seam is likely",
            ),
        ],
        errors: Vec::new(),
        seams: BTreeMap::from([
            (
                "Ha".to_string(),
                vec![line(&ha[0], &ha[1]), line(&ha[2], &ha[3])],
            ),
            (
                "OIII".to_string(),
                vec![SeamLine {
                    a: 0,
                    b: 1,
                    points: vec![[1.0, 1.0], [2.0, 2.0]],
                    flagged: false,
                }],
            ),
        ]),
    };
    assert!(!oiii.is_empty());
    let mut state = State::new(s, jobs);
    state.checks.set_report(report);
    let mut h = harness(state);
    step_to(&mut h, mosaic_ready);
    h.run_steps(2);
    // Off by default: no seam drawn.
    assert!(!toggle_on(&h, "Seams"));
    assert!(h.state().w.mosaic().lines().is_empty());
    let warning = Tone::Warning.color();
    let neutral = Tone::Neutral.color();
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    assert!(!stroked(&h, frame, warning, LINE_WIDTH_STRONG));
    // On: the group's seams, the one showing in the warning colour and
    // thicker, the other neutral.
    h.get(toggle("Seams")).click();
    h.run_steps(2);
    assert!(toggle_on(&h, "Seams"));
    let lines = h.state().w.mosaic().lines().to_vec();
    assert_eq!(lines.len(), 2, "{lines:?}");
    let flagged = line(&ha[0], &ha[1]).points;
    for l in &lines {
        let (color, width) = if l.points == flagged {
            (warning, LINE_WIDTH_STRONG)
        } else {
            (neutral, LINE_WIDTH)
        };
        assert_eq!((l.color, l.width), (color, width), "{:?}", l.points);
    }
    assert!(stroked(&h, frame, warning, LINE_WIDTH_STRONG));
    assert!(stroked(&h, frame, neutral, LINE_WIDTH));
    // The seam showing has its badge.
    let o = meet(geo[&ha[0]].1, geo[&ha[1]].1);
    let mid = [(o[0] + 4.0 + o[2] - 4.0) / 2.0, o[1] + 4.0];
    assert!(badge_at(h.state().w.mosaic().badges(), mid).is_some());
    // The plan draws them the same.
    h.get_by_label("Plan").click();
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    assert_eq!(h.state().w.plan().lines(), lines.as_slice());
    let (plan_frame, _) = h.state().w.plan().canvas_frame().unwrap();
    assert!(stroked(&h, plan_frame, warning, LINE_WIDTH_STRONG));
    // Accepted, the seam goes neutral like any accepted problem (and loses
    // its badge).
    h.state_mut()
        .session
        .mosaic
        .config
        .accepted_checks
        .extend(shows.keys());
    h.run_steps(2);
    assert!(
        h.state()
            .w
            .plan()
            .lines()
            .iter()
            .all(|l| l.color == neutral && l.width == LINE_WIDTH)
    );
    assert!(h.state().w.plan().badges().is_empty());
    assert!(!stroked(&h, plan_frame, warning, LINE_WIDTH_STRONG));
    // While the plan draws in another frame than the analysed one the
    // seams are in, it draws none.
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    assert!(h.state().w.plan().lines().is_empty());
    assert!(!stroked(&h, plan_frame, neutral, LINE_WIDTH));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn shift_s_toggles_seams() {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let (root, s, jobs) = merged("window-shift-s");
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    let strong = |h: &Harness<'static, State>| {
        DisplayStretch::load(&h.state().session.mosaic.config.ui).strong
    };
    assert!(!toggle_on(&h, "Seams"));
    h.key_press_modifiers(Modifiers::SHIFT, Key::S);
    h.run_steps(2);
    assert!(toggle_on(&h, "Seams"));
    assert!(!strong(&h), "Shift+S is not Strong");
    h.key_press_modifiers(Modifiers::SHIFT, Key::S);
    h.run_steps(2);
    assert!(!toggle_on(&h, "Seams"));
    // S alone is Strong.
    h.key_press(Key::S);
    h.run_steps(2);
    assert!(!toggle_on(&h, "Seams"));
    assert!(strong(&h));
    // The toggle's tooltip names the shortcut.
    let keys = h
        .ctx
        .format_shortcut(&KeyboardShortcut::new(Modifiers::SHIFT, Key::S));
    let at = h.get(toggle("Seams")).rect().center();
    let want = format!("Show where the merge changes from one panel to the next ({keys})");
    assert!(tooltip_shows(&mut h, at, &want), "{want}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn chips_show_problem_counts() {
    let (root, mut s, jobs) = merged("window-chip-counts");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[Composition {
            id: 1,
            name: "Both".into(),
            channels: [Some(("Ha".into(), 1.0)), Some(("OIII".into(), 1.0)), None],
        }],
    );
    let ha = members(&s, "Ha");
    let oiii = members(&s, "OIII");
    let accepted = problem_at(
        CheckCode::NoOverlap,
        CheckTarget::Panel(oiii[1].clone()),
        &["OIII"],
        "Accepted",
    );
    s.mosaic.config.accepted_checks.extend(accepted.keys());
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![
            // Ha: a panel problem that names no group (its panel's group
            // counts it), and a group-wide one.
            problem_at(
                CheckCode::FileChanged,
                CheckTarget::Panel(ha[0].clone()),
                &[],
                "Changed",
            ),
            problem_at(
                CheckCode::MixedChannels,
                CheckTarget::Group("Ha".into()),
                &["Ha"],
                "Mixed",
            ),
            // OIII: one problem; an accepted one and a note do not count.
            problem_at(
                CheckCode::ThinOverlap,
                CheckTarget::Overlap(oiii[0].clone(), oiii[1].clone()),
                &["OIII"],
                "Thin",
            ),
            accepted,
            at(
                CheckCode::EmptyArea,
                Severity::Note,
                CheckTarget::Panel(oiii[2].clone()),
                &["OIII"],
                "Empty",
            ),
        ],
        errors: Vec::new(),
        seams: Default::default(),
    });
    let mut h = harness(state);
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(3);
    assert_eq!(chip_state(&h, "Ha · 4").as_deref(), Some("2 problems"));
    assert_eq!(chip_state(&h, "OIII · 4").as_deref(), Some("1 problem"));
    // A composite: the sum over its members.
    assert_eq!(chip_state(&h, "Both").as_deref(), Some("3 problems"));
    // Accepting one counts at once.
    let changed = h.state().checks.report().unwrap().checks[0].clone();
    h.state_mut()
        .session
        .mosaic
        .config
        .accepted_checks
        .extend(changed.keys());
    h.run_steps(2);
    assert_eq!(chip_state(&h, "Ha · 4").as_deref(), Some("1 problem"));
    assert_eq!(chip_state(&h, "Both").as_deref(), Some("2 problems"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_seams_toggle_is_remembered() {
    let (root, s, jobs) = merged("window-seams-kept");
    let dir = s.mosaic.dir.clone();
    let mut h = harness(State::new(s, jobs));
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    h.get(toggle("Seams")).click();
    h.run_steps(2);
    assert!(toggle_on(&h, "Seams"));
    flush(&mut h);
    drop(h);
    let mut h = reopen(&dir);
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    assert!(toggle_on(&h, "Seams"), "on, as left");
    h.get(toggle("Seams")).click();
    h.run_steps(2);
    flush(&mut h);
    drop(h);
    let mut h = reopen(&dir);
    step_until(&mut h, |st| st.w.view().is_some());
    h.run_steps(2);
    assert!(!toggle_on(&h, "Seams"), "off, as left");
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}
