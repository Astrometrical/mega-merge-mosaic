//! FITS panel reading, from mpp-core: the primary-HDU pixel reader
//! (`BITPIX` 8/16/32/−32/−64 with `BZERO`/`BSCALE`/`BLANK`, rows exposed
//! top-down whatever the file's `ROWORDER`) and its parsed header. See
//! [`mpp_core::formats::fits`] for the supported subset and its tests.

pub use mpp_core::formats::fits::FitsHeader;
/// The FITS panel reader (mpp-core's [`mpp_core::formats::fits::FitsImage`]).
pub use mpp_core::formats::fits::FitsImage as FitsPanel;
