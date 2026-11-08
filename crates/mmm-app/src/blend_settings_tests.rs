use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mmm_core::mosaic::{set_group_override, set_settings};
use mmm_core::settings::{BlendOverride, GroupOverride};
use mmm_core::tiles::TileStore;

use super::*;
use crate::jobs::tests::{analyzed, drain};
use crate::start::tests::step_until;

struct State {
    panel: SettingsPanel,
    session: OpenSession,
    jobs: JobRunner,
    applied: Vec<ChangeImpact>,
}

/// The panel over an analysed two-group session, polled and drawn each
/// frame as the merged mosaic view does.
fn harness(tag: &str) -> (PathBuf, Harness<'static, State>) {
    let (root, session, jobs) = analyzed(tag);
    let h = Harness::builder()
        .with_size(egui::vec2(320.0, 700.0))
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, st: &mut State| {
                st.jobs.poll(&mut st.session);
                let ctx = ui.ctx().clone();
                if let Some(i) = st.panel.poll(&ctx, &mut st.session, &mut st.jobs) {
                    st.applied.push(i);
                }
                if let Some(i) = st.panel.ui(ui, &mut st.session, &mut st.jobs) {
                    st.applied.push(i);
                }
            },
            State {
                panel: SettingsPanel::new(),
                session,
                jobs,
                applied: Vec::new(),
            },
        );
    (root, h)
}

fn value(h: &Harness<'static, State>, role: Role, label: &str) -> String {
    h.get_by_role_and_label(role, label)
        .accesskit_node()
        .value()
        .unwrap_or_default()
}

fn type_feather(h: &mut Harness<'static, State>, text: &str) {
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .type_text(text);
    h.step();
}

fn valid0(path: &std::path::Path) -> usize {
    TileStore::open(path).unwrap().valid_count(0)
}

