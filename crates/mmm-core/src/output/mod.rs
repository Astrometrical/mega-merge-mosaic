//! Output writers: streaming [`RowSink`] implementations (FITS, PNG preview).

pub mod fits;
pub mod png;

use crate::Result;
use crate::blend::RowSink;

/// Fans one blend out to two sinks (e.g. FITS file + PNG preview) in a single
/// pass over the data.
pub struct Tee<'a> {
    first: &'a mut dyn RowSink,
    second: &'a mut dyn RowSink,
}

impl<'a> Tee<'a> {
    /// Pair two sinks; every `begin`/`band`/`finish` call reaches both.
    pub fn new(first: &'a mut dyn RowSink, second: &'a mut dyn RowSink) -> Self {
        Self { first, second }
    }
}

impl RowSink for Tee<'_> {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.first.begin(w, h, ch)?;
        self.second.begin(w, h, ch)
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        self.first.band(y0, rows)?;
        self.second.band(y0, rows)
    }

    fn finish(&mut self) -> Result<()> {
        self.first.finish()?;
        self.second.finish()
    }
}
