//! Drawing the start screen (spec §5.2): the hero stage over the whole
//! window, the brand lockup at the top left, the session block on the
//! left, the shelf (or the All sessions grid) along the bottom, and the
//! first-run screen. What the user does is collected as [`Cmd`]s, applied
//! by the screen after the frame.

use std::path::{Path, PathBuf};

use egui::{
    Align, Color32, Id, Key, Label, Layout, Rect, ScrollArea, TextStyle, Ui, UiBuilder, Vec2,
};
use egui::{pos2, vec2};
use mega_ui::brand::{BrandImages, LockupSize, lockup};
use mega_ui::groups::{GROUP_PALETTE, physical_color};
use mega_ui::icons;
use mega_ui::theme::Tone;
use mega_ui::theme::palette::{SURFACE_900, TEXT_EMPHASIS};
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::{CONTROL_HEIGHT, CONTROL_HEIGHT_LARGE, space};
use mega_ui::widgets::{
    ButtonKind, CARD_HEIGHT, CARD_WIDTH, MegaButton, SessionCardData, chip, icon_button_of,
    notes_field, overline, session_card, status, text_input,
};
use mmm_core::mosaic::SINGLE_GROUP;

use super::facts::{Availability, CardFacts};
use super::{
    Cmd, DialogPurpose, Drawn, StartScreen, Thumb, ago, card_look, count, display_path,
    expand_home, status_line, unix_now,
};
use crate::session::SESSION_EXT;

/// Widest the session block grows, in points.
const BLOCK_MAX_WIDTH: f32 = 480.0;
/// Where the session block starts, as a fraction of the window's height.
const BLOCK_TOP: f32 = 0.25;
/// How far above the window the hero image starts, as a fraction of its
/// height.
const IMAGE_OVERHANG: f32 = 0.02;
/// How far above the window's bottom the hero image ends, as a fraction of
/// its height; higher where the shelf reaches above that, so the hero's
/// label and controls never sit under the cards.
const IMAGE_BOTTOM_INSET: f32 = 0.18;
/// Width of the first-run column.
const FIRST_RUN_WIDTH: f32 = 480.0;
/// How far above the window's centre the first-run column starts.
const FIRST_RUN_LIFT: f32 = 140.0;
/// The large lockup's logo height (as [`LockupSize::Large`] documents).
const LARGE_LOGO_H: f32 = 46.0;
/// The large lockup's name height (as [`LockupSize::Large`] documents).
const LARGE_NAME_H: f32 = 22.0;
/// What mmm does, on the first-run screen.
const FIRST_RUN_LINE: &str = "Merge your stacked panels into one seamless mosaic.";
/// The notes field's hint.
const NOTES_HINT: &str = "Notes about this session";

impl StartScreen {
    /// Draw the whole screen in `ui.max_rect()`; `target` is the session
    /// the hero and the block show.
    pub(super) fn draw(&mut self, ui: &mut Ui, target: Option<&Path>, now: f64, out: &mut Drawn) {
        let full = ui.max_rect();
        let first_run = self.library.entries().is_empty();
        let bottom = self.bottom_area(full);
        let image = image_rect(full, (!first_run).then_some(bottom.top()));
        // The night sky the starfield and the image sit on, whatever the
        // screen is drawn in.
        ui.painter().rect_filled(full, 0.0, SURFACE_900);
        ui.scope_builder(UiBuilder::new().max_rect(full), |ui| {
            self.stage.show(ui, image, now);
        });
        if first_run {
            self.first_run(ui, full, out);
            return;
        }
        if let Some(images) = &self.brand {
            let at = full.min + Vec2::splat(space::XXL);
            let rect = Rect::from_min_size(at, vec2(full.width() - 2.0 * space::XXL, LARGE_LOGO_H));
            lockup(
                &mut ui.new_child(UiBuilder::new().max_rect(rect)),
                images,
                LockupSize::Compact,
            );
        }
        if let Some(target) = target {
            let left = full.left() + space::XXL;
            let width = BLOCK_MAX_WIDTH.min(full.width() - 2.0 * space::XXL);
            // A quarter down, or higher when the window is too short for
            // the block, but under the lockup; it scrolls if still short.
            let end = bottom.top() - space::L;
            let top = (full.top() + BLOCK_TOP * full.height())
                .min(end - self.block_height)
                .max(full.top() + space::XXL + LARGE_LOGO_H + space::L);
            let end = end.max(top + CONTROL_HEIGHT_LARGE);
            let rect = Rect::from_min_max(pos2(left, top), pos2(left + width, end));
            self.block(ui, rect, target, now, out);
        }
        if self.all_sessions {
            self.all_grid(ui, bottom, target, out);
        } else {
            self.shelf(ui, bottom, target, out);
        }
    }

