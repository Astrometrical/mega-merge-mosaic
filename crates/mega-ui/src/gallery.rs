//! The gallery: every token and component of the Mega UI standard on one
//! scrolling page, the egui counterpart of the web's style guide page.
//! Look here before building anything new. `cargo run -p mega-ui --example
//! gallery` shows it in a window; the snapshot tests render it too.

use std::collections::BTreeSet;
use std::sync::Arc;

use egui::{
    Color32, ColorImage, CornerRadius, FontId, Mesh, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, TextStyle, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2,
};

use crate::brand::{self, BrandImages, LockupSize};
use crate::checks::{CheckItem, checks_list};
use crate::groups::GROUP_PALETTE;
use crate::hero::{HeroStage, HeroView};
use crate::icons;
use crate::inspector::{Badge, InspectorState, Section, inspector, inspector_width};
use crate::starfield::Starfield;
use crate::table::{self, TableEvent, TableState, sectioned_table};
use crate::theme::palette::*;
use crate::theme::text::{Type, text};
use crate::theme::tokens::{radius, space};
use crate::theme::{Severity, Tone};
use crate::viewer::ImageTileSource;
use crate::widgets::{
    ButtonKind, ChoiceChip, MegaButton, NotesMode, SessionCardData, StepState, StripStep, Toast,
    Toasts, chip, choice_chip, icon_button, notes_view, overline, progress, property_grid,
    property_row, property_row_numeric, property_row_ui, segmented, session_card, status,
    step_strip, submenu, text_input,
};

/// The gallery's interactive state.
pub struct GalleryState {
    segment: u8,
    name: String,
    notes: String,
    notes_mode: NotesMode,
    inspector: InspectorState,
    table: TableState,
    table_selection: BTreeSet<u64>,
    checks: Vec<CheckItem>,
    toasts: Toasts,
    brand: Option<BrandImages>,
    starfield: Starfield,
    hero: Option<HeroStage>,
    portrait_hero: Option<HeroStage>,
    card_thumbs: Option<[TextureHandle; 2]>,
}

impl Default for GalleryState {
    fn default() -> Self {
        let mut checks = vec![
            CheckItem {
                severity: Severity::Problem,
                title: "Panel 5 has no astrometric solution".into(),
                detail: "Plate-solve it, then add it again. The rest of the plan is unaffected."
                    .into(),
                accepted: false,
                acceptable: true,
            },
            CheckItem {
                severity: Severity::Note,
                title: "OIII 13–15: strong background gradient".into(),
                detail: "May show as seams. Consider gradient removal before merging.".into(),
                accepted: false,
                acceptable: false,
            },
        ];
        checks.extend((1..=41).map(|i| CheckItem {
            severity: Severity::Passed,
            title: format!("Passed check {i}"),
            detail: String::new(),
            accepted: false,
            acceptable: false,
        }));
        GalleryState {
            segment: 0,
            name: "Carina Nebula SHO".into(),
            notes: "Shooting the 4th row next new moon.
Panel 7: gradient in the lower left, redo DBE."
                .into(),
            notes_mode: NotesMode::default(),
            inspector: {
                let mut s = InspectorState::default();
                s.toggle("problems");
                s
            },
            table: {
                // Sorted by integration.
                let mut t = TableState::default();
                t.sort_by(2);
                t
            },
            table_selection: BTreeSet::from([8]),
            checks,
            toasts: Toasts::default(),
            brand: None,
            starfield: Starfield::new(7),
            hero: None,
            portrait_hero: None,
            card_thumbs: None,
        }
    }
}

/// One gallery section under an overline heading.
fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(space::XL);
    overline(ui, title);
    ui.add_space(space::S);
    add(ui);
}

/// A colour swatch with its name below.
fn swatch(ui: &mut Ui, color: Color32, name: &str) {
    ui.vertical(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(84.0, 40.0), Sense::hover());
        ui.painter().rect(
            r,
            CornerRadius::same(radius::CONTROL),
            color,
            Stroke::new(1.0, BORDER),
            StrokeKind::Inside,
        );
        ui.label(text(Type::Numeric, name).color(TEXT_SUBTLE));
    });
}

