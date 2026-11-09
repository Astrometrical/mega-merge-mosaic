//! Problems badged where they are, panel outlines and seam lines on both
//! canvases, the Seams toggle, and the chips' problem counts (spec §7.2–§7.4).

use std::collections::BTreeMap;

use egui::Color32;
use egui::epaint::ColorMode;
use mega_ui::canvas_layers::{CanvasBadge, LABEL_OFFSET};
use mega_ui::theme::tokens::{LINE_WIDTH, LINE_WIDTH_STRONG};
use mega_ui::theme::{Tone, palette};
use mmm_core::checks::CheckTarget;
use mmm_core::health::SeamLine;
use mmm_core::session::Session;

use super::*;

/// The panels of `group` in `s`, in configuration order.
pub(super) fn members(s: &OpenSession, group: &str) -> Vec<PathBuf> {
    group_members(s, &[group.to_string()])
        .remove(group)
        .expect("the group")
}

/// Each panel of `group`'s id and content bbox in the group's analysis
/// (read from its `session.json`, not through the window).
pub(super) fn analysed(s: &OpenSession, group: &str) -> BTreeMap<PathBuf, (usize, [f64; 4])> {
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
pub(super) fn meet(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

/// A rect's centre.
pub(super) fn centre(r: [f64; 4]) -> [f64; 2] {
    [(r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0]
}

/// A check of `severity` at `target`.
pub(super) fn at(
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
pub(super) fn problem_at(
    code: CheckCode,
    target: CheckTarget,
    groups: &[&str],
    message: &str,
) -> Check {
    at(code, Severity::Problem, target, groups, message)
}

/// The seam of group `group` between `a` and `b`.
pub(super) fn seam(group: &str, a: &std::path::Path, b: &std::path::Path) -> CheckTarget {
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

/// Canvas points rounded to a hundredth of a pixel, sorted: a set of badge
/// places to compare.
fn spots(points: impl IntoIterator<Item = [f64; 2]>) -> Vec<[i64; 2]> {
    let mut v: Vec<[i64; 2]> = points
        .into_iter()
        .map(|p| p.map(|c| (c * 100.0).round() as i64))
        .collect();
    v.sort();
    v
}

/// The badge button drawn at screen point `at` (its pill's centre).
pub(super) fn badge_button<'h>(
    h: &'h Harness<'static, State>,
    at: egui::Pos2,
) -> egui_kittest::Node<'h> {
    h.query_all(By::new().role(Role::Button))
        .find(|n| {
            // "{n} problem(s) at …" or "… in a coverage gap".
            let said = n.accesskit_node().label().unwrap_or_default();
            let mut words = said.split(' ');
            let counted = words.next().is_some_and(|w| w.parse::<u32>().is_ok())
                && words.next().is_some_and(|w| w.starts_with("problem"));
            counted && n.rect().expand(1.0).contains(at)
        })
        .unwrap_or_else(|| panic!("a badge at {at:?}"))
}

/// A canvas rect framed as [`target_rect`](crate::marks::target_rect)
/// rounds it: whole pixels, covering it.
pub(super) fn covering(r: [f64; 4]) -> [f64; 4] {
    [r[0].floor(), r[1].floor(), r[2].ceil(), r[3].ceil()]
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
pub(super) fn plan_ready(st: &State) -> bool {
    plan_placed(st) && !st.w.plan().layout().rects.is_empty()
}

/// The mosaic is drawn, with the layout in.
pub(super) fn mosaic_ready(st: &State) -> bool {
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
        // A thin overlap and a seam of the same pair: one badge, "2".
        problem_at(
            CheckCode::ThinOverlap,
            CheckTarget::Overlap(ha[1].clone(), ha[0].clone()),
            &["Ha"],
            "Thin",
        ),
        problem_at(
            CheckCode::ResidualSeam,
            seam("Ha", &ha[0], &ha[1]),
            &["Ha"],
            "A seam shows between 0 and 1",
        ),
        // A seam alone: badged at its overlap, framed at its lines.
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
    // Each at its place: a panel's at its outline's top-right, a pair's
    // (its overlap's and its seam's problems) at the overlap's centre, an
    // area's at its centre.
    let r0 = rect(&ha[0]);
    // Each says what and where: the place in its name, its problems'
    // titles on hover (one per line) and as its accessible description.
    let name = |p: &PathBuf| crate::panel_list::file_name(p);
    let pair = |a: &PathBuf, b: &PathBuf| {
        let (lo, hi) = (a.min(b), a.max(b));
        format!("the overlap of {} and {}", name(lo), name(hi))
    };
    let want = [
        (
            [r0[2], r0[1]],
            2,
            format!("2 problems at {}", name(&ha[0])),
            "Panel 0 changed\nPanel 0 alone",
        ),
        (
            centre(meet(r0, rect(&ha[1]))),
            2,
            format!("2 problems at {}", pair(&ha[0], &ha[1])),
            "Thin\nA seam shows between 0 and 1",
        ),
        (
            centre(o13),
            1,
            format!("1 problem at {}", pair(&ha[1], &ha[3])),
            "A seam shows",
        ),
        (
            [40.0, 40.0],
            1,
            "1 problem in a coverage gap".to_string(),
            "A hole",
        ),
    ];
    for (p, n, said, titles) in &want {
        let b = badge_at(&plan, *p).unwrap_or_else(|| panic!("a badge at {p:?}: {plan:?}"));
        assert_eq!(b.count, *n);
        assert_eq!(b.tone, Tone::Problem, "the problem icon, not colour alone");
        assert_eq!(&b.description, said);
        assert_eq!(b.hover.as_deref(), Some(*titles));
    }
    // Read to a screen reader as buttons: named for what and where,
    // described by their problems.
    let read = |h: &Harness<'static, State>| {
        for (_, _, said, titles) in &want {
            let found: Vec<Option<String>> = h
                .query_all(By::new().role(Role::Button).label(said.as_str()))
                .map(|n| n.accesskit_node().description())
                .collect();
            assert_eq!(found, [Some(titles.to_string())], "{said}");
        }
    };
    read(&h);
    // A badge clicked on the plan goes to its place: the lone seam's
    // badge frames its lines.
    let at = h
        .state()
        .w
        .plan()
        .to_screen((centre(o13)[0], centre(o13)[1]));
    badge_button(&h, at.expect("the plan drawn")).click();
    h.run_steps(3);
    let (plan_frame, _) = h.state().w.plan().canvas_frame().unwrap();
    let lines_bbox = [line[0][0], line[0][1], line[1][0], line[1][1]];
    assert_camera(
        h.state().w.plan().camera(),
        Camera::framing(covering(lines_bbox), plan_frame.size()),
    );
    // The mosaic badges the same places, the same way (the whole canvas
    // in view again).
    h.get_by_label("Mosaic").click();
    step_to(&mut h, |st| {
        mosaic_ready(st) && !st.w.mosaic().badges().is_empty()
    });
    h.state_mut().w.frame([0, 0, 600, 420]);
    h.run_steps(3);
    assert_eq!(by_id(h.state().w.mosaic().badges()), plan);
    read(&h);
    // A badge clicked on the mosaic goes to its place too: the panel's
    // bbox is framed.
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    let cam = h.state().w.mosaic().camera().unwrap();
    badge_button(&h, screen(frame, cam, (r0[2], r0[1]))).click();
    h.run_steps(3);
    assert_camera(
        h.state().w.mosaic().camera(),
        Camera::framing(covering(r0), frame.size()),
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn outlines_show_over_the_mosaic() {
    let (root, mut s, jobs) = merged("window-mosaic-outlines");
    add_roi(&mut s);
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
            let at = screen(frame, cam, (r[0], r[1])) + LABEL_OFFSET;
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
    // Under the ROIs: the panel's outline (a stroked rect) is painted
    // before the ROI's (dashed: not selected).
    let on_screen = |r: [f64; 4]| {
        egui::Rect::from_two_pos(
            screen(frame, cam, (r[0], r[1])),
            screen(frame, cam, (r[2], r[3])),
        )
    };
    let panel0 = on_screen(geo[&ha[0]].1);
    let outline = h.output().shapes.iter().position(|cs| match &cs.shape {
        egui::Shape::Rect(s) => {
            s.stroke.color == palette::ACCENT_500
                && (s.rect.min - panel0.min).length() < 0.5
                && (s.rect.max - panel0.max).length() < 0.5
        }
        _ => false,
    });
    let roi = on_screen(h.state().w.mosaic().rois()[0].canvas.map(|v| v as f64)).expand(1.0);
    let roi_color = h.state().w.mosaic().roi_color(1);
    let region = h.output().shapes.iter().position(|cs| match &cs.shape {
        egui::Shape::LineSegment { points, stroke } => {
            stroke.color == roi_color && points.iter().all(|p| roi.contains(*p))
        }
        _ => false,
    });
    let (outline, region) = (outline.expect("outline"), region.expect("ROI dashes"));
    assert!(outline < region, "outline {outline}, ROI {region}");
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
    // 2 and 3 only disagrees (a note), and the pair's overlap is thin (a
    // problem badged with the pair, not the seam's own: no warning); OIII's
    // are not shown. A panel and an area have problems too.
    let shows = problem_at(
        CheckCode::ResidualSeam,
        seam("Ha", &ha[0], &ha[1]),
        &["Ha"],
        "A seam shows",
    );
    let mut hole = problem_at(
        CheckCode::CoverageHole,
        CheckTarget::Group("Ha".into()),
        &["Ha"],
        "A hole",
    );
    hole.rect = Some([20, 30, 60, 50]);
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
            problem_at(
                CheckCode::ThinOverlap,
                CheckTarget::Overlap(ha[3].clone(), ha[2].clone()),
                &["Ha"],
                "Thin",
            ),
            problem_at(
                CheckCode::NoOverlap,
                CheckTarget::Panel(ha[2].clone()),
                &["Ha"],
                "Panel 2 alone",
            ),
            hole,
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
    // The seam showing has its badge, its pair's, at the overlap.
    let r = |i: usize| geo[&ha[i]].1;
    let pair01 = centre(meet(r(0), r(1)));
    let pair23 = centre(meet(r(2), r(3)));
    let panel2 = [r(2)[2], r(2)[1]];
    let area = [40.0, 40.0];
    let places = |badges: &[CanvasBadge]| spots(badges.iter().map(|b| b.at));
    let sorted = |ps: &[[f64; 2]]| spots(ps.iter().copied());
    assert_eq!(
        places(h.state().w.mosaic().badges()),
        sorted(&[pair01, pair23, panel2, area])
    );
    // The plan draws them the same.
    h.get_by_label("Plan").click();
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    assert_eq!(h.state().w.plan().lines(), lines.as_slice());
    let (plan_frame, _) = h.state().w.plan().canvas_frame().unwrap();
    assert!(stroked(&h, plan_frame, warning, LINE_WIDTH_STRONG));
    assert_eq!(
        places(h.state().w.plan().badges()),
        sorted(&[pair01, pair23, panel2, area])
    );
    // Accepted, the seam goes neutral like any accepted problem (and its
    // pair loses its badge).
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
    assert_eq!(
        places(h.state().w.plan().badges()),
        sorted(&[pair23, panel2, area])
    );
    assert!(!stroked(&h, plan_frame, warning, LINE_WIDTH_STRONG));
    // While the plan draws in another frame than the analysed one the
    // seams are in, it draws none, and badges only its panels (placed by
    // its own outlines), not the pair or the area.
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    assert!(h.state().w.plan().lines().is_empty());
    assert!(!stroked(&h, plan_frame, neutral, LINE_WIDTH));
    assert_eq!(places(h.state().w.plan().badges()), sorted(&[panel2]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composite_gathers_its_members_badges_and_seams() {
    let (root, mut s, jobs) = merged("window-composite-marks");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[Composition {
            id: 1,
            name: "Both".into(),
            channels: [Some(("Ha".into(), 1.0)), Some(("OIII".into(), 1.0)), None],
        }],
    );
    let (ha, oiii) = (members(&s, "Ha"), members(&s, "OIII"));
    let (gh, go) = (analysed(&s, "Ha"), analysed(&s, "OIII"));
    let line = |geo: &BTreeMap<PathBuf, (usize, [f64; 4])>, a: &PathBuf, b: &PathBuf| {
        let o = meet(geo[a].1, geo[b].1);
        let (x, y) = (geo[a].0 as u16, geo[b].0 as u16);
        SeamLine {
            a: x.min(y),
            b: x.max(y),
            points: vec![[o[0] + 2.0, o[1] + 2.0], [o[0] + 2.0, o[3] - 2.0]],
            flagged: false,
        }
    };
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![
            problem_at(
                CheckCode::NoOverlap,
                CheckTarget::Panel(ha[0].clone()),
                &["Ha"],
                "Ha alone",
            ),
            problem_at(
                CheckCode::NoOverlap,
                CheckTarget::Panel(oiii[1].clone()),
                &["OIII"],
                "OIII alone",
            ),
        ],
        errors: Vec::new(),
        seams: BTreeMap::from([
            ("Ha".to_string(), vec![line(&gh, &ha[0], &ha[1])]),
            ("OIII".to_string(), vec![line(&go, &oiii[2], &oiii[3])]),
        ]),
    });
    state.w.select_view(ViewKind::Composition(1));
    let mut h = harness(state);
    step_to(&mut h, |st| {
        mosaic_ready(st) && !st.w.mosaic().badges().is_empty()
    });
    h.get(toggle("Seams")).click();
    h.run_steps(2);
    assert_eq!(h.state().w.view(), Some(&ViewKind::Composition(1)));
    // Both members' problems and seams, and all their panels' outlines.
    let want = spots([
        [gh[&ha[0]].1[2], gh[&ha[0]].1[1]],
        [go[&oiii[1]].1[2], go[&oiii[1]].1[1]],
    ]);
    let points = |lines: &[mega_ui::canvas_layers::Polyline]| {
        let mut v: Vec<Vec<[f64; 2]>> = lines.iter().map(|l| l.points.clone()).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    let mut seams = vec![
        line(&gh, &ha[0], &ha[1]).points,
        line(&go, &oiii[2], &oiii[3]).points,
    ];
    seams.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mosaic = h.state().w.mosaic();
    assert_eq!(spots(mosaic.badges().iter().map(|b| b.at)), want);
    assert_eq!(points(mosaic.lines()), seams);
    let labels: Vec<String> = mosaic.outlines().iter().map(|o| o.label.clone()).collect();
    assert_eq!(labels, (1..=8).map(|i| i.to_string()).collect::<Vec<_>>());
    // The plan shows the composite the same way.
    h.get_by_label("Plan").click();
    step_to(&mut h, plan_ready);
    h.run_steps(2);
    let plan = h.state().w.plan();
    assert_eq!(spots(plan.badges().iter().map(|b| b.at)), want);
    assert_eq!(points(plan.lines()), seams);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_composites_coincident_problems_share_one_badge() {
    let (root, mut s, jobs) = merged("window-composite-coincident");
    save_compositions(
        &mut s.mosaic.config.ui,
        &[Composition {
            id: 1,
            name: "Both".into(),
            channels: [Some(("Ha".into(), 1.0)), Some(("OIII".into(), 1.0)), None],
        }],
    );
    let (ha, oiii) = (members(&s, "Ha"), members(&s, "OIII"));
    let (gh, go) = (analysed(&s, "Ha"), analysed(&s, "OIII"));
    // The groups share their pointings.
    for (a, b) in ha.iter().zip(&oiii) {
        assert_eq!(gh[a].1, go[b].1, "{a:?} and {b:?}");
    }
    let thin = |panels: &[PathBuf], group: &str| {
        problem_at(
            CheckCode::ThinOverlap,
            CheckTarget::Overlap(panels[0].clone(), panels[1].clone()),
            &[group],
            &format!("Thin in {group}"),
        )
    };
    let mut state = State::new(s, jobs);
    state.checks.set_report(ChecksReport {
        checks: vec![
            thin(&ha, "Ha"),
            thin(&oiii, "OIII"),
            // A panel's problem in one group, a note on its twin in the
            // other.
            problem_at(
                CheckCode::NoOverlap,
                CheckTarget::Panel(ha[2].clone()),
                &["Ha"],
                "Ha 2 alone",
            ),
            at(
                CheckCode::EmptyArea,
                Severity::Note,
                CheckTarget::Panel(oiii[2].clone()),
                &["OIII"],
                "OIII 2 is half empty",
            ),
        ],
        errors: Vec::new(),
        seams: Default::default(),
    });
    state.w.select_view(ViewKind::Composition(1));
    let mut h = harness(state);
    step_to(&mut h, |st| {
        mosaic_ready(st) && !st.w.mosaic().badges().is_empty()
    });
    h.run_steps(2);
    // One badge where the two thin overlaps coincide, counting both; the
    // chip counts them too.
    let o01 = meet(gh[&ha[0]].1, gh[&ha[1]].1);
    let badges = h.state().w.mosaic().badges().to_vec();
    assert_eq!(badges.len(), 2, "{badges:?}");
    let b = badge_at(&badges, centre(o01)).expect("the pair's badge");
    assert_eq!(b.count, 2);
    assert_eq!(chip_state(&h, "Both").as_deref(), Some("3 problems"));
    // Opened, the pair lists both groups' checks there.
    h.state_mut().w.frame([0, 0, 600, 420]);
    h.run_steps(3);
    let (frame, _) = h.state().w.mosaic().viewer_frame().unwrap();
    let cam = h.state().w.mosaic().camera().unwrap();
    let c = centre(o01);
    badge_button(&h, screen(frame, cam, (c[0], c[1]))).click();
    h.run_steps(3);
    for title in ["Thin in Ha", "Thin in OIII"] {
        assert_eq!(
            h.query_all_by_role_and_label(Role::Button, title).count(),
            2,
            "{title} in Problems and the pair's checks"
        );
    }
    // A panel lists its twin's checks too (the twin's note included).
    h.state_mut().w.frame([0, 0, 600, 420]);
    h.run_steps(3);
    let r2 = gh[&ha[2]].1;
    let cam = h.state().w.mosaic().camera().unwrap();
    badge_button(&h, screen(frame, cam, (r2[2], r2[1]))).click();
    h.run_steps(3);
    assert_eq!(
        h.state().w.selection(),
        Some(&Selected::Panel(ha[2].clone()))
    );
    h.get_by_label("Checks for this panel");
    assert_eq!(h.query_all_by_label("Ha 2 alone").count(), 2);
    h.get_by_label("OIII 2 is half empty");
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
