//! Streaming FITS output: a [`RowSink`] over mpp-core's
//! [`FitsWriter`].
//!
//! Output: single HDU, `BITPIX = -32`, `NAXIS = 3`, big-endian samples,
//! rows stored **top-down** (`ROWORDER= 'TOP-DOWN'`) with WCS cards in the
//! same top-down convention (`astrometry::wcs_cards`) — empirically the one
//! layout on which PixInsight, Astropy and DS9 agree. Bands may arrive in
//! any order; each channel slice lands at its absolute file offset.

use std::path::Path;

use mpp_core::formats::fits_write::FitsWriter;
pub use mpp_core::formats::fits_write::keywords_for_output;

use crate::Result;
use crate::blend::RowSink;
use crate::formats::FitsKeyword;

/// Streaming FITS sink. Construct with the (already filtered/shifted) keyword
/// list from [`keywords_for_output`]; dimensions arrive via `begin`.
pub struct FitsSink(FitsWriter);

impl FitsSink {
    /// Create/truncate the output file; the header is written on `begin`.
    pub fn create(path: &Path, keywords: Vec<FitsKeyword>) -> Result<Self> {
        Ok(FitsSink(FitsWriter::create(path, keywords)?))
    }
}

impl RowSink for FitsSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        Ok(self.0.begin(w, h, ch)?)
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        Ok(self.0.write_band(y0, rows)?)
    }

    fn finish(&mut self) -> Result<()> {
        Ok(self.0.finish()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::fits::FitsPanel;

    #[test]
    fn fits_sink_streams_through_row_sink() {
        let dir = std::env::temp_dir().join(format!("mmm-fits-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.fits");
        let (w, h, ch) = (5u64, 4u64, 2u64);
        let plane = (w * h) as usize;
        let src: Vec<f32> = (0..plane * ch as usize).map(|i| i as f32 / 40.0).collect();
        let kws = vec![FitsKeyword {
            name: "OBJECT".into(),
            value: "'M42'".into(),
            comment: String::new(),
        }];
        let mut sink: Box<dyn RowSink> = Box::new(FitsSink::create(&path, kws).unwrap());
        sink.begin(w, h, ch).unwrap();
        // Two bands, the second first: positional writes.
        let band = |y0: usize, rows: usize| -> Vec<f32> {
            (0..ch as usize)
                .flat_map(|c| src[c * plane + y0 * w as usize..][..rows * w as usize].to_vec())
                .collect()
        };
        sink.band(3, &band(3, 1)).unwrap();
        sink.band(0, &band(0, 3)).unwrap();
        sink.finish().unwrap();
        let img = FitsPanel::open(&path).unwrap();
        let mut out = vec![0f32; plane];
        for c in 0..ch {
            img.decode_rows(c, 0, h as usize, &mut out);
            assert_eq!(out, src[c as usize * plane..][..plane], "channel {c}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