    /// The shelf's (or the grid's) area along the bottom: a header row and
    /// one card row; the grid takes a second row where the block leaves
    /// room.
    fn bottom_area(&self, full: Rect) -> Rect {
        let one_row = CONTROL_HEIGHT + space::S + CARD_HEIGHT;
        let height = if self.all_sessions {
            let two_rows = one_row + space::M + CARD_HEIGHT;
            let room = full.bottom() - space::XXL - (self.block_bottom + space::XL);
            two_rows.min(room).max(one_row)
        } else {
            one_row
        };
        let bottom = full.bottom() - space::XXL;
        Rect::from_min_max(
            pos2(full.left() + space::XXL, bottom - height),
            pos2(full.right() - space::XXL, bottom),
        )
    }

    /// The session block in `rect`, scrolling when the window is too short.
    fn block(&mut self, ui: &mut Ui, rect: Rect, target: &Path, now: f64, out: &mut Drawn) {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        let mut height = 0.0;
        ScrollArea::vertical()
            .id_salt("start-block")
            .max_height(rect.height())
            .auto_shrink([false, true])
            .show(&mut child, |ui| {
                self.block_contents(ui, target, now, out);
                height = ui.min_rect().height();
            });
        self.block_height = height;
        self.block_bottom = rect.top() + height;
    }

