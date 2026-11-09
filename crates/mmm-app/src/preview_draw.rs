//! Drawing the ROI previews' patches (spec §7.3, §8.2): a cached
//! [`PreviewPatch`] becomes a texture, converted with the stretch of the
//! canvas beneath it, and goes to the canvas as a [`Patch`] clipped to its
//! ROI.
//!
//! - **The stretch is the canvas's, read only.** The window hands each layer
//!   the stretch the canvas draws with (the shown member's
//!   [`crate::tile_source::MosaicTileSource::stretch`] on the mosaic,
//!   [`crate::plan_view::PlanView::stretch_for_view`] on the plan). Nothing
//!   here works a stretch out from a patch, so a preview never moves the
//!   display stretch.
//! - **Layers.** A group view has one layer (the group's patch); a composite
//!   has one per member (its patch, tinted by the composition's channel
//!   colour times the weight, mixed additively like
//!   [`crate::tile_source::CompositeTileSource`]). A member without a patch
//!   adds nothing; with no layer at all nothing is drawn.
//! - **Conversion** runs on a worker ([`Task`]): a 4 MP three-channel patch
//!   takes tens of milliseconds, too long for a frame. A texture is
//!   converted again only when one of its layers' patch, stretch, tint or
//!   mode changes (the key); the texture drawn meanwhile is the previous
//!   one. Textures of ROIs not drawn this frame are dropped.
//! - **Compare** (spec §8.3): the compared ROI has a texture per side, the
//!   live preview (B) and the frozen A, both kept converted (each with the
//!   canvas's stretch, so they compare fairly) and only the side shown
//!   drawn: switching sides shows the other at once. A texture whose layers
//!   another already holds converted borrows it (A, as the compare begins,
//!   is B's).

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{ColorImage, TextureHandle, TextureOptions};
use mega_ui::canvas_layers::Patch;
use mmm_core::mosaic::PreviewPatch;
use mmm_core::stretch::StretchParams;

use crate::previews::Side;
use crate::tile_source::{Mix, planar_image};
use crate::util::{Poll, Task};

/// One patch of a drawn ROI, with how to convert it.
#[derive(Clone)]
pub struct LayerSpec {
    /// The patch.
    pub patch: Arc<PreviewPatch>,
    /// The canvas's stretch: one entry per channel, or one entry for a
    /// `mono` layer.
    pub stretch: Vec<StretchParams>,
    /// The layer's RGB tint (white for a group view).
    pub tint: [f32; 3],
    /// Shown as the channel mean, grey (a plan composition's channel).
    pub mono: bool,
}

impl LayerSpec {
    /// Whether converting `other` gives the same image as converting this.
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.patch, &other.patch)
            && self.stretch == other.stretch
            && self.tint == other.tint
            && self.mono == other.mono
    }
}

/// Whether two layer lists convert to the same image.
fn same_layers(a: &[LayerSpec], b: &[LayerSpec]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
}

/// What one of an ROI's textures on the canvas shown is converted from.
pub struct PatchWant {
    /// The ROI.
    pub roi: u64,
    /// Which of its previews: the live one (B), or a compare's frozen A.
    pub side: Side,
    /// The ROI's canvas rect: nothing outside it is drawn.
    pub clip: [f64; 4],
    /// The layers (none: nothing drawn, nothing kept).
    pub layers: Vec<LayerSpec>,
    /// Drawn this frame. A side not shown is kept converted all the same.
    pub shown: bool,
}

/// One texture of an ROI on one canvas.
#[derive(Default)]
struct Entry {
    /// The texture drawn, with the layers it was converted from.
    ready: Option<(Vec<LayerSpec>, TextureHandle)>,
    /// The conversion in flight, with its layers.
    pending: Option<(Vec<LayerSpec>, Task<Option<ColorImage>>)>,
    /// Layers whose conversion gave nothing (not tried again).
    failed: Option<Vec<LayerSpec>>,
    /// The image of the texture drawn (tests).
    #[cfg(test)]
    image: Option<Arc<ColorImage>>,
    /// Conversions started (tests).
    #[cfg(test)]
    conversions: usize,
}

/// The textures of the ROIs' patches on the canvas shown: one per ROI and
/// side, kept while it keeps being wanted.
#[derive(Default)]
pub struct PatchTextures {
    entries: BTreeMap<(u64, Side), Entry>,
    /// Each ROI's side drawn last frame (tests).
    #[cfg(test)]
    drawn: BTreeMap<u64, Side>,
    /// What the canvas was last given (tests).
    #[cfg(test)]
    last: Vec<Patch>,
}

