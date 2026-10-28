//! XISF panel reading, from mpp-core: monolithic XISF 1.0 units at
//! document revision 1.02 (every compression codec, checksums, both byte
//! orders and storage models, UInt8/16/32 and Float32/64 samples, multiple
//! images). An uncompressed full-canvas Float32 panel is served zero-copy
//! from its mmap ([`XisfPanel::is_mapped`]); any other is decoded once into
//! the session's decode cache (`crate::decoded`). See
//! [`mpp_core::formats::xisf`] for the conformance details and tests.

/// The XISF panel reader (mpp-core's [`mpp_core::formats::xisf::XisfImage`]).
pub use mpp_core::formats::xisf::XisfImage as XisfPanel;
pub use mpp_core::formats::xisf::{XisfHeader, XisfOpenOptions};
