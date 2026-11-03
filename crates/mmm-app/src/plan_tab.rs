//! The Plan tab (spec §5.3): every panel in place in the shared frame, drawn
//! from its low-resolution preview as each one arrives, with outlines,
//! numbers, selection, the panel's properties and its checks.
//!
//! - **Views.** Each group, then each composition (a colour mix of groups,
//!   [`crate::compositions`]). With grouping off there is one view and no
//!   selector.
//! - **Off the UI thread.** Previews become RGBA8 images through the Plan
//!   display stretch on a small worker pool ([`Converter`]); the UI thread
//!   only uploads finished images, at most [`MAX_UPLOADS_PER_FRAME`] a
//!   frame. The automatic display stretch (statistics of the view's
//!   previews), the frame and analysis layout ([`load_layout`]) and the
//!   selected panel's header ([`read_info`]) are each read on a worker too.
//! - **Placement.** A panel is drawn at its preview's rect. Until its
//!   preview arrives it is drawn as a dashed outline where the last
//!   analysis placed it, if it has been analysed. The canvas extent is the
//!   shared frame's canvas, which does not move as previews arrive (so the
//!   user's zoom is kept).
//! - **Stretch.** One display stretch per view, independent of Blend's:
//!   automatic (recomputed as previews arrive and once they are all in)
//!   until the user sets one, which is saved in `MosaicConfig.ui` under
//!   [`PLAN_STRETCH_KEY`] by group name, or by
//!   [`composition_stretch_key`] for a composition. The save is a small
//!   `mosaic.json` write on the UI thread, rate-limited as in Blend (the
//!   documented exception).
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
use mega_ui::checks::{CheckAction, CheckItem, checks_list};
use mega_ui::plan::{Layer, PlacedItem, PlanCanvas};
use mega_ui::theme::{Severity as UiSeverity, palette};
use mmm_core::checks::{Check, Severity, SolutionKind, is_accepted, panel_facts};
use mmm_core::formats::InputPanel;
use mmm_core::mosaic::FRAME_FILE;
use mmm_core::overlap::OverlapGraph;
use mmm_core::preview::Preview;
use mmm_core::reference::ReferenceFrame;
use mmm_core::session::Session;
use mmm_core::stretch::{StretchParams, apply, auto_stretch, channel_stats};

use crate::blend_tab::{Debounce, Poll, Task, group_names};
use crate::checks::ChecksReport;
use crate::compositions::{
    Composition, ViewKind, composition_editor, composition_stretch_key, load_compositions,
    save_compositions, unique_composition_name,
};
use crate::panels_tab::{group_colors, load_group_colors};
use crate::session::OpenSession;
use crate::status::format_canvas;
use crate::stretch_panel::{
    PLAN_STRETCH_KEY, StretchChange, StretchState, save_stretch_at, saved_stretch_at, stretch_panel,
};

/// Most preview textures uploaded in one frame (as the tiled viewer).
pub const MAX_UPLOADS_PER_FRAME: usize = 32;
/// Most samples the automatic stretch statistics read per channel.
const STATS_MAX_SAMPLES: usize = 1 << 20;
/// Width of the side panel.
const SIDE_WIDTH: f32 = 300.0;
/// Most preview converter threads.
const MAX_CONVERTERS: usize = 4;
/// The display channel colours of a composition (R, G, B).
const CHANNEL_COLORS: [Color32; 3] = [
    Color32::from_rgb(255, 0, 0),
    Color32::from_rgb(0, 255, 0),
    Color32::from_rgb(0, 0, 255),
];

/// What the Plan tab reads from the shell's pipeline and checks each frame.
pub struct PlanFeed<'a> {
    /// The previews received for the current inputs, by panel path.
    pub previews: &'a BTreeMap<PathBuf, Arc<Preview>>,
    /// Changes when the previews are dropped (the inputs changed).
    pub generation: u64,
    /// Every preview of the current inputs has been run.
    pub complete: bool,
    /// Previews are running or about to start (no Resume is offered).
    pub previews_busy: bool,
    /// The last checks report, if any.
    pub checks: Option<&'a ChecksReport>,
}

