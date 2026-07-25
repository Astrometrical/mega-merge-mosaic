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
//!
//! # Public API map
//!
//! A frontend drives the pipeline through these modules:
//!
//! - [`analyze`] — entry point: scan input panels into a [`session::Session`]
//!   (L8 summaries, overlap graph, photometric solve, residual surfaces).
//! - [`session`] — the persistent session directory: canvas geometry, panel
//!   metadata, and the paths of every cached analysis artifact.
//! - [`blend`] — stream the merged mosaic to any [`blend::RowSink`], with
//!   [`blend::BlendParams`] / [`blend::BlendMode`] selecting feather,
//!   two-band, or pyramid blending.
//! - [`output`] — ready-made sinks: streaming FITS, autostretched PNG
//!   preview, and a [`output::Tee`] to feed both in one pass.
//! - [`diag`] — seam/ownership diagnostics for reporting UIs.
//! - [`formats`] + [`panel_reader`] — input access: the XISF reader and the
//!   storage-agnostic row reader the pipeline consumes.
//! - [`astrometry`] + [`align`] — WCS extraction/emission and the solved-panel
//!   reprojection path.
//! - [`synth`] — synthetic ground-truth mosaics, exposed so integration tests
//!   and benchmarks (in-tree and downstream) never need multi-GB real data.
//!
//! The remaining modules ([`summary`], [`overlap`], [`photometry`],
//! [`surfaces`], [`seam`], [`pyramid`], [`flatten`], [`linalg`]) are the
//! algorithm stages themselves; they are public so their artifacts can be
//! loaded and inspected independently, but most frontends only need the
//! list above.

#![warn(missing_docs)]

pub mod align;
pub mod analyze;
pub mod astrometry;
pub mod blend;
pub mod diag;
pub mod error;
pub mod flatten;
pub mod formats;
pub mod linalg;
pub mod output;
pub mod overlap;
pub mod panel_reader;
pub mod photometry;
pub mod pyramid;
pub mod seam;
pub mod session;
pub mod summary;
pub mod surfaces;
pub mod synth;

pub use error::{Error, Result};
