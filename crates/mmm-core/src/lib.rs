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
//! - [`formats`] + [`panel_reader`] — input access: the XISF and FITS readers
//!   behind the format-agnostic [`formats::InputPanel`], and the
//!   storage-agnostic row reader the pipeline consumes. For band-cached
//!   readers (IPC, FITS) a row slice is valid only until the same thread's
//!   next `row` call lands in a different band — hold one band's rows at a time.
//! - [`astrometry`] + [`align`] — WCS extraction/emission and the solved-panel
//!   reprojection path.
//! - [`mod@reference`] — the shared [`reference::ReferenceFrame`] several
//!   sessions adopt so their outputs share one pixel grid (multi-filter
//!   mosaics): derivation over every group's panels, persistence as
//!   `*.mmm-frame.json`, and the fit checks analyze applies.
//! - [`job`] — cooperative cancellation ([`job::CancelToken`]), progress
//!   reporting, and a cancellable [`blend::RowSink`] wrapper.
//! - [`settings`] — serialisable analyze/blend settings, per-group overrides,
//!   and the [`settings::ChangeImpact`] of editing them.
//! - [`tiles`] — the memory-mapped tiled result store: a multi-level tile
//!   pyramid with per-tile validity flags, written by a background blend
//!   ([`tiles::TileStore`]) while viewers read it ([`tiles::TileReader`]);
//!   [`tiles::blend_tiles`] blends a canvas rect into it and rebuilds the
//!   parent levels, cancellably.
//! - [`mosaic`] — the mosaic project: `mosaic.json` configuration, panel
//!   grouping rules and the on-disk session layout; [`mosaic::Mosaic`] with
//!   [`mosaic::analyze_all`] (every group onto one shared reference frame)
//!   and [`mosaic::blend_group`] (a group into its tiled result), plus
//!   settings edits that stale exactly the affected result stores.
//! - [`stretch`] — display stretch maths: per-channel median/MAD statistics
//!   and an auto stretch (display only; the data stays linear).
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
pub(crate) mod band_cache;
pub mod blend;
pub mod checks;
pub mod decoded;
pub mod diag;
pub mod error;
pub mod flatten;
pub mod formats;
pub mod health;
pub mod ipc;
pub mod job;
pub mod linalg;
pub mod mosaic;
pub mod output;
pub mod overlap;
pub mod panel_reader;
pub mod photometry;
pub mod pyramid;
pub mod reference;
pub mod seam;
pub mod session;
pub mod settings;
pub mod stretch;
pub mod summary;
pub mod surfaces;
pub mod synth;
pub mod tiles;

pub use error::{Error, Result};
