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

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{ColorImage, TextureHandle, TextureOptions};
use mega_ui::canvas_layers::Patch;
use mmm_core::mosaic::PreviewPatch;
use mmm_core::stretch::StretchParams;

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

/// What one ROI draws on one canvas.
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

/// The textures of the ROIs' patches on the canvas shown: one per ROI, kept
/// while the ROI keeps being drawn.
#[derive(Default)]
pub struct PatchTextures {
    entries: BTreeMap<u64, Entry>,
    /// What the canvas was last given (tests).
    #[cfg(test)]
    last: Vec<Patch>,
}

impl PatchTextures {
    /// The patches to draw this frame. `wants`: per ROI drawn (id, canvas
    /// rect, layers); one with no layers draws nothing. Textures of other
    /// ROIs are dropped. Call every frame the canvas is drawn.
    pub fn patches(
        &mut self,
        ctx: &egui::Context,
        wants: Vec<(u64, [f64; 4], Vec<LayerSpec>)>,
    ) -> Vec<Patch> {
        self.entries
            .retain(|id, _| wants.iter().any(|(r, _, l)| r == id && !l.is_empty()));
        let mut out = Vec::new();
        for (id, clip, layers) in wants {
            let Some(first) = layers.first() else {
                continue;
            };
            let rect = first.patch.rect.map(|v| v as f64);
            let e = self.entries.entry(id).or_default();
            e.follow(ctx, id, layers);
            if let Some((_, texture)) = &e.ready {
                out.push(Patch {
                    rect,
                    clip,
                    texture: texture.clone(),
                });
            }
        }
        #[cfg(test)]
        {
            self.last = out.clone();
        }
        out
    }

    /// The image the texture of ROI `roi` was converted to (tests).
    #[cfg(test)]
    pub fn image(&self, roi: u64) -> Option<Arc<ColorImage>> {
        self.entries.get(&roi)?.image.clone()
    }

    /// How many conversions ROI `roi`'s texture took so far (tests).
    #[cfg(test)]
    pub fn conversions(&self, roi: u64) -> usize {
        self.entries.get(&roi).map_or(0, |e| e.conversions)
    }

    /// The patches the canvas was last given (tests).
    #[cfg(test)]
    pub fn last(&self) -> &[Patch] {
        &self.last
    }
}

impl Entry {
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
