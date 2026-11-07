//! The gallery: every token and component of the Mega UI standard on one
//! scrolling page, the egui counterpart of the web's style guide page.
//! Look here before building anything new. `cargo run -p mega-ui --example
//! gallery` shows it in a window; the snapshot tests render it too.

use egui::{
    Color32, CornerRadius, FontId, Mesh, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind,
    TextStyle, Ui, Vec2,
};

use crate::brand::{self, BrandImages, LockupSize};
use crate::checks::{CheckItem, checks_list};
use crate::groups::GROUP_PALETTE;
use crate::icons;
use crate::theme::palette::*;
use crate::theme::text::{Type, text};
use crate::theme::tokens::{radius, space};
use crate::theme::{Severity, Tone};
use crate::widgets::{
    ButtonKind, MegaButton, Toast, Toasts, chip, icon_button, notes_field, overline, progress,
    property_grid, property_row, property_row_numeric, property_row_ui, segmented, status,
    text_input,
};

/// The gallery's interactive state.
pub struct GalleryState {
    segment: u8,
    name: String,
    notes: String,
    checks: Vec<CheckItem>,
    toasts: Toasts,
    brand: Option<BrandImages>,
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
            notes: "Shooting the 4th row next new moon.".into(),
            checks,
            toasts: Toasts::default(),
            brand: None,
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
            notes_field(ui, &mut state.notes, "Notes about this session", true);
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
        ui.add_space(space::XXL);
    });
    let _ = state.toasts.show(ui.ctx(), now);
}