    /// Overline, name, when opened and where, chips, status, checks,
    /// notes and the buttons.
    fn block_contents(&mut self, ui: &mut Ui, target: &Path, now: f64, out: &mut Drawn) {
        ui.spacing_mut().item_spacing.y = space::S;
        let enabled = self.enabled();
        let is_default = self.default_hero().as_deref() == Some(target);
        overline(
            ui,
            if is_default {
                "Continue where you left off"
            } else {
                "Session"
            },
        );
        ui.add(Label::new(text(Type::Hero, self.name_of(target))).truncate());
        if let Some(e) = self.library.entries().iter().find(|e| e.path == target) {
            let opened = ago(unix_now(), e.last_opened);
            let meta = format!("Opened {opened} · {}", display_path(target));
            ui.add(Label::new(text(Type::Small, meta)).truncate());
        }
        let facts = self.facts.get(target);
        if let Some(f) = facts {
            ui.add_space(space::XS);
            chips(ui, f);
            status_rows(ui, f);
        }
        let availability = facts.map(|f| f.availability.clone());
        let available = availability == Some(Availability::Available);
        let busy = availability == Some(Availability::Busy);
        if available || busy {
            // A busy session's notes belong to the other window: read-only.
            let editable = available && self.notes.loaded && enabled;
            // One id per session: undo never brings another session's text.
            let id = Id::new("mmm-start-notes").with(target);
            let r = stable(ui, id, |ui| {
                notes_field(ui, &mut self.notes.text, NOTES_HINT, editable)
            });
            let r = if busy {
                r.on_hover_text("Open in another window: its notes are read-only here.")
            } else {
                r
            };
            if r.changed() {
                self.notes.edited_at = Some(now);
            }
            if r.lost_focus() && self.notes.edited_at.is_some() {
                self.notes.save_now = true;
            }
        }
        if let Some(a @ (Availability::Missing | Availability::Unreadable(_))) = &availability {
            ui.add_enabled_ui(enabled, |ui| {
                ui.horizontal(|ui| {
                    let locate =
                        MegaButton::new(ButtonKind::Secondary, "Locate…").icon(icons::FOLDER_OPEN);
                    if *a == Availability::Missing && ui.add(locate).clicked() {
                        let purpose = DialogPurpose::Locate(target.to_path_buf());
                        out.cmds.push(Cmd::Dialog(purpose));
                    }
                    let remove = MegaButton::new(ButtonKind::Quiet, "Remove from list");
                    if ui.add(remove).clicked() {
                        out.cmds.push(Cmd::Remove(target.to_path_buf()));
                    }
                });
            });
        }
        ui.add_space(space::S);
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                let resume = MegaButton::new(ButtonKind::Primary, "Resume session").large();
                let resume = ui.add_enabled(available, resume);
                let resume = if busy {
                    resume.on_disabled_hover_text("Open in another window")
                } else {
                    resume
                };
                if resume.clicked() {
                    out.cmds.push(Cmd::Open(target.to_path_buf()));
                }
                let new = MegaButton::new(ButtonKind::Ghost, "New mosaic").icon(icons::PLUS);
                if ui.add(new).clicked() {
                    out.cmds.push(Cmd::NewMosaic);
                }
                open_buttons(ui, out);
            });
        });
        self.typed_row(ui, enabled, out);
        self.messages(ui);
    }

    /// The shelf: "Recent sessions", "All sessions ›", and as many cards
    /// as fit, pinned first.
    fn shelf(&self, ui: &mut Ui, rect: Rect, target: Option<&Path>, out: &mut Drawn) {
        let mut ui = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        ui.spacing_mut().item_spacing.y = space::S;
        ui.horizontal(|ui| {
            overline(ui, "Recent sessions");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let all = MegaButton::new(ButtonKind::Quiet, "All sessions")
                    .trailing_icon(icons::CHEVRON_RIGHT);
                if ui.add(all).clicked() {
                    out.cmds.push(Cmd::ShowAll(true));
                }
            });
        });
        let fit = ((rect.width() + space::M) / (CARD_WIDTH + space::M))
            .floor()
            .max(1.0) as usize;
        let paths: Vec<PathBuf> = self
            .library
            .shelf_order()
            .into_iter()
            .take(fit)
            .map(|e| e.path.clone())
            .collect();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::M;
            for path in &paths {
                self.card(ui, path, target, out);
            }
        });
    }

    /// Every session's card in a scrolling grid, with a search over names
    /// and notes; "‹ Back" or Esc returns to the shelf.
    fn all_grid(&mut self, ui: &mut Ui, rect: Rect, target: Option<&Path>, out: &mut Drawn) {
        let mut ui = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        ui.spacing_mut().item_spacing.y = space::S;
        ui.horizontal(|ui| {
            let back = MegaButton::new(ButtonKind::Quiet, "Back").icon(icons::ARROW_LEFT);
            if ui.add(back).clicked() {
                out.cmds.push(Cmd::ShowAll(false));
            }
            stable(ui, Id::new("mmm-start-search"), |ui| {
                text_input(ui, &mut self.search, "Search sessions and notes")
            });
        });
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            out.cmds.push(Cmd::ShowAll(false));
        }
        let query = self.search.trim().to_lowercase();
        let paths: Vec<PathBuf> = self
            .library
            .shelf_order()
            .into_iter()
            .filter(|e| self.matches(&e.path, &query))
            .map(|e| e.path.clone())
            .collect();
        ScrollArea::vertical()
            .id_salt("start-all")
            .auto_shrink([false, false])
            .show(&mut ui, |ui| {
                if paths.is_empty() {
                    ui.label(text(Type::Small, "No sessions match."));
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(space::M);
                    for path in &paths {
                        self.card(ui, path, target, out);
                    }
                });
            });
    }

    /// Whether `path`'s name or notes contain `query` (lower case; empty
    /// matches everything).
    fn matches(&self, path: &Path, query: &str) -> bool {
        query.is_empty()
            || self.name_of(path).to_lowercase().contains(query)
            || self
                .facts
                .get(path)
                .is_some_and(|f| f.notes.to_lowercase().contains(query))
    }

    /// One session card: a click selects it, a double-click opens it (when
    /// available); the selected card has a pin toggle.
    fn card(&self, ui: &mut Ui, path: &Path, target: Option<&Path>, out: &mut Drawn) {
        let Some(entry) = self.library.entries().iter().find(|e| e.path == path) else {
            return;
        };
        let facts = self.facts.get(path);
        let (status, dimmed) = card_look(facts);
        let selected = target == Some(path);
        let thumbnail = match self.thumbs.get(path) {
            Some(Thumb::Ready(Some(t))) => Some(t),
            _ => None,
        };
        let title = self.name_of(path);
        let card = SessionCardData {
            title: &title,
            status: status.as_ref().map(|(tone, s)| (*tone, s.as_str())),
            // The selected card's pin toggle stands in for the marker.
            pinned: entry.pinned && !selected,
            dimmed,
            selected,
            thumbnail,
        };
        let r = session_card(ui, &card);
        if ui.is_rect_visible(r.rect) {
            out.thumbs.push(path.to_path_buf());
        }
        let openable =
            self.enabled() && facts.is_some_and(|f| f.availability == Availability::Available);
        if r.double_clicked() {
            if openable {
                out.cmds.push(Cmd::Open(path.to_path_buf()));
            }
        } else if r.clicked() {
            out.cmds.push(Cmd::Select(path.to_path_buf()));
        }
        if selected {
            let size = Vec2::splat(CONTROL_HEIGHT);
            let at = pos2(r.rect.right() - space::S - size.x, r.rect.top() + space::S);
            let mut pin_ui = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(at, size)));
            let (glyph, tip) = if entry.pinned {
                (icons::PIN_OFF, "Unpin")
            } else {
                (icons::PIN, "Pin")
            };
            if pin_ui
                .add(icon_button_of(ButtonKind::Ghost, glyph, tip))
                .clicked()
            {
                out.cmds.push(Cmd::TogglePin(path.to_path_buf()));
            }
        }
    }

    /// The typed-path row, once opened from More → Type a path….
    fn typed_row(&mut self, ui: &mut Ui, enabled: bool, out: &mut Drawn) {
        if !self.typed_open {
            return;
        }
        ui.add_enabled_ui(enabled, |ui| {
            let caption = if self.no_dialog {
                "No dialog? Type a path"
            } else {
                "Session folder"
            };
            let label = ui.label(text(Type::Small, caption));
            ui.horizontal(|ui| {
                let hint = format!("/path/to/Name.{SESSION_EXT}");
                let field = stable(ui, Id::new("mmm-start-typed-path"), |ui| {
                    text_input(ui, &mut self.typed_path, &hint)
                })
                .labelled_by(label.id);
                if std::mem::take(&mut self.focus_typed) {
                    field.request_focus();
                    field.scroll_to_me(None);
                }
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let open = ui
                    .add(MegaButton::new(ButtonKind::Secondary, "Open"))
                    .clicked();
                let create = ui
                    .add(MegaButton::new(ButtonKind::Secondary, "Create"))
                    .clicked();
                let typed = self.typed_path.trim();
                if typed.is_empty() {
                    return;
                }
                // Enter does what the dialog that gave no answer was for.
                let create = create || (entered && self.no_dialog);
                let open = open || (entered && !self.no_dialog);
                if create {
                    out.cmds.push(Cmd::Create(expand_home(typed)));
                } else if open {
                    out.cmds.push(Cmd::Open(expand_home(typed)));
                }
            });
        });
    }

    /// Progress (an open, a dialog), the error and the notice.
    fn messages(&self, ui: &mut Ui) {
        let spinner_row = |ui: &mut Ui, label: &str| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(text(Type::Small, label));
            });
        };
        if let Some(label) = &self.busy {
            spinner_row(ui, label);
        }
        if self.dialog.is_some() {
            spinner_row(ui, "Waiting for the file dialog…");
        }
        if let Some(e) = &self.error {
            let color = Tone::Problem.color();
            ui.horizontal(|ui| {
                let size = TextStyle::Small.resolve(ui.style()).size;
                ui.label(icons::rich(icons::CIRCLE_X, size).color(color));
                ui.add(Label::new(text(Type::Small, e).color(color)).wrap());
            });
        }
        if let Some(n) = &self.notice {
            ui.add(Label::new(text(Type::Small, n)).wrap());
        }
    }

    /// First run (no recent sessions): the large lockup on the starfield,
    /// what mmm does, **New mosaic** and **Open…**. No shelf.
    fn first_run(&mut self, ui: &mut Ui, full: Rect, out: &mut Drawn) {
        let width = FIRST_RUN_WIDTH.min(full.width() - 2.0 * space::XXL);
        let top = (full.center().y - FIRST_RUN_LIFT).max(full.top() + space::XXL);
        let rect = Rect::from_min_max(
            pos2(full.center().x - width / 2.0, top),
            pos2(full.center().x + width / 2.0, full.bottom() - space::XXL),
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Center)),
        );
        let enabled = self.enabled();
        ScrollArea::vertical()
            .id_salt("start-first-run")
            .auto_shrink([false, true])
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing.y = space::S;
                if let Some(images) = &self.brand {
                    let size = vec2(lockup_width(images), LARGE_LOGO_H);
                    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                        lockup(ui, images, LockupSize::Large);
                    });
                }
                ui.add_space(space::XL);
                ui.label(text(Type::Body, FIRST_RUN_LINE).color(TEXT_EMPHASIS));
                ui.add_space(space::XL);
                // Centred by last frame's width (egui lays rows out once).
                let row = vec2(self.first_run_row.max(1.0), CONTROL_HEIGHT_LARGE);
                let mut used = 0.0;
                ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let new = MegaButton::new(ButtonKind::Primary, "New mosaic")
                            .icon(icons::PLUS)
                            .large();
                        if ui.add(new).clicked() {
                            out.cmds.push(Cmd::NewMosaic);
                        }
                        open_buttons(ui, out);
                    });
                    used = ui.min_rect().width();
                });
                if (used - self.first_run_row).abs() > 0.5 {
                    self.first_run_row = used;
                    ui.ctx().request_repaint();
                }
                ui.add_space(space::M);
                let column = vec2(width, 0.0);
                ui.allocate_ui_with_layout(column, Layout::top_down(Align::Min), |ui| {
                    self.typed_row(ui, enabled, out);
                    self.messages(ui);
                });
            });
    }
}