impl PatchTextures {
    /// The patches to draw this frame: those of `wants` shown whose texture
    /// is ready. Each want's texture is kept converted, shown or not; one
    /// with no layers draws nothing. Textures not wanted are dropped. Call
    /// every frame the canvas is drawn.
    pub fn patches(&mut self, ctx: &egui::Context, wants: Vec<PatchWant>) -> Vec<Patch> {
        self.entries.retain(|(roi, side), _| {
            wants
                .iter()
                .any(|w| (w.roi, w.side) == (*roi, *side) && !w.layers.is_empty())
        });
        #[cfg(test)]
        self.drawn.clear();
        let mut out = Vec::new();
        for w in wants {
            if w.layers.is_empty() {
                continue;
            }
            let at = (w.roi, w.side);
            let lent = self.lend(at, &w.layers);
            let e = self.entries.entry(at).or_default();
            if let Some(lent) = lent {
                e.borrow(lent, &w.layers);
            }
            e.follow(ctx, w.roi, w.layers);
            // The rect of the patch the texture drawn was made from (not
            // the layers wanted now: a new rect waits for its texture).
            if w.shown
                && let Some((made, texture)) = &e.ready
                && let Some(first) = made.first()
            {
                out.push(Patch {
                    rect: first.patch.rect.map(|v| v as f64),
                    clip: w.clip,
                    texture: texture.clone(),
                });
                #[cfg(test)]
                self.drawn.insert(w.roi, w.side);
            }
        }
        #[cfg(test)]
        {
            self.last = out.clone();
        }
        out
    }

    /// A ready texture of another entry than `at` converted from `layers`.
    fn lend(&self, at: (u64, Side), layers: &[LayerSpec]) -> Option<Lent> {
        self.entries
            .iter()
            .filter(|(k, _)| **k != at)
            .find_map(|(_, e)| {
                let (made, texture) = e.ready.as_ref()?;
                same_layers(made, layers).then(|| Lent {
                    layers: made.clone(),
                    texture: texture.clone(),
                    #[cfg(test)]
                    image: e.image.clone(),
                })
            })
    }

    /// The image of ROI `roi`'s texture drawn last frame (tests).
    #[cfg(test)]
    pub fn image(&self, roi: u64) -> Option<Arc<ColorImage>> {
        let side = *self.drawn.get(&roi)?;
        self.entries.get(&(roi, side))?.image.clone()
    }

    /// Whether ROI `roi` has a texture (drawn or kept) for `side` (tests).
    #[cfg(test)]
    pub fn has(&self, roi: u64, side: Side) -> bool {
        self.entries.contains_key(&(roi, side))
    }

    /// The side of ROI `roi` drawn last frame (tests).
    #[cfg(test)]
    pub fn drawn_side(&self, roi: u64) -> Option<Side> {
        self.drawn.get(&roi).copied()
    }

    /// How many conversions ROI `roi`'s textures took so far (tests).
    #[cfg(test)]
    pub fn conversions(&self, roi: u64) -> usize {
        self.entries
            .iter()
            .filter(|((r, _), _)| *r == roi)
            .map(|(_, e)| e.conversions)
            .sum()
    }

    /// The patches the canvas was last given (tests).
    #[cfg(test)]
    pub fn last(&self) -> &[Patch] {
        &self.last
    }
}

/// A texture lent by one entry to another wanting the same layers.
struct Lent {
    layers: Vec<LayerSpec>,
    texture: TextureHandle,
    #[cfg(test)]
    image: Option<Arc<ColorImage>>,
}

impl Entry {
    /// Take `lent` (converted from `layers`) as the texture drawn, unless
    /// it is already, dropping a conversion of other layers in flight.
    fn borrow(&mut self, lent: Lent, layers: &[LayerSpec]) {
        if self
            .ready
            .as_ref()
            .is_some_and(|(k, _)| same_layers(k, layers))
        {
            return;
        }
        #[cfg(test)]
        {
            self.image = lent.image;
        }
        self.ready = Some((lent.layers, lent.texture));
        self.pending = None;
        self.failed = None;
    }