#[test]
fn panel_shows_current_values_and_override_note() {
    let (root, mut h) = harness("settings-shows");
    {
        let st = h.state_mut();
        let m = &mut st.session.mosaic;
        let mut a = m.config.analyze.clone();
        a.gain = GainMode::Unity;
        let b = BlendSettings {
            mode: BlendMode::TwoBand,
            feather_px: 100.0,
            flatten: Some(1),
            ..m.config.blend.clone()
        };
        set_settings(m, a, b).unwrap();
        let o = GroupOverride {
            blend: BlendOverride {
                feather_px: Some(64.0),
                ..Default::default()
            },
            ..Default::default()
        };
        set_group_override(m, "Ha", o).unwrap();
    }
    h.run_steps(2);
    h.get_by_label("Merge settings");
    assert_eq!(value(&h, Role::ComboBox, "Mode"), "TwoBand");
    assert_eq!(value(&h, Role::TextInput, "Feather (px)"), "100");
    assert_eq!(value(&h, Role::ComboBox, "Flatten"), "1 (plane)");
    assert_eq!(value(&h, Role::ComboBox, "Input"), "Auto");
    assert_eq!(value(&h, Role::ComboBox, "Surface"), "2 (quadratic)");
    assert_eq!(value(&h, Role::ComboBox, "Gain"), "Unity");
    h.get_by_label("Ha has its own overrides (CLI)");
    assert!(
        h.query_by_label("OIII has its own overrides (CLI)")
            .is_none()
    );
    // Both kinds are captioned.
    h.get_by_label("MERGE · re-merge only");
    h.get_by_label("ANALYSIS · reruns analysis, every tile goes stale");
    // Nothing staged: no Apply.
    assert!(h.query_by_label("Apply").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn staging_shows_cost_and_revert_discards() {
    let (root, mut h) = harness("settings-stage");
    // Something blended, so there are tiles to count.
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        drain(&mut st.jobs, &mut st.session);
    }
    h.run_steps(2);
    type_feather(&mut h, "0"); // 256 → 2560
    assert_eq!(
        h.state().panel.draft().map(|d| d.blend.feather_px),
        Some(2560.0)
    );
    // Not applied: the session keeps its settings.
    assert_eq!(h.state().session.mosaic.config.blend.feather_px, 256.0);
    // The cost, counted on a worker: Ha's 6 level-0 tiles are valid.
    step_until(&mut h, |st| st.panel.counts().is_some() && st.panel.idle());
    h.run_steps(1);
    // Only Ha has a result store (OIII was never blended).
    assert_eq!(h.state().panel.counts(), Some((6, 6)));
    h.get_by_label_contains(
        "Existing results go stale; merge again to see the change (6 of 6 tiles)",
    );
    h.get_by_label("Revert").click();
    h.run_steps(2);
    assert!(h.state().panel.draft().is_none());
    assert_eq!(value(&h, Role::TextInput, "Feather (px)"), "256");
    assert!(h.query_by_label("Apply").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invalid_feather_is_rejected() {
    let (root, mut h) = harness("settings-bad-feather");
    h.run_steps(2);
    type_feather(&mut h, "x");
    h.get_by_label("Feather must be a number of pixels greater than 0.");
    assert!(h.state().panel.feather_error().is_some());
    assert!(h.state().panel.draft().is_none(), "nothing staged");
    // Zero (and NaN) parse but the core refuses them.
    {
        let st = h.state_mut();
        st.panel.revert();
    }
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .focus();
    h.step();
    for _ in 0..3 {
        h.key_press(egui::Key::Backspace);
        h.step();
    }
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .type_text("0");
    h.step();
    h.run_steps(1);
    let err = h
        .state()
        .panel
        .feather_error()
        .unwrap_or_default()
        .to_string();
    assert!(err.starts_with("Feather must be"), "{err}");
    // NaN parses as a float too; the core refuses it.
    h.key_press(egui::Key::Backspace);
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .type_text("NaN");
    h.run_steps(2);
    assert_eq!(value(&h, Role::TextInput, "Feather (px)"), "NaN");
    let err = h
        .state()
        .panel
        .feather_error()
        .unwrap_or_default()
        .to_string();
    assert!(err.contains("finite"), "{err}");
    // The refused value is never staged (the draft keeps the last valid one).
    let draft = h.state().panel.draft().map(|d| d.blend.feather_px);
    assert!(draft.is_none_or(f32::is_finite), "{draft:?}");
    // Another change is staged, but Apply stays disabled.
    {
        let st = h.state_mut();
        let mut e = SettingsEdit::of(&st.session.mosaic.config);
        e.blend.defect_veto = false;
        st.panel.stage(e, &st.session.mosaic.config);
    }
    h.run_steps(2);
    assert!(h.get_by_label("Apply").accesskit_node().is_disabled());
    {
        let st = h.state_mut();
        assert_eq!(st.panel.apply(&mut st.session, &mut st.jobs), None);
    }
    assert_eq!(h.state().session.mosaic.config.blend.feather_px, 256.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn blend_local_apply_stales_tiles_without_reanalysis() {
    let (root, mut h) = harness("settings-reblend");
    let ha = {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        drain(&mut st.jobs, &mut st.session);
        st.session.mosaic.result_path("Ha")
    };
    assert_eq!(valid0(&ha), 6);
    h.run_steps(2);
    h.get_by_role_and_label(Role::ComboBox, "Defect veto")
        .click();
    h.run_steps(2);
    h.get_by_label("Off").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    h.run_steps(2);
    let st = h.state();
    assert_eq!(st.applied, [ChangeImpact::Reblend]);
    assert!(!st.session.mosaic.config.blend.defect_veto);
    assert!(st.session.mosaic.config.needs_analyze.is_empty());
    assert_eq!(valid0(&ha), 0, "every tile stale");
    assert!(st.panel.draft().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn global_apply_marks_needs_analyze() {
    let (root, mut h) = harness("settings-reanalyze");
    h.run_steps(2);
    h.get_by_role_and_label(Role::ComboBox, "Gain").click();
    h.run_steps(2);
    h.get_by_label("Unity").click();
    h.run_steps(2);
    h.get_by_label_contains("Analysis reruns and every result goes stale");
    h.get_by_label("Apply").click();
    h.run_steps(2);
    let cfg = &h.state().session.mosaic.config;
    assert_eq!(h.state().applied, [ChangeImpact::Reanalyze]);
    assert_eq!(cfg.analyze.gain, GainMode::Unity);
    assert_eq!(
        cfg.needs_analyze.iter().cloned().collect::<Vec<_>>(),
        ["Ha", "OIII"]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn apply_during_a_blend_cancels_it_and_applies_when_idle() {
    let (root, mut h) = harness("settings-pending");
    h.run_steps(2);
    let gate = {
        let st = h.state_mut();
        let gate = st.jobs.hold();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        st.jobs.start_blend(&mut st.session, "OIII", None);
        gate
    };
    type_feather(&mut h, "0");
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert!(h.state().panel.has_pending());
    assert!(h.state().applied.is_empty(), "not while the blend runs");
    assert_eq!(h.state().session.mosaic.config.blend.feather_px, 256.0);
    h.get_by_label_contains("Waiting for the running job to stop");
    gate.open();
    step_until(&mut h, |st| !st.panel.has_pending());
    let st = h.state();
    assert_eq!(st.applied, [ChangeImpact::Reblend]);
    assert_eq!(st.session.mosaic.config.blend.feather_px, 2560.0);
    assert!(!st.jobs.busy());
    // The blends were cancelled: none recorded.
    assert!(
        !st.session
            .mosaic
            .config
            .provenance
            .iter()
            .any(|p| p.action == "blend")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_waiting_edit_applies_when_the_session_is_left() {
    let (root, mut s, mut jobs) = analyzed("settings-apply-waiting");
    let gate = jobs.hold();
    jobs.start_blend(&mut s, "Ha", None);
    let mut panel = SettingsPanel::new();
    let mut e = SettingsEdit::of(&s.mosaic.config);
    e.blend.feather_px = 100.0;
    panel.stage(e, &s.mosaic.config);
    assert_eq!(panel.apply(&mut s, &mut jobs), None);
    assert!(panel.has_pending(), "waits for the merge to stop");
    // Leaving the session: the jobs stop, then the edit applies at once.
    gate.open();
    let (_, stopped) = jobs.stop_all(&mut s, std::time::Duration::from_secs(20));
    assert!(stopped);
    assert_eq!(
        panel.apply_waiting(&mut s, &mut jobs),
        Some(ChangeImpact::Reblend)
    );
    assert!(!panel.has_pending());
    let saved = mmm_core::mosaic::MosaicConfig::load(&s.mosaic.dir).unwrap();
    assert_eq!(saved.blend.feather_px, 100.0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn blend_local_edit_spares_a_running_analysis() {
    let (root, mut s, mut jobs) = analyzed("settings-spare-analysis");
    let ctx = egui::Context::default();
    let gate = jobs.hold();
    let analysis = jobs.start_analyze(&mut s);
    let mut panel = SettingsPanel::new();
    let mut e = SettingsEdit::of(&s.mosaic.config);
    e.blend.feather_px = 100.0;
    panel.stage(e, &s.mosaic.config);
    // Blend-local: only blends are in the way, and none runs.
    assert_eq!(panel.apply(&mut s, &mut jobs), Some(ChangeImpact::Reblend));
    assert!(!panel.editing(&s.mosaic.config));
    // A blend queued behind the analysis is cancelled by the next one...
    jobs.start_blend(&mut s, "Ha", None);
    let mut e = SettingsEdit::of(&s.mosaic.config);
    e.blend.feather_px = 50.0;
    panel.stage(e, &s.mosaic.config);
    assert_eq!(panel.apply(&mut s, &mut jobs), None);
    assert!(panel.has_pending() && !panel.editing(&s.mosaic.config));
    let mut events = jobs.poll(&mut s);
    assert_eq!(
        panel.poll(&ctx, &mut s, &mut jobs),
        Some(ChangeImpact::Reblend)
    );
    assert_eq!(s.mosaic.config.blend.feather_px, 50.0);
    // ...but the analysis runs on.
    gate.open();
    events.extend(drain(&mut jobs, &mut s));
    assert!(events.contains(&JobEvent::Finished {
        id: analysis,
        label: "Analyze".into()
    }));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn changed_marker_follows_the_last_blend() {
    let (root, mut h) = harness("settings-marker");
    let marks = |h: &Harness<'static, State>| h.query_all_by_label(CHANGED_MARK).count();
    h.run_steps(2);
    // Never blended: nothing to compare with.
    assert_eq!(marks(&h), 0);
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        drain(&mut st.jobs, &mut st.session);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 0);
    // A staged change is marked, and stays marked once applied...
    {
        let st = h.state_mut();
        let mut e = SettingsEdit::of(&st.session.mosaic.config);
        e.blend.mode = BlendMode::Feather;
        st.panel.stage(e, &st.session.mosaic.config);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 1);
    assert_eq!(
        changed_fields(
            &h.state().session.mosaic.config,
            &h.state().panel.shown(&h.state().session.mosaic.config)
        ),
        [Field::Mode]
    );
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert_eq!(marks(&h), 1);
    // ...until a reblend uses it.
    {
        let st = h.state_mut();
        st.jobs
            .start_blend(&mut st.session, "Ha", Some([0, 0, 256, 256]));
        drain(&mut st.jobs, &mut st.session);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn last_blend_reads_provenance() {
    let (root, mut s, mut jobs) = analyzed("settings-last-blend");
    assert_eq!(last_blend(&s.mosaic.config), None);
    jobs.start_blend(&mut s, "OIII", Some([0, 0, 256, 256]));
    drain(&mut jobs, &mut s);
    let (a, b) = last_blend(&s.mosaic.config).unwrap();
    assert_eq!(a, Some(s.mosaic.config.analyze.clone()));
    assert_eq!(b, s.mosaic.config.blend);
    // A group with overrides does not stand for the mosaic-wide settings.
    let o = GroupOverride {
        blend: BlendOverride {
            mode: Some(BlendMode::Feather),
            ..Default::default()
        },
        ..Default::default()
    };
    set_group_override(&mut s.mosaic, "OIII", o).unwrap();
    assert_eq!(last_blend(&s.mosaic.config), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cost_text_says_what_goes_stale() {
    assert_eq!(
        cost_text(ChangeImpact::Reblend, Some((3, 12))),
        "Existing results go stale; merge again to see the change (3 of 12 tiles)."
    );
    assert_eq!(
        cost_text(ChangeImpact::Reanalyze, Some((0, 0))),
        "Analysis reruns and every result goes stale."
    );
    assert_eq!(
        cost_text(ChangeImpact::Reanalyze, Some((2, 6))),
        "Analysis reruns and every result goes stale (2 of 6 tiles)."
    );
    assert!(cost_text(ChangeImpact::Reblend, None).contains("counting"));
    assert_eq!(
        count_tiles(&[PathBuf::from("/nonexistent/result.mmmt")]),
        (0, 0)
    );
}
