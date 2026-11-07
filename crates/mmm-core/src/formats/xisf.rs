//! XISF panel reading, from mpp-core: monolithic XISF 1.0 units at
//! document revision 1.02 (every compression codec, checksums, both byte
//! orders and storage models, UInt8/16/32 and Float32/64 samples, multiple
//! images). An uncompressed Float32 panel in 0:1 is served zero-copy from
//! its mmap ([`XisfPanel::is_mapped`]); any other uncompressed, planar,
//! little-endian one is converted to f32 on read from the same mmap
//! ([`XisfPanel::native_planes`]); the rest (compressed, big-endian,
//! interleaved) are decoded once, in their own sample format, into the
//! session's decode cache (`crate::decoded`). See
//! [`mpp_core::formats::xisf`] for the conformance details and tests.

/// The XISF panel reader (mpp-core's [`mpp_core::formats::xisf::XisfImage`]).
pub use mpp_core::formats::xisf::XisfImage as XisfPanel;
pub use mpp_core::formats::xisf::{XisfHeader, XisfOpenOptions};
