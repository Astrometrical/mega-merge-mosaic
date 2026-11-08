use egui::accesskit::Role;
use egui::{Modifiers, Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;

struct State {
    table: TableState,
    columns: Vec<Column<'static>>,
    sections: Vec<Section<'static>>,
    /// Every event so far.
    all: Vec<TableEvent>,
    /// The events of the last frame.
    last: Vec<TableEvent>,
}

fn columns() -> Vec<Column<'static>> {
    vec![
        Column {
            title: "Panel",
            numeric: false,
        },
        Column {
            title: "Scale",
            numeric: true,
        },
        Column {
            title: "Filter",
            numeric: false,
        },
    ]
}

fn row(id: u64, name: &str, scale: &str, filter: &str) -> Row<'static> {
    Row {
        id,
        cells: vec![name.to_owned(), scale.to_owned(), filter.to_owned()],
        problem: None,
        tag: None,
        selected: false,
        highlighted: false,
    }
}

fn heading(id: &'static str, title: &str) -> Heading<'static> {
    Heading {
        id,
        title: title.to_owned(),
        color: None,
        warning: None,
        rolls_up: false,
        drop_target: false,
    }
}

/// Section "ha" ("Ha · 2": a1, a2) and section "oiii" ("OIII · 1": b1).
fn two_sections() -> Vec<Section<'static>> {
    vec![
        Section {
            heading: Some(heading("ha", "Ha · 2")),
            rows: vec![row(1, "a1", "1.5", "Ha"), row(2, "a2", "12.25", "Ha")],
        },
        Section {
            heading: Some(heading("oiii", "OIII · 1")),
            rows: vec![row(3, "b1", "123.125", "OIII")],
        },
    ]
}

/// One rolled-up section "ha" of ten rows p0..p9; p7 has a problem.
fn rolled_up() -> Vec<Section<'static>> {
    let rows = (0..10)
        .map(|i| Row {
            problem: (i == 7).then_some("No astrometric solution"),
            ..row(i, &format!("p{i}"), "1.21", "Ha")
        })
        .collect();
    vec![Section {
        heading: Some(Heading {
            rolls_up: true,
            ..heading("ha", "Ha · 10")
        }),
        rows,
    }]
}

fn harness(sections: Vec<Section<'static>>) -> Harness<'static, State> {
    Harness::builder()
        .with_size(Vec2::new(600.0, 600.0))
        .build_ui_state(
            |ui, s: &mut State| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let events =
                    sectioned_table(ui, &mut s.table, &s.columns, &s.sections, |ui, id| {
                        ui.label(format!("menu for {id}"));
                    });
                s.all.extend(events.iter().cloned());
                s.last = events;
            },
            State {
                table: TableState::default(),
                columns: columns(),
                sections,
                all: Vec::new(),
                last: Vec::new(),
            },
        )
}

/// The rect of the painted text `s`, if any is painted this frame.
fn text_rect(h: &Harness<'_, State>, s: &str) -> Option<Rect> {
    h.output().shapes.iter().find_map(|c| match &c.shape {
        egui::Shape::Text(t) if t.galley.text() == s => {
            Some(Rect::from_min_size(t.pos, t.galley.size()))
        }
        _ => None,
    })
}

