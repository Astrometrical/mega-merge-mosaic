//! The plan canvas (spec §5.3): every panel in place in the shared frame,
//! drawn from its low-resolution preview as each one arrives, with outlines,
//! numbers and selection. Only the canvas: the window around it picks the
//! view ([`PlanView::select_view`]) and the look ([`CanvasLook`]), and shows
//! the selected panel's details where it likes
//! ([`PlanView::selection_ui`]).
//!
//! - **Views.** Each group, then each composition (a colour mix of groups,
//!   [`crate::compositions`]). With grouping off there is one view.
//! - **Off the UI thread.** Previews become RGBA8 images through the display
//!   stretch on a small worker pool ([`Converter`]); the UI thread only
//!   uploads finished images, at most [`MAX_UPLOADS_PER_FRAME`] a frame.
//!   The automatic display stretch (statistics of the view's previews), the
//!   frame and analysis layout ([`load_layout`]) and the selected panel's
//!   header ([`read_info`]) are each read on a worker too.
//! - **Placement.** A panel is drawn at its preview's rect. Until its
//!   preview arrives it is drawn as a dashed outline where the last
//!   analysis placed it or, failing that, at its header-only WCS footprint
//!   in a solved frame ([`load_footprints`], once per previews generation
//!   and frame). The canvas extent is the shared frame's canvas, which does
//!   not move as previews arrive (so the user's zoom is kept).
//! - **Camera.** [`PlanView::set_camera`] moves the canvas without
//!   animation, from the next frame drawn. The canvas fits its extent on its
//!   first frame and whenever the extent changes; the camera is shown in
//!   place of those fits ([`PlanCanvas::set_view_for`]), so they are never
//!   painted. Set before the frame's canvas is known (the extent is then the
//!   drawn panels'), it is kept for each new extent until the canvas draws
//!   the frame's.
//! - **Memory.** The textures are dropped once the view has been hidden for
//!   [`RELEASE_AFTER`] ([`PlanView::release_if_hidden`]) and rebuilt from
//!   the in-memory previews when it is shown again.
//! - **Stretch.** Always automatic: [`DisplayStretch::params`] of the view's
//!   preview statistics, with the look's Linked and Strong
//!   ([`CanvasLook::stretch`]). It is recomputed as previews arrive, once
//!   they are all in, and when the view or the look changes. The manual
//!   stretch earlier versions saved per view (`plan_stretch` in
//!   `MosaicConfig.ui`) is ignored.
//! - **Compositions** add their channels' layers: each mapped group's
//!   previews are drawn as additive layers tinted red, green or blue times
//!   the channel's weight. Overlapping previews of one group add up too
//!   (brighter overlaps in a composition view).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, ColorImage, RichText};
use mega_ui::plan::{Layer, PlacedItem, PlanCanvas};
use mega_ui::theme::palette;
use mega_ui::theme::tokens::space;
use mmm_core::checks::{Severity, SolutionKind, is_accepted, panel_facts};
use mmm_core::formats::InputPanel;
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::FRAME_FILE;
use mmm_core::overlap::OverlapGraph;
use mmm_core::preview::{Preview, aligned_bbox, footprint, preview_frame_path};
use mmm_core::reference::ReferenceFrame;
use mmm_core::session::Session;
use mmm_core::stretch::{ChannelStats, StretchParams, apply, channel_stats};

use crate::camera::Camera;
use crate::checks::ChecksReport;
use crate::compositions::{Composition, ViewKind, composition_stretch_key, load_compositions};
use crate::display_stretch::DisplayStretch;
use crate::panel_edits::{group_colors, load_group_colors};
use crate::session::OpenSession;
use crate::status::format_canvas;
use crate::util::{Poll, Task, group_names};

/// Most preview textures uploaded in one frame (as the tiled viewer).
pub const MAX_UPLOADS_PER_FRAME: usize = 32;
/// Most samples the automatic stretch statistics read per channel.
const STATS_MAX_SAMPLES: usize = 1 << 20;
/// The plan textures are dropped once the view has been hidden this long.
pub const RELEASE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
/// Most preview converter threads.
const MAX_CONVERTERS: usize = 4;
/// The display channel colours of a composition (R, G, B).
const CHANNEL_COLORS: [Color32; 3] = [
    Color32::from_rgb(255, 0, 0),
    Color32::from_rgb(0, 255, 0),
    Color32::from_rgb(0, 0, 255),
];

/// What the plan reads from the shell's pipeline and checks each frame.
pub struct PlanFeed<'a> {
    /// The previews received for the current inputs, by panel path.
    pub previews: &'a BTreeMap<PathBuf, Arc<Preview>>,
    /// Changes when the previews are dropped (the inputs changed).
    pub generation: u64,
    /// Every preview of the current inputs has been run.
    pub complete: bool,
    /// The last checks report, if any.
    pub checks: Option<&'a ChecksReport>,
}

/// How the window around a canvas wants it drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasLook {
    /// Panel outlines.
    pub outlines: bool,
    /// Panel numbers.
    pub numbers: bool,
    /// The regions of interest (the merged mosaic draws them; the plan
    /// does not yet).
    pub rois: bool,
    /// The automatic display stretch's toggles.
    pub stretch: DisplayStretch,
}

impl Default for CanvasLook {
    fn default() -> Self {
        CanvasLook {
            outlines: true,
            numbers: true,
            rois: true,
            stretch: DisplayStretch::default(),
        }
    }
}

/// How a preview becomes an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Mode {
    /// Grey for mono, RGB from the first three channels for colour; the
    /// stretch is per channel.
    Native,
    /// The channel mean, grey, through the first stretch entry.
    Mono,
}

/// One texture of the view: a panel's preview as is (`slot: None`, a group
/// view) or for composition channel `slot`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TexKey {
    path: PathBuf,
    slot: Option<usize>,
}

/// A conversion for the [`Converter`] pool.
struct ConvertJob {
    epoch: u64,
    key: TexKey,
    preview: Arc<Preview>,
    mode: Mode,
    stretch: Vec<StretchParams>,
}

