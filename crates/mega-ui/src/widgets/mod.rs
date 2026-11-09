//! Mega UI components, styled only from [`crate::theme`] tokens.

pub mod button;
pub mod chip;
pub mod menu;
pub mod notes;
pub mod ordered_list;
pub mod progress;
pub mod property;
pub mod segmented;
pub mod session_card;
pub mod status_line;
pub mod step_strip;
pub mod text_input;
pub mod toast;

pub use button::{ButtonKind, MegaButton, focus_ring, icon_button, icon_button_of};
pub use chip::{CHIP_MAX_WIDTH, ChipMark, ChoiceChip, chip, choice_chip};
pub use menu::submenu;
pub use notes::{NOTES_LINES, NotesMode, NotesResponse, notes_view};
pub use ordered_list::{ListEvent, ListRow, ListState, ordered_list, ordered_list_with};
pub use progress::{progress, progress_bar};
pub use property::{property_grid, property_row, property_row_numeric, property_row_ui};
pub use segmented::{Segment, segmented, segmented_with};
pub use session_card::{CARD_HEIGHT, CARD_THUMB_HEIGHT, CARD_WIDTH, SessionCardData, session_card};
pub use status_line::{overline, status};
pub use step_strip::{StepState, StripEvent, StripStep, step_strip, step_strip_width};
pub use text_input::text_input;
pub use toast::{MAX_TOASTS, TOAST_LIFETIME_S, Toast, ToastEvent, ToastId, Toasts};

#[cfg(test)]
#[path = "display_tests.rs"]
mod display_tests;

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "ordered_list_tests.rs"]
mod ordered_list_tests;
