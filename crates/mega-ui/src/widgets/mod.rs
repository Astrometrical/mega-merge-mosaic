//! Mega UI components, styled only from [`crate::theme`] tokens.

pub mod button;
pub mod chip;
pub mod notes;
pub mod progress;
pub mod property;
pub mod segmented;
pub mod status_line;
pub mod toast;

pub use button::{ButtonKind, MegaButton, icon_button};
pub use chip::{CHIP_MAX_WIDTH, chip};
pub use notes::notes_field;
pub use progress::{progress, progress_bar};
pub use property::{property_grid, property_row, property_row_numeric, property_row_ui};
pub use segmented::segmented;
pub use status_line::{overline, status};
pub use toast::{MAX_TOASTS, TOAST_LIFETIME_S, Toast, ToastEvent, ToastId, Toasts};

#[cfg(test)]
#[path = "display_tests.rs"]
mod display_tests;

#[cfg(test)]
#[path = "input_tests.rs"]
mod input_tests;