/// A finished conversion.
type Converted = (u64, TexKey, ColorImage);

/// A small pool of threads turning previews into RGBA8 images. Jobs of an
/// older epoch than the current one are skipped. The threads end when the
/// converter is dropped.
struct Converter {
    tx: Sender<ConvertJob>,
    rx: Receiver<Converted>,
    epoch: Arc<AtomicU64>,
}

impl Converter {
    fn start(ctx: &egui::Context) -> Option<Self> {
        let (tx, jobs) = mpsc::channel::<ConvertJob>();
        Self::start_on(ctx, tx, jobs, Arc::new(AtomicU64::new(0)))
    }

    /// Start the threads on `jobs` (fed by `tx`) at `epoch`.
    fn start_on(
        ctx: &egui::Context,
        tx: Sender<ConvertJob>,
        jobs: Receiver<ConvertJob>,
        epoch: Arc<AtomicU64>,
    ) -> Option<Self> {
        let (done, rx) = mpsc::channel::<Converted>();
        let jobs = Arc::new(Mutex::new(jobs));
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get() / 2)
            .clamp(1, MAX_CONVERTERS);
        let mut started = 0;
        for i in 0..threads {
            let (jobs, done, epoch, ctx) = (
                Arc::clone(&jobs),
                done.clone(),
                Arc::clone(&epoch),
                ctx.clone(),
            );
            let spawned = std::thread::Builder::new()
                .name(format!("mmm-plan-convert-{i}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let rx = jobs.lock().unwrap_or_else(|e| e.into_inner());
                            match rx.recv() {
                                Ok(job) => job,
                                Err(_) => return,
                            }
                        };
                        if job.epoch != epoch.load(Ordering::Relaxed) {
                            continue;
                        }
                        let image = preview_image(&job.preview, job.mode, &job.stretch);
                        if done.send((job.epoch, job.key, image)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                });
            match spawned {
                Ok(_) => started += 1,
                Err(e) => tracing::error!("cannot start a preview converter: {e}"),
            }
        }
        (started > 0).then_some(Converter { tx, rx, epoch })
    }
}

/// `p` as an RGBA8 image through `stretch` (see [`Mode`]). A pixel with
/// every channel zero (no data) is transparent.
fn preview_image(p: &Preview, mode: Mode, stretch: &[StretchParams]) -> ColorImage {
    let (w, h) = (p.width as usize, p.height as usize);
    let n = w * h;
    let ch = (p.channels as usize).max(1);
    let fallback = StretchParams {
        black: 0.0,
        white: 1.0,
        midtones: 0.5,
    };
    let param = |c: usize| {
        stretch
            .get(c)
            .or_else(|| stretch.last())
            .copied()
            .unwrap_or(fallback)
    };
    let byte = |c: usize, v: f32| (apply(&param(c), v) * 255.0).round() as u8;
    let sample = |c: usize, i: usize| p.data.get(c * n + i).copied().unwrap_or(0.0);
    let pixels = (0..n)
        .map(|i| {
            if (0..ch).all(|c| sample(c, i) == 0.0) {
                return Color32::TRANSPARENT;
            }
            match mode {
                Mode::Native if ch >= 3 => Color32::from_rgb(
                    byte(0, sample(0, i)),
                    byte(1, sample(1, i)),
                    byte(2, sample(2, i)),
                ),
                Mode::Native => Color32::from_gray(byte(0, sample(0, i))),
                Mode::Mono => {
                    let mean = (0..ch).map(|c| sample(c, i)).sum::<f32>() / ch as f32;
                    Color32::from_gray(byte(0, mean))
                }
            }
        })
        .collect();
    ColorImage::new([w, h], pixels)
}

/// Channels a group view shows: 3 when any preview is colour, else 1 (0
/// with no previews).
fn view_channels<'a>(previews: impl IntoIterator<Item = &'a Arc<Preview>>) -> usize {
    previews
        .into_iter()
        .map(|p| if p.channels >= 3 { 3 } else { 1 })
        .max()
        .unwrap_or(0)
}

/// Sampled planar values of `previews` (covered pixels only) for
/// `channels` channels in `mode`, at most [`STATS_MAX_SAMPLES`] each, with
/// the sample count.
fn sample_planes(previews: &[Arc<Preview>], mode: Mode, channels: usize) -> (Vec<f32>, usize) {
    let total: usize = previews.iter().map(|p| (p.width * p.height) as usize).sum();
    let stride = total.div_ceil(STATS_MAX_SAMPLES).max(1);
    let mut planes: Vec<Vec<f32>> = vec![Vec::new(); channels];
    for p in previews {
        let n = (p.width * p.height) as usize;
        let ch = (p.channels as usize).max(1);
        let sample = |c: usize, i: usize| p.data.get(c * n + i).copied().unwrap_or(0.0);
        for i in (0..n).step_by(stride) {
            if (0..ch).all(|c| sample(c, i) == 0.0) {
                continue;
            }
            match mode {
                Mode::Native => {
                    for (c, plane) in planes.iter_mut().enumerate() {
                        plane.push(sample(c.min(ch - 1), i));
                    }
                }
                Mode::Mono => {
                    let mean = (0..ch).map(|c| sample(c, i)).sum::<f32>() / ch as f32;
                    planes[0].push(mean);
                }
            }
        }
    }
    let count = planes.first().map_or(0, Vec::len);
    (planes.concat(), count)
}