    /// Take a finished conversion, and start one for `layers` unless the
    /// texture (or the conversion in flight) is already theirs.
    fn follow(&mut self, ctx: &egui::Context, id: u64, layers: Vec<LayerSpec>) {
        if let Some((key, task)) = &self.pending {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(image) => {
                    let key = key.clone();
                    self.pending = None;
                    match image {
                        Some(image) => {
                            #[cfg(test)]
                            {
                                self.image = Some(Arc::new(image.clone()));
                            }
                            let texture = ctx.load_texture(
                                format!("roi-preview-{id}"),
                                image,
                                TextureOptions::LINEAR,
                            );
                            self.ready = Some((key, texture));
                        }
                        None => self.failed = Some(key),
                    }
                }
                Poll::Lost => {
                    self.failed = self.pending.take().map(|(k, _)| k);
                }
            }
        }
        let current = |k: &[LayerSpec]| same_layers(k, &layers);
        if self.ready.as_ref().is_some_and(|(k, _)| current(k))
            || self.pending.as_ref().is_some_and(|(k, _)| current(k))
            || self.failed.as_ref().is_some_and(|k| current(k))
        {
            return;
        }
        #[cfg(test)]
        {
            self.conversions += 1;
        }
        let work = layers.clone();
        let task = Task::spawn("mmm-preview-image", ctx, move || convert(&work));
        match task {
            Some(task) => self.pending = Some((layers, task)),
            None => self.failed = Some(layers),
        }
    }
}

/// The RGBA8 image of `layers` (all on the first one's grid): one plain
/// layer through its stretch, else the additive tinted mix. Pixels with no
/// data are transparent. `None` with nothing to convert.
pub fn convert(layers: &[LayerSpec]) -> Option<ColorImage> {
    let first = layers.first()?;
    let size = [first.patch.width as usize, first.patch.height as usize];
    let plain = |l: &LayerSpec| l.tint == [1.0; 3] && !l.mono;
    if let [only] = layers
        && plain(only)
    {
        return layer_image(only);
    }
    let mut mix = Mix::new(size);
    let mut any = false;
    for l in layers {
        // A member on another grid cannot be mixed in.
        if l.patch.rect != first.patch.rect
            || (l.patch.width, l.patch.height) != (first.patch.width, first.patch.height)
        {
            continue;
        }
        if let Some(img) = layer_image(l) {
            mix.add(&img, l.tint);
            any = true;
        }
    }
    any.then(|| mix.image())
}

