use egui::accesskit::Role;
use egui::{Key, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::ordered_list::{ListEvent, ListRow, ListState, drop_index, line_gap, ordered_list_with};
use crate::theme::tokens::space;

struct State {
    list: ListState,
    rows: Vec<(u64, String, Option<String>)>,
    selected: Option<u64>,
    events: Vec<ListEvent>,
}

fn harness() -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(400.0, 400.0))
        // Short frames, so two clicks of a few frames each are a double-click.
        .with_step_dt(0.01)
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let rows: Vec<ListRow> = s
                    .rows
                    .iter()
                    .map(|(id, label, detail)| ListRow {
                        id: *id,
                        label,
                        detail: detail.as_deref(),
                        selected: s.selected == Some(*id),
                    })
                    .collect();
                let events = ordered_list_with(ui, "rois", &rows, &mut s.list, |ui, id| {
                    ui.label(format!("details for {id}"));
                });
                s.events.extend(events);
            },
            State {
                list: ListState::default(),
                rows: vec![
                    (1, "Region 1".into(), Some("up to date".into())),
                    (2, "Region 2".into(), None),
                    (3, "Region 3".into(), Some("out of date".into())),
                ],
                selected: None,
                events: Vec::new(),
            },
        )
}

fn rect(h: &Harness<'_, State>, label: &str) -> Rect {
    h.get_by_label(label).rect()
}

#[test]
fn click_selects_and_activates() {
    let mut h = harness();
    h.run();
    h.get_by_label("Region 2").click();
    h.run();
    assert_eq!(
        h.state().events,
        vec![ListEvent::Select(2), ListEvent::Activate(2)]
    );
}

#[test]
fn double_click_renames_inline() {
    let mut h = harness();
    h.run();
    double_click(&mut h, "Region 1");
    h.run();
    h.run();
    h.get_by_role(Role::TextInput).type_text("Core");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(
        h.state()
            .events
            .contains(&ListEvent::Rename(1, "Core".into())),
        "{:?}",
        h.state().events
    );
    assert!(h.query_by_role(Role::TextInput).is_none());
}

#[test]
fn the_edit_action_renames_too() {
    let mut h = harness();
    h.run();
    h.hover_at(rect(&h, "Region 2").center());
    h.run();
    h.get_by_label("Rename Region 2").click();
    h.run();
    h.run();
    h.get_by_role(Role::TextInput).type_text("Lobe");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(
        h.state()
            .events
            .contains(&ListEvent::Rename(2, "Lobe".into()))
    );
}

#[test]
fn escape_cancels_a_rename() {
    let mut h = harness();
    h.run();
    double_click(&mut h, "Region 1");
    h.run();
    h.run();
    h.get_by_role(Role::TextInput).type_text("Core");
    h.run();
    h.key_press(Key::Escape);
    h.run();
    assert!(
        !h.state()
            .events
            .iter()
            .any(|e| matches!(e, ListEvent::Rename(..)))
    );
    assert!(h.query_by_role(Role::TextInput).is_none());
    h.get_by_label("Region 1");
}

#[test]
fn empty_rename_is_refused() {
    let mut h = harness();
    h.run();
    double_click(&mut h, "Region 1");
    h.run();
    h.run();
    // The old name is selected: deleting it leaves nothing.
    h.key_press(Key::Backspace);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert!(
        !h.state()
            .events
            .iter()
            .any(|e| matches!(e, ListEvent::Rename(..)))
    );
    h.get_by_role(Role::TextInput);
}

#[test]
fn move_up_down_emit_moves() {
    let mut h = harness();
    h.run();
    h.hover_at(rect(&h, "Region 2").center());
    h.run();
    h.get_by_label("Move Region 2 up").click();
    h.run();
    h.hover_at(rect(&h, "Region 2").center());
    h.run();
    h.get_by_label("Move Region 2 down").click();
    h.run();
    let moves: Vec<_> = h
        .state()
        .events
        .iter()
        .filter(|e| matches!(e, ListEvent::Move { .. }))
        .cloned()
        .collect();
    assert_eq!(
        moves,
        vec![
            ListEvent::Move { id: 2, to: 0 },
            ListEvent::Move { id: 2, to: 2 }
        ]
    );
}

