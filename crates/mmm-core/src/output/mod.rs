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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    /// Records every call; optionally errors on `band`.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
        fail_band: bool,
    }

    impl RowSink for Recorder {
        fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
            self.calls.push(format!("begin {w}x{h}x{ch}"));
            Ok(())
        }

        fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
            if self.fail_band {
                return Err(Error::format("recorder", "band failure"));
            }
            self.calls.push(format!("band {y0} len {}", rows.len()));
            Ok(())
        }

        fn finish(&mut self) -> Result<()> {
            self.calls.push("finish".into());
            Ok(())
        }
    }

    #[test]
    fn tee_forwards_every_call_to_both_sinks_in_order() {
        let (mut a, mut b) = (Recorder::default(), Recorder::default());
        let mut tee = Tee::new(&mut a, &mut b);
        tee.begin(4, 2, 1).unwrap();
        tee.band(0, &[0.0; 4]).unwrap();
        tee.band(1, &[0.0; 4]).unwrap();
        tee.finish().unwrap();
        let expect = ["begin 4x2x1", "band 0 len 4", "band 1 len 4", "finish"];
        assert_eq!(a.calls, expect);
        assert_eq!(b.calls, expect);
    }

    #[test]
    fn tee_propagates_first_sink_error_without_calling_second() {
        let mut a = Recorder {
            fail_band: true,
            ..Default::default()
        };
        let mut b = Recorder::default();
        let mut tee = Tee::new(&mut a, &mut b);
        tee.begin(2, 1, 1).unwrap();
        assert!(tee.band(0, &[0.0; 2]).is_err());
        assert_eq!(
            b.calls,
            ["begin 2x1x1"],
            "second sink must not see the band"
        );
    }
}