/// The font the painted text `s` uses.
fn text_font(h: &Harness<'_, State>, s: &str) -> egui::FontId {
    h.output()
        .shapes
        .iter()
        .find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text() == s => {
                Some(t.galley.job.sections[0].format.font_id.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text {s:?} painted"))
}

#[test]
fn headings_and_rows_draw_in_order() {
    let mut h = harness(two_sections());
    h.run();
    let header = text_rect(&h, "PANEL").expect("column heading PANEL");
    let mut last = header.min.y;
    for l in ["Ha · 2", "a1", "a2", "OIII · 1", "b1"] {
        let y = h.get_by_label(l).rect().min.y;
        assert!(y > last, "{l} at {y} after {last}");
        last = y;
    }
    // Headings are buttons; rows are selectable list items.
    h.get_by_role_and_label(Role::Button, "Ha · 2");
    h.get_by_role_and_label(Role::ListItem, "a1");
}

#[test]
fn numeric_columns_align_right() {
    let mut h = harness(two_sections());
    h.run();
    let header = text_rect(&h, "SCALE").expect("column heading SCALE");
    for n in ["1.5", "12.25", "123.125"] {
        let r = text_rect(&h, n).unwrap_or_else(|| panic!("no cell {n}"));
        assert!(
            (r.right() - header.right()).abs() < 0.5,
            "{n} ends at {}, the heading at {}",
            r.right(),
            header.right()
        );
    }
    let numeric = Type::Numeric.text_style().resolve(&h.ctx.global_style());
    assert_eq!(text_font(&h, "12.25"), numeric, "numbers in the mono face");
    // Text columns align left.
    let filter = text_rect(&h, "FILTER").expect("column heading FILTER");
    for t in ["Ha", "OIII"] {
        let r = text_rect(&h, t).unwrap_or_else(|| panic!("no cell {t}"));
        assert!((r.left() - filter.left()).abs() < 0.5, "{t} at {r:?}");
    }
}

#[test]
fn rolled_up_sections_show_their_first_rows_and_problems() {
    let mut h = harness(rolled_up());
    h.run();
    for i in [0, 1, 2, 7] {
        h.get_by_label(&format!("p{i}"));
    }
    for i in [3, 4, 5, 6, 8, 9] {
        assert!(h.query_by_label(&format!("p{i}")).is_none(), "p{i} shown");
    }
    h.get_by_role_and_label(Role::Button, "… 6 more");
    // The problem row carries the warning icon.
    let p7 = h.get_by_label("p7").rect();
    let icon = text_rect(&h, &icons::TRIANGLE_ALERT.to_string()).expect("problem icon");
    assert!(p7.contains_rect(icon), "{icon:?} not in {p7:?}");
}

#[test]
fn the_more_row_expands_the_section() {
    let mut h = harness(rolled_up());
    h.run();
    h.get_by_label("… 6 more").click();
    h.run();
    for i in 0..10 {
        h.get_by_label(&format!("p{i}"));
    }
    assert!(h.query_by_label("… 6 more").is_none());
    assert!(h.state().table.is_expanded("ha"));
}

#[test]
fn a_heading_folds_its_rows() {
    let mut h = harness(two_sections());
    h.run();
    h.get_by_label("Ha · 2").click();
    h.run();
    assert!(h.query_by_label("a1").is_none());
    assert!(h.query_by_label("a2").is_none());
    h.get_by_label("b1");
    assert!(h.state().table.is_folded("ha"));
    h.get_by_label("Ha · 2").click();
    h.run();
    h.get_by_label("a1");
    assert!(!h.state().table.is_folded("ha"));
}

#[test]
fn clicks_report_their_modifiers() {
    let mut h = harness(two_sections());
    h.run();
    h.get_by_label("a1").click();
    h.run();
    h.get_by_label("a2").click_modifiers(Modifiers::SHIFT);
    h.run();
    h.get_by_label("b1").click_modifiers(Modifiers::CTRL);
    h.run();
    let clicks: Vec<_> = h
        .state()
        .all
        .iter()
        .filter(|e| matches!(e, TableEvent::Click { .. }))
        .cloned()
        .collect();
    assert_eq!(
        clicks,
        vec![
            TableEvent::Click {
                id: 1,
                modifiers: Modifiers::NONE
            },
            TableEvent::Click {
                id: 2,
                modifiers: Modifiers::SHIFT
            },
            TableEvent::Click {
                id: 3,
                modifiers: Modifiers::CTRL
            },
        ]
    );
}

#[test]
fn hovering_a_row_reports_it() {
    let mut h = harness(two_sections());
    h.run();
    let a2 = h.get_by_label("a2").rect();
    h.hover_at(a2.center());
    h.run();
    let hovers: Vec<_> = h
        .state()
        .last
        .iter()
        .filter(|e| matches!(e, TableEvent::Hover(_)))
        .collect();
    assert_eq!(hovers, vec![&TableEvent::Hover(2)]);
    // A heading is not a row.
    let heading = h.get_by_label("OIII · 1").rect();
    h.hover_at(heading.center());
    h.run();
    assert!(
        !h.state()
            .last
            .iter()
            .any(|e| matches!(e, TableEvent::Hover(_)))
    );
}

#[test]
fn the_row_menu_opens_from_its_button() {
    let mut h = harness(two_sections());
    h.run();
    assert!(
        h.query_by_label("Panel actions").is_none(),
        "hidden at rest"
    );
    let a1 = h.get_by_label("a1").rect();
    h.hover_at(a1.center());
    h.run();
    let button = h.get_by_label("Panel actions");
    assert!(
        a1.contains_rect(button.rect()),
        "the button sits in its row"
    );
    button.click();
    h.run();
    h.get_by_label("menu for 1");
    assert!(
        !h.state()
            .all
            .iter()
            .any(|e| matches!(e, TableEvent::Click { .. })),
        "the button is not a row click"
    );
}

#[test]
fn the_row_menu_shows_while_its_row_has_focus() {
    let mut h = harness(two_sections());
    h.run();
    h.get_by_label("b1").focus();
    h.run();
    let b1 = h.get_by_label("b1").rect();
    let button = h.get_by_label("Panel actions");
    assert!(b1.contains_rect(button.rect()));
}

#[test]
fn dragging_rows_onto_a_heading_drops_them() {
    let mut sections = two_sections();
    for s in &mut sections {
        if let Some(hd) = &mut s.heading {
            hd.drop_target = true;
        }
    }
    sections[0].rows[0].selected = true;
    sections[0].rows[1].selected = true;
    let mut h = harness(sections);
    h.run();

    let drag = |h: &mut Harness<'_, State>, from: Rect, to: Rect| {
        h.drag_at(from.center());
        h.run();
        h.hover_at(from.center() + Vec2::new(0.0, space::L));
        h.run();
        h.hover_at(to.center());
        h.run();
        h.drop_at(to.center());
        h.run();
    };

    // A selected row carries the whole selection.
    let a1 = h.get_by_label("a1").rect();
    let oiii = h.get_by_label("OIII · 1").rect();
    drag(&mut h, a1, oiii);
    // An unselected row carries itself.
    let b1 = h.get_by_label("b1").rect();
    let ha = h.get_by_label("Ha · 2").rect();
    drag(&mut h, b1, ha);

    let drops: Vec<_> = h
        .state()
        .all
        .iter()
        .filter(|e| matches!(e, TableEvent::Drop { .. }))
        .cloned()
        .collect();
    assert_eq!(
        drops,
        vec![
            TableEvent::Drop {
                ids: vec![1, 2],
                section: "oiii".to_owned()
            },
            TableEvent::Drop {
                ids: vec![3],
                section: "ha".to_owned()
            },
        ]
    );
    assert!(
        !h.state()
            .all
            .iter()
            .any(|e| matches!(e, TableEvent::Click { .. })),
        "a drag is not a click"
    );
}

#[test]
fn headings_without_drop_target_take_no_drops() {
    let mut h = harness(two_sections());
    h.run();
    let a1 = h.get_by_label("a1").rect();
    let oiii = h.get_by_label("OIII · 1").rect();
    h.drag_at(a1.center());
    h.run();
    h.hover_at(pos2(a1.center().x, oiii.center().y));
    h.run();
    h.drop_at(oiii.center());
    h.run();
    assert!(
        !h.state()
            .all
            .iter()
            .any(|e| matches!(e, TableEvent::Drop { .. }))
    );
}
