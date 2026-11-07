//! The brand lockup: the Astrometrical logo mark, the app name (an image
//! pre-rendered from the Angora display face; see
//! `assets/brand/make-name.sh`), and "BY" with the Astrometrical wordmark.

use egui::{
    Align, ColorImage, Context, Image, Layout, Response, TextureFilter, TextureHandle,
    TextureOptions, Ui, Vec2,
};

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
    /// Top left of a screen: 34 pt logo, 12 pt name.
    Compact,
    /// First run and empty states: 72 pt logo, 22 pt name.
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
    ColorImage::from_rgba_unmultiplied(size, &rgba)
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
    let (logo_h, name_h, mark_h) = match size {
        LockupSize::Compact => (34.0, 12.0, 7.0),
        LockupSize::Large => (72.0, 22.0, 10.0),
    };
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = space::M;
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
}