/// What the user asked of the shell in a frame.
#[derive(Debug, Default)]
pub struct PlanOutput {
    /// Run the (paused) previews again.
    pub resume_previews: bool,
    /// Accept these checks.
    pub accept: Vec<Check>,
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
        let (done, rx) = mpsc::channel::<Converted>();
        let jobs = Arc::new(Mutex::new(jobs));
        let epoch = Arc::new(AtomicU64::new(0));
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
/// set for a group view, three Mono sets (R, G, B) for a composition.
fn view_auto_stretch(
    sets: Vec<(Vec<Arc<Preview>>, Mode)>,
    linked: bool,
) -> Option<Vec<StretchParams>> {
    let mut stats = Vec::new();
    for (previews, mode) in &sets {
        let channels = match mode {
            Mode::Native => view_channels(previews.iter()),
            Mode::Mono => 1,
        };
        if channels == 0 {
            stats.push(mmm_core::stretch::ChannelStats {
                median: 0.0,
                mad: 0.0,
            });
            continue;
        }
        let (planar, n) = sample_planes(previews, *mode, channels);
        stats.extend(channel_stats(&planar, n, channels));
    }
    (!stats.is_empty()).then(|| auto_stretch(&stats, linked))
}

/// The shared frame's canvas and what the last analyses say about the
/// panels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Canvas `(width, height)` of the shared frame, if it has been derived.
    pub canvas: Option<(u64, u64)>,
    /// Each analysed panel's content bbox in the frame, by panel file.
    pub rects: BTreeMap<PathBuf, [f64; 4]>,
    /// Each analysed panel's overlapping panels (in its group).
    pub overlaps: BTreeMap<PathBuf, Vec<PathBuf>>,
}