#[test]
fn the_ends_cannot_move_outwards() {
    let mut h = harness();
    h.run();
    h.hover_at(rect(&h, "Region 1").center());
    h.run();
    assert!(
        h.get_by_label("Move Region 1 up")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        !h.get_by_label("Move Region 1 down")
            .accesskit_node()
            .is_disabled()
    );
    h.hover_at(rect(&h, "Region 3").center());
    h.run();
    assert!(
        h.get_by_label("Move Region 3 down")
            .accesskit_node()
            .is_disabled()
    );
    assert!(
        !h.get_by_label("Move Region 3 up")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn actions_show_on_hover_or_focus_only() {
    let mut h = harness();
    h.run();
    assert!(h.query_by_label("Delete Region 1").is_none());
    h.hover_at(rect(&h, "Region 1").center());
    h.run();
    assert_eq!(h.query_all_by_label("Delete Region 1").count(), 1);
    h.hover_at(Vec2::new(390.0, 390.0).to_pos2());
    h.run();
    assert!(h.query_by_label("Delete Region 1").is_none());
    h.get_by_label("Region 3").focus();
    h.run();
    h.run();
    assert_eq!(h.query_all_by_label("Delete Region 3").count(), 1);
}

#[test]
fn delete_emits() {
    let mut h = harness();
    h.run();
    h.hover_at(rect(&h, "Region 3").center());
    h.run();
    h.get_by_label("Delete Region 3").click();
    h.run();
    assert_eq!(h.state().events, vec![ListEvent::Delete(3)]);
}

#[test]
fn drag_reorders() {
    let mut h = harness();
    h.run();
    let from = rect(&h, "Region 1");
    let below_last = rect(&h, "Region 3").center() + Vec2::new(0.0, 10.0);
    h.drag_at(from.center());
    h.run();
    h.hover_at(from.center() + Vec2::new(0.0, space::L));
    h.run();
    h.hover_at(below_last);
    h.run();
    h.drop_at(below_last);
    h.run();
    assert_eq!(
        h.state().events,
        vec![ListEvent::Move { id: 1, to: 2 }],
        "a drag is not a click"
    );
}

#[test]
fn dropping_where_it_was_moves_nothing() {
    let mut h = harness();
    h.run();
    let from = rect(&h, "Region 2");
    h.drag_at(from.center());
    h.run();
    h.hover_at(from.center() + Vec2::new(0.0, space::L));
    h.run();
    h.hover_at(from.center() + Vec2::new(0.0, 2.0));
    h.run();
    h.drop_at(from.center() + Vec2::new(0.0, 2.0));
    h.run();
    assert!(h.state().events.is_empty(), "{:?}", h.state().events);
}

#[test]
fn details_draw_under_the_selected_row() {
    let mut h = harness();
    h.state_mut().selected = Some(2);
    h.run();
    let row = rect(&h, "Region 2");
    let details = rect(&h, "details for 2");
    let next = rect(&h, "Region 3");
    assert!(details.top() >= row.bottom(), "{details:?} {row:?}");
    assert!(details.bottom() <= next.top() + 0.5, "{details:?} {next:?}");
    assert!(h.query_by_label("details for 1").is_none());
}

#[test]
fn row_details_text_shows() {
    let mut h = harness();
    h.run();
    let row = h.get_by_label("Region 1");
    assert_eq!(
        row.accesskit_node().description().as_deref(),
        Some("up to date")
    );
    assert!(
        h.get_by_label("Region 2")
            .accesskit_node()
            .description()
            .is_none()
    );
}

/// Two clicks in quick succession on the row labelled `label`.
fn double_click(h: &mut Harness<'_, State>, label: &str) {
    h.get_by_label(label).click();
    h.step();
    h.get_by_label(label).click();
    h.run();
}

#[test]
fn keyboard_activates_and_renames_a_focused_row() {
    let mut h = harness();
    h.run();
    h.get_by_label("Region 2").focus();
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(
        h.state().events,
        vec![ListEvent::Select(2), ListEvent::Activate(2)]
    );
    h.key_press(Key::F2);
    h.run();
    h.run();
    assert_eq!(h.state().list.editing(), Some(2));
    h.get_by_role(Role::TextInput);
}

fn renames(h: &Harness<'_, State>) -> Vec<ListEvent> {
    let renames = |e: &&ListEvent| matches!(e, ListEvent::Rename(..));
    h.state().events.iter().filter(renames).cloned().collect()
}

#[test]
fn blurring_an_empty_rename_cancels_it() {
    let mut h = harness();
    h.run();
    double_click(&mut h, "Region 1");
    h.run();
    h.run();
    h.key_press(Key::Backspace);
    h.run();
    // Focus goes elsewhere: the editor closes and the old name stays.
    h.get_by_label("Region 3").click();
    h.run();
    h.run();
    assert!(renames(&h).is_empty());
    assert!(h.query_by_role(Role::TextInput).is_none());
    assert_eq!(h.state().list.editing(), None);
    h.get_by_label("Region 1");
}

#[test]
fn blurring_a_rename_commits_it() {
    let mut h = harness();
    h.run();
    double_click(&mut h, "Region 1");
    h.run();
    h.run();
    h.get_by_role(Role::TextInput).type_text("Core");
    h.run();
    h.get_by_label("Region 3").click();
    h.run();
    h.run();
    assert_eq!(renames(&h), vec![ListEvent::Rename(1, "Core".into())]);
    assert!(h.query_by_role(Role::TextInput).is_none());
}

/// Rows 20 tall, 2 apart, from y = 0: centres 10, 32, 54, 76.
fn centres() -> Vec<f32> {
    (0..4).map(|i| 10.0 + 22.0 * i as f32).collect()
}

#[test]
fn dragging_up_lands_before_the_row_pointed_at() {
    // Row 3 (index 3) dragged to the top half of row 1 (index 1).
    assert_eq!(drop_index(&centres(), 3, 25.0), Some(1));
    // And to above everything.
    assert_eq!(drop_index(&centres(), 3, -5.0), Some(0));
}

#[test]
fn dragging_down_mid_list_lands_after_the_rows_passed() {
    // Row 0 dragged below row 1's centre, above row 2's: lands at index 1.
    assert_eq!(drop_index(&centres(), 0, 40.0), Some(1));
}

#[test]
fn dragging_to_the_end_lands_last() {
    assert_eq!(drop_index(&centres(), 0, 200.0), Some(3));
    assert_eq!(drop_index(&centres(), 2, 200.0), Some(3));
}

#[test]
fn dropping_in_its_own_slot_is_a_no_op() {
    // Index 1: just above or just below its own row.
    assert_eq!(drop_index(&centres(), 1, 20.0), None);
    assert_eq!(drop_index(&centres(), 1, 40.0), None);
    // The last row dragged to the end stays put.
    assert_eq!(drop_index(&centres(), 3, 200.0), None);
}

#[test]
fn the_insertion_line_sits_in_the_gap_it_lands_in() {
    let rects: Vec<Rect> = (0..4)
        .map(|i| {
            let top = 22.0 * i as f32;
            Rect::from_min_size(egui::pos2(0.0, top), Vec2::new(100.0, 20.0))
        })
        .collect();
    // Upward: row 3 to index 1: the gap between rows 0 and 1.
    assert_eq!(line_gap(&rects, 1, 3), 21.0);
    // Mid-list downward: row 0 to index 1: the gap after row 1.
    assert_eq!(line_gap(&rects, 1, 0), 43.0);
    // To the end: below the last row, to the top: above the first.
    assert_eq!(
        line_gap(&rects, 3, 0),
        rects[3].bottom() + crate::theme::tokens::space::XS / 2.0
    );
    assert_eq!(
        line_gap(&rects, 0, 2),
        -crate::theme::tokens::space::XS / 2.0
    );
}