/// **Open…** and the More menu next to it (Type a path…).
fn open_buttons(ui: &mut Ui, out: &mut Drawn) {
    if ui
        .add(MegaButton::new(ButtonKind::Quiet, "Open…"))
        .clicked()
    {
        out.cmds.push(Cmd::Dialog(DialogPurpose::Open));
    }
    let more = ui.add(icon_button_of(
        ButtonKind::Quiet,
        icons::CHEVRON_DOWN,
        "More",
    ));
    egui::Popup::menu(&more).show(|ui| {
        if ui.button("Type a path…").clicked() {
            out.cmds.push(Cmd::TypePath);
        }
    });
}

/// "{n} panels" and one "{group} · {count}" chip per group, in the group's
/// colour (its saved one, else its physical one, else neutral).
fn chips(ui: &mut Ui, f: &CardFacts) {
    let Some(panels) = f.panels else {
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::S);
        chip(ui, &count(panels, "panel"), None);
        for (group, n) in &f.groups {
            // An ungrouped session's one group says nothing the count doesn't.
            if group != SINGLE_GROUP {
                chip(ui, &format!("{group} · {n}"), group_tint(f, group));
            }
        }
    });
}

/// The colour of `group`'s chip, if it has one.
fn group_tint(f: &CardFacts, group: &str) -> Option<Color32> {
    f.group_colors
        .get(group)
        .copied()
        .or_else(|| physical_color(group))
        .and_then(|i| GROUP_PALETTE.get(i).copied())
}

