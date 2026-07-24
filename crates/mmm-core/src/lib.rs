//! Mega Merge Mosaic core library.
//!
//! Merges/blends pre-aligned astrophotography mosaic panels (as produced by
//! PixInsight's MosaicByCoordinates) into a seamless mosaic. Panels are
//! full-canvas frames on a common projection, with hard zeros outside each
//! panel's coverage — all processing exploits that sparsity: work happens in
//! overlap bands, never globally.
//!
//! Pipeline stages (each independently cacheable in a session directory):
//!   ingest → coverage/overlap graph → photometric solve → seam → blend → output
//!
//! This crate is UI-agnostic: the `mmm` CLI and any future GUI are thin
//! frontends over [`session`].

pub mod analyze;
pub mod astrometry;
pub mod blend;
pub mod diag;
pub mod error;
pub mod formats;
pub mod linalg;
pub mod output;
pub mod overlap;
pub mod photometry;
pub mod seam;
pub mod session;
pub mod summary;
pub mod surfaces;
pub mod synth;

pub use error::{Error, Result};