/// The automatic display stretch of a view (worker thread). `sets`: per
/// stretch entry group, its previews and how they are shown — one Native
/// set for a group view, three Mono sets (R, G, B) for a composition. An
/// entry with no previews (an empty composition channel) gets the stretch
/// of zero statistics and, when `stretch` is linked, takes no part in the
/// shared stretch, which every entry then gets.
fn view_auto_stretch(
    sets: Vec<(Vec<Arc<Preview>>, Mode)>,
    stretch: DisplayStretch,
) -> Option<Vec<StretchParams>> {
    let zero = ChannelStats {
        median: 0.0,
        mad: 0.0,
    };
    // Per stretch entry: its statistics, `None` with no previews.
    let mut stats: Vec<Option<ChannelStats>> = Vec::new();
    for (previews, mode) in &sets {
        let channels = match mode {
            Mode::Native => view_channels(previews.iter()),
            Mode::Mono => 1,
        };
        if previews.is_empty() || channels == 0 {
            stats.push(None);
            continue;
        }
        let (planar, n) = sample_planes(previews, *mode, channels);
        stats.extend(channel_stats(&planar, n, channels).into_iter().map(Some));
    }
    if stats.is_empty() {
        return None;
    }
    let present: Vec<ChannelStats> = stats.iter().flatten().copied().collect();
    if stretch.linked && !present.is_empty() {
        let shared = stretch.params(&present)[0];
        return Some(vec![shared; stats.len()]);
    }
    let all: Vec<ChannelStats> = stats.iter().map(|s| s.unwrap_or(zero)).collect();
    Some(stretch.params(&all))
}

/// The shared frame's canvas and what the last analyses say about the
/// panels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// The shared frame, if it has been derived.
    pub frame: Option<ReferenceFrame>,
    /// Each analysed panel's content bbox in the frame, by panel file.
    pub rects: BTreeMap<PathBuf, [f64; 4]>,
    /// Each analysed panel's overlapping panels (in its group).
    pub overlaps: BTreeMap<PathBuf, Vec<PathBuf>>,
}

/// Read the frame and the analyses of `groups` (their session
/// directories). The frame is the previews' (`frames.0`), else, before any
/// complete previews run, the analysed one (`frames.1`). Blocking (worker
/// thread); what cannot be read is left out and logged.
pub fn load_layout(frames: (&Path, &Path), groups: &[PathBuf]) -> Layout {
    let mut out = Layout {
        frame: ReferenceFrame::load(frames.0)
            .or_else(|_| ReferenceFrame::load(frames.1))
            .ok(),
        ..Layout::default()
    };
    for dir in groups {
        let loaded = Session::open(dir)
            .and_then(|s| OverlapGraph::load(&s.overlap_graph_path()).map(|graph| (s, graph)));
        let (session, graph) = match loaded {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Plan: cannot read the analysis in {}: {e}", dir.display());
                continue;
            }
        };
        let file = |id: usize| {
            session
                .panels
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.source.clone().unwrap_or_else(|| p.path.clone()))
        };
        for p in &session.panels {
            let path = p.source.clone().unwrap_or_else(|| p.path.clone());
            let b = p.bbox;
            out.rects.insert(
                path.clone(),
                [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64],
            );
            out.overlaps.entry(path).or_default();
        }
        for e in &graph.edges {
            if let (Some(a), Some(b)) = (file(e.a), file(e.b)) {
                out.overlaps.entry(a.clone()).or_default().push(b.clone());
                out.overlaps.entry(b).or_default().push(a);
            }
        }
    }
    out
}

/// Each of `panels`' outline in `frame`: the header-only footprint in a
/// solved frame (see [`footprint`]), the strided covered-area scan in an
/// aligned one (see [`aligned_bbox`]). Blocking (worker thread); a panel
/// whose outline cannot be found is left out, and a cancelled `ctl` ends the
/// search early with what has been found.
pub fn load_footprints(
    frame: &ReferenceFrame,
    panels: &[PathBuf],
    ctl: &JobCtl,
) -> BTreeMap<PathBuf, [f64; 4]> {
    let mut out = BTreeMap::new();
    for p in panels {
        let found = match frame {
            ReferenceFrame::Aligned { .. } => aligned_bbox(p, ctl),
            ReferenceFrame::Solved { .. } => footprint(p, frame),
        };
        match found {
            Ok(Some(r)) => {
                out.insert(p.clone(), r);
            }
            Ok(None) => {}
            Err(mmm_core::error::Error::Cancelled) => break,
            Err(e) => tracing::debug!("Plan: no outline for {}: {e}", p.display()),
        }
    }
    out
}

/// Header facts of a panel for its details.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelInfo {
    /// Width in pixels.
    pub width: u64,
    /// Height in pixels.
    pub height: u64,
    /// Channel count.
    pub channels: u64,
    /// Astrometric solution kind.
    pub solution: SolutionKind,
    /// Pixel scale (arcsec / px), when solved.
    pub scale_arcsec: Option<f64>,
}

/// Read `path`'s header facts (worker thread).
pub fn read_info(path: &Path) -> Result<PanelInfo, String> {
    let panel = InputPanel::open(path).map_err(|e| e.to_string())?;
    let facts = panel_facts(path).map_err(|e| e.to_string())?;
    Ok(PanelInfo {
        width: panel.width(),
        height: panel.height(),
        channels: facts.channels,
        solution: facts.solution,
        scale_arcsec: facts.scale_arcsec,
    })
}

/// The command that shows `path` in the platform's file manager: Windows
/// `explorer /select,<path>`, macOS `open -R <path>`, elsewhere `xdg-open`
/// on the parent directory.
pub fn show_in_folder_command(path: &Path) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("explorer");
        // One raw argument: explorer does not parse a quoted "/select,…".
        c.raw_arg(format!("/select,\"{}\"", path.display()));
        c
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    }
}