/// A left-to-right gradient standing in for imagery behind ghost buttons.
fn imagery(ui: &Ui, rect: Rect) {
    let mut mesh = Mesh::default();
    let (l, r) = (ACCENT_600, HA_500);
    for (pos, c) in [
        (rect.left_top(), l),
        (rect.right_top(), r),
        (rect.right_bottom(), r),
        (rect.left_bottom(), l),
    ] {
        mesh.colored_vertex(pos, c);
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
}

/// A diagonal gradient image from `a` to `b`, standing in for a mosaic.
fn gradient_image(size: [usize; 2], a: Color32, b: Color32) -> ColorImage {
    let [w, h] = size;
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 / w as f32 + y as f32 / h as f32) / 2.0;
            pixels.push(a.lerp_to_gamma(b, t));
        }
    }
    ColorImage::new(size, pixels)
}

/// [`gradient_image`] with faint seams between `cols` × `rows` panels,
/// which show how much of the mosaic the hero crops.
fn panel_grid_image(size: [usize; 2], a: Color32, b: Color32, grid: [usize; 2]) -> ColorImage {
    let mut img = gradient_image(size, a, b);
    let [w, h] = size;
    let seam = |p: Color32| p.lerp_to_gamma(Color32::WHITE, 0.35);
    for y in 0..h {
        for x in 0..w {
            let on_col = (1..grid[0]).any(|c| x.abs_diff(c * w / grid[0]) < 1);
            let on_row = (1..grid[1]).any(|r| y.abs_diff(r * h / grid[1]) < 1);
            if on_col || on_row {
                img.pixels[y * w + x] = seam(img.pixels[y * w + x]);
            }
        }
    }
    img
}

/// A hero stage over two generated gradients of `size` pixels with seams
/// between `grid` panels, frozen at time zero so the gallery (and its
/// snapshots) stay still.
fn gallery_hero(size: [usize; 2], grid: [usize; 2]) -> HeroStage {
    let view = |label: &str, a: Color32, b: Color32| HeroView {
        label: label.to_string(),
        source: Arc::new(ImageTileSource::new(
            panel_grid_image(size, a, b, grid),
            256,
        )),
    };
    let mut stage = HeroStage::new_synchronous();
    stage.set_views(
        vec![
            view("Ha", ACCENT_600, HA_500),
            view("OIII", SURFACE_700, ACCENT_500),
        ],
        0,
        0.0,
    );
    stage
}

/// Draw `hero` as the start screen does, full bleed in a 1000 × 320 strip
/// whose left third is the content column.
fn hero_section(ui: &mut Ui, hero: &mut HeroStage) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1000.0, 320.0), Sense::hover());
    // Constant time keeps the stage on its first view, mid-drift.
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(rect);
        hero.show(ui, rect.left() + rect.width() / 3.0, 0.0);
    });
}

/// The table section, sorted by integration at first: a rolled-up group
/// with a problem row, and a group with a warning, a highlighted row and
/// a row moved by hand. Clicks select (Shift or Ctrl add); headings sort.
fn table_section(ui: &mut Ui, state: &mut TableState, selection: &mut BTreeSet<u64>) {
    let column = |title, numeric| table::Column { title, numeric };
    let columns = [
        column("Panel", false),
        column("Filter", false),
        column("Integration", true),
        column("Scale", true),
    ];
    // Integration in minutes, shown as "3h 05m".
    let row = |id: u64, filter: &str, minutes: u32| table::Row {
        id,
        cells: vec![
            format!("Carina_P{id}.xisf"),
            filter.to_owned(),
            format!("{}h {:02}m", minutes / 60, minutes % 60),
            "1.21″".to_owned(),
        ],
        problem: None,
        tag: None,
        selected: selection.contains(&id),
        highlighted: false,
        sort_keys: vec![
            table::SortKey::Text(format!("Carina_P{id}.xisf")),
            table::SortKey::Text(filter.to_owned()),
            table::SortKey::Number(f64::from(minutes)),
            table::SortKey::Number(1.21),
        ],
    };
    let minutes = [185, 170, 200, 185, 0, 155];
    let mut ha: Vec<_> = (1..=6)
        .map(|i| row(i, "Ha", minutes[i as usize - 1]))
        .collect();
    ha[4].problem = Some("No astrometric solution: plate-solve it, then add it again");
    ha[4].cells[2] = "—".to_owned();
    ha[4].sort_keys[2] = table::SortKey::Unknown;
    let mut oiii: Vec<_> = (7..=9).map(|i| row(i, "OIII", 200)).collect();
    oiii[0].highlighted = true;
    oiii[2].tag = Some("by hand");
    oiii[2].cells[1] = "Ha".to_owned();
    let heading = |id, title: &str, color| table::Heading {
        id,
        title: title.to_owned(),
        color: Some(color),
        warning: None,
        rolls_up: true,
        drop_target: true,
    };
    let sections = [
        table::Section {
            heading: Some(heading("ha", "Ha · 6 · 15h 25m", GROUP_PALETTE[0])),
            rows: ha,
        },
        table::Section {
            heading: Some(table::Heading {
                warning: Some("3 fewer than Ha".to_owned()),
                ..heading("oiii", "OIII · 3 · 10h 00m", GROUP_PALETTE[2])
            }),
            rows: oiii,
        },
    ];
    let events = sectioned_table(ui, state, &columns, &sections, |ui, _id| {
        submenu(ui, "Move to", |ui| {
            for group in ["Ha", "OIII"] {
                if ui.button(group).clicked() {
                    ui.close();
                }
            }
        });
        let _ = ui.button("Remove");
    });
    for event in events {
        if let TableEvent::Click { id, modifiers } = event {
            if modifiers.shift || modifiers.command || modifiers.ctrl {
                if !selection.remove(&id) {
                    selection.insert(id);
                }
            } else {
                *selection = BTreeSet::from([id]);
            }
        }
    }
}

