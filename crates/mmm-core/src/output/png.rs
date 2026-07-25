//! PNG preview writer implementing [`RowSink`].
//!
//! Buffers the whole (downsampled) image — the only dense buffer allowed in
//! the pipeline, which is why images above [`MAX_PIXELS`] are refused with a
//! clear error. On `finish`, each channel is autostretched with the classic
//! median/MAD midtone transfer: shadows clip at `max(0, median − 2.8·1.4826·MAD)`,
//! then an MTF midtones balance is chosen so the median lands at 0.25.
//! Output is 8-bit RGB (grayscale for single-channel).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::blend::RowSink;
use crate::{Error, Result};

/// Refuse to buffer more pixels than this: PNG output is for downsampled
/// previews, not full-resolution mosaics.
pub const MAX_PIXELS: u64 = 64_000_000;

/// Autostretch midtone target for the median of covered pixels.
const MIDTONE_TARGET: f64 = 0.25;
/// Shadow clip: `median − 2.8·1.4826·MAD` (1.4826·MAD ≈ σ for a normal dist).
const SHADOW_SIGMAS: f64 = 2.8;

/// Buffering PNG sink with autostretch on `finish`.
pub struct PngSink {
    path: PathBuf,
    dims: (u64, u64, u64),
    data: Vec<f32>,
}

impl PngSink {
    /// New sink targeting `path`; nothing is written until `finish`.
    pub fn create(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            dims: (0, 0, 0),
            data: Vec::new(),
        }
    }
}

impl RowSink for PngSink {
    fn begin(&mut self, w: u64, h: u64, ch: u64) -> Result<()> {
        if w == 0 || h == 0 {
            return Err(Error::format(&self.path, "PNG dimensions must be nonzero"));
        }
        if w * h > MAX_PIXELS {
            return Err(Error::format(
                &self.path,
                format!(
                    "PNG preview limited to {} Mpx, got {}x{} = {:.0} Mpx — use a larger --downsample",
                    MAX_PIXELS / 1_000_000,
                    w,
                    h,
                    (w * h) as f64 / 1e6
                ),
            ));
        }
        if ch != 1 && ch != 3 {
            return Err(Error::format(
                &self.path,
                format!("PNG output supports 1 (grayscale) or 3 (RGB) channels, got {ch}"),
            ));
        }
        self.dims = (w, h, ch);
        self.data = vec![0.0f32; (w * h * ch) as usize];
        Ok(())
    }

