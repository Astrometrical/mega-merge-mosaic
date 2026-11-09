//! Compare A / B on an ROI preview (spec §8.3): **Compare** on the ROI's
//! canvas label freezes its preview as A; B follows the draft; the A / B
//! toggle stays, and Space, the selected button and the preview itself are
//! momentary switches showing the other side while held.

use mmm_core::settings::BlendSettings;
use mmm_core::stretch::StretchParams;

use super::previews::{
    CORE, HALO, delivered, draft_feather, drawn_with_canvas_stretch, last_started_key, release,
    show_hoo, shown, started,
};
use super::*;
use crate::blend_settings::load_draft;
use crate::previews::tests::Gate;
use crate::previews::{Compare, Peek, Side};
use crate::tile_source::planar_image;

/// The compare running, if any.
fn compare(st: &State) -> Option<&Compare> {
    st.w.previews().compare()
}

/// The side shown now (B outside compare).
fn shown_side(st: &State) -> Side {
    st.w.previews().shown_side()
}

/// "{side} · feather {n} px · flatten {f}", as the label's second row says.
fn settings_text(side: &str, s: &BlendSettings) -> String {
    let flatten = s.flatten.map_or("off".to_string(), |f| f.to_string());
    format!("{side} · feather {} px · flatten {flatten}", s.feather_px)
}

/// The stretch the shown canvas draws `group` with.
fn canvas_stretch(st: &State, group: &str) -> Option<Vec<StretchParams>> {
    match st.w.mode() {
        CanvasMode::Mosaic => st.w.mosaic().member_stretch(group),
        CanvasMode::Plan => st.w.plan().stretch_for_view().map(<[_]>::to_vec),
    }
}

/// Core (Ha) is drawn from side `side`'s texture, converted from that
/// side's patch through the canvas's stretch.
fn drawn(st: &State, side: Side) -> bool {
    if st.w.preview_drawn_side(7) != Some(side) {
        return false;
    }
    let patch = match side {
        Side::A => match compare(st) {
            Some(c) => c.a["Ha"].1.clone(),
            None => return false,
        },
        Side::B => match st.w.previews().patch(7, "Ha") {
            Some((_, p)) => p,
            None => return false,
        },
    };
    let (Some(stretch), Some(image)) = (canvas_stretch(st, "Ha"), st.w.preview_image(7)) else {
        return false;
    };
    let n = (patch.width * patch.height) as usize;
    *image
        == planar_image(
            &patch.data,
            n,
            patch.width as usize,
            (patch.width as usize, patch.height as usize),
            patch.channels as usize,
            &stretch,
        )
}

/// The viewer's (or the plan's) rect in the last frame.
fn frame(h: &Harness<'static, State>) -> egui::Rect {
    match h.state().w.mode() {
        CanvasMode::Mosaic => h.state().w.mosaic().viewer_frame().unwrap().0,
        CanvasMode::Plan => h.state().w.plan().canvas_frame().unwrap().0,
    }
}

/// The text of the label's line (the shown side's settings), painted in
/// the canvas.
fn says(h: &Harness<'static, State>, text: &str) -> bool {
    painted(h, frame(h), text).is_some()
}

/// Core's preview merged and up to date on the mosaic; previews started
/// from now on wait for the gate returned.
fn up_to_date(tag: &str) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    up_to_date_with(tag, |_| {})
}

/// [`up_to_date`], with `prepare` run on the state before the first frame.
fn up_to_date_with(
    tag: &str,
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    up_to_date_roi(tag, CORE, prepare)
}

/// ROI `roi` alone, its preview merged and up to date on the mosaic, and
/// selected (its label offers Compare); previews started from now on wait
/// for the gate returned.
fn up_to_date_roi(
    tag: &str,
    roi: (u64, &str, [u64; 4]),
    prepare: impl FnOnce(&mut State),
) -> (PathBuf, Harness<'static, State>, Arc<Gate>) {
    let (root, mut h, gate) = shown(tag, &[roi], prepare);
    step_to(&mut h, |st| started(st).len() == 1);
    let gate = release(&mut h, gate);
    step_to(&mut h, |st| delivered(st, roi.0, "Ha") == 1);
    step_until_row(&mut h, roi.1, "up to date");
    h.state_mut().w.mosaic.select_roi(roi.0);
    h.run_steps(2);
    (root, h, gate)
}