/// One layer through its stretch.
fn layer_image(l: &LayerSpec) -> Option<ColorImage> {
    let p = &l.patch;
    let (w, h) = (p.width as usize, p.height as usize);
    let channels = p.channels as usize;
    let n = w * h;
    if channels == 0 || n == 0 || p.data.len() < channels * n || l.stretch.is_empty() {
        return None;
    }
    if l.mono {
        let mut mean = vec![0.0f32; n];
        for plane in p.data.chunks_exact(n).take(channels) {
            for (m, v) in mean.iter_mut().zip(plane) {
                *m += v;
            }
        }
        for m in &mut mean {
            *m /= channels as f32;
        }
        return Some(planar_image(&mean, n, w, (w, h), 1, &l.stretch[..1]));
    }
    // Colour needs three entries; a short stretch repeats its last.
    let mut stretch = l.stretch.clone();
    while stretch.len() < channels.min(3) {
        stretch.push(stretch[stretch.len() - 1]);
    }
    Some(planar_image(&p.data, n, w, (w, h), channels, &stretch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PATIENCE;

    fn patch(rect: [u64; 4], downsample: u32, channels: u32) -> Arc<PreviewPatch> {
        let w = (rect[2] - rect[0]).div_ceil(u64::from(downsample)) as u32;
        let h = (rect[3] - rect[1]).div_ceil(u64::from(downsample)) as u32;
        let n = (w * h) as usize;
        Arc::new(PreviewPatch {
            rect,
            width: w,
            height: h,
            channels,
            downsample,
            data: (0..n * channels as usize)
                .map(|i| 0.1 + (i % 7) as f32 / 10.0)
                .collect(),
        })
    }

    fn layer(patch: Arc<PreviewPatch>) -> LayerSpec {
        let s = StretchParams {
            black: 0.0,
            white: 1.0,
            midtones: 0.5,
        };
        LayerSpec {
            patch,
            stretch: vec![s; 3],
            tint: [1.0; 3],
            mono: false,
        }
    }

    /// ROI `roi`'s live preview over `clip`, shown.
    fn want(roi: u64, clip: [f64; 4], layers: Vec<LayerSpec>) -> PatchWant {
        PatchWant {
            roi,
            side: Side::B,
            clip,
            layers,
            shown: true,
        }
    }

    /// Call `patches` until it gives one (the conversion is on a worker).
    fn until_drawn(
        t: &mut PatchTextures,
        ctx: &egui::Context,
        wants: impl Fn() -> Vec<PatchWant>,
    ) -> Vec<Patch> {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            let out = t.patches(ctx, wants());
            if !out.is_empty() {
                return out;
            }
            assert!(std::time::Instant::now() < deadline, "never converted");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_downsampled_patch_covers_its_aligned_rect() {
        // An 8x patch of a 2000 x 1000 aligned rect: ceil(w/8) x ceil(h/8)
        // samples, drawn over the whole rect (not the texture's size).
        let p = patch([256, 512, 2256, 1512], 8, 3);
        assert_eq!((p.width, p.height), (250, 125));
        let img = convert(&[layer(p.clone())]).unwrap();
        assert_eq!(img.size, [250, 125]);
        let ctx = egui::Context::default();
        let mut t = PatchTextures::default();
        let clip = [300.0, 600.0, 900.0, 900.0];
        let out = until_drawn(&mut t, &ctx, || vec![want(1, clip, vec![layer(p.clone())])]);
        assert_eq!(out[0].rect, [256.0, 512.0, 2256.0, 1512.0]);
        assert_eq!(out[0].clip, clip);
        assert_eq!(out[0].texture.size(), [250, 125]);
    }

    #[test]
    fn the_rect_drawn_is_that_of_the_texture_drawn() {
        let a = patch([0, 0, 256, 256], 1, 3);
        let b = patch([256, 256, 768, 512], 1, 3);
        let ctx = egui::Context::default();
        let mut t = PatchTextures::default();
        let clip = [0.0, 0.0, 900.0, 900.0];
        let out = until_drawn(&mut t, &ctx, || vec![want(1, clip, vec![layer(a.clone())])]);
        assert_eq!(out[0].rect, [0.0, 0.0, 256.0, 256.0]);
        // The new patch's conversion has just been started: the texture of
        // the old one is still drawn, over the old rect.
        let out = t.patches(&ctx, vec![want(1, clip, vec![layer(b.clone())])]);
        assert_eq!(out[0].rect, [0.0, 0.0, 256.0, 256.0]);
        assert_eq!(out[0].texture.size(), [256, 256]);
        // Once converted, the new one over its own rect.
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            let out = t.patches(&ctx, vec![want(1, clip, vec![layer(b.clone())])]);
            if out[0].rect == [256.0, 256.0, 768.0, 512.0] {
                assert_eq!(out[0].texture.size(), [512, 256]);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "never converted");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn both_sides_stay_converted_and_only_the_shown_one_draws() {
        let a = patch([0, 0, 256, 256], 1, 3);
        let b = patch([0, 0, 256, 256], 1, 1);
        let ctx = egui::Context::default();
        let mut t = PatchTextures::default();
        let clip = [0.0, 0.0, 900.0, 900.0];
        let sides = |shown: Side| {
            let (a, b) = (a.clone(), b.clone());
            move || {
                vec![
                    PatchWant {
                        side: Side::A,
                        shown: shown == Side::A,
                        ..want(1, clip, vec![layer(a.clone())])
                    },
                    PatchWant {
                        shown: shown == Side::B,
                        ..want(1, clip, vec![layer(b.clone())])
                    },
                ]
            }
        };
        let out = until_drawn(&mut t, &ctx, sides(Side::B));
        assert_eq!(out.len(), 1, "one side drawn");
        assert_eq!(t.drawn_side(1), Some(Side::B));
        // A was converted meanwhile: switching draws it at once, with no
        // new conversion.
        let deadline = std::time::Instant::now() + PATIENCE;
        while t
            .entries
            .get(&(1, Side::A))
            .is_none_or(|e| e.ready.is_none())
        {
            assert!(std::time::Instant::now() < deadline, "A never converted");
            std::thread::sleep(std::time::Duration::from_millis(5));
            t.patches(&ctx, sides(Side::B)());
        }
        let conversions = t.conversions(1);
        let out = t.patches(&ctx, sides(Side::A)());
        assert_eq!((out.len(), t.drawn_side(1)), (1, Some(Side::A)));
        assert_eq!(t.conversions(1), conversions);
        // A side wanting what the other has converted borrows its texture.
        let mut t = PatchTextures::default();
        until_drawn(&mut t, &ctx, || vec![want(2, clip, vec![layer(a.clone())])]);
        let both = vec![
            want(2, clip, vec![layer(a.clone())]),
            PatchWant {
                side: Side::A,
                shown: false,
                ..want(2, clip, vec![layer(a.clone())])
            },
        ];
        t.patches(&ctx, both);
        assert_eq!(t.conversions(2), 1, "borrowed, not converted");
        assert!(t.entries[&(2, Side::A)].ready.is_some());
    }
}
