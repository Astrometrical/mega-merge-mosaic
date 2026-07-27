//! Shared-memory IPC transport for driving the pipeline from a host process
//! (the PixInsight PCL module) without writing panel pixels to disk.
//!
//! The host spawns `mmm-ipc-worker` and talks to it over the worker's
//! stdin/stdout ([`protocol`]) plus a named shared-memory segment ([`shm`])
//! carved into fixed band-sized slots. Panel pixels are pulled on demand: a
//! worker thread asks for a band, the host fills a slot and replies, the
//! worker copies the band into a per-thread buffer ([`reader`]) and frees the
//! slot. Blended output streams back the same way ([`sink`]). The pipeline is
//! reached through a [`source::PanelSource`], so `analyze`/`blend` never learn
//! whether pixels came from a file or the host.
pub mod client;
pub mod protocol;
pub mod reader;
pub mod shm;
pub mod sink;
pub mod source;

// Widened beyond plain `#[cfg(test)]` so the reference host-serving loop is
// also reachable from other crates' integration tests (which can't see this
// crate's `#[cfg(test)]` items) via the `testkit` feature — see
// `mmm-ipc-worker/tests/end_to_end.rs`. There is exactly one implementation
// of the serving loop, under `testhost`; `testkit` is a same-cfg re-export,
// not a copy.
#[cfg(any(test, feature = "testkit"))]
pub mod testhost;

/// Re-exports [`testhost`] (the reference in-process host implementation
/// shared by `mmm-core`'s own unit tests) under the name external crates
/// opt into via the `testkit` feature. Not a second implementation — see
/// the `testhost` module docs for the one serving loop both names reach.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit {
    pub use super::testhost::*;
}

/// Wire-protocol version exchanged in the init handshake; a mismatch aborts
/// the run rather than risking a misinterpretation of later frames.
pub const IPC_PROTOCOL_VERSION: u32 = 2;