/// Compare Core, then change the feather width so B merges again (it is
/// let through): A and B now differ. The draft's settings before (A's).
fn comparing_two_sides(
    h: &mut Harness<'static, State>,
    gate: Arc<Gate>,
) -> (BlendSettings, Arc<Gate>) {
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_some(), "comparing");
    let a = load_draft(&h.state().session.mosaic.config);
    let n = delivered(h.state(), 7, "Ha");
    set_draft_feather(&mut h.state_mut().session, a.feather_px + 60.0);
    step_to(h, |st| started(st).len() == n + 1);
    let gate = release(h, gate);
    step_to(h, |st| delivered(st, 7, "Ha") == n + 1);
    step_to(h, |st| drawn(st, Side::B));
    let st = h.state();
    let a_patch = &compare(st).unwrap().a["Ha"].1;
    let (_, b_patch) = st.w.previews().patch(7, "Ha").unwrap();
    assert!(!Arc::ptr_eq(a_patch, &b_patch), "B was merged again");
    (a, gate)
}

/// Press the primary button at `at` and hold it there.
fn press(h: &mut Harness<'static, State>, at: egui::Pos2) {
    h.hover_at(at);
    h.drag_at(at);
    h.step();
}

/// Let the primary button go at `at`.
fn let_go(h: &mut Harness<'static, State>, at: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

/// The label's accessible description.
fn described(h: &Harness<'static, State>) -> String {
    h.get_by_label("Core on the canvas")
        .accesskit_node()
        .description()
        .unwrap_or_default()
}

fn toggled(h: &Harness<'static, State>, label: &str) -> Option<egui::accesskit::Toggled> {
    h.get_by_label(label).accesskit_node().toggled()
}

#[test]
fn compare_freezes_a_and_b_follows_the_draft() {
    let (root, mut h, gate) = up_to_date("compare-freeze");
    assert!(compare(h.state()).is_none());
    let (key, patch) = h.state().w.previews().patch(7, "Ha").unwrap();
    let (key, patch) = (key.clone(), patch.clone());
    let draft = load_draft(&h.state().session.mosaic.config);
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    // A: the preview as it was, with the draft it was made from. B, the live
    // preview, is selected and shown.
    let c = compare(h.state()).expect("comparing Core");
    assert_eq!(c.roi, 7);
    assert_eq!(c.a_settings, draft);
    assert_eq!(c.a["Ha"].0, key);
    assert!(Arc::ptr_eq(&c.a["Ha"].1, &patch));
    assert_eq!((c.selected, c.shown()), (Side::B, Side::B));
    // B follows the draft: it merges again with the new settings, and the
    // label says so.
    let feather = draft_feather(h.state());
    set_draft_feather(&mut h.state_mut().session, feather + 60.0);
    step_to(&mut h, |st| started(st).len() == 2);
    assert_eq!(
        last_started_key(h.state()).settings.feather_px,
        feather + 60.0
    );
    h.step();
    assert!(says(&h, "B · merging…"));
    let gate = release(&mut h, gate);
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 2);
    step_to(&mut h, |st| {
        drawn(st, Side::B) && drawn_with_canvas_stretch(st, 7, "Ha")
    });
    let b = load_draft(&h.state().session.mosaic.config);
    h.step();
    assert!(says(&h, &settings_text("B", &b)));
    // A is as it was: never merged again.
    let c = compare(h.state()).unwrap();
    assert_eq!(c.a["Ha"].0, key);
    assert!(Arc::ptr_eq(&c.a["Ha"].1, &patch));
    // A selected: A's patch is drawn, through the canvas's stretch, and the
    // label gives A's settings.
    h.get_by_label("Show A of Core").click();
    h.run_steps(2);
    assert_eq!(compare(h.state()).unwrap().selected, Side::A);
    step_to(&mut h, |st| drawn(st, Side::A));
    assert!(says(&h, &settings_text("A", &draft)));
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), 2, "A never merges");
    // Back to B.
    h.get_by_label("Show B of Core").click();
    step_to(&mut h, |st| drawn(st, Side::B));
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_toggle_stays_and_space_peeks_the_other_side() {
    let (root, mut h, gate) = up_to_date_with("compare-space", |st| {
        st.w.inspector_mut().reveal(NOTES);
    });
    let (_, gate) = comparing_two_sides(&mut h, gate);
    // The selected side stays shown.
    h.run_steps(10);
    assert!(drawn(h.state(), Side::B));
    assert_eq!(
        toggled(&h, "Show B of Core"),
        Some(egui::accesskit::Toggled::True)
    );
    // From B, Space held shows A, for as long as it is held.
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    assert!(compare(h.state()).unwrap().peeks.contains(&Peek::Space));
    step_to(&mut h, |st| drawn(st, Side::A));
    h.run_steps(30);
    assert!(drawn(h.state(), Side::A));
    // B stays the one selected; the peek is announced.
    assert_eq!(compare(h.state()).unwrap().selected, Side::B);
    assert_eq!(
        toggled(&h, "Show B of Core"),
        Some(egui::accesskit::Toggled::True)
    );
    assert!(described(&h).starts_with("Peeking"), "{}", described(&h));
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    step_to(&mut h, |st| drawn(st, Side::B));
    assert!(described(&h).starts_with("Showing"), "{}", described(&h));
    // From A, it shows B.
    h.get_by_label("Show A of Core").click();
    h.run_steps(10);
    assert!(drawn(h.state(), Side::A));
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    step_to(&mut h, |st| drawn(st, Side::B));
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    // Let go while a text field has the keyboard: the peek still ends.
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    h.get_by_label("Add notes").click();
    h.run_steps(2);
    h.get_by_role(Role::MultilineTextInput).focus();
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B, "still held");
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn holding_the_selected_button_peeks_and_clicking_the_other_switches() {
    let (root, mut h, gate) = up_to_date("compare-button");
    let (_, gate) = comparing_two_sides(&mut h, gate);
    // Pressing and holding B (selected): A while held, however long.
    let b = h.get_by_label("Show B of Core").rect().center();
    press(&mut h, b);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    assert!(compare(h.state()).unwrap().peeks.contains(&Peek::Button));
    h.run_steps(100);
    assert_eq!(shown_side(h.state()), Side::A);
    step_to(&mut h, |st| drawn(st, Side::A));
    let_go(&mut h, b);
    h.run_steps(2);
    let c = compare(h.state()).unwrap();
    assert_eq!((c.selected, c.shown()), (Side::B, Side::B));
    // A click on A switches to it, and it stays.
    h.get_by_label("Show A of Core").click();
    h.run_steps(10);
    let c = compare(h.state()).unwrap();
    assert_eq!((c.selected, c.shown()), (Side::A, Side::A));
    // Holding A (now selected) shows B while held.
    let a = h.get_by_label("Show A of Core").rect().center();
    press(&mut h, a);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    let_go(&mut h, a);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    // Holding the other one (B) is no peek; letting go clicks it: B.
    press(&mut h, b);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    let_go(&mut h, b);
    h.run_steps(2);
    let c = compare(h.state()).unwrap();
    assert_eq!((c.selected, c.shown()), (Side::B, Side::B));
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn holding_on_the_preview_peeks_and_a_drag_still_pans() {
    let (root, mut h, gate) = up_to_date("compare-preview");
    let (_, gate) = comparing_two_sides(&mut h, gate);
    // Core fills the view.
    h.state_mut().w.frame(CORE.2);
    h.run_steps(3);
    let rect = frame(&h);
    let cam = h.state().w.mosaic().camera().unwrap();
    // Low in Core, clear of its label.
    let inside = screen(rect, cam, (150.0, 155.0));
    press(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    assert!(compare(h.state()).unwrap().peeks.contains(&Peek::Preview));
    // Held still past a click's time: still A, nothing panned.
    h.run_steps(100);
    assert_eq!(shown_side(h.state()), Side::A);
    assert_camera(h.state().w.mosaic().camera(), cam);
    let_go(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    // A drag pans, and ends the peek even while still held.
    press(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    for i in 1..=4 {
        h.hover_at(inside + egui::vec2(0.0, -15.0 * i as f32));
        h.step();
    }
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    // The pan eases in.
    h.run_steps(30);
    let moved = h.state().w.mosaic().camera().unwrap();
    assert!(
        (moved.centre.1 - cam.centre.1).abs() > 1.0,
        "{moved:?} {cam:?}"
    );
    h.hover_at(inside);
    h.step();
    assert_eq!(shown_side(h.state()), Side::B, "a drag stays a drag");
    let_go(&mut h, inside);
    h.run_steps(2);
    // Outside Core, or on its label: no peek.
    let label = h.get_by_label("Core on the canvas").rect();
    let outside = screen(rect, cam, (250.0, 155.0));
    for at in [outside, label.left_bottom() + egui::vec2(2.0, -2.0)] {
        press(&mut h, at);
        h.run_steps(2);
        assert_eq!(shown_side(h.state()), Side::B, "{at:?}");
        let_go(&mut h, at);
    }
    // On the plan too.
    h.key_press(egui::Key::M);
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Plan && plan_placed(st)
    });
    h.run_steps(3);
    let rect = frame(&h);
    let cam = h.state().w.plan().camera().unwrap();
    let inside = screen(rect, cam, (150.0, 155.0));
    press(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    let_go(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::B);
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn use_a_restores_a_settings_and_ends_compare() {
    let (root, mut h, gate) = up_to_date("compare-use-a");
    let (a, gate) = comparing_two_sides(&mut h, gate);
    let merges = started(h.state()).len();
    let a_key = compare(h.state()).unwrap().a["Ha"].0.clone();
    h.get_by_label("Use A for Core").click();
    h.step();
    // A's settings are the draft again, saved as a draft edit is.
    assert!(compare(h.state()).is_none());
    assert_eq!(load_draft(&h.state().session.mosaic.config), a);
    assert!(h.state().w.save.pending(), "the draft awaits its save");
    // The preview is A's, which is what the draft makes: nothing merges.
    assert_eq!(h.state().w.previews().cached_key(7, "Ha"), Some(&a_key));
    step_until_row(&mut h, "Core", "up to date");
    h.run_steps(10);
    assert_eq!(started(h.state()).len(), merges);
    // The label is back to one row, and A's texture is let go of.
    assert!(h.query_by_label("Compare Core").is_some());
    assert!(h.query_by_label("Use A for Core").is_none());
    assert!(!h.state().w.shown_patches().has(7, Side::A));
    // Done leaves compare keeping the draft.
    let (_, gate) = comparing_two_sides(&mut h, gate);
    let b = load_draft(&h.state().session.mosaic.config);
    assert_ne!(b, a);
    assert!(h.state().w.shown_patches().has(7, Side::A));
    h.get_by_label("Done comparing Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    assert_eq!(load_draft(&h.state().session.mosaic.config), b);
    assert!(!h.state().w.shown_patches().has(7, Side::A));
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compare_ends_when_its_roi_goes_or_an_analysis_starts() {
    let (root, mut h, gate) = up_to_date("compare-ends");
    // Previews run freely from here.
    gate.open();
    let enter = |h: &mut Harness<'static, State>| {
        step_until_row(h, "Core", "up to date");
        h.run_steps(2);
        h.get_by_label("Compare Core").click();
        h.run_steps(2);
        assert!(compare(h.state()).is_some());
    };
    // Hidden by the ROIs toggle.
    enter(&mut h);
    h.key_press(egui::Key::R);
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    h.key_press(egui::Key::R);
    // The view's groups change.
    enter(&mut h);
    show_hoo(h.state_mut());
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    h.state_mut().w.select_view(ViewKind::Group("Ha".into()));
    // Moved (as a reprojection moves it).
    enter(&mut h);
    let moved = [300, 200, 380, 260];
    h.state_mut().w.mosaic.move_roi(7, moved);
    h.step();
    assert!(compare(h.state()).is_none());
    h.state_mut().w.mosaic.move_roi(7, CORE.2);
    // Deleted.
    enter(&mut h);
    hover_row(&mut h, "Core");
    h.get_by_label("Delete Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    let toasts = std::mem::take(&mut h.state_mut().toasts);
    let undo = toasts.into_iter().find_map(|t| t.undo).expect("an undo");
    {
        let st = h.state_mut();
        st.w.undo(undo, &mut st.session, &mut st.jobs);
    }
    h.run_steps(2);
    // An analysis starts: compare ends, and is not offered while it runs.
    enter(&mut h);
    let analysis = h.state_mut().jobs.hold();
    {
        let st = h.state_mut();
        let id = st.jobs.start_analyze(&mut st.session);
        st.w.track_analysis(id);
    }
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    assert!(h.query_by_label("Compare Core").is_none());
    analysis.open();
    step_to(&mut h, |st| !st.jobs.busy());
    // Leaving the session ends it too.
    enter(&mut h);
    let _ = h.state_mut().w.stop_previews();
    assert!(compare(h.state()).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn space_is_typing_in_a_text_field() {
    let (root, mut h, gate) = up_to_date_with("compare-typing", |st| {
        st.w.inspector_mut().reveal(NOTES);
    });
    let (_, gate) = comparing_two_sides(&mut h, gate);
    h.get_by_label("Add notes").click();
    h.run_steps(2);
    h.get_by_role(Role::MultilineTextInput).focus();
    h.run_steps(2);
    h.key_down(egui::Key::Space);
    h.get_by_role(Role::MultilineTextInput).type_text(" ");
    h.run_steps(3);
    assert_eq!(shown_side(h.state()), Side::B, "typed, not a peek");
    assert!(compare(h.state()).unwrap().peeks.is_empty());
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(notes(&h.state().session.mosaic.config.ui), " ");
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_label_is_one_row_outside_compare_and_two_in_it() {
    let (root, mut h, gate) = up_to_date("compare-label");
    let rect = frame(&h);
    let cam = h.state().w.mosaic().camera().unwrap();
    // One row: the name where the painted label was, Compare, the state.
    let corner = screen(rect, cam, (100.0, 100.0));
    let (at, size) = painted(&h, rect, "Core").expect("the name");
    assert!((at - (corner + egui::vec2(4.0, 2.0))).length() < 1.0);
    let name = egui::Rect::from_min_size(at, size);
    let one_row = |r: egui::Rect| (r.center().y - name.center().y).abs() < 1.0;
    let compare_button = h.get_by_label("Compare Core").rect();
    assert!(compare_button.left() > name.right() && one_row(compare_button));
    let (state_at, state_size) = painted(&h, rect, "up to date").expect("the state");
    let state = egui::Rect::from_min_size(state_at, state_size);
    assert!(state.left() > compare_button.right() && one_row(state));
    // The name is drawn once: no painted overlay label besides.
    let names = h
        .output()
        .shapes
        .iter()
        .filter(|cs| matches!(&cs.shape, egui::Shape::Text(t) if t.galley.text() == "Core" && rect.contains(t.pos)))
        .count();
    assert_eq!(names, 1);
    // Comparing: the controls on the name's row, the settings on a second.
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    let controls = [
        "Show A of Core",
        "Show B of Core",
        "Use A for Core",
        "Done comparing Core",
    ];
    let rects = |h: &Harness<'static, State>| -> Vec<egui::Rect> {
        controls.iter().map(|c| h.get_by_label(c).rect()).collect()
    };
    let first = rects(&h);
    assert!(first.iter().all(|r| one_row(*r) && r.left() > name.right()));
    let draft = load_draft(&h.state().session.mosaic.config);
    let below = |h: &Harness<'static, State>, text: &str| {
        let (at, _) = painted(h, rect, text).expect(text);
        let bottom = first.iter().map(|r| r.bottom()).fold(0.0, f32::max);
        at.y >= bottom && (at.x - name.left()).abs() < 0.5
    };
    assert!(below(&h, &settings_text("B", &draft)));
    // The text changes (B merging, a peek at A); the controls stay put.
    set_draft_feather(&mut h.state_mut().session, draft.feather_px + 60.0);
    step_to(&mut h, |st| started(st).len() == 2);
    h.step();
    assert!(below(&h, "B · merging…"));
    assert_eq!(rects(&h), first);
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    assert!(below(&h, &settings_text("A", &draft)));
    assert_eq!(rects(&h), first);
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn space_presses_a_focused_control() {
    let (root, mut h, gate) = up_to_date("compare-space-focus");
    let (_, gate) = comparing_two_sides(&mut h, gate);
    // A option focused (keyboard navigation): Space chooses it, no peek.
    h.get_by_label("Show A of Core").focus();
    h.run_steps(2);
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    let c = compare(h.state()).unwrap();
    assert_eq!((c.selected, c.shown()), (Side::A, Side::A));
    assert!(c.peeks.is_empty(), "{:?}", c.peeks);
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    // Done focused: Space presses it, and compare ends.
    h.get_by_label("Done comparing Core").focus();
    h.run_steps(2);
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn compare_is_offered_on_the_selected_roi_only() {
    let (root, mut h, gate) = shown("compare-selected", &[CORE, HALO], |_| {});
    gate.open();
    step_until_row(&mut h, "Core", "up to date");
    step_until_row(&mut h, "Halo", "up to date");
    h.run_steps(2);
    let rect = frame(&h);
    // None selected: plain painted labels, no Compare.
    assert!(h.query_by_label("Compare Core").is_none());
    assert!(h.query_by_label("Compare Halo").is_none());
    assert!(painted(&h, rect, "Core").is_some() && painted(&h, rect, "Halo").is_some());
    // Core picked in the list: its label offers Compare; Halo's stays
    // painted.
    h.get_by_label("Core").click();
    h.run_steps(3);
    assert!(h.query_by_label("Compare Core").is_some());
    assert!(h.query_by_label("Compare Halo").is_none());
    // Both in view again; Core compared, then Halo selected: Core keeps its
    // compare controls and Halo offers Compare.
    h.state_mut().w.frame([0, 0, 220, 180]);
    h.run_steps(3);
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    assert_eq!(compare(h.state()).map(|c| c.roi), Some(7));
    h.state_mut().w.mosaic.select_roi(9);
    h.run_steps(2);
    assert!(h.query_by_label("Done comparing Core").is_some());
    assert!(h.query_by_label("Compare Halo").is_some());
    assert!(h.state().w.shown_patches().has(7, Side::A));
    // Compare on Halo ends Core's: one at a time.
    h.get_by_label("Compare Halo").click();
    h.run_steps(2);
    assert_eq!(compare(h.state()).map(|c| c.roi), Some(9));
    assert!(h.query_by_label("Done comparing Core").is_none());
    assert!(h.query_by_label("Done comparing Halo").is_some());
    assert!(!h.state().w.shown_patches().has(7, Side::A));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_click_in_the_compared_preview_selects_no_panel_on_the_plan() {
    let (root, mut h, gate) = up_to_date("compare-plan-click");
    gate.open();
    h.key_press(egui::Key::M);
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Plan && plan_placed(st)
    });
    h.run_steps(3);
    let rect = frame(&h);
    let cam = h.state().w.plan().camera().unwrap();
    let click = |h: &mut Harness<'static, State>, p: (f64, f64)| {
        let at = screen(rect, cam, p);
        h.hover_at(at);
        h.drag_at(at);
        let_go(h, at);
        h.run_steps(2);
    };
    // Low in Core, clear of its label, on a panel.
    let inside = (150.0, 155.0);
    // Not comparing: a click there selects the panel under it.
    click(&mut h, inside);
    assert!(h.state().w.plan().selected().is_some());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().w.plan().selected().is_none());
    // Comparing: it selects nothing (a press there is a peek)...
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_some());
    click(&mut h, inside);
    assert!(h.state().w.plan().selected().is_none());
    // ...while a click on a panel outside Core still does.
    let core = CORE.2.map(|v| v as f64);
    let outside = h
        .state()
        .w
        .plan()
        .items()
        .iter()
        .map(|it| {
            (
                (it.rect[0] + it.rect[2]) / 2.0,
                (it.rect[1] + it.rect[3]) / 2.0,
            )
        })
        .find(|c| !(c.0 >= core[0] && c.0 < core[2] && c.1 >= core[1] && c.1 < core[3]))
        .expect("a panel centre outside Core");
    click(&mut h, outside);
    assert!(h.state().w.plan().selected().is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_toggle_tip_tells_keyboard_users_how_to_peek() {
    let (root, mut h, gate) = up_to_date("compare-toggle-tip");
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_some());
    // Space presses a focused control: Esc lets go of the focus first.
    let at = h.get_by_label("Show A of Core").rect().center();
    let want = "A: the preview as compare began. B: the Merge settings now. Hold the selected \
                one, or the preview, to see the other. Esc, then hold Space to peek";
    assert!(tooltip_shows(&mut h, at, want), "{want}");
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_plan_in_another_frame_keeps_no_quiet_rect() {
    let (root, mut h, gate) = up_to_date("compare-plan-frame");
    gate.open();
    h.key_press(egui::Key::M);
    step_to(&mut h, |st| {
        st.w.mode() == CanvasMode::Plan && plan_placed(st)
    });
    h.run_steps(3);
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_some());
    // A canvas-changing edit: the plan draws in another frame than the
    // analysed one, and shows no ROI or patch; the compare goes on.
    h.state_mut().session.frame = Some(mmm_core::reference::ReferenceFrame::Aligned {
        width: 7,
        height: 9,
        wcs: None,
    });
    h.run_steps(3);
    assert!(compare(h.state()).is_some());
    // A click where Core was is an ordinary click: it selects the panel
    // under it, as nothing of Core shows there.
    let rect = frame(&h);
    let cam = h.state().w.plan().camera().unwrap();
    let at = screen(rect, cam, (150.0, 155.0));
    h.hover_at(at);
    h.drag_at(at);
    let_go(&mut h, at);
    h.run_steps(2);
    assert!(h.state().w.plan().selected().is_some());
    let _ = std::fs::remove_dir_all(&root);
}

/// "Big": most of the 600 × 420 canvas.
const BIG: (u64, &str, [u64; 4]) = (11, "Big", [50, 50, 550, 400]);

#[test]
fn zoomed_into_a_big_roi_its_label_stays_reachable_and_the_canvas_buttons_never_peek() {
    let (root, mut h, gate) = up_to_date_roi("compare-big", BIG, |_| {});
    gate.open();
    h.get_by_label("Compare Big").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_some());
    // Zoomed in: Big covers the whole view, its corner far off screen.
    let c = Camera {
        centre: (300.0, 225.0),
        zoom: 4.0,
    };
    h.state_mut().w.mosaic.set_camera(c);
    h.run_steps(3);
    let rect = frame(&h);
    assert!(
        screen(rect, c, (50.0, 50.0)).x < rect.left(),
        "the corner is off screen"
    );
    // The label sits at the top-left of what shows, clear of the canvas's
    // buttons, all of it inside the view.
    let buttons = h.state().w.mosaic().controls_rect().expect("the buttons");
    for name in [
        "Show A of Big",
        "Show B of Big",
        "Use A for Big",
        "Done comparing Big",
    ] {
        let r = h.get_by_label(name).rect();
        assert!(rect.contains_rect(r), "{name} {r:?} in {rect:?}");
        assert!(
            !r.intersects(buttons),
            "{name} {r:?} on the buttons {buttons:?}"
        );
    }
    // The canvas's own buttons over the preview: held, no peek.
    let plus = h
        .query(By::new().label("+").predicate(|n| n.toggled().is_none()))
        .expect("the zoom in button")
        .rect()
        .center();
    press(&mut h, plus);
    h.run_steps(100);
    assert_eq!(shown_side(h.state()), Side::B);
    let_go(&mut h, plus);
    h.run_steps(2);
    // The preview itself still peeks.
    let inside = rect.center() + egui::vec2(0.0, 100.0);
    press(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    let_go(&mut h, inside);
    h.run_steps(2);
    // Done, reachable: compare ends.
    h.get_by_label("Done comparing Big").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn b_out_of_date_and_not_merging_says_so() {
    let (root, mut h, gate) = up_to_date("compare-b-stale");
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    // Invalid typed settings hold previews back: B is out of date and not
    // merging.
    h.state_mut().w.mosaic.force_feather_error();
    let feather = draft_feather(h.state());
    set_draft_feather(&mut h.state_mut().session, feather + 60.0);
    h.run_steps(3);
    assert_eq!(started(h.state()).len(), 1);
    assert!(says(&h, "B · out of date"), "{:?}", described(&h));
    // Valid again: B merges.
    h.state_mut().w.mosaic.clear_feather_error();
    step_to(&mut h, |st| started(st).len() == 2);
    h.step();
    assert!(says(&h, "B · merging…"));
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

/// The window loses (`false`) or regains the keyboard focus.
fn window_focus(h: &mut Harness<'static, State>, focused: bool) {
    h.input_mut().focused = focused;
    h.event(egui::Event::WindowFocused(focused));
    h.run_steps(2);
}

#[test]
fn losing_the_window_focus_ends_the_peeks() {
    let (root, mut h, gate) = up_to_date("compare-focus-lost");
    let (_, gate) = comparing_two_sides(&mut h, gate);
    // The selected button held.
    let b = h.get_by_label("Show B of Core").rect().center();
    press(&mut h, b);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    window_focus(&mut h, false);
    assert_eq!(shown_side(h.state()), Side::B, "button");
    window_focus(&mut h, true);
    let_go(&mut h, b);
    h.run_steps(2);
    // The preview held.
    h.state_mut().w.frame(CORE.2);
    h.run_steps(3);
    let cam = h.state().w.mosaic().camera().unwrap();
    let inside = screen(frame(&h), cam, (150.0, 155.0));
    press(&mut h, inside);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    window_focus(&mut h, false);
    assert_eq!(shown_side(h.state()), Side::B, "preview");
    window_focus(&mut h, true);
    let_go(&mut h, inside);
    h.run_steps(2);
    // Space held.
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(shown_side(h.state()), Side::A);
    window_focus(&mut h, false);
    assert_eq!(shown_side(h.state()), Side::B, "space");
    window_focus(&mut h, true);
    h.key_up(egui::Key::Space);
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_settings_text_gives_the_groups_override() {
    use mmm_core::settings::{BlendOverride, GroupOverride};
    let (root, mut h, gate) = up_to_date_with("compare-override", |st| {
        st.session.mosaic.config.group_overrides.insert(
            "Ha".into(),
            GroupOverride {
                blend: BlendOverride {
                    feather_px: Some(77.0),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    });
    gate.open();
    let draft = load_draft(&h.state().session.mosaic.config);
    assert_ne!(draft.feather_px, 77.0);
    let flatten = draft.flatten.map_or("off".to_string(), |f| f.to_string());
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    // What Ha merges with: the override's feather, not the draft's.
    assert!(says(&h, &format!("B · feather 77 px · flatten {flatten}")));
    h.key_down(egui::Key::Space);
    h.run_steps(2);
    assert!(says(&h, &format!("A · feather 77 px · flatten {flatten}")));
    h.key_up(egui::Key::Space);
    h.run_steps(2);
    // The draft's feather changes: Ha's does not (nothing merges).
    set_draft_feather(&mut h.state_mut().session, draft.feather_px + 60.0);
    h.run_steps(5);
    assert!(says(&h, &format!("B · feather 77 px · flatten {flatten}")));
    assert_eq!(started(h.state()).len(), 1);
    // Use A puts the draft back (the override stays on top of it).
    let a_key = compare(h.state()).unwrap().a["Ha"].0.clone();
    h.get_by_label("Use A for Core").click();
    h.run_steps(2);
    assert_eq!(load_draft(&h.state().session.mosaic.config), draft);
    assert_eq!(h.state().w.previews().cached_key(7, "Ha"), Some(&a_key));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn use_a_clears_an_invalid_feather() {
    let (root, mut h, gate) = up_to_date_with("compare-use-a-invalid", |st| {
        st.w.inspector_mut().reveal(MERGE_SETTINGS);
    });
    let (a, gate) = comparing_two_sides(&mut h, gate);
    // Typed into the feather field: not a width, so merging is blocked.
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .focus();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Feather (px)")
        .type_text("x");
    h.run_steps(2);
    assert!(h.state().w.mosaic().input_error().is_some());
    // Use A: A's settings, the field showing A's feather, nothing blocked.
    h.get_by_label("Use A for Core").click();
    h.run_steps(3);
    assert!(compare(h.state()).is_none());
    assert_eq!(load_draft(&h.state().session.mosaic.config), a);
    assert!(h.state().w.mosaic().input_error().is_none());
    let field = h.get_by_role_and_label(Role::TextInput, "Feather (px)");
    assert_eq!(
        field.accesskit_node().value().as_deref(),
        Some(crate::blend_settings::feather_text(a.feather_px).as_str())
    );
    gate.open();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_roi_just_above_the_canvas_bottom_keeps_its_label_reachable() {
    let (root, mut h, gate) = up_to_date("compare-near-bottom");
    gate.open();
    h.get_by_label("Compare Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_some());
    // Core's top edge a few points above the canvas's bottom.
    let rect = frame(&h);
    let cam = h.state().w.mosaic().camera().unwrap();
    let below = f64::from(rect.bottom() - 5.0 - rect.center().y) / cam.zoom;
    let c = Camera {
        centre: (cam.centre.0, 100.0 - below),
        zoom: cam.zoom,
    };
    h.state_mut().w.mosaic.set_camera(c);
    h.run_steps(3);
    let top = screen(rect, c, (150.0, 100.0)).y;
    assert!(
        top < rect.bottom() && top > rect.bottom() - 10.0,
        "{top} in {rect:?}"
    );
    // The label is lifted inside the canvas: Done is there, and works.
    for name in [
        "Show A of Core",
        "Show B of Core",
        "Use A for Core",
        "Done comparing Core",
    ] {
        let r = h.get_by_label(name).rect();
        assert!(rect.contains_rect(r), "{name} {r:?} in {rect:?}");
    }
    h.get_by_label("Done comparing Core").click();
    h.run_steps(2);
    assert!(compare(h.state()).is_none());
    let _ = std::fs::remove_dir_all(&root);
}