/// The status line, then "{p} problems · {n} notes" when checked.
fn status_rows(ui: &mut Ui, f: &CardFacts) {
    if let Some((tone, label)) = status_line(f) {
        let r = status(ui, tone, label);
        if let Availability::Unreadable(why) = &f.availability {
            let _ = r.on_hover_text(why);
        }
    }
    let problems = f.problems.map(|p| {
        let tone = if p > 0 { Tone::Problem } else { Tone::Neutral };
        (tone, count(p, "problem"))
    });
    let notes = f.notes_count.map(|n| {
        let tone = if n > 0 { Tone::Warning } else { Tone::Neutral };
        (tone, count(n, "note"))
    });
    let parts: Vec<(Tone, String)> = [problems, notes].into_iter().flatten().collect();
    if parts.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        for (i, (tone, label)) in parts.into_iter().enumerate() {
            if i > 0 {
                ui.label(text(Type::Small, "·"));
            }
            ui.label(text(Type::Small, label).color(tone.color()));
        }
    });
}

/// The hero image's area: the right two thirds of `full`, from a little
/// above it to [`IMAGE_BOTTOM_INSET`] above its bottom, ending above
/// `shelf_top` when given.
fn image_rect(full: Rect, shelf_top: Option<f32>) -> Rect {
    let h = full.height();
    let mut bottom = full.bottom() - IMAGE_BOTTOM_INSET * h;
    if let Some(top) = shelf_top {
        bottom = bottom.min(top - space::S);
    }
    Rect::from_min_max(
        pos2(
            full.left() + full.width() / 3.0,
            full.top() - IMAGE_OVERHANG * h,
        ),
        pos2(full.right(), bottom.max(full.top())),
    )
}

/// Run `add` in a child ui with the explicit id `id`, so the text field it
/// adds keeps its id (and the keyboard focus) when widgets above it come
/// and go, e.g. when a session's facts arrive.
fn stable<R>(ui: &mut Ui, id: Id, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(UiBuilder::new().id(id), add).inner
}

/// The large lockup's width: logo, gap and name image.
fn lockup_width(images: &BrandImages) -> f32 {
    let at = |size: Vec2, h: f32| size.x * h / size.y.max(1.0);
    at(images.logo_size(), LARGE_LOGO_H) + space::M + at(images.name_size(), LARGE_NAME_H)
}
