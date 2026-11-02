//! Generic egui components and the Astrometrical theme shared by the Mega
//! apps.
//!
//! This crate is deliberately generic: it must not depend on `mmm-core` (or
//! any app engine), so it can later be lifted into workflow-platform or
//! Photometrical. App-specific knowledge lives in the app crates.
#![warn(missing_docs)]

pub mod jobs;
pub mod status;
pub mod theme;
pub mod viewer;