/// Show `path` in the file manager, from a helper thread (never blocks the
/// UI); failures are logged.
fn show_in_folder(path: &Path) {
    let path = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("mmm-show-in-folder".into())
        .spawn(move || match show_in_folder_command(&path).spawn() {
            Ok(mut child) => {
                let _ = child.wait();
            }
            Err(e) => tracing::warn!("cannot show {} in its folder: {e}", path.display()),
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start the file manager thread: {e}");
    }
}

/// What a drawn item showed (for the self-test log and tests).
#[derive(Clone, Debug, PartialEq)]
pub struct ItemSnapshot {
    /// The panel file.
    pub path: PathBuf,
    /// Placement in the frame.
    pub rect: [f64; 4],
    /// Per layer: (tint, additive).
    pub layers: Vec<(Color32, bool)>,
    /// Outline colour (Hα for a panel with an unaccepted problem).
    pub outline: Color32,
    /// The panel number shown.
    pub label: String,
}

/// What the automatic stretch was last computed for.
#[derive(Clone, Debug, PartialEq)]
struct AutoKey {
    view: String,
    generation: u64,
    complete: bool,
    /// Preview count, rounded up to a power of two (recomputed as they
    /// arrive, a few times).
    bucket: usize,
    stretch: DisplayStretch,
    mapping: Vec<Option<String>>,
}

/// What the layout was last loaded for.
#[derive(Clone, Debug, PartialEq)]
struct LayoutKey {
    generation: u64,
    complete: bool,
    analysed: Vec<PathBuf>,
    provenance: usize,
}

/// A group drawn in a view.
struct DrawnGroup {
    /// The group's name.
    name: String,
    /// Its panels, in configuration order.
    panels: Vec<PathBuf>,
    /// Per layer: the composition slot (`None` in a group view) and tint.
    layers: Vec<(Option<usize>, Color32)>,
}

/// Each group's panels in configuration order (from the resolved groups,
/// once per frame); every panel under each name when the groups are
/// unknown.
pub fn group_members(session: &OpenSession, names: &[String]) -> BTreeMap<String, Vec<PathBuf>> {
    match &session.groups {
        Some(g) => g.processing_groups().into_iter().collect(),
        None => {
            let all: Vec<PathBuf> = session
                .mosaic
                .config
                .panels
                .iter()
                .map(|p| p.path.clone())
                .collect();
            names.iter().map(|n| (n.clone(), all.clone())).collect()
        }
    }
}

/// Whether `path` is one of the session's panels.
fn is_panel(session: &OpenSession, path: &Path) -> bool {
    session.mosaic.config.panels.iter().any(|p| p.path == path)
}

/// The view resolved for a frame.
struct ViewSpec {
    /// Texture identity and summary key (group name, or `comp:<name>`).
    key: String,
    /// The groups drawn, in order.
    groups: Vec<DrawnGroup>,
    /// Per composition slot, its group (empty for a group view).
    mapping: Vec<Option<String>>,
}

/// A camera set from outside, kept for each extent the canvas draws until
/// it draws the frame's (see the module docs).
#[derive(Clone, Copy, Debug)]
struct HeldCamera {
    camera: Camera,
    /// The extent it was last handed to the canvas for.
    extent: Option<[f64; 4]>,
    /// The canvas has drawn a frame through it since it was handed over.
    drawn: bool,
}

/// The plan canvas of the open session: what it draws, and the work behind
/// it (see the module docs).
pub struct PlanView {
    view: Option<ViewKind>,
    canvas: PlanCanvas,
    selected: Option<PathBuf>,
    held_camera: Option<HeldCamera>,
    /// The view key and previews generation the textures belong to.
    textures_for: Option<(String, u64)>,
    textures: HashMap<TexKey, egui::TextureHandle>,
    /// Sent to the converter in the current epoch.
    requested: HashSet<TexKey>,
    /// Conversions of the current epoch uploaded.
    uploaded: usize,
    converter: Option<Converter>,
    uploads: VecDeque<(TexKey, ColorImage)>,
    /// Each texture's image as uploaded (tests read its texels).
    #[cfg(test)]
    uploaded_images: HashMap<TexKey, ColorImage>,
    epoch: u64,
    /// The view's display stretch, once known.
    stretch: Option<Vec<StretchParams>>,
    auto: Option<Task<(AutoKey, Option<Vec<StretchParams>>)>>,
    auto_for: Option<AutoKey>,
    layout: Layout,
    layout_task: Option<Task<Layout>>,
    layout_for: Option<LayoutKey>,
    /// Header-only footprints of the panels in the frame (solved frames).
    footprints: BTreeMap<PathBuf, [f64; 4]>,
    footprint_task: Option<Task<BTreeMap<PathBuf, [f64; 4]>>>,
    /// Cancels the running footprint search (its generation or frame went).
    footprint_cancel: CancelToken,
    /// The previews generation and frame the footprints were found for.
    footprints_for: Option<(u64, ReferenceFrame)>,
    /// The previews generation the header facts belong to.
    info_generation: u64,
    /// When the view was last drawn (textures are dropped once hidden).
    last_shown: Option<Instant>,
    info: BTreeMap<PathBuf, Result<PanelInfo, String>>,
    info_task: Option<Task<(PathBuf, Result<PanelInfo, String>)>>,
    last_items: Vec<ItemSnapshot>,
    view_key: Option<String>,
    unplaced: usize,
    /// Layers of arrived previews not drawn (no texture yet) last frame.
    missing_layers: usize,
    settled: bool,
}

impl Default for PlanView {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanView {
    /// A view showing the first group once drawn.
    pub fn new() -> Self {
        PlanView {
            view: None,
            canvas: PlanCanvas::new(),
            selected: None,
            held_camera: None,
            textures_for: None,
            textures: HashMap::new(),
            requested: HashSet::new(),
            uploaded: 0,
            converter: None,
            uploads: VecDeque::new(),
            #[cfg(test)]
            uploaded_images: HashMap::new(),
            epoch: 0,
            stretch: None,
            auto: None,
            auto_for: None,
            layout: Layout::default(),
            layout_task: None,
            layout_for: None,
            footprints: BTreeMap::new(),
            footprint_task: None,
            footprint_cancel: CancelToken::new(),
            footprints_for: None,
            info_generation: 0,
            last_shown: None,
            info: BTreeMap::new(),
            info_task: None,
            last_items: Vec::new(),
            view_key: None,
            unplaced: 0,
            missing_layers: 0,
            settled: false,
        }
    }

    /// Show `view` (one that no longer exists falls back to the first group
    /// when drawn).
    pub fn select_view(&mut self, view: ViewKind) {
        self.view = Some(view);
    }

    /// The view shown (once drawn).
    pub fn view(&self) -> Option<&ViewKind> {
        self.view.as_ref()
    }

    /// Where the canvas looks: a camera set and not drawn yet, else what the
    /// last frame showed; `None` with neither.
    pub fn camera(&self) -> Option<Camera> {
        if let Some(held) = self.held_camera
            && !held.drawn
        {
            return Some(held.camera);
        }
        let (_, now) = self.canvas.last_frame()?;
        Some(Camera::of(self.canvas.view(), now))
    }

    /// Look through `camera` from the next frame on, with no animation (see
    /// the module docs for how it survives the canvas fitting its extent).
    pub fn set_camera(&mut self, camera: Camera) {
        self.held_camera = Some(HeldCamera {
            camera,
            extent: None,
            drawn: false,
        });
    }

    /// The selected panel.
    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Select the panel `path` (or none); the canvas selects by click.
    #[cfg(test)]
    pub fn select(&mut self, path: Option<PathBuf>) {
        self.selected = path;
    }

    /// The automatic display stretch in use, once known.
    pub fn stretch_for_view(&self) -> Option<&[StretchParams]> {
        self.stretch.as_deref()
    }

    /// What the last frame drew, item by item.
    pub fn items(&self) -> &[ItemSnapshot] {
        &self.last_items
    }

    /// Panels of the view with no place to draw them yet (no preview, no
    /// analysis and no footprint), last frame.
    pub fn unplaced(&self) -> usize {
        self.unplaced
    }

    /// True once the last frame had every preview in (complete), each drawn
    /// with all its layers through the current stretch, and no worker task,
    /// conversion or upload pending.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Drop the textures (and pending conversions) if the view has not been
    /// drawn for [`RELEASE_AFTER`]; they are rebuilt from the previews when
    /// it is shown again. Call every frame.
    pub fn release_if_hidden(&mut self, now: Instant) {
        if self
            .last_shown
            .is_some_and(|t| now.saturating_duration_since(t) > RELEASE_AFTER)
            && (!self.textures.is_empty() || !self.requested.is_empty())
        {
            self.textures.clear();
            self.textures_for = None;
            self.new_epoch();
        }
    }

    /// The canvas's rect and time in the last frame drawn.
    pub fn canvas_frame(&self) -> Option<(egui::Rect, f64)> {
        self.canvas.last_frame()
    }

    /// The shared frame's canvas, once read.
    pub fn frame_canvas(&self) -> Option<(u64, u64)> {
        self.layout.frame.as_ref().map(ReferenceFrame::canvas)
    }

    /// The shown view's key (a group name, or a composition's
    /// [`composition_stretch_key`]).
    pub fn view_key(&self) -> Option<&str> {
        self.view_key.as_deref()
    }

    /// Draw the canvas filling `ui`, and handle its input: a click selects
    /// a panel, a click on no panel or Esc clears the selection.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        look: &CanvasLook,
    ) {
        let ctx = ui.ctx().clone();
        self.last_shown = Some(Instant::now());
        if self.info_generation != feed.generation {
            // The panel files may have changed: read their headers again.
            self.info_generation = feed.generation;
            self.info.clear();
        }
        let groups = group_names(session);
        // Group membership, once per frame.
        let members = group_members(session, &groups);
        let compositions = if session.mosaic.config.grouping.enabled {
            load_compositions(&session.mosaic.config.ui)
        } else {
            Vec::new()
        };
        let valid = match &self.view {
            Some(ViewKind::Group(g)) => groups.contains(g),
            Some(ViewKind::Composition(i)) => *i < compositions.len(),
            None => false,
        };
        if !valid {
            self.view = groups.first().cloned().map(ViewKind::Group);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|p| !is_panel(session, p))
        {
            self.selected = None;
        }
        let spec = self.view_spec(session, &groups, &members, &compositions, feed);
        self.update(&ctx, session, feed, spec.as_ref(), look.stretch);
        let items = match &spec {
            Some(spec) => self.build_items(session, spec, feed),
            None => {
                self.last_items.clear();
                self.missing_layers = 0;
                Vec::new()
            }
        };
        match self.extent(session, &items) {
            Some((extent, fixed)) => self.canvas_ui(ui, session, extent, fixed, &items, look),
            None => {
                let text = if session.mosaic.config.panels.is_empty() {
                    "Add panels to plan a mosaic: use Add panels… or drop the files here."
                } else {
                    "Placing the panels…"
                };
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(text).color(palette::TEXT_MUTED));
                });
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ctx.egui_wants_keyboard_input() {
            self.selected = None;
        }
        self.settled = feed.complete
            && self.layout_task.is_none()
            && self.footprint_task.is_none()
            && self.auto.is_none()
            && self.info_task.is_none()
            && self.uploads.is_empty()
            && self.uploaded == self.requested.len()
            && self.missing_layers == 0;
    }

    /// The canvas over `extent` (`fixed`: the frame's, not the drawn
    /// panels'), through a held camera.
    fn canvas_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        extent: [f64; 4],
        fixed: bool,
        items: &[PlacedItem],
        look: &CanvasLook,
    ) {
        let now = ui.input(|i| i.time);
        if let Some(held) = &mut self.held_camera
            && held.extent != Some(extent)
        {
            let Camera { centre, zoom } = held.camera;
            self.canvas.set_view_for(extent, centre, zoom);
            held.extent = Some(extent);
            held.drawn = false;
        }
        let r = self
            .canvas
            .show(ui, extent, items, look.outlines, look.numbers);
        if self.canvas.last_frame().is_some_and(|(_, t)| t == now) {
            if let Some(held) = &mut self.held_camera {
                held.drawn = true;
            }
            if fixed {
                // No fit of a later extent to keep it from.
                self.held_camera = None;
            }
        }
        let path_of = |id: u64| {
            session
                .mosaic
                .config
                .panels
                .get(id as usize)
                .map(|p| p.path.clone())
        };
        if let Some(id) = r.clicked {
            self.selected = path_of(id);
        } else if r.clicked_empty {
            self.selected = None;
        }
    }

    /// The view to draw this frame, if any.
    fn view_spec(
        &self,
        session: &OpenSession,
        groups: &[String],
        members: &BTreeMap<String, Vec<PathBuf>>,
        compositions: &[Composition],
        feed: &PlanFeed<'_>,
    ) -> Option<ViewSpec> {
        let panels_of = |g: &str| members.get(g).cloned().unwrap_or_default();
        let colors = group_colors(
            &load_group_colors(&session.mosaic.config.ui),
            groups.iter().map(String::as_str),
        );
        match self.view.as_ref()? {
            ViewKind::Group(g) => {
                let tint = if session.mosaic.config.grouping.enabled {
                    colors.get(g).copied().unwrap_or(Color32::WHITE)
                } else {
                    Color32::WHITE
                };
                let panels = panels_of(g);
                let channels = view_channels(panels.iter().filter_map(|p| feed.previews.get(p)));
                // Colour previews are drawn in their own colours.
                let tint = if channels >= 3 { Color32::WHITE } else { tint };
                Some(ViewSpec {
                    key: g.clone(),
                    groups: vec![DrawnGroup {
                        name: g.clone(),
                        panels,
                        layers: vec![(None, tint)],
                    }],
                    mapping: Vec::new(),
                })
            }
            ViewKind::Composition(i) => {
                let c = compositions.get(*i)?;
                let mut drawn: Vec<DrawnGroup> = Vec::new();
                for (slot, ch) in c.channels.iter().enumerate() {
                    let Some((g, w)) = ch else { continue };
                    let tint = scale_color(CHANNEL_COLORS[slot], *w);
                    match drawn.iter_mut().find(|d| &d.name == g) {
                        Some(d) => d.layers.push((Some(slot), tint)),
                        None => drawn.push(DrawnGroup {
                            name: g.clone(),
                            panels: panels_of(g),
                            layers: vec![(Some(slot), tint)],
                        }),
                    }
                }
                Some(ViewSpec {
                    key: composition_stretch_key(&c.name),
                    groups: drawn,
                    mapping: c
                        .channels
                        .iter()
                        .map(|ch| ch.as_ref().map(|(g, _)| g.clone()))
                        .collect(),
                })
            }
        }
    }

    /// Poll the workers, follow view and stretch changes, start conversions
    /// and upload finished images.
    fn update(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        spec: Option<&ViewSpec>,
        stretch: DisplayStretch,
    ) {
        self.poll_tasks();
        self.start_layout(ctx, session, feed);
        self.start_footprints(ctx, session, feed);
        let Some(spec) = spec else {
            self.view_key = None;
            return;
        };
        // A new view (or composition name) starts its stretch afresh.
        if self.view_key.as_deref() != Some(&spec.key) {
            self.view_key = Some(spec.key.clone());
            self.stretch = None;
            self.auto = None;
            self.auto_for = None;
            self.new_epoch();
        }
        // Textures belong to one view and one previews generation.
        let tex_for = (spec.key.clone(), feed.generation);
        if self.textures_for.as_ref() != Some(&tex_for) {
            self.textures_for = Some(tex_for);
            self.textures.clear();
            self.new_epoch();
        }
        self.start_auto(ctx, feed, spec, stretch);
        self.start_conversions(ctx, feed, spec);
        self.take_conversions(ctx);
    }

    fn poll_tasks(&mut self) {
        if let Some(task) = &self.layout_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(layout) => {
                    self.layout_task = None;
                    self.layout = layout;
                }
                Poll::Lost => {
                    self.layout_task = None;
                    tracing::warn!("Plan: reading the layout failed (see the log)");
                }
            }
        }
        if let Some(task) = &self.footprint_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(f) => {
                    self.footprint_task = None;
                    self.footprints = f;
                }
                Poll::Lost => self.footprint_task = None,
            }
        }
        if let Some(task) = &self.info_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done((path, info)) => {
                    self.info_task = None;
                    self.info.insert(path, info);
                }
                Poll::Lost => self.info_task = None,
            }
        }
        if let Some(task) = &self.auto {
            let done = match task.poll() {
                Poll::Pending => return,
                Poll::Done(v) => Some(v),
                Poll::Lost => None,
            };
            self.auto = None;
            if let Some((key, Some(stretch))) = done
                && self.auto_for.as_ref() == Some(&key)
                && self.stretch.as_ref() != Some(&stretch)
            {
                self.stretch = Some(stretch);
                self.new_epoch();
            }
        }
    }

    /// Read the frame and the analyses again when the previews or analyses
    /// may have moved them.
    fn start_layout(&mut self, ctx: &egui::Context, session: &OpenSession, feed: &PlanFeed<'_>) {
        let cfg = &session.mosaic.config;
        let analysed: Vec<PathBuf> = cfg
            .group_dirs
            .keys()
            .filter(|g| !cfg.needs_analyze.contains(*g))
            .map(|g| session.mosaic.group_session_dir(g))
            .collect();
        let key = LayoutKey {
            generation: feed.generation,
            // Previews save the frame before the first one arrives.
            complete: feed.complete || !feed.previews.is_empty(),
            analysed: analysed.clone(),
            provenance: cfg.provenance.len(),
        };
        if self.layout_task.is_some() || self.layout_for.as_ref() == Some(&key) {
            return;
        }
        self.layout_for = Some(key);
        // The plan shows the panels where the previews put them: the
        // previews' frame, not the analysed one.
        let previews = preview_frame_path(&session.mosaic.dir);
        let analysed_frame = session.mosaic.dir.join(FRAME_FILE);
        self.layout_task = Task::spawn("mmm-plan-layout", ctx, move || {
            load_layout((&previews, &analysed_frame), &analysed)
        });
    }

    /// Find the panels' outlines once per previews generation and frame:
    /// header-only for solved frames, a strided pixel scan for aligned ones
    /// (both on a worker). A search for a superseded generation or frame is
    /// cancelled and its result dropped.
    fn start_footprints(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
    ) {
        let Some(frame) = self.layout.frame.clone() else {
            return;
        };
        let key = (feed.generation, frame);
        if self.footprints_for.as_ref() == Some(&key) {
            return;
        }
        // Whatever is running is for another generation or frame.
        self.footprint_cancel.cancel();
        self.footprint_task = None;
        self.footprint_cancel = CancelToken::new();
        let frame = key.1.clone();
        self.footprints_for = Some(key);
        let panels: Vec<PathBuf> = session
            .mosaic
            .config
            .panels
            .iter()
            .map(|p| p.path.clone())
            .collect();
        let cancel = self.footprint_cancel.clone();
        self.footprint_task = Task::spawn("mmm-plan-footprints", ctx, move || {
            load_footprints(&frame, &panels, &JobCtl::new(cancel, None))
        });
    }

    /// Compute the automatic stretch when the view's previews have changed
    /// enough, or the look's stretch has.
    fn start_auto(
        &mut self,
        ctx: &egui::Context,
        feed: &PlanFeed<'_>,
        spec: &ViewSpec,
        stretch: DisplayStretch,
    ) {
        if self.auto.is_some() {
            return;
        }
        let of = |g: &str| -> Vec<Arc<Preview>> {
            spec.groups
                .iter()
                .find(|d| d.name == g)
                .map(|d| {
                    d.panels
                        .iter()
                        .filter_map(|p| feed.previews.get(p).cloned())
                        .collect()
                })
                .unwrap_or_default()
        };
        let sets: Vec<(Vec<Arc<Preview>>, Mode)> = if spec.mapping.is_empty() {
            spec.groups
                .iter()
                .map(|d| (of(&d.name), Mode::Native))
                .collect()
        } else {
            spec.mapping
                .iter()
                .map(|g| (g.as_deref().map(of).unwrap_or_default(), Mode::Mono))
                .collect()
        };
        let count: usize = sets.iter().map(|(p, _)| p.len()).sum();
        if count == 0 {
            return;
        }
        let key = AutoKey {
            view: spec.key.clone(),
            generation: feed.generation,
            complete: feed.complete,
            bucket: count.next_power_of_two(),
            stretch,
            mapping: spec.mapping.clone(),
        };
        if self.auto_for.as_ref() == Some(&key) {
            return;
        }
        self.auto_for = Some(key.clone());
        self.auto = Task::spawn("mmm-plan-auto-stretch", ctx, move || {
            (key, view_auto_stretch(sets, stretch))
        });
    }

    /// Send the view's previews without a texture (in this epoch) to the
    /// converter.
    fn start_conversions(&mut self, ctx: &egui::Context, feed: &PlanFeed<'_>, spec: &ViewSpec) {
        let Some(stretch) = self.stretch.clone() else {
            return;
        };
        if self.converter.is_none() {
            self.converter = Converter::start(ctx);
            if let Some(conv) = &self.converter {
                conv.epoch.store(self.epoch, Ordering::Relaxed);
            }
        }
        let Some(conv) = &self.converter else { return };
        for d in &spec.groups {
            for path in &d.panels {
                let Some(preview) = feed.previews.get(path) else {
                    continue;
                };
                for (slot, _) in &d.layers {
                    let key = TexKey {
                        path: path.clone(),
                        slot: *slot,
                    };
                    if self.requested.contains(&key) {
                        continue;
                    }
                    let (mode, stretch) = match slot {
                        None => (Mode::Native, stretch.clone()),
                        Some(s) => (Mode::Mono, stretch.get(*s).copied().into_iter().collect()),
                    };
                    let job = ConvertJob {
                        epoch: self.epoch,
                        key: key.clone(),
                        preview: Arc::clone(preview),
                        mode,
                        stretch,
                    };
                    if conv.tx.send(job).is_ok() {
                        self.requested.insert(key);
                    }
                }
            }
        }
    }

    /// Take finished images and upload up to [`MAX_UPLOADS_PER_FRAME`].
    fn take_conversions(&mut self, ctx: &egui::Context) {
        if let Some(conv) = &self.converter {
            while let Ok((epoch, key, image)) = conv.rx.try_recv() {
                if epoch == self.epoch {
                    self.uploads.push_back((key, image));
                }
            }
        }
        for _ in 0..MAX_UPLOADS_PER_FRAME {
            let Some((key, image)) = self.uploads.pop_front() else {
                break;
            };
            let name = format!("plan:{}:{:?}", key.path.display(), key.slot);
            #[cfg(test)]
            self.uploaded_images.insert(key.clone(), image.clone());
            let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
            self.textures.insert(key, tex);
            self.uploaded += 1;
        }
        if !self.uploads.is_empty() {
            ctx.request_repaint();
        }
    }

    /// Convert everything again (the stretch or view changed); textures
    /// already shown stay until replaced.
    fn new_epoch(&mut self) {
        self.epoch += 1;
        self.requested.clear();
        self.uploads.clear();
        self.uploaded = 0;
        if let Some(conv) = &self.converter {
            conv.epoch.store(self.epoch, Ordering::Relaxed);
        }
    }

    /// The items of the view, and the snapshot of what they show.
    fn build_items(
        &mut self,
        session: &OpenSession,
        spec: &ViewSpec,
        feed: &PlanFeed<'_>,
    ) -> Vec<PlacedItem> {
        let cfg = &session.mosaic.config;
        let flagged: BTreeSet<&PathBuf> = feed
            .checks
            .map(|r| {
                r.checks
                    .iter()
                    .filter(|c| c.severity == Severity::Problem && !is_accepted(cfg, c))
                    .flat_map(|c| c.panels.iter())
                    .collect()
            })
            .unwrap_or_default();
        let ids: HashMap<&PathBuf, usize> = cfg
            .panels
            .iter()
            .enumerate()
            .map(|(i, p)| (&p.path, i))
            .collect();
        let mut items = Vec::new();
        let mut snaps = Vec::new();
        self.unplaced = 0;
        self.missing_layers = 0;
        // Numbers run across the view (unique in a composition too).
        let mut number = 0;
        for d in &spec.groups {
            for path in &d.panels {
                let path = path.clone();
                number += 1;
                let preview = feed.previews.get(&path);
                // Preview, else where analysis put it, else the header
                // footprint (solved frames); dashed until the preview.
                let known = self
                    .layout
                    .rects
                    .get(&path)
                    .or_else(|| self.footprints.get(&path));
                let rect = match (preview, known) {
                    (Some(p), _) => p.rect,
                    (None, Some(r)) => *r,
                    (None, None) => {
                        self.unplaced += 1;
                        continue;
                    }
                };
                let mut drawn = Vec::new();
                if preview.is_some() {
                    for (slot, tint) in &d.layers {
                        let key = TexKey {
                            path: path.clone(),
                            slot: *slot,
                        };
                        match self.textures.get(&key) {
                            Some(tex) => drawn.push(Layer {
                                texture: tex.clone(),
                                tint: *tint,
                                additive: slot.is_some(),
                            }),
                            None => self.missing_layers += 1,
                        }
                    }
                }
                let Some(&id) = ids.get(&path) else {
                    continue;
                };
                let flag = flagged.contains(&path);
                let outline = if flag {
                    palette::HA_500
                } else {
                    palette::ACCENT_500
                };
                snaps.push(ItemSnapshot {
                    path: path.clone(),
                    rect,
                    layers: drawn.iter().map(|l| (l.tint, l.additive)).collect(),
                    outline,
                    label: number.to_string(),
                });
                items.push(PlacedItem {
                    id: id as u64,
                    rect,
                    layers: drawn,
                    outline,
                    label: number.to_string(),
                    marker: flag.then_some(palette::HA_500),
                    selected: self.selected.as_ref() == Some(&path),
                });
            }
        }
        self.last_items = snaps;
        items
    }

    /// The canvas extent and whether it is the shared frame's (stable as
    /// previews arrive), else the union of the drawn rects before the frame
    /// is known.
    fn extent(&self, session: &OpenSession, items: &[PlacedItem]) -> Option<([f64; 4], bool)> {
        if let Some((w, h)) = self.frame_canvas().or(session.canvas()) {
            return Some(([0.0, 0.0, w as f64, h as f64], true));
        }
        items
            .iter()
            .map(|it| it.rect)
            .reduce(|a, b| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            })
            .map(|e| (e, false))
    }

    /// The selected panel's details: its name and path, Show in folder and
    /// Copy path, its header facts (read on a worker) and the panels it
    /// overlaps; a hint when none is selected.
    pub fn selection_ui(&mut self, ui: &mut egui::Ui, session: &OpenSession) {
        ui.label(RichText::new("Panel").color(palette::TEXT_STRONG));
        let Some(path) = self.selected.clone().filter(|p| is_panel(session, p)) else {
            ui.label(RichText::new("Click a panel to select it.").color(palette::TEXT_MUTED));
            return;
        };
        if !self.info.contains_key(&path) && self.info_task.is_none() {
            let p = path.clone();
            self.info_task = Task::spawn("mmm-plan-panel-info", ui.ctx(), move || {
                let info = read_info(&p);
                (p, info)
            });
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        ui.label(RichText::new(name).strong().color(palette::TEXT_STRONG));
        ui.add(
            egui::Label::new(
                RichText::new(path.display().to_string())
                    .small()
                    .color(palette::TEXT_MUTED),
            )
            .wrap(),
        );
        ui.horizontal(|ui| {
            if ui
                .button("Show in folder")
                .on_hover_text("Open the folder holding this file")
                .clicked()
            {
                show_in_folder(&path);
            }
            if ui
                .button("Copy path")
                .on_hover_text("Copy the full path to the clipboard")
                .clicked()
            {
                ui.ctx().copy_text(path.display().to_string());
            }
        });
        egui::Grid::new("plan-panel-props")
            .num_columns(2)
            .spacing([space::M, space::XS])
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(RichText::new(k).color(palette::TEXT_BODY));
                    ui.label(RichText::new(v).color(palette::TEXT_STRONG));
                    ui.end_row();
                };
                match self.info.get(&path) {
                    None => row(ui, "Header", "Reading…".into()),
                    Some(Err(e)) => row(ui, "Header", format!("Unreadable: {e}")),
                    Some(Ok(info)) => {
                        row(ui, "Size", format_canvas(info.width, info.height));
                        row(ui, "Channels", info.channels.to_string());
                        row(ui, "Solution", solution_text(info.solution).into());
                        if let Some(s) = info.scale_arcsec {
                            row(ui, "Scale", format!("{s:.3}″/px"));
                        }
                    }
                }
            });
        ui.label(RichText::new("Overlaps").color(palette::TEXT_BODY));
        match self.layout.overlaps.get(&path) {
            None => {
                ui.label(RichText::new("Shown once analysed.").color(palette::TEXT_MUTED));
            }
            Some(o) if o.is_empty() => {
                ui.label(RichText::new("No other panel").color(palette::HA_400));
            }
            Some(o) => {
                for p in o {
                    let n = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(RichText::new(n).color(palette::TEXT_STRONG));
                }
            }
        }
    }
}

#[cfg(test)]
impl PlanView {
    /// Screen position of canvas point `p` in the last frame drawn.
    pub(crate) fn to_screen(&self, p: (f64, f64)) -> Option<egui::Pos2> {
        let (rect, now) = self.canvas.last_frame()?;
        Some(self.canvas.view().image_to_screen(p, rect, now))
    }

    /// Whether `path`'s header facts have been read.
    pub(crate) fn has_info(&self, path: &Path) -> bool {
        self.info.contains_key(path)
    }
}

/// `c` with its RGB scaled by `w` (clamped to `[0, 255]`).
fn scale_color(c: Color32, w: f32) -> Color32 {
    let s = |v: u8| (f32::from(v) * w).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

/// Plain text of a solution kind.
fn solution_text(k: SolutionKind) -> &'static str {
    match k {
        SolutionKind::None => "None (registered)",
        SolutionKind::Linear => "Linear (TAN)",
        SolutionKind::Distorted => "With distortion",
    }
}

#[cfg(test)]
#[path = "plan_view_tests.rs"]
pub(crate) mod tests;
