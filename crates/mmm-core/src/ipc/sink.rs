//! Streams blended output rows back to the host over the shared-memory
//! transport, mirroring how input bands are pulled.

use std::sync::Arc;

use crate::Result;
use crate::blend::RowSink;
use crate::ipc::client::HostLink;

/// A [`RowSink`] that forwards each blended band straight to the host over a
/// [`HostLink`], instead of accumulating output in memory or writing it to a
/// file.
///
/// Bands are handed off as-is: `blend` already delivers them in row order,
/// so this type does no buffering or reordering of its own.
pub struct ShmRowSink {
    link: Arc<HostLink>,
    /// Canvas width, captured from `begin` so `band` can derive how many
    /// rows a given planar buffer covers.
    w: u64,
    /// Canvas channel count, captured from `begin` for the same reason.
    ch: u64,
    /// Canvas height, captured from `begin` so `band` can report
    /// `Progress { stage: "blend", .. }` against the right total.
    out_h: u64,
}

impl ShmRowSink {
    /// Creates a sink that streams bands to the host over `link`.
    pub fn new(link: Arc<HostLink>) -> ShmRowSink {
        ShmRowSink {
            link,
            w: 0,
            ch: 0,
            out_h: 0,
        }
    }
}

impl RowSink for ShmRowSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        self.w = w;
        self.ch = ch;
        self.out_h = h;
        self.link.begin_output(w, h, ch)
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let rows_count = rows.len() as u64 / (self.ch * self.w);
        self.link.send_output_band(y0, rows_count, rows)?;
        self.link
            .send_progress("blend", (y0 + rows_count).min(self.out_h), self.out_h);
        Ok(())
    }

    // `finish` is deliberately a no-op: the `HostLink` completion handshake
    // (`finish_ok`, which sends `WorkerMsg::Done`) is owned by the worker's
    // top-level `run()` loop, not by the sink. A worker run may drive this
    // sink as just one stage of a job sharing one `HostLink`, and only the
    // top level knows when the *whole job* is done; if `finish` here also
    // called `finish_ok`, a `Done` frame would go out here AND from `run()`,
    // double-sending it. Do not "fix" this by forwarding to `finish_ok`.
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::RowSink;
    use crate::ipc::client::HostLink;
    use crate::ipc::testhost::MockHost;

    #[test]
    fn streamed_bands_reassemble_on_the_host() {
        let (w, h, ch) = (5u64, 7u64, 2u64);
        let job = MockHost::output_job(w, h, ch, /*output_slots*/ 2, w * ch * 4 * 4);
        let (host, r, wr) = MockHost::spawn(job.clone(), vec![]);
        let link = HostLink::start(job, r, wr).unwrap();
        let mut sink = ShmRowSink::new(link.clone());
        sink.begin(w, h, ch).unwrap();
        // Two bands of 4 and 3 rows; value = c*100 + y*10 + x.
        let band = |y0: u64, rows: u64| {
            (0..ch)
                .flat_map(|c| {
                    (0..rows).flat_map(move |ry| {
                        (0..w).map(move |x| (c * 100 + (y0 + ry) * 10 + x) as f32)
                    })
                })
                .collect::<Vec<_>>()
        };
        sink.band(0, &band(0, 4)).unwrap();
        sink.band(4, &band(4, 3)).unwrap();
        link.finish_ok().unwrap();
        // `result()` before `join()`: `HostSide::join` consumes `self` (it
        // blocks for thread shutdown, nothing more to return), so it has to
        // come last. The data itself is already complete by this point —
        // `send_output_band` blocks for the host's ack, so both bands are
        // guaranteed processed before `finish_ok` was even sent.
        let (geom, out) = host.result();
        host.join();
        assert_eq!(geom, (w, h, ch));
        for c in 0..ch {
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        out[((c * h + y) * w + x) as usize],
                        (c * 100 + y * 10 + x) as f32
                    );
                }
            }
        }
    }
}
