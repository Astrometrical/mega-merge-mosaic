//! A [`TileSource`] over a plain in-memory image.

use egui::{Color32, ColorImage};

use super::{Fetched, PyramidGeometry, TileKey, TileSource};

/// A [`TileSource`] over an in-memory image: a pyramid built once, up front,
/// by 2x2 box downsampling (transparent pixels do not count towards the
/// average).
///
/// Always generation 0 and always `Valid`; tiles at the right and bottom
/// edges are cropped to the image.
pub struct ImageTileSource {
    geom: PyramidGeometry,
    /// Level images, finest first; `levels[l]` is `geom.level_dims(l)` big.
    levels: Vec<ColorImage>,
}

impl ImageTileSource {
    /// Build the pyramid of `image` with `tile`-pixel tiles.
    pub fn new(image: ColorImage, tile: u32) -> Self {
        let geom = PyramidGeometry::new(image.size[0] as u64, image.size[1] as u64, tile);
        let mut levels = vec![image];
        for _ in 1..geom.levels {
            let next = halve(levels.last().expect("at least one level"));
            levels.push(next);
        }
        Self { geom, levels }
    }
}

/// 2x2 box downsample of `src` (odd edges average what exists).
fn halve(src: &ColorImage) -> ColorImage {
    let [w, h] = src.size;
    let (nw, nh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
    let mut out = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        for x in 0..nw {
            let (mut sum, mut n) = ([0u32; 4], 0u32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                if sx >= w || sy >= h {
                    continue;
                }
                let p = src.pixels[sy * w + sx];
                if p.a() == 0 {
                    continue;
                }
                for (s, c) in sum.iter_mut().zip(p.to_array()) {
                    *s += u32::from(c);
                }
                n += 1;
            }
            out.push(if n == 0 {
                Color32::TRANSPARENT
            } else {
                let avg = |c: u32| ((c + n / 2) / n) as u8;
                Color32::from_rgba_premultiplied(avg(sum[0]), avg(sum[1]), avg(sum[2]), avg(sum[3]))
            });
        }
    }
    ColorImage::new([nw, nh], out)
}

impl TileSource for ImageTileSource {
    fn geometry(&self) -> PyramidGeometry {
        self.geom
    }

    fn generation(&self) -> u64 {
        0
    }

    fn fetch(&self, key: TileKey) -> Fetched {
        let Some(img) = self.levels.get(key.level as usize) else {
            return Fetched::Missing;
        };
        let t = self.geom.tile.max(1) as usize;
        let [w, h] = img.size;
        let (x0, y0) = (key.tx as usize * t, key.ty as usize * t);
        if x0 >= w || y0 >= h {
            return Fetched::Missing;
        }
        let (x1, y1) = ((x0 + t).min(w), (y0 + t).min(h));
        let mut px = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            px.extend_from_slice(&img.pixels[y * w + x0..y * w + x1]);
        }
        Fetched::Valid(ColorImage::new([x1 - x0, y1 - y0], px))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_source_builds_a_pyramid() {
        let (w, h) = (1000usize, 600usize);
        let px: Vec<Color32> = (0..w * h)
            .map(|i| Color32::from_rgb((i % 251) as u8, (i % 13) as u8, 7))
            .collect();
        let src = ImageTileSource::new(ColorImage::new([w, h], px.clone()), 256);
        let g = src.geometry();
        assert_eq!((g.width, g.height, g.tile), (1000, 600, 256));
        assert_eq!(g.levels, 3); // 1000x600 -> 500x300 -> 250x150
        assert_eq!(g.tiles(g.levels - 1), (1, 1));
        assert_eq!(src.generation(), 0);
        let Fetched::Valid(t) = src.fetch(TileKey {
            level: 0,
            tx: 0,
            ty: 0,
        }) else {
            panic!("valid")
        };
        assert_eq!(t.size, [256, 256]);
        assert_eq!(t.pixels[0], px[0]);
        // Edge tile crops to the image.
        let Fetched::Valid(t) = src.fetch(TileKey {
            level: 0,
            tx: 3,
            ty: 2,
        }) else {
            panic!("valid")
        };
        assert_eq!(t.size, [1000 - 768, 600 - 512]);
        // The top level is one tile of the whole (downsampled) image.
        let Fetched::Valid(t) = src.fetch(TileKey {
            level: 2,
            tx: 0,
            ty: 0,
        }) else {
            panic!("valid")
        };
        assert_eq!(t.size, [250, 150]);
        // Out of range tiles are missing.
        assert!(matches!(
            src.fetch(TileKey {
                level: 0,
                tx: 9,
                ty: 9
            }),
            Fetched::Missing
        ));
    }

    #[test]
    fn downsampling_ignores_transparent_pixels() {
        let mut px = vec![Color32::TRANSPARENT; 4 * 4];
        px[0] = Color32::from_rgb(200, 100, 50);
        let src = ImageTileSource::new(ColorImage::new([4, 4], px), 2);
        let Fetched::Valid(t) = src.fetch(TileKey {
            level: 1,
            tx: 0,
            ty: 0,
        }) else {
            panic!("valid")
        };
        // The 2x2 block with one opaque pixel keeps its colour at full alpha.
        assert_eq!(t.pixels[0], Color32::from_rgb(200, 100, 50));
        assert_eq!(t.pixels[1], Color32::TRANSPARENT);
    }
}
