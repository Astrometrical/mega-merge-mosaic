use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mmm_core::checks::{Check, CheckCode, Severity};
use mmm_core::mosaic::MosaicConfig;
use mmm_core::preview::Preview;

use super::*;
use crate::checks::ChecksReport;
use crate::display_stretch::DisplayStretch;
use crate::jobs::tests::analyzed;
use crate::plan_view::tests::{panels, previews_of};
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
    h.state().tab.plan.to_screen(c).expect("drawn")
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
    step_until(&mut h, |s| s.tab.plan.has_info(&ha[0]));
    h.get_by_label("600 × 420 (0.3 MP)");
    h.get_by_label("None (registered)");
    // New inputs (a generation): the header is read again.
    h.state_mut().generation = 2;
    h.step();
    assert!(!h.state().tab.plan.has_info(&ha[0]));
    step_until(&mut h, |s| s.tab.plan.has_info(&ha[0]));
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
fn stretch_toggles_save_the_display_stretch() {
    let (root, s, _r) = analyzed("plan-stretch-toggles");
    let ha = panels(&s, "Ha");
    let previews = previews_of(&s, &ha);
    let mut h = harness(s);
    h.state_mut().previews = previews;
    h.state_mut().complete = true;
    step_until(&mut h, |s| s.tab.is_settled());
    let normal = h.state().tab.stretch().map(<[_]>::to_vec);
    h.get_by_label("Strong").click();
    h.run();
    let want = DisplayStretch {
        linked: false,
        strong: true,
    };
    assert_eq!(
        DisplayStretch::load(&h.state().session.mosaic.config.ui),
        want
    );
    // The canvas follows.
    step_until(&mut h, |s| {
        s.tab.is_settled() && s.tab.stretch().map(<[_]>::to_vec) != normal
    });
    // Saved, rate-limited.
    let dir = h.state().session.mosaic.dir.clone();
    step_until(&mut h, |_| {
        MosaicConfig::load(&dir).is_ok_and(|c| DisplayStretch::load(&c.ui) == want)
    });
    h.get_by_label("Linked").click();
    h.run();
    assert!(DisplayStretch::load(&h.state().session.mosaic.config.ui).linked);
    let _ = std::fs::remove_dir_all(&root);
}
