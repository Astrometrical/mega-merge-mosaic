//! Compare A / B on an ROI preview (spec §8.3): **Compare** on the ROI's
//! canvas label freezes its preview as A; B follows the draft; the A / B
//! toggle stays, and Space, the selected button and the preview itself are
//! momentary switches showing the other side while held.

use mmm_core::settings::BlendSettings;
use mmm_core::stretch::StretchParams;

use super::previews::{
    CORE, delivered, draft_feather, drawn_with_canvas_stretch, last_started_key, release, show_hoo,
    shown, started,
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
    let (root, mut h, gate) = shown(tag, &[CORE], prepare);
    step_to(&mut h, |st| started(st).len() == 1);
    let gate = release(&mut h, gate);
    step_to(&mut h, |st| delivered(st, 7, "Ha") == 1);
    step_until_row(&mut h, "Core", "up to date");
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
    // The label is back to one row.
    assert!(h.query_by_label("Compare Core").is_some());
    assert!(h.query_by_label("Use A for Core").is_none());
    // Done leaves compare keeping the draft.
    let (_, gate) = comparing_two_sides(&mut h, gate);
    let b = load_draft(&h.state().session.mosaic.config);
    assert_ne!(b, a);
    h.get_by_label("Done comparing Core").click();
    h.step();
    assert!(compare(h.state()).is_none());
    assert_eq!(load_draft(&h.state().session.mosaic.config), b);
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
