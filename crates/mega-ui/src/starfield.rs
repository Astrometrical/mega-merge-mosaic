//! Static starfield behind the start screen's hero.

/// One star.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Star {
    /// Position relative to the field's top left.
    pub pos: egui::Pos2,
    /// Radius in points.
    pub radius: f32,
    /// Colour with alpha.
    pub color: egui::Color32,
}

/// Spectral star types from the Astrometrical website's loading starfield:
/// `(r, g, b, weight)` for O, B, A, F, G, K, M.
const STAR_TYPES: [(u8, u8, u8, u32); 7] = [
    (155, 176, 255, 1),
    (170, 191, 255, 3),
    (202, 215, 255, 6),
    (248, 247, 255, 10),
    (255, 244, 234, 15),
    (255, 210, 161, 20),
    (255, 180, 111, 20),
];

/// Square points of field area per star.
const AREA_PER_STAR: f32 = 900.0;

/// splitmix64 generator (no `rand` dependency).
struct SplitMix(u64);

impl SplitMix {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Stars for a field of `size`: pure and deterministic in `(size, seed)`.
/// One star per 900 square points, spectral colours by weight, alpha
/// `u^3 * 0.9 + 0.08`, radius 0.7 (1.4 for 3 % of stars).
pub fn stars(size: egui::Vec2, seed: u64) -> Vec<Star> {
    let area = size.x.max(0.0) * size.y.max(0.0);
    let count = (area / AREA_PER_STAR).round() as usize;
    let total: u32 = STAR_TYPES.iter().map(|t| t.3).sum();
    let mut rng = SplitMix(seed);
    (0..count)
        .map(|_| {
            let pos = egui::pos2(rng.unit() * size.x, rng.unit() * size.y);
            let mut pick = (rng.unit() * total as f32) as u32;
            let mut rgb = STAR_TYPES[STAR_TYPES.len() - 1];
            for t in STAR_TYPES {
                if pick < t.3 {
                    rgb = t;
                    break;
                }
                pick -= t.3;
            }
            let u = rng.unit();
            let alpha = ((u * u * u * 0.9 + 0.08) * 255.0).round() as u8;
            let radius = if rng.unit() < 0.03 { 1.4 } else { 0.7 };
            Star {
                pos,
                radius,
                color: egui::Color32::from_rgba_unmultiplied(rgb.0, rgb.1, rgb.2, alpha),
            }
        })
        .collect()
}

/// A starfield that caches its mesh per size.
pub struct Starfield {
    seed: u64,
    cached: Option<(egui::Vec2, egui::Mesh)>,
}

impl Starfield {
    /// A starfield generated from `seed`.
    pub fn new(seed: u64) -> Self {
        Self { seed, cached: None }
    }

    /// Paint the field into `rect`. The mesh is rebuilt only when the size
    /// changes.
    pub fn paint(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        let size = rect.size();
        if self.cached.as_ref().is_none_or(|(s, _)| *s != size) {
            let mut mesh = egui::Mesh::default();
            for star in stars(size, self.seed) {
                let r = star.radius;
                let base = mesh.vertices.len() as u32;
                for (dx, dy) in [(-r, -r), (r, -r), (r, r), (-r, r)] {
                    mesh.colored_vertex(star.pos + egui::vec2(dx, dy), star.color);
                }
                mesh.add_triangle(base, base + 1, base + 2);
                mesh.add_triangle(base, base + 2, base + 3);
            }
            self.cached = Some((size, mesh));
        }
        if let Some((_, mesh)) = &self.cached {
            let mut m = mesh.clone();
            m.translate(rect.min.to_vec2());
            painter.add(egui::Shape::mesh(m));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, vec2};

    #[test]
    fn stars_are_deterministic() {
        let v = vec2(400.0, 300.0);
        assert_eq!(stars(v, 7), stars(v, 7));
        assert_ne!(stars(v, 7), stars(v, 8));
    }

    #[test]
    fn star_count_follows_area() {
        assert_eq!(stars(vec2(900.0, 300.0), 1).len(), 300);
    }

    #[test]
    fn stars_stay_inside() {
        let v = vec2(640.0, 480.0);
        for s in stars(v, 3) {
            assert!(s.pos.x >= 0.0 && s.pos.x <= v.x && s.pos.y >= 0.0 && s.pos.y <= v.y);
        }
    }

    #[test]
    fn star_colours_are_spectral() {
        let types = [
            (155, 176, 255),
            (170, 191, 255),
            (202, 215, 255),
            (248, 247, 255),
            (255, 244, 234),
            (255, 210, 161),
            (255, 180, 111),
        ];
        for s in stars(vec2(900.0, 900.0), 5) {
            // Color32 is premultiplied, so match each type at the star's alpha.
            let a = s.color.a();
            assert!((20..=251).contains(&a), "alpha {a}");
            assert!(
                types
                    .iter()
                    .any(|&(r, g, b)| Color32::from_rgba_unmultiplied(r, g, b, a) == s.color),
                "{:?}",
                s.color
            );
        }
    }
}
