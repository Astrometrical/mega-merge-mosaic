//! The brand lockup: the Astrometrical logo mark, the app name (an image
//! pre-rendered from the Angora display face; see
//! `assets/brand/make-name.sh`), and "BY" with the Astrometrical wordmark.

use egui::{
    Align, ColorImage, Context, Image, Layout, Response, TextureFilter, TextureHandle,
    TextureOptions, Ui, Vec2,
};

use crate::theme::text::Type;
use crate::theme::tokens::space;
use crate::widgets::overline;

/// The Astrometrical logo mark, white on transparent (also the window
/// icon).
pub const LOGO_PNG: &[u8] = include_bytes!("../assets/brand/logo-white.png");
/// The Astrometrical wordmark, white on transparent.
pub const WORDMARK_PNG: &[u8] = include_bytes!("../assets/brand/wordmark-white.png");
/// "MEGA MERGE MOSAIC" in Angora, white on transparent.
pub const MEGA_MERGE_MOSAIC_PNG: &[u8] =
    include_bytes!("../assets/brand/name-mega-merge-mosaic.png");

/// Lockup sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockupSize {
    /// Top left of a screen: 22 pt logo mark, 12 pt name, 7 pt wordmark.
    Compact,
    /// First run and empty states: 46 pt logo mark, 22 pt name, 10 pt
    /// wordmark.
    Large,
}

/// Decode an embedded PNG into RGBA pixels.
fn decode(png_bytes: &[u8]) -> ColorImage {
    let mut decoder = png::Decoder::new(png_bytes);
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().expect("embedded PNG header");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("embedded PNG data");
    let size = [info.width as usize, info.height as usize];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()]
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[l, a]| [l, l, l, a])
            .collect(),
        other => panic!("embedded PNG must have alpha, got {other:?}"),
    };
    trim_transparent(size, &rgba)
}

/// The image cropped to its non-transparent pixels, so a lockup size is
/// the artwork's own size, not its padding's.
fn trim_transparent([w, h]: [usize; 2], rgba: &[u8]) -> ColorImage {
    let opaque = |x: usize, y: usize| rgba[(y * w + x) * 4 + 3] > 0;
    let rows: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| opaque(x, y))).collect();
    let cols: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| opaque(x, y))).collect();
    let (Some(&y0), Some(&y1), Some(&x0), Some(&x1)) =
        (rows.first(), rows.last(), cols.first(), cols.last())
    else {
        return ColorImage::from_rgba_unmultiplied([w, h], rgba);
    };
    let (tw, th) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in y0..=y1 {
        out.extend_from_slice(&rgba[(y * w + x0) * 4..(y * w + x1 + 1) * 4]);
    }
    ColorImage::from_rgba_unmultiplied([tw, th], &out)
}

/// The lockup's images as textures, loaded once per window.
pub struct BrandImages {
    logo: TextureHandle,
    name: TextureHandle,
    wordmark: TextureHandle,
    app_name: String,
}

impl BrandImages {
    /// Load the logo, the wordmark and the app name image `app_name_png`;
    /// `app_name` is the name image's accessible label.
    pub fn load(ctx: &Context, app_name_png: &[u8], app_name: &str) -> Self {
        let options = TextureOptions {
            mipmap_mode: Some(TextureFilter::Linear),
            ..TextureOptions::LINEAR
        };
        let tex = |id: &str, png: &[u8]| ctx.load_texture(id, decode(png), options);
        BrandImages {
            logo: tex("mega-ui-brand-logo", LOGO_PNG),
            name: tex("mega-ui-brand-name", app_name_png),
            wordmark: tex("mega-ui-brand-wordmark", WORDMARK_PNG),
            app_name: app_name.to_owned(),
        }
    }

    /// The logo image's pixel size.
    pub fn logo_size(&self) -> Vec2 {
        self.logo.size_vec2()
    }

    /// The app name image's pixel size.
    pub fn name_size(&self) -> Vec2 {
        self.name.size_vec2()
    }

    /// The wordmark image's pixel size.
    pub fn wordmark_size(&self) -> Vec2 {
        self.wordmark.size_vec2()
    }
}

/// `size` scaled to `height`.
fn at_height(size: Vec2, height: f32) -> Vec2 {
    Vec2::new(size.x * height / size.y, height)
}