    fn band(&mut self, y0: u64, rows: &[f32]) -> Result<()> {
        let (w, h, ch) = self.dims;
        let stride = (w * ch) as usize;
        if stride == 0 || !rows.len().is_multiple_of(stride) {
            return Err(Error::format(
                &self.path,
                format!(
                    "band length {} is not a multiple of ch*w = {stride}",
                    rows.len()
                ),
            ));
        }
        let band_rows = (rows.len() / stride) as u64;
        if y0 + band_rows > h {
            return Err(Error::format(
                &self.path,
                format!("band rows {y0}..{} exceed image height {h}", y0 + band_rows),
            ));
        }
        let (w, band_rows) = (w as usize, band_rows as usize);
        for c in 0..ch as usize {
            let src = &rows[c * band_rows * w..][..band_rows * w];
            let dst = (c * h as usize + y0 as usize) * w;
            self.data[dst..dst + band_rows * w].copy_from_slice(src);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let (w, h, ch) = (
            self.dims.0 as usize,
            self.dims.1 as usize,
            self.dims.2 as usize,
        );
        let plane = w * h;

        // Per-channel stretch parameters from the covered pixels only
        // (covered ⟺ all channels nonzero, matching the blend's convention).
        let params: Vec<(f32, f32)> = (0..ch)
            .map(|c| {
                let mut covered: Vec<f32> = (0..plane)
                    .filter(|&i| (0..ch).all(|k| self.data[k * plane + i] != 0.0))
                    .map(|i| self.data[c * plane + i])
                    .collect();
                stretch_params(&mut covered)
            })
            .collect();

        let mut buf = vec![0u8; plane * ch];
        for (i, px) in buf.chunks_exact_mut(ch).enumerate() {
            for (c, out) in px.iter_mut().enumerate() {
                let (clip, midtone) = params[c];
                let v = self.data[c * plane + i];
                let x = ((v - clip) / (1.0 - clip)).clamp(0.0, 1.0);
                *out = (mtf(midtone, x) * 255.0 + 0.5) as u8;
            }
        }

        let file = File::create(&self.path).map_err(|e| Error::io(&self.path, e))?;
        let mut enc = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
        enc.set_color(if ch == 3 {
            png::ColorType::Rgb
        } else {
            png::ColorType::Grayscale
        });
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc
            .write_header()
            .map_err(|e| Error::format(&self.path, format!("PNG header: {e}")))?;
        writer
            .write_image_data(&buf)
            .map_err(|e| Error::format(&self.path, format!("PNG data: {e}")))?;
        Ok(())
    }
}

/// Shadow clip `c` and MTF midtones balance `m'` for one channel's covered
/// values (consumed as scratch). `m'` is chosen so the clipped-normalized
/// median lands at [`MIDTONE_TARGET`]. Shared with the seam-map diagnostics
/// ([`crate::diag`]), which stretch a luminance plane the same way.
pub(crate) fn stretch_params(vals: &mut [f32]) -> (f32, f32) {
    if vals.is_empty() {
        return (0.0, 0.5);
    }
    let median = |v: &mut [f32]| -> f32 {
        let mid = v.len() / 2;
        *v.select_nth_unstable_by(mid, f32::total_cmp).1
    };
    let m = median(vals) as f64;
    for v in vals.iter_mut() {
        *v = (*v as f64 - m).abs() as f32;
    }
    let mad = median(vals) as f64;

    let clip = (m - SHADOW_SIGMAS * 1.4826 * mad).clamp(0.0, 0.99);
    let x = ((m - clip) / (1.0 - clip)).clamp(0.0, 1.0);
    let midtone = if x <= 0.0 || x >= 1.0 {
        0.5
    } else {
        // Solve MTF(m', x) = t for m': m' = x(t−1) / (2tx − t − x).
        let t = MIDTONE_TARGET;
        x * (t - 1.0) / (2.0 * t * x - t - x)
    };
    (clip as f32, midtone as f32)
}

/// Midtones transfer function; `m` is the midtones balance.
#[inline]
pub(crate) fn mtf(m: f32, x: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else {
        ((m - 1.0) * x) / ((2.0 * m - 1.0) * x - m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmm-png-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn decode(path: &Path) -> (u32, u32, png::ColorType, Vec<u8>) {
        let decoder = png::Decoder::new(File::open(path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info.width, info.height, info.color_type, buf)
    }

    #[test]
    fn writes_rgb_preview_with_stretch() {
        let dir = tmpdir("rgb");
        let path = dir.join("p.png");
        let (w, h) = (4usize, 2usize);
        // 3 planes of 4×2. Pixel (0,0) uncovered (all channels 0); pixel (3,1)
        // is the brightest.
        let mut planes = vec![0f32; 3 * w * h];
        for c in 0..3 {
            for y in 0..h {
                for x in 0..w {
                    if x == 0 && y == 0 {
                        continue;
                    }
                    planes[(c * h + y) * w + x] = 0.01 + 0.02 * (y * w + x) as f32;
                }
            }
        }
        let mut sink = PngSink::create(&path);
        sink.begin(w as u64, h as u64, 3).unwrap();
        sink.band(0, &planes).unwrap();
        sink.finish().unwrap();

        let (dw, dh, color, buf) = decode(&path);
        assert_eq!((dw, dh), (4, 2));
        assert_eq!(color, png::ColorType::Rgb);
        assert_eq!(buf.len(), w * h * 3);
        // Uncovered pixel is black; brightest covered pixel beats a dim one.
        assert_eq!(&buf[0..3], &[0, 0, 0]);
        let px = |x: usize, y: usize| buf[(y * w + x) * 3];
        assert!(px(3, 1) > px(1, 0), "stretch must preserve ordering");
        // Covered values are 0.03..0.15; the median (0.09, pixel (0,1)) must
        // land at the midtone target 0.25 → 64/255. (Shadow clip is 0 here:
        // median − 2.8·1.4826·MAD < 0.)
        let mid = px(0, 1) as i32;
        assert!(
            (mid - 64).abs() <= 1,
            "median pixel should map to ~64, got {mid}"
        );
        assert!(
            i32::from(px(3, 1)) > mid,
            "bright end must stretch above the midtone"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_grayscale_for_single_channel() {
        let dir = tmpdir("gray");
        let path = dir.join("g.png");
        let planes: Vec<f32> = (0..12).map(|i| 0.005 + 0.01 * i as f32).collect();
        let mut sink = PngSink::create(&path);
        sink.begin(4, 3, 1).unwrap();
        sink.band(0, &planes).unwrap();
        sink.finish().unwrap();

        let (dw, dh, color, buf) = decode(&path);
        assert_eq!((dw, dh), (4, 3));
        assert_eq!(color, png::ColorType::Grayscale);
        assert_eq!(buf.len(), 12);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_oversized_images() {
        let dir = tmpdir("big");
        let mut sink = PngSink::create(&dir.join("big.png"));
        let err = sink.begin(9000, 8000, 3);
        assert!(err.is_err(), "72 Mpx must be refused");
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("downsample"),
            "error should point at --downsample: {msg}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_unsupported_channel_counts() {
        let dir = tmpdir("ch");
        let mut sink = PngSink::create(&dir.join("c.png"));
        assert!(sink.begin(4, 4, 2).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
