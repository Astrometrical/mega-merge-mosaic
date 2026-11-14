//! Headless, narration-paced recordings of egui apps.
//!
//! The crate has three parts:
//!
//! - [`narration`]: the typed reader for the `narration.json` manifest that
//!   `narrate build` writes (clips, durations, captions, status);
//! - the recorder: a scripted, headless driver that renders an egui app frame
//!   by frame and paces its steps to the narration;
//! - the outputs: `silent.mp4`, `timeline.json`, `captions.vtt` and stills.
#![warn(missing_docs)]

pub mod narration;
pub mod outputs;
pub mod schedule;
