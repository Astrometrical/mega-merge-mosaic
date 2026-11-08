use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mmm_core::mosaic::MosaicConfig;

use super::*;
use crate::jobs::tests::analyzed;
use crate::start::tests::step_until;

struct State {
    tab: BlendTab,
    session: OpenSession,
    jobs: JobRunner,
}

/// The tab, driven as the shell drives it.
fn harness(state: State) -> Harness<'static, State> {
    Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                for e in s.jobs.poll(&mut s.session) {
                    s.tab.on_event(&e);
                }
                s.tab.poll_settings(ui.ctx(), &mut s.session, &mut s.jobs);
                s.tab.poll(ui.ctx(), &mut s.session, &mut s.jobs);
                s.tab.ui(ui, &mut s.session, &mut s.jobs, None);
            },
            state,
        )
}

#[test]
fn toolbar_merges_and_toggles_the_stretch_without_analyze() {
    let (root, session, jobs) = analyzed("blend-tab-toolbar");
    let mut h = harness(State {
        tab: BlendTab::new(),
        session,
        jobs,
    });
    // Ha's result has been looked for: there is none yet.
    step_until(&mut h, |st| st.tab.absent().is_some());
    h.get_by_label("Not blended yet.");
    assert!(
        h.query_by_label("Analyze").is_none(),
        "analysis is automatic"
    );
    // The merge button merges every group.
    let gate = h.state_mut().jobs.hold();
    h.get_by_label("Blend whole mosaic").click();
    h.run_steps(2);
    let labels: Vec<String> = h
        .state()
        .jobs
        .views()
        .into_iter()
        .map(|v| v.label)
        .collect();
    assert_eq!(labels, ["Blend Ha", "Blend OIII"]);
    gate.open();
    step_until(&mut h, |st| !st.jobs.busy() && st.tab.source().is_some());
    // Strong is the session's display stretch, saved with it.
    h.get_by_label("Strong").click();
    h.run_steps(2);
    let st = h.state();
    assert!(DisplayStretch::load(&st.session.mosaic.config.ui).strong);
    let back = MosaicConfig::load(&st.session.mosaic.dir).unwrap();
    assert!(DisplayStretch::load(&back.ui).strong);
    let _ = std::fs::remove_dir_all(&root);
}
