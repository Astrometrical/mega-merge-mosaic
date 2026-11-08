//! Menu entries beyond egui's buttons.

use egui::containers::menu::{MenuConfig, SubMenuButton};
use egui::{Button, PopupCloseBehavior, TextStyle, Ui, WidgetInfo, WidgetType};

use crate::icons;

/// A submenu entry in a menu (a [`egui::Popup::menu`] or a menu bar's
/// menu): `label` with a trailing chevron in the icon font (text fonts lack
/// "▾"), opening `contents` beside it on hover or click. Clicks inside the
/// submenu leave it open, so it can hold a text field: an item that acts
/// closes the menu itself (`ui.close()`). Read out as `label` alone.
/// Returns what `contents` returned, while the submenu is open.
pub fn submenu<R>(ui: &mut Ui, label: &str, contents: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let size = TextStyle::Body.resolve(ui.style()).size;
    let button = Button::new(label).right_text(icons::rich(icons::CHEVRON_DOWN, size));
    let config = MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside);
    let (response, inner) = SubMenuButton::from_button(button)
        .config(config)
        .ui(ui, contents);
    // Not the chevron's private-use glyph.
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    inner.map(|r| r.inner)
}

#[cfg(test)]
#[path = "menu_tests.rs"]
mod tests;
