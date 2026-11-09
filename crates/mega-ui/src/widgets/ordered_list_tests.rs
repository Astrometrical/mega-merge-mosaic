use egui::accesskit::Role;
use egui::{Key, Rect, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::ordered_list::{ListEvent, ListRow, ListState, ordered_list_with};
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
    h.get_by_label("Rename").click();
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
    h.get_by_label("Move up").click();
    h.run();
    h.hover_at(rect(&h, "Region 2").center());
    h.run();
    h.get_by_label("Move down").click();
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
    assert!(h.get_by_label("Move up").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Move down").accesskit_node().is_disabled());
    h.hover_at(rect(&h, "Region 3").center());
    h.run();
    assert!(h.get_by_label("Move down").accesskit_node().is_disabled());
    assert!(!h.get_by_label("Move up").accesskit_node().is_disabled());
}

#[test]
fn actions_show_on_hover_or_focus_only() {
    let mut h = harness();
    h.run();
    assert!(h.query_by_label("Delete").is_none());
    h.hover_at(rect(&h, "Region 1").center());
    h.run();
    assert_eq!(h.query_all_by_label("Delete").count(), 1);
    h.hover_at(Vec2::new(390.0, 390.0).to_pos2());
    h.run();
    assert!(h.query_by_label("Delete").is_none());
    h.get_by_label("Region 3").focus();
    h.run();
    h.run();
    assert_eq!(h.query_all_by_label("Delete").count(), 1);
}

#[test]
fn delete_emits() {
    let mut h = harness();
    h.run();
    h.hover_at(rect(&h, "Region 3").center());
    h.run();
    h.get_by_label("Delete").click();
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