/// Draw the gallery.
pub fn show(ui: &mut Ui, state: &mut GalleryState) {
    let now = ui.input(|i| i.time);
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.set_max_width(1100.0);

        section(ui, "Type scale", |ui| {
            for (t, spec, sample) in [
                (Type::Hero, "hero · 32 / semibold", "Carina Nebula SHO"),
                (Type::Title, "title · 20 / semibold", "Mosaic plan"),
                (Type::Heading, "heading · 15 / semibold", "Overlap analysis"),
                (
                    Type::Body,
                    "body · 13",
                    "18 panels registered to a shared reference frame.",
                ),
                (Type::Small, "small · 12", "Opened 2 days ago"),
                (Type::Overline, "overline · 10.5 caps", "Recent sessions"),
                (
                    Type::Numeric,
                    "numeric · mono 12",
                    "24 310 × 15 870 px · 1.62″/px",
                ),
            ] {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(180.0, 18.0), Sense::hover());
                    ui.painter().text(
                        r.left_center(),
                        egui::Align2::LEFT_CENTER,
                        spec,
                        FontId::monospace(11.0),
                        TEXT_SUBTLE,
                    );
                    ui.label(text(t, sample));
                });
            }
        });

        section(ui, "Text roles", |ui| {
            for (c, role) in [
                (TEXT_STRONG, "strong #eee: headings, primary values"),
                (
                    TEXT_EMPHASIS,
                    "emphasis #d0d0d0: lead text, property values",
                ),
                (TEXT_BODY, "body #aaa: default"),
                (TEXT_MUTED, "muted #999: secondary, labels"),
                (TEXT_SUBTLE, "subtle #8a8a8a: captions, placeholders"),
            ] {
                ui.label(RichText::new(role).color(c));
            }
        });

        section(ui, "Surfaces", |ui| {
            ui.horizontal(|ui| {
                swatch(ui, SURFACE_900, "900 window");
                swatch(ui, SURFACE_800, "800 panel");
                swatch(ui, SURFACE_700, "700 raised");
                swatch(ui, BORDER, "600 border");
                swatch(ui, BORDER_STRONG, "500 strong");
            });
        });

        section(ui, "Semantic colours", |ui| {
            ui.horizontal(|ui| {
                swatch(ui, ACCENT_500, "accent OIII");
                swatch(ui, GREEN_300, "passed");
                swatch(ui, SII_400, "warn SII");
                swatch(ui, HA_500, "problem Hα");
                swatch(ui, HA_400, "Hα text");
            });
        });

        section(ui, "Group palette", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, c) in GROUP_PALETTE.iter().enumerate() {
                    swatch(ui, *c, &format!("slot {i}"));
                }
            });
        });

        section(ui, "Buttons", |ui| {
            ui.horizontal(|ui| {
                ui.add(MegaButton::new(ButtonKind::Primary, "New mosaic").icon(icons::PLUS));
                ui.add(MegaButton::new(ButtonKind::Secondary, "Secondary"));
                ui.add(MegaButton::new(ButtonKind::Quiet, "Quiet"));
                ui.add(MegaButton::new(ButtonKind::Danger, "Remove panel"));
                ui.add_enabled(false, MegaButton::new(ButtonKind::Secondary, "Disabled"));
                ui.add(icon_button(icons::PIN, "Pin session"));
                ui.add(icon_button(icons::SEARCH, "Search"));
            });
            ui.add_space(space::S);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 66.0), Sense::hover());
            imagery(ui, rect);
            let inner = rect.shrink(space::L);
            ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                ui.horizontal(|ui| {
                    ui.add(MegaButton::new(ButtonKind::Primary, "Resume session").large());
                    ui.add(MegaButton::new(ButtonKind::Ghost, "Ghost, over imagery"));
                });
            });
        });

        section(ui, "Inputs", |ui| {
            text_input(ui, &mut state.name, "Session name");
            ui.add_space(space::S);
            segmented(
                ui,
                &mut state.segment,
                &[(0, "Composite"), (1, "Hα"), (2, "OIII"), (3, "SII")],
            );
            ui.add_space(space::S);
            let (notes, mode) = (&mut state.notes, &mut state.notes_mode);
            notes_view(ui, notes, mode, "Notes about this session", true);
        });

        section(ui, "Chips", |ui| {
            ui.horizontal(|ui| {
                chip(ui, "18 panels", None);
                chip(ui, "Hα · 6", Some(GROUP_PALETTE[0]));
                chip(ui, "OIII · 6", Some(GROUP_PALETTE[2]));
                chip(ui, "SII · 6", Some(GROUP_PALETTE[3]));
                chip(ui, "L", None);
                chip(ui, "OSC", None);
                chip(ui, "Ha_3nm_2026-10-12_flat-corrected_master_light", None);
            });
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                for (label, color, selected, dashed) in [
                    ("Hα", Some(GROUP_PALETTE[0]), true, false),
                    ("OIII", Some(GROUP_PALETTE[2]), false, false),
                    ("Composite", None, false, true),
                    ("Composite", None, true, true),
                ] {
                    choice_chip(
                        ui,
                        &ChoiceChip {
                            label,
                            color,
                            selected,
                            dashed,
                        },
                    );
                }
            });
        });

        section(ui, "Step strip", |ui| {
            let step = |label: &str, state| StripStep {
                label: label.to_owned(),
                state,
                cancellable: false,
                tooltip: None,
            };
            step_strip(
                ui,
                &[
                    step("Todo", StepState::Todo),
                    step("Next", StepState::Next),
                    step("Done", StepState::Done),
                    step("Warn", StepState::Warn),
                    StripStep {
                        cancellable: true,
                        ..step(
                            "Merging",
                            StepState::Working {
                                fraction: Some(0.43),
                            },
                        )
                    },
                ],
            );
        });

        section(ui, "Inspector", |ui| {
            let sections = [
                Section {
                    id: "display",
                    title: "Display",
                    badge: None,
                },
                Section {
                    id: "problems",
                    title: "Problems",
                    badge: Some(Badge {
                        text: "2".to_owned(),
                        tone: Tone::Warning,
                    }),
                },
                Section {
                    id: "notes",
                    title: "Notes",
                    badge: None,
                },
            ];
            let width = inspector_width(&state.inspector);
            ui.allocate_ui(Vec2::new(width, 240.0), |ui| {
                inspector(ui, &mut state.inspector, &sections, |ui, id| {
                    ui.label(format!("{id} body"));
                });
            });
        });

        section(ui, "Table", |ui| {
            ui.scope(|ui| {
                ui.set_max_width(640.0);
                table_section(ui, &mut state.table, &mut state.table_selection);
            });
        });

        section(ui, "Status", |ui| {
            ui.horizontal(|ui| {
                status(ui, Tone::Success, "Merged");
                status(ui, Tone::Info, "Plan ready");
                status(ui, Tone::Warning, "Re-merge needed");
                status(ui, Tone::Problem, "1 problem");
                status(ui, Tone::Neutral, "No panels");
            });
        });

        section(ui, "Check list", |ui| {
            ui.set_max_width(560.0);
            let _ = checks_list(ui, "Checks", &state.checks, None);
        });

        section(ui, "Property grid", |ui| {
            property_grid(ui, "gallery-props", |ui| {
                property_row_numeric(ui, "Canvas", "24 310 × 15 870 px");
                property_row_numeric(ui, "Scale", "1.62″/px");
                property_row(ui, "Exports", "3 groups, 1 composite");
                property_row_ui(ui, "Merged", |ui| {
                    status(ui, Tone::Warning, "2 panels added since");
                });
            });
        });

        section(ui, "Progress", |ui| {
            ui.set_max_width(560.0);
            progress(ui, "Merging OIII", Some(0.62), "62% · 0:41 left");
            ui.add_space(space::S);
            progress(ui, "Reading headers", None, "12 files");
        });

        section(ui, "Toasts", |ui| {
            if ui
                .add(MegaButton::new(ButtonKind::Secondary, "Show a toast"))
                .clicked()
            {
                state.toasts.push(
                    Toast {
                        tone: Tone::Success,
                        title: "Export finished".into(),
                        detail: Some("4 files · D:\\Astro\\Carina\\export".into()),
                        action: Some("Show".into()),
                    },
                    now,
                );
            }
        });

        section(ui, "Icons", |ui| {
            egui::Grid::new("gallery-icons")
                .num_columns(6)
                .spacing(Vec2::new(space::XL, space::S))
                .show(ui, |ui| {
                    for (i, (name, glyph)) in icons::ALL.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(icons::rich(*glyph, 18.0).color(TEXT_STRONG));
                            ui.label(
                                RichText::new(*name)
                                    .text_style(TextStyle::Small)
                                    .color(TEXT_MUTED),
                            );
                        });
                        if i % 6 == 5 {
                            ui.end_row();
                        }
                    }
                });
        });

        section(ui, "Brand lockup", |ui| {
            let images = state.brand.get_or_insert_with(|| {
                BrandImages::load(ui.ctx(), brand::MEGA_MERGE_MOSAIC_PNG, "Mega Merge Mosaic")
            });
            brand::lockup(ui, images, LockupSize::Compact);
            ui.add_space(space::L);
            brand::lockup(ui, images, LockupSize::Large);
        });

        section(ui, "Starfield", |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(560.0, 160.0), Sense::hover());
            ui.painter().rect_filled(rect, 0.0, SURFACE_900);
            state
                .starfield
                .paint(&ui.painter().with_clip_rect(rect), rect);
        });

        section(ui, "Hero stage", |ui| {
            let hero = state
                .hero
                .get_or_insert_with(|| gallery_hero([640, 480], [1, 1]));
            hero_section(ui, hero);
        });

        // A portrait mosaic of 2 × 4 panels (the aspect of 9255 × 18310) in
        // the landscape image area: it fills the area, cropped top and
        // bottom, with no hard edges.
        section(ui, "Hero stage · portrait mosaic", |ui| {
            let hero = state
                .portrait_hero
                .get_or_insert_with(|| gallery_hero([243, 480], [2, 4]));
            hero_section(ui, hero);
        });

        section(ui, "Session cards", |ui| {
            let ctx = ui.ctx().clone();
            let thumbs = state.card_thumbs.get_or_insert_with(|| {
                let tex = |name: &str, a, b| {
                    ctx.load_texture(
                        name,
                        gradient_image([200, 80], a, b),
                        TextureOptions::LINEAR,
                    )
                };
                [
                    tex("gallery-card-a", ACCENT_600, HA_500),
                    tex("gallery-card-b", SURFACE_700, ACCENT_500),
                ]
            });
            ui.horizontal_wrapped(|ui| {
                let status = Some((Tone::Success, "Merged"));
                let base = |title, thumb| SessionCardData {
                    title,
                    status,
                    pinned: false,
                    dimmed: false,
                    selected: false,
                    thumbnail: Some(thumb),
                };
                let _ = session_card(ui, &base("Normal", &thumbs[0]));
                let _ = session_card(
                    ui,
                    &SessionCardData {
                        selected: true,
                        ..base("Selected", &thumbs[1])
                    },
                );
                let _ = session_card(
                    ui,
                    &SessionCardData {
                        pinned: true,
                        ..base("Pinned", &thumbs[0])
                    },
                );
                let _ = session_card(
                    ui,
                    &SessionCardData {
                        dimmed: true,
                        status: Some((Tone::Warning, "Missing")),
                        ..base("Dimmed", &thumbs[1])
                    },
                );
            });
        });
        ui.add_space(space::XXL);
    });
    let _ = state.toasts.show(ui.ctx(), now);
}