/// Read the frame (`frame`) and the analyses of `groups` (their session
/// directories). Blocking (worker thread); what cannot be read is left out
/// and logged.
pub fn load_layout(frame: &Path, groups: &[PathBuf]) -> Layout {
    let mut out = Layout {
        canvas: ReferenceFrame::load(frame).ok().map(|f| f.canvas()),
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

/// Header facts of a panel for the side panel.
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
    linked: bool,
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

/// A group drawn in a view: its name and, per layer, the composition
/// slot (`None` in a group view) and the tint.
type DrawnGroup = (String, Vec<(Option<usize>, Color32)>);

/// The view resolved for a frame.
struct ViewSpec {
    /// Stretch and texture identity (group name, or `comp:<name>`).
    key: String,
    /// Per drawn group: its name and, per layer, `(slot, tint)`.
    groups: Vec<DrawnGroup>,
    /// Channels of the stretch.
    channels: usize,
    /// Per composition slot, its group (empty for a group view).
    mapping: Vec<Option<String>>,
}

/// The Plan tab's state for the open session.
pub struct PlanTab {
    view: Option<ViewKind>,
    canvas: PlanCanvas,
    show_outlines: bool,
    show_numbers: bool,
    selected: Option<PathBuf>,
    show_all_checks: bool,
    /// The view key and previews generation the textures belong to.
    textures_for: Option<(String, u64)>,
    textures: HashMap<TexKey, egui::TextureHandle>,
    /// Sent to the converter in the current epoch.
    requested: HashSet<TexKey>,
    converter: Option<Converter>,
    uploads: VecDeque<(TexKey, ColorImage)>,
    epoch: u64,
    /// The view's display stretch, once known.
    stretch: Option<Vec<StretchParams>>,
    manual: bool,
    linked: bool,
    auto: Option<Task<(AutoKey, Option<Vec<StretchParams>>)>>,
    auto_for: Option<AutoKey>,
    save: Debounce,
    layout: Layout,
    layout_task: Option<Task<Layout>>,
    layout_for: Option<LayoutKey>,
    info: BTreeMap<PathBuf, Result<PanelInfo, String>>,
    info_task: Option<Task<(PathBuf, Result<PanelInfo, String>)>>,
    last_items: Vec<ItemSnapshot>,
    view_key: Option<String>,
    unplaced: usize,
    /// Layers of arrived previews not drawn (no texture yet) last frame.
    missing_layers: usize,
    settled: bool,
}

impl Default for PlanTab {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanTab {
    /// A tab showing the first group once drawn.
    pub fn new() -> Self {
        PlanTab {
            view: None,
            canvas: PlanCanvas::new(),
            show_outlines: true,
            show_numbers: true,
            selected: None,
            show_all_checks: false,
            textures_for: None,
            textures: HashMap::new(),
            requested: HashSet::new(),
            converter: None,
            uploads: VecDeque::new(),
            epoch: 0,
            stretch: None,
            manual: false,
            linked: false,
            auto: None,
            auto_for: None,
            save: Debounce::default(),
            layout: Layout::default(),
            layout_task: None,
            layout_for: None,
            info: BTreeMap::new(),
            info_task: None,
            last_items: Vec::new(),
            view_key: None,
            unplaced: 0,
            missing_layers: 0,
            settled: false,
        }
    }

    /// Show `view`.
    pub fn select_view(&mut self, view: ViewKind) {
        self.view = Some(view);
    }

    /// The view shown (once drawn).
    pub fn view(&self) -> Option<&ViewKind> {
        self.view.as_ref()
    }

    /// Select the panel `path` (or none).
    #[cfg(test)]
    pub fn select(&mut self, path: Option<PathBuf>) {
        self.selected = path;
    }

    /// The selected panel.
    #[cfg(test)]
    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// What the last frame drew, item by item.
    pub fn items(&self) -> &[ItemSnapshot] {
        &self.last_items
    }

    /// The display stretch of the view, once known.
    pub fn stretch(&self) -> Option<&[StretchParams]> {
        self.stretch.as_deref()
    }

    /// The shared frame's canvas, once read.
    pub fn frame_canvas(&self) -> Option<(u64, u64)> {
        self.layout.canvas
    }

    /// True once the last frame had every preview in (complete), each drawn
    /// with all its layers, and no worker task or upload pending.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Save a pending (rate-limited) display stretch or composition change
    /// now (the window is closing). A small `mosaic.json` write.
    pub fn flush_save(&mut self, session: &OpenSession) {
        if !self.save.pending() {
            return;
        }
        self.save.saved(Instant::now());
        if let Err(e) = session.mosaic.save() {
            tracing::warn!("Plan settings not saved: {e}");
        }
    }

    /// Draw the tab.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        feed: &PlanFeed<'_>,
    ) -> PlanOutput {
        let ctx = ui.ctx().clone();
        let mut out = PlanOutput::default();
        let grouping = session.mosaic.config.grouping.enabled;
        let groups = group_names(session);
        let compositions = if grouping {
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
        let spec = self.view_spec(session, &groups, &compositions, feed);
        self.update(&ctx, session, feed, spec.as_ref());

        egui::Panel::top("plan-toolbar").show(ui, |ui| {
            self.toolbar(
                ui,
                session,
                &groups,
                &compositions,
                grouping,
                feed,
                &mut out,
            );
        });
        egui::Panel::right("plan-side")
            .resizable(false)
            .exact_size(SIDE_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if let Some(ViewKind::Composition(i)) = self.view.clone()
                        && let Some(c) = compositions.get(i)
                    {
                        self.composition_ui(ui, session, &groups, &compositions, i, c);
                        ui.add_space(12.0);
                    }
                    self.stretch_ui(ui, session, spec.as_ref());
                    ui.add_space(12.0);
                    self.selection_ui(ui);
                    ui.add_space(12.0);
                    self.checks_ui(ui, session, feed, &mut out);
                });
            });
        let items = match &spec {
            Some(spec) => self.build_items(session, spec, feed),
            None => {
                self.last_items.clear();
                self.missing_layers = 0;
                Vec::new()
            }
        };
        let extent = self.extent(session, &items);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette::SURFACE_900))
            .show(ui, |ui| match extent {
                Some(extent) => {
                    let r =
                        self.canvas
                            .show(ui, extent, &items, self.show_outlines, self.show_numbers);
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
                None => {
                    let text = if session.mosaic.config.panels.is_empty() {
                        "Add panels in the Panels tab to plan a mosaic."
                    } else {
                        "Placing the panels…"
                    };
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new(text).color(palette::TEXT_MUTED));
                    });
                }
            });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ctx.egui_wants_keyboard_input() {
            self.selected = None;
        }
        self.settled = feed.complete
            && self.layout_task.is_none()
            && self.auto.is_none()
            && self.info_task.is_none()
            && self.uploads.is_empty()
            && self.missing_layers == 0;
        self.save_if_due(&ctx, session);
        out
    }

    /// The view to draw this frame, if any.
    fn view_spec(
        &self,
        session: &OpenSession,
        groups: &[String],
        compositions: &[Composition],
        feed: &PlanFeed<'_>,
    ) -> Option<ViewSpec> {
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
                let panels = group_panels(session, g);
                let channels = view_channels(panels.iter().filter_map(|p| feed.previews.get(p)));
                // Colour previews are drawn in their own colours.
                let tint = if channels >= 3 { Color32::WHITE } else { tint };
                Some(ViewSpec {
                    key: g.clone(),
                    groups: vec![(g.clone(), vec![(None, tint)])],
                    channels,
                    mapping: Vec::new(),
                })
            }
            ViewKind::Composition(i) => {
                let c = compositions.get(*i)?;
                let mut drawn: Vec<DrawnGroup> = Vec::new();
                for (slot, ch) in c.channels.iter().enumerate() {
                    let Some((g, w)) = ch else { continue };
                    let tint = scale_color(CHANNEL_COLORS[slot], *w);
                    match drawn.iter_mut().find(|(n, _)| n == g) {
                        Some((_, layers)) => layers.push((Some(slot), tint)),
                        None => drawn.push((g.clone(), vec![(Some(slot), tint)])),
                    }
                }
                Some(ViewSpec {
                    key: composition_stretch_key(&c.name),
                    groups: drawn,
                    channels: 3,
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
    ) {
        self.poll_tasks();
        self.start_layout(ctx, session, feed);
        let Some(spec) = spec else {
            self.view_key = None;
            return;
        };
        // A new view (or composition name) starts its stretch afresh.
        if self.view_key.as_deref() != Some(&spec.key) {
            self.view_key = Some(spec.key.clone());
            let saved = saved_stretch_at(&session.mosaic.config.ui, PLAN_STRETCH_KEY, &spec.key);
            self.manual = saved.is_some();
            self.linked = saved
                .as_ref()
                .is_some_and(|s| s.len() > 1 && s.iter().all(|p| *p == s[0]));
            self.stretch = saved;
            self.auto = None;
            self.auto_for = None;
            self.new_epoch();
        }
        // Textures belong to one view and one previews generation.
        let tex_for = (spec.key.clone(), feed.generation);
        if self.textures_for.as_ref() != Some(&tex_for) {
            self.textures_for = Some(tex_for);
            self.textures.clear();
            self.uploads.clear();
            self.new_epoch();
        }
        self.start_auto(ctx, session, feed, spec);
        self.start_conversions(ctx, session, feed, spec);
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
                && !self.manual
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
        let frame = session.mosaic.dir.join(FRAME_FILE);
        self.layout_task = Task::spawn("mmm-plan-layout", ctx, move || {
            load_layout(&frame, &analysed)
        });
    }

    /// Compute the automatic stretch when the view's previews have changed
    /// enough (unless the user set one).
    fn start_auto(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        spec: &ViewSpec,
    ) {
        if self.manual || self.auto.is_some() {
            return;
        }
        let of = |g: &str| -> Vec<Arc<Preview>> {
            group_panels(session, g)
                .iter()
                .filter_map(|p| feed.previews.get(p).cloned())
                .collect()
        };
        let sets: Vec<(Vec<Arc<Preview>>, Mode)> = if spec.mapping.is_empty() {
            spec.groups
                .iter()
                .map(|(g, _)| (of(g), Mode::Native))
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
            linked: self.linked,
            mapping: spec.mapping.clone(),
        };
        if self.auto_for.as_ref() == Some(&key) {
            return;
        }
        self.auto_for = Some(key.clone());
        let linked = self.linked;
        self.auto = Task::spawn("mmm-plan-auto-stretch", ctx, move || {
            (key, view_auto_stretch(sets, linked))
        });
    }

    /// Send the view's previews without a texture (in this epoch) to the
    /// converter.
    fn start_conversions(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        spec: &ViewSpec,
    ) {
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
        for (g, layers) in &spec.groups {
            for path in group_panels(session, g) {
                let Some(preview) = feed.previews.get(&path) else {
                    continue;
                };
                for (slot, _) in layers {
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
            let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
            self.textures.insert(key, tex);
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
        for (g, layers) in &spec.groups {
            for (n, path) in group_panels(session, g).into_iter().enumerate() {
                let preview = feed.previews.get(&path);
                let rect = match (preview, self.layout.rects.get(&path)) {
                    (Some(p), _) => p.rect,
                    (None, Some(r)) => *r,
                    (None, None) => {
                        self.unplaced += 1;
                        continue;
                    }
                };
                let mut drawn = Vec::new();
                if preview.is_some() {
                    for (slot, tint) in layers {
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
                });
                items.push(PlacedItem {
                    id: id as u64,
                    rect,
                    layers: drawn,
                    outline,
                    label: format!("{}", n + 1),
                    marker: flag.then_some(palette::HA_500),
                    selected: self.selected.as_ref() == Some(&path),
                });
            }
        }
        self.last_items = snaps;
        items
    }

    /// The canvas extent: the shared frame (stable as previews arrive), or
    /// the union of the drawn rects before the frame is known.
    fn extent(&self, session: &OpenSession, items: &[PlacedItem]) -> Option<[f64; 4]> {
        if let Some((w, h)) = self.layout.canvas.or(session.canvas) {
            return Some([0.0, 0.0, w as f64, h as f64]);
        }
        items.iter().map(|it| it.rect).reduce(|a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        groups: &[String],
        compositions: &[Composition],
        grouping: bool,
        feed: &PlanFeed<'_>,
        out: &mut PlanOutput,
    ) {
        ui.horizontal(|ui| {
            if grouping {
                let shown = match &self.view {
                    Some(ViewKind::Group(g)) => g.clone(),
                    Some(ViewKind::Composition(i)) => compositions
                        .get(*i)
                        .map(|c| c.name.clone())
                        .unwrap_or_default(),
                    None => String::new(),
                };
                let mut chosen = self.view.clone();
                let mut add = false;
                egui::ComboBox::from_label("View")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for g in groups {
                            let v = Some(ViewKind::Group(g.clone()));
                            ui.selectable_value(&mut chosen, v, g);
                        }
                        if !compositions.is_empty() {
                            ui.separator();
                        }
                        for (i, c) in compositions.iter().enumerate() {
                            let v = Some(ViewKind::Composition(i));
                            ui.selectable_value(&mut chosen, v, &c.name);
                        }
                        ui.separator();
                        add = ui.selectable_label(false, "+ composition").clicked();
                    });
                if add {
                    let i = add_composition(session, groups, compositions);
                    self.save.touch();
                    chosen = Some(ViewKind::Composition(i));
                }
                if chosen != self.view {
                    self.view = chosen;
                }
                ui.separator();
            }
            ui.checkbox(&mut self.show_outlines, "Outlines")
                .on_hover_text("Panel outlines");
            ui.checkbox(&mut self.show_numbers, "Numbers")
                .on_hover_text("Panel numbers, in each group");
            ui.separator();
            let total = session.mosaic.config.panels.len();
            if total > 0 && !feed.complete {
                if feed.previews_busy {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Previews {} of {total}", feed.previews.len()))
                            .color(palette::TEXT_BODY),
                    );
                } else {
                    ui.label(RichText::new("Previews paused —").color(palette::TEXT_BODY));
                    if ui
                        .button("Resume")
                        .on_hover_text("Build the remaining panel previews")
                        .clicked()
                    {
                        out.resume_previews = true;
                    }
                }
            }
            if self.unplaced > 0 {
                ui.label(
                    RichText::new(format!("{} not placed yet", self.unplaced))
                        .color(palette::TEXT_MUTED),
                );
            }
        });
    }

    fn composition_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        groups: &[String],
        compositions: &[Composition],
        index: usize,
        c: &Composition,
    ) {
        ui.label(RichText::new("Composition").color(palette::TEXT_STRONG));
        let others: Vec<String> = compositions
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, o)| o.name.clone())
            .collect();
        let mut edited = c.clone();
        let ui_value = &mut session.mosaic.config.ui;
        let mut list = compositions.to_vec();
        if ui
            .push_id(("composition", index), |ui| {
                composition_editor(ui, &mut edited, groups, &others)
            })
            .inner
        {
            if edited.name != c.name {
                // The display stretch follows the composition's name.
                let old = composition_stretch_key(&c.name);
                let saved = saved_stretch_at(ui_value, PLAN_STRETCH_KEY, &old);
                save_stretch_at(ui_value, PLAN_STRETCH_KEY, &old, None);
                if let Some(s) = saved {
                    let new = composition_stretch_key(&edited.name);
                    save_stretch_at(ui_value, PLAN_STRETCH_KEY, &new, Some(&s));
                }
            }
            list[index] = edited;
            save_compositions(ui_value, &list);
            self.save.touch();
        }
        if ui.button("Delete composition").clicked() {
            save_stretch_at(
                ui_value,
                PLAN_STRETCH_KEY,
                &composition_stretch_key(&c.name),
                None,
            );
            list.remove(index);
            save_compositions(ui_value, &list);
            self.save.touch();
            self.view = groups.first().cloned().map(ViewKind::Group);
        }
    }

    fn stretch_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut OpenSession,
        spec: Option<&ViewSpec>,
    ) {
        let channels = spec.map_or(0, |s| s.channels);
        let default = StretchParams {
            black: 0.0,
            white: 1.0,
            midtones: 0.5,
        };
        let mut state = StretchState {
            stretch: self
                .stretch
                .clone()
                .unwrap_or_else(|| vec![default; channels]),
            channels,
            linked: self.linked,
            manual: self.manual,
            auto_busy: self.auto.is_some(),
        };
        if state.stretch.len() < channels {
            let last = state.stretch.last().copied().unwrap_or(default);
            state.stretch.resize(channels, last);
        }
        let change = stretch_panel(ui, &mut state);
        self.linked = state.linked;
        let Some(key) = spec.map(|s| s.key.clone()) else {
            return;
        };
        let ui_value = &mut session.mosaic.config.ui;
        match change {
            StretchChange::None => {}
            StretchChange::Auto => {
                self.manual = false;
                self.auto_for = None;
                self.auto = None;
                save_stretch_at(ui_value, PLAN_STRETCH_KEY, &key, None);
                self.save.touch();
            }
            StretchChange::Manual(s) => {
                self.manual = true;
                self.auto = None;
                save_stretch_at(ui_value, PLAN_STRETCH_KEY, &key, Some(&s));
                self.save.touch();
                self.stretch = Some(s);
                self.new_epoch();
            }
        }
    }

    fn selection_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Panel").color(palette::TEXT_STRONG));
        let Some(path) = self.selected.clone() else {
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
            .spacing([12.0, 4.0])
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
                ui.label(RichText::new("No other panel").color(palette::HA_500));
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

    fn checks_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        out: &mut PlanOutput,
    ) {
        let cfg = &session.mosaic.config;
        let filter = self.selected.clone().filter(|_| !self.show_all_checks);
        let mut items = Vec::new();
        let mut sources: Vec<Option<&Check>> = Vec::new();
        if let Some(report) = feed.checks {
            if filter.is_none() {
                for e in &report.errors {
                    items.push(CheckItem {
                        severity: UiSeverity::Problem,
                        title: e.clone(),
                        detail: String::new(),
                        accepted: false,
                        acceptable: false,
                    });
                    sources.push(None);
                }
            }
            for c in &report.checks {
                if filter.as_ref().is_some_and(|p| !c.panels.contains(p)) {
                    continue;
                }
                items.push(CheckItem {
                    severity: match c.severity {
                        Severity::Problem => UiSeverity::Problem,
                        Severity::Note => UiSeverity::Note,
                        Severity::Passed => UiSeverity::Passed,
                    },
                    title: c.message.clone(),
                    detail: c.remedy.clone().unwrap_or_default(),
                    accepted: is_accepted(cfg, c),
                    acceptable: c.severity != Severity::Passed,
                });
                sources.push(Some(c));
            }
        }
        let heading = if self.selected.is_some() && !self.show_all_checks {
            "Checks for this panel"
        } else {
            "Checks"
        };
        let show_all = self.selected.is_some().then_some(&mut self.show_all_checks);
        for action in checks_list(ui, heading, &items, show_all) {
            let CheckAction::Accept(i) = action;
            if let Some(Some(c)) = sources.get(i) {
                out.accept.push((*c).clone());
            }
        }
    }

    /// Save a pending change once the rate limit allows (documented
    /// exception: a small `mosaic.json` write on the UI thread).
    fn save_if_due(&mut self, ctx: &egui::Context, session: &OpenSession) {
        let now = Instant::now();
        if self.save.due(now) {
            self.save.saved(now);
            if let Err(e) = session.mosaic.save() {
                tracing::warn!("Plan settings not saved: {e}");
            }
        } else if let Some(wait) = self.save.wait(now) {
            ctx.request_repaint_after(wait);
        }
    }
}

