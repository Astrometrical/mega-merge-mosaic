use std::path::PathBuf;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mmm_core::mosaic::{MosaicConfig, commit_blend_settings, last_analyze_settings};
use mmm_core::mosaic::{set_group_override, set_settings};
use mmm_core::settings::{BlendOverride, GroupOverride};
use mmm_core::tiles::TileStore;

use super::*;
use crate::jobs::JobEvent;
use crate::jobs::tests::{analyzed, drain};

struct State {
    panel: SettingsPanel,
    session: OpenSession,
    jobs: JobRunner,
    applied: Vec<ChangeImpact>,
    /// The groups the panel is showing (a view's members).
    groups: Vec<String>,
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
                let groups = st.groups.clone();
                if let Some(i) = st.panel.ui(ui, &mut st.session, &mut st.jobs, &groups) {
                    st.applied.push(i);
                }
            },
            State {
                panel: SettingsPanel::new(),
                session,
                jobs,
                applied: Vec::new(),
                groups: vec!["Ha".into()],
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
    // The inspector's section is the only fold: no header of its own.
    assert!(h.query_by_label("Merge settings").is_none());
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
    // Merge fields edit at once, analysis fields apply at once: no buttons.
    assert!(h.query_by_label("Apply").is_none());
    assert!(h.query_by_label("Revert").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

fn analysis(gain: GainMode, cfg: &MosaicConfig) -> AnalyzeSettings {
    AnalyzeSettings {
        gain,
        ..cfg.analyze.clone()
    }
}

#[test]
fn merge_fields_edit_the_draft() {
    let (root, mut h) = harness("settings-draft");
    let ha = {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        drain(&mut st.jobs, &mut st.session);
        st.session.mosaic.result_path("Ha")
    };
    h.run_steps(2);
    // A merge is running: the edit must not cancel it.
    let gate = h.state_mut().jobs.hold();
    let oiii = {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "OIII", None)
    };
    type_feather(&mut h, "0"); // 256 -> 2560
    let st = h.state();
    assert_eq!(
        load_draft(&st.session.mosaic.config).feather_px,
        2560.0,
        "the draft"
    );
    assert_eq!(
        st.session.mosaic.config.ui[MERGE_DRAFT_KEY]["feather_px"],
        2560.0
    );
    assert_eq!(st.session.mosaic.config.blend.feather_px, 256.0);
    assert!(st.session.mosaic.config.needs_analyze.is_empty());
    assert_eq!(valid0(&ha), 6, "nothing went stale");
    assert!(st.jobs.blending(), "no job was cancelled");
    assert!(!st.panel.has_pending());
    assert!(st.applied.is_empty());
    // The window is told to save.
    assert!(h.state_mut().panel.take_draft_changed());
    assert!(!h.state_mut().panel.take_draft_changed());
    // A combo too.
    h.get_by_role_and_label(Role::ComboBox, "Defect veto")
        .click();
    h.run_steps(2);
    h.get_by_label("Off").click();
    h.run_steps(2);
    let st = h.state();
    assert!(!load_draft(&st.session.mosaic.config).defect_veto);
    assert!(st.session.mosaic.config.blend.defect_veto);
    assert!(h.query_by_label("Apply").is_none());
    // The running merge was not cancelled: it finishes.
    gate.open();
    let st = h.state_mut();
    let events = drain(&mut st.jobs, &mut st.session);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, JobEvent::Finished { id, .. } if *id == oiii)),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, JobEvent::Cancelled { .. }))
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_draft_survives_reopening() {
    let (root, mut s, _jobs) = analyzed("settings-draft-reopen");
    let mut draft = s.mosaic.config.blend.clone();
    draft.mode = BlendMode::Feather;
    draft.feather_px = 99.0;
    // Without a draft key the config's own settings show.
    assert_eq!(load_draft(&s.mosaic.config), s.mosaic.config.blend);
    save_draft(&mut s.mosaic.config.ui, &draft);
    s.mosaic.save().unwrap();
    let loaded = MosaicConfig::load(&s.mosaic.dir).unwrap();
    assert_eq!(load_draft(&loaded), draft);
    assert_eq!(loaded.blend, s.mosaic.config.blend);
    // Other `ui` keys are kept; an unreadable draft falls back.
    let mut ui = serde_json::json!({"other": 1});
    save_draft(&mut ui, &draft);
    assert_eq!(ui["other"], 1);
    let mut broken = s.mosaic.config.clone();
    broken.ui[MERGE_DRAFT_KEY] = serde_json::json!("nonsense");
    assert_eq!(load_draft(&broken), broken.blend);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invalid_feather_is_rejected() {
    let (root, mut h) = harness("settings-bad-feather");
    h.run_steps(2);
    assert_eq!(h.state().panel.input_error(), None);
    type_feather(&mut h, "x");
    h.get_by_label("Feather must be a number of pixels greater than 0.");
    assert_eq!(
        h.state().panel.input_error(),
        Some("Feather width must be a number above 0")
    );
    // The last valid draft is kept.
    assert_eq!(
        load_draft(&h.state().session.mosaic.config).feather_px,
        256.0
    );
    // Zero parses but the core refuses it; the draft keeps the last value.
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .focus();
    h.step();
    for _ in 0..4 {
        h.key_press(egui::Key::Backspace);
        h.step();
    }
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .type_text("0");
    h.run_steps(2);
    let err = h
        .state()
        .panel
        .feather_error()
        .unwrap_or_default()
        .to_string();
    assert!(err.starts_with("Feather must be"), "{err}");
    assert!(h.state().panel.input_error().is_some());
    h.key_press(egui::Key::Backspace);
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .type_text("NaN");
    h.run_steps(2);
    let err = h
        .state()
        .panel
        .feather_error()
        .unwrap_or_default()
        .to_string();
    assert!(err.contains("finite"), "{err}");
    assert!(
        load_draft(&h.state().session.mosaic.config)
            .feather_px
            .is_finite()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn analysis_fields_apply_at_once_with_undo() {
    let (root, mut h) = harness("settings-analysis-now");
    h.run_steps(2);
    h.get_by_role_and_label(Role::ComboBox, "Gain").click();
    h.run_steps(2);
    h.get_by_label("Unity").click();
    h.run_steps(2);
    let st = h.state();
    let cfg = &st.session.mosaic.config;
    assert_eq!(st.applied, [ChangeImpact::Reanalyze]);
    assert_eq!(cfg.analyze.gain, GainMode::Unity);
    assert_eq!(
        cfg.needs_analyze.iter().cloned().collect::<Vec<_>>(),
        ["Ha", "OIII"]
    );
    assert!(h.query_by_label("Apply").is_none());
    // The previous values, once.
    let prev = h.state_mut().panel.take_analysis_undo().unwrap();
    assert_eq!(prev.gain, GainMode::Fit);
    assert_eq!(h.state_mut().panel.take_analysis_undo(), None);
    // Undo is an ordinary analysis edit.
    let st = h.state_mut();
    assert_eq!(
        st.panel.edit_analysis(prev, &mut st.session, &mut st.jobs),
        Some(ChangeImpact::Reanalyze)
    );
    assert_eq!(st.session.mosaic.config.analyze.gain, GainMode::Fit);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn undo_of_an_analysis_change_waits_for_the_running_analysis() {
    let (root, mut s, mut jobs) = analyzed("settings-undo-waits");
    let ctx = egui::Context::default();
    let mut panel = SettingsPanel::new();
    let fit = s.mosaic.config.analyze.clone();
    let unity = analysis(GainMode::Unity, &s.mosaic.config);
    assert_eq!(
        panel.edit_analysis(unity, &mut s, &mut jobs),
        Some(ChangeImpact::Reanalyze)
    );
    let prev = panel.take_analysis_undo().unwrap();
    assert_eq!(prev, fit);
    // The analysis it caused is running when Undo is pressed.
    let gate = jobs.hold();
    let id = jobs.start_analyze(&mut s);
    assert_eq!(panel.edit_analysis(prev, &mut s, &mut jobs), None);
    assert!(panel.has_pending());
    assert!(panel.editing(&s.mosaic.config));
    assert_eq!(s.mosaic.config.analyze.gain, GainMode::Unity, "not yet");
    gate.open();
    let deadline = Instant::now() + crate::testutil::PATIENCE;
    let mut events = Vec::new();
    let mut applied = None;
    while applied.is_none() {
        assert!(Instant::now() < deadline, "never applied");
        events.extend(jobs.poll(&mut s));
        applied = panel.poll(&ctx, &mut s, &mut jobs);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(applied, Some(ChangeImpact::Reanalyze));
    assert!(events.contains(&JobEvent::Cancelled { id }));
    assert_eq!(s.mosaic.config.analyze.gain, GainMode::Fit);
    assert!(!panel.has_pending());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_waiting_analysis_edit_applies_when_the_session_is_left() {
    let (root, mut s, mut jobs) = analyzed("settings-apply-waiting");
    let gate = jobs.hold();
    jobs.start_analyze(&mut s);
    let mut panel = SettingsPanel::new();
    let unity = analysis(GainMode::Unity, &s.mosaic.config);
    assert_eq!(panel.edit_analysis(unity, &mut s, &mut jobs), None);
    assert!(panel.has_pending(), "waits for the analysis to stop");
    gate.open();
    let (_, stopped) = jobs.stop_all(&mut s, crate::testutil::PATIENCE);
    assert!(stopped);
    assert_eq!(
        panel.apply_waiting(&mut s, &mut jobs),
        Some(ChangeImpact::Reanalyze)
    );
    assert!(!panel.has_pending());
    let saved = MosaicConfig::load(&s.mosaic.dir).unwrap();
    assert_eq!(saved.analyze.gain, GainMode::Unity);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn analysis_edit_leaves_the_merge_settings_alone() {
    let (root, mut s, mut jobs) = analyzed("settings-analysis-keeps-blend");
    let mut draft = s.mosaic.config.blend.clone();
    draft.feather_px = 77.0;
    save_draft(&mut s.mosaic.config.ui, &draft);
    let unity = analysis(GainMode::Unity, &s.mosaic.config);
    let mut panel = SettingsPanel::new();
    let before = s.mosaic.config.blend.clone();
    panel.edit_analysis(unity, &mut s, &mut jobs);
    assert_eq!(s.mosaic.config.blend, before);
    assert_eq!(load_draft(&s.mosaic.config), draft);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_dot_follows_the_last_merge() {
    let (root, mut h) = harness("settings-marker");
    let marks = |h: &Harness<'static, State>| h.query_all_by_label(CHANGED_MARK).count();
    h.run_steps(2);
    // Never merged: nothing to compare with.
    assert_eq!(marks(&h), 0);
    {
        let st = h.state_mut();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        drain(&mut st.jobs, &mut st.session);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 0);
    // A draft change is marked for a group that was merged...
    {
        let st = h.state_mut();
        let mut d = load_draft(&st.session.mosaic.config);
        d.mode = BlendMode::Feather;
        st.panel.edit_draft(&mut st.session.mosaic.config, d);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 1);
    // ...not for one that never was, and a composite counts any member.
    h.state_mut().groups = vec!["OIII".into()];
    h.run_steps(2);
    assert_eq!(marks(&h), 0);
    h.state_mut().groups = vec!["OIII".into(), "Ha".into()];
    h.run_steps(2);
    assert_eq!(marks(&h), 1);
    h.state_mut().groups = vec!["Ha".into()];
    // A merge that used the draft clears the merge dot.
    {
        let st = h.state_mut();
        let d = load_draft(&st.session.mosaic.config);
        commit_blend_settings(&mut st.session.mosaic, d).unwrap();
        st.jobs.start_blend(&mut st.session, "Ha", None);
        drain(&mut st.jobs, &mut st.session);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 0, "the merge used the draft");
    // A group's override decides what the draft means for it. The last
    // merge used the draft (Feather).
    let over = |mode| GroupOverride {
        blend: BlendOverride {
            mode: Some(mode),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        mmm_core::mosaic::last_merge_settings(&h.state().session.mosaic.config, "Ha")
            .unwrap()
            .mode,
        BlendMode::Feather
    );
    // An override that differs from the last merge shows the dot...
    set_group_override(
        &mut h.state_mut().session.mosaic,
        "Ha",
        over(BlendMode::Pyramid),
    )
    .unwrap();
    h.run_steps(2);
    assert_eq!(marks(&h), 1);
    // ...one that equals it shows none.
    set_group_override(
        &mut h.state_mut().session.mosaic,
        "Ha",
        over(BlendMode::Feather),
    )
    .unwrap();
    h.run_steps(2);
    assert_eq!(marks(&h), 0);
    set_group_override(
        &mut h.state_mut().session.mosaic,
        "Ha",
        GroupOverride::default(),
    )
    .unwrap();
    h.run_steps(2);
    // An analysis change is marked against the last analysis.
    {
        let st = h.state_mut();
        let a = analysis(GainMode::Unity, &st.session.mosaic.config);
        assert_eq!(
            last_analyze_settings(&st.session.mosaic.config, "Ha")
                .unwrap()
                .gain,
            GainMode::Fit
        );
        st.panel.edit_analysis(a, &mut st.session, &mut st.jobs);
    }
    h.run_steps(2);
    assert_eq!(marks(&h), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_old_session_shows_no_dot() {
    // No draft key, a merged session: the draft is the config's settings.
    let (root, mut s, mut jobs) = analyzed("settings-old-session");
    jobs.start_blend(&mut s, "Ha", None);
    drain(&mut jobs, &mut s);
    assert!(s.mosaic.config.ui.get(MERGE_DRAFT_KEY).is_none());
    let cfg = &s.mosaic.config;
    let groups = ["Ha".to_string()];
    assert_eq!(
        changed_fields(cfg, &load_draft(cfg), &cfg.analyze, &groups),
        Vec::<Field>::new()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_invalid_draft_is_never_written() {
    let (root, mut s, _jobs) = analyzed("settings-invalid-draft");
    let mut panel = SettingsPanel::new();
    let mut bad = s.mosaic.config.blend.clone();
    bad.feather_px = 0.0;
    panel.edit_draft(&mut s.mosaic.config, bad);
    assert!(s.mosaic.config.ui.get(MERGE_DRAFT_KEY).is_none());
    assert!(!panel.take_draft_changed());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_cost_note_shows_once_after_a_merge() {
    let (root, mut h) = harness("settings-cost-note");
    let note = "Re-runs the analysis; the mosaic goes out of date";
    h.run_steps(2);
    assert!(h.query_by_label(note).is_none(), "nothing merged yet");
    h.state_mut().session.merged = true;
    h.run_steps(2);
    assert_eq!(h.query_all_by_label(note).count(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_pending_analysis_edit_stays_editable() {
    let (root, mut h) = harness("settings-pending-editable");
    h.run_steps(2);
    let gate = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        st.jobs.start_analyze(&mut st.session);
    }
    h.get_by_role_and_label(Role::ComboBox, "Gain").click();
    h.run_steps(2);
    h.get_by_label("Unity").click();
    h.run_steps(2);
    assert!(h.state().panel.has_pending());
    // Setting the old value back clears it; nothing was applied.
    h.get_by_role_and_label(Role::ComboBox, "Gain").click();
    h.run_steps(2);
    h.get_by_label("Fit").click();
    h.run_steps(2);
    let st = h.state();
    assert!(!st.panel.has_pending());
    assert!(st.applied.is_empty());
    assert_eq!(st.session.mosaic.config.analyze.gain, GainMode::Fit);
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_edit_is_reported_for_a_toast() {
    let (root, mut s, mut jobs) = analyzed("settings-failed-edit");
    let mut panel = SettingsPanel::new();
    assert_eq!(panel.take_problem(), None);
    // A directory where mosaic.json goes: the save fails.
    let file = s.mosaic.dir.join(mmm_core::mosaic::CONFIG_FILE);
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let unity = analysis(GainMode::Unity, &s.mosaic.config);
    assert_eq!(panel.edit_analysis(unity, &mut s, &mut jobs), None);
    let why = panel.take_problem().expect("a problem");
    assert!(why.starts_with("Could not change the settings"), "{why}");
    assert_eq!(panel.take_problem(), None, "once");
    let _ = std::fs::remove_dir_all(&root);
}