/// The brand lockup at `size`.
pub fn lockup(ui: &mut Ui, images: &BrandImages, size: LockupSize) -> Response {
    let (logo_h, name_h, mark_h): (f32, f32, f32) = match size {
        LockupSize::Compact => (22.0, 12.0, 7.0),
        LockupSize::Large => (46.0, 22.0, 10.0),
    };
    // A row as tall as the taller of the logo and the text column, both
    // centred in it.
    // (A bare centred layout would centre in all the height a scroll area
    // offers, and `horizontal` rows are at least a control high.)
    let by_h = ui
        .text_style_height(&Type::Overline.text_style())
        .max(mark_h);
    let row = Vec2::new(ui.available_width(), logo_h.max(name_h + space::XS + by_h));
    ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = space::M;
        ui.spacing_mut().interact_size.y = 0.0;
        ui.add(
            Image::new((images.logo.id(), at_height(images.logo_size(), logo_h)))
                .alt_text("Astrometrical logo"),
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::XS;
            ui.add(
                Image::new((images.name.id(), at_height(images.name_size(), name_h)))
                    .alt_text(&images.app_name),
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                overline(ui, "by");
                ui.add(
                    Image::new((
                        images.wordmark.id(),
                        at_height(images.wordmark_size(), mark_h),
                    ))
                    .alt_text("Astrometrical"),
                );
            });
        });
    })
    .response
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    #[test]
    fn embedded_images_decode() {
        let ctx = egui::Context::default();
        let images = BrandImages::load(&ctx, MEGA_MERGE_MOSAIC_PNG, "Mega Merge Mosaic");
        for size in [
            images.logo_size(),
            images.name_size(),
            images.wordmark_size(),
        ] {
            assert!(size.x > 0.0 && size.y > 0.0, "{size:?}");
        }
        let name = images.name_size();
        assert!(
            name.x > 20.0 * name.y,
            "name image is a wide strip: {name:?}"
        );
        // Transparent borders are trimmed, so sizes are the artwork's own:
        // the wordmark PNG is 589×64 with its letters in 541×21.
        assert_eq!(images.wordmark_size(), Vec2::new(541.0, 21.0));
        assert_eq!(images.logo_size(), Vec2::new(100.0, 80.0));
    }

    #[test]
    fn lockup_has_accessible_labels() {
        let mut images = None;
        let mut h = Harness::new_ui(move |ui| {
            if !crate::testutil::themed(ui) {
                return;
            }
            let images = images.get_or_insert_with(|| {
                BrandImages::load(ui.ctx(), MEGA_MERGE_MOSAIC_PNG, "Mega Merge Mosaic")
            });
            lockup(ui, images, LockupSize::Compact);
        });
        h.run();
        h.get_by_label("Astrometrical logo");
        h.get_by_label("Mega Merge Mosaic");
        h.get_by_label("Astrometrical");
    }

    #[test]
    fn lockup_lines_up_inside_a_scroll_area() {
        let mut images = None;
        let mut h = Harness::builder()
            .with_size(egui::vec2(600.0, 800.0))
            .build_ui(move |ui| {
                if !crate::testutil::themed(ui) {
                    return;
                }
                let images = images.get_or_insert_with(|| {
                    BrandImages::load(ui.ctx(), MEGA_MERGE_MOSAIC_PNG, "Mega Merge Mosaic")
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    lockup(ui, images, LockupSize::Compact);
                });
            });
        h.run();
        let logo = h.get_by_label("Astrometrical logo").rect();
        let name = h.get_by_label("Mega Merge Mosaic").rect();
        let mark = h.get_by_label("Astrometrical").rect();
        assert!(logo.min.y < 60.0, "the lockup sits at the top: {logo:?}");
        let column = name.union(mark);
        assert!(
            (logo.center().y - column.center().y).abs() < 2.0,
            "logo and text column are centred on each other: logo {logo:?}, column {column:?}"
        );
        assert!(
            mark.min.y > name.max.y,
            "the wordmark row sits under the name"
        );
        assert!(
            mark.height() >= 7.0 - 0.5,
            "the wordmark is legible: {mark:?}"
        );
    }
}