/// The panels of `group` in configuration order (every panel when the
/// groups are unknown).
fn group_panels(session: &OpenSession, group: &str) -> Vec<PathBuf> {
    match &session.groups {
        Some(g) => g
            .processing_groups()
            .into_iter()
            .find(|(n, _)| n == group)
            .map(|(_, p)| p)
            .unwrap_or_default(),
        None => session
            .mosaic
            .config
            .panels
            .iter()
            .map(|p| p.path.clone())
            .collect(),
    }
}

/// Add a new composition (HOO-like defaults from the first groups) to the
/// session's `ui`; returns its index. Not saved (the caller rate-limits).
fn add_composition(
    session: &mut OpenSession,
    groups: &[String],
    compositions: &[Composition],
) -> usize {
    let taken: Vec<String> = compositions.iter().map(|c| c.name.clone()).collect();
    let mut c = Composition::new(unique_composition_name(&taken));
    if !groups.is_empty() {
        let pick = |i: usize| Some((groups[i.min(groups.len() - 1)].clone(), 1.0));
        c.channels = [pick(0), pick(1), pick(2)];
    }
    let mut list = compositions.to_vec();
    list.push(c);
    save_compositions(&mut session.mosaic.config.ui, &list);
    list.len() - 1
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
#[path = "plan_tab_tests.rs"]
mod tests;
