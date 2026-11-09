//! The plan canvas (spec §5.3): every panel in place in the shared frame,
//! drawn from its low-resolution preview as each one arrives, with outlines,
//! numbers and selection. Only the canvas: the window around it picks the
//! view ([`PlanView::select_view`]) and the look ([`CanvasLook`]), shows
//! the selected panel's details where it likes
//! ([`PlanView::selection_ui`]), and links it to the panel list: it selects
//! ([`PlanView::select`]) and highlights ([`PlanView::set_highlight`]) the
//! panel of a row, and reads the panel under the pointer
//! ([`PlanView::hovered`]) and a click on no panel
//! ([`PlanView::take_clicked_empty`]: the window's overlap or seam
//! selection goes too). The details' Overlaps rows are reported back: the
//! window selects the neighbour and frames the overlap.
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
//!   analysis placed it or, failing that, at its [`Outline`]: an earlier
//!   preview's rect (kept while an edit drops the previews and they are
//!   read again), or read from the file on a worker ([`find_outlines`]:
//!   the header-only WCS footprint in a solved frame, a strided scan of the
//!   covered area in an aligned one). Outlines are kept per frame, across
//!   edits: only a panel with no place yet is read, and a file changed on
//!   disk (checked once per previews generation, by its fingerprint) is
//!   read again; a new frame drops them all. The search passes over panels
//!   placed meanwhile and stops when the plan is dropped (the session
//!   left). The canvas extent is the shared frame's canvas, which does not
//!   move as previews arrive (so the user's zoom is kept).
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
//! - **Over the panels.** The window hands the plan the regions of interest
//!   as overlays (outlines and names, [`PlanView::set_overlays`]) and may arm
//!   its draw tool ([`PlanView::set_drawing`]): a rectangle drawn is taken
//!   once with [`PlanView::take_drawn`]. Both only while the plan draws in
//!   the analysed frame ([`PlanView::frame_matches`]), as are the seam
//!   lines it hands over ([`PlanView::set_lines`]).
//! - **Problems** ([`crate::marks`]): the window hands the plan the view's
//!   problems, one mark per place ([`PlanView::set_marks`]). A panel's badge
//!   sits at the top-right corner of its outline as drawn here, and the
//!   panel is outlined in the problem colour; an overlap's, a seam's or an
//!   area's sits where the analysis places it, only in the analysed frame.
//!   A badge clicked is taken with [`PlanView::take_badge_clicked`]. While
//!   the plan is not drawn, the window keeps its layout current
//!   ([`PlanView::follow_layout`]): the merged mosaic places its outlines
//!   and badges with it.
//! - **Compositions** add their channels' layers: each mapped group's
//!   previews are drawn as additive layers tinted red, green or blue (at
//!   full strength: weights are always 1, as the export writes them).
//!   Overlapping previews of one group add up too
//!   (brighter overlaps in a composition view).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, ColorImage, RichText};
use mega_ui::canvas_layers::{CanvasBadge, Overlay, Polyline, Tool};
use mega_ui::icons;
use mega_ui::plan::{Layer, PlacedItem, PlanCanvas};
use mega_ui::theme::palette;
use mega_ui::theme::text::{Type, text};
use mega_ui::theme::tokens::space;
use mega_ui::widgets::{
    ButtonKind, MegaButton, overline, property_grid, property_row, property_row_numeric,
};
use mmm_core::checks::SolutionKind;
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::{FRAME_FILE, Fingerprint, fingerprint};
use mmm_core::overlap::OverlapGraph;
use mmm_core::panel_header::{PanelHeader, read_panel_header};
use mmm_core::preview::{Preview, aligned_bbox, footprint, preview_frame_path};
use mmm_core::reference::ReferenceFrame;
use mmm_core::session::Session;
use mmm_core::stretch::{ChannelStats, StretchParams, apply, channel_stats};

use crate::camera::Camera;
use crate::compositions::{Composition, ViewKind, composition_stretch_key, load_compositions};
use crate::display_stretch::DisplayStretch;
use crate::marks::{Place, ProblemMark, badge_point, outline_color};
use crate::panel_edits::{group_colors, load_group_colors};
use crate::panel_list::{UNKNOWN, file_name, integration_text};
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

/// What the plan reads from the shell's pipeline each frame.
pub struct PlanFeed<'a> {
    /// The previews received for the current inputs, by panel path.
    pub previews: &'a BTreeMap<PathBuf, Arc<Preview>>,
    /// Changes when the previews are dropped (the inputs changed).
    pub generation: u64,
    /// Every preview of the current inputs has been run.
    pub complete: bool,
}

/// How the window around a canvas wants it drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasLook {
    /// Panel outlines, on the plan and on the merged mosaic alike.
    pub outlines: bool,
    /// Panel numbers.
    pub numbers: bool,
    /// Where the merge changes from one panel to the next (the seam
    /// lines), on the plan and on the merged mosaic alike.
    pub seams: bool,
    /// The regions of interest, outlined and named on the plan and on the
    /// merged mosaic alike.
    pub rois: bool,
    /// The automatic display stretch's toggles.
    pub stretch: DisplayStretch,
}

impl Default for CanvasLook {
    /// Outlines (with numbers) and ROIs on, seams off.
    fn default() -> Self {
        CanvasLook {
            outlines: true,
            numbers: true,
            seams: false,
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
    /// Each analysed group's panels by file, with their ids in its
    /// analysis (the ids its seam lines name).
    pub ids: BTreeMap<String, BTreeMap<PathBuf, usize>>,
}

/// Read the frame and the analyses of `groups` (each group's name and
/// session directory). The frame is the previews' (`frames.0`), else,
/// before any complete previews run, the analysed one (`frames.1`).
/// Blocking (worker thread); what cannot be read is left out and logged.
pub fn load_layout(frames: (&Path, &Path), groups: &[(String, PathBuf)]) -> Layout {
    let mut out = Layout {
        frame: ReferenceFrame::load(frames.0)
            .or_else(|_| ReferenceFrame::load(frames.1))
            .ok(),
        ..Layout::default()
    };
    for (name, dir) in groups {
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
        let ids = out.ids.entry(name.clone()).or_default();
        for p in &session.panels {
            let path = p.source.clone().unwrap_or_else(|| p.path.clone());
            let b = p.bbox;
            out.rects.insert(
                path.clone(),
                [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64],
            );
            ids.insert(path.clone(), p.id);
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

/// A panel's outline in the shared frame, and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Outline {
    /// Its preview's rect (the previews run checks the file itself).
    Preview([f64; 4]),
    /// Read from the file (see [`find_outlines`]).
    Read {
        /// The file's fingerprint when read (`None`: it could not be read).
        fingerprint: Option<Fingerprint>,
        /// The outline, if one was found.
        rect: Option<[f64; 4]>,
    },
}

impl Outline {
    /// Where the panel is, if known.
    pub fn rect(&self) -> Option<&[f64; 4]> {
        match self {
            Outline::Preview(r) => Some(r),
            Outline::Read { rect, .. } => rect.as_ref(),
        }
    }
}

/// Read the outline in `frame` of each of `panels` that is new or changed
/// on disk since its outline was read (each comes with the fingerprint it
/// was read at, if it was): the header-only footprint in a solved frame
/// (see [`footprint`]), the strided covered-area scan in an aligned one
/// (see [`aligned_bbox`]). A panel no longer in `wanted` when its turn
/// comes (its preview arrived meanwhile, or it left the inputs) is passed
/// over; one whose outline cannot be found is recorded without one, so it
/// is not read again until its file changes. Blocking (worker thread); a
/// cancelled `ctl` ends the search early with what has been found.
pub fn find_outlines(
    frame: &ReferenceFrame,
    panels: Vec<(PathBuf, Option<Option<Fingerprint>>)>,
    wanted: &Mutex<BTreeSet<PathBuf>>,
    ctl: &JobCtl,
) -> BTreeMap<PathBuf, Outline> {
    let mut out = BTreeMap::new();
    for (path, read_at) in panels {
        if ctl.check().is_err() {
            break;
        }
        let still_wanted = wanted.lock().map_or(true, |w| w.contains(&path));
        if !still_wanted {
            continue;
        }
        let fingerprint = fingerprint(&path).ok();
        if fingerprint.is_some() && read_at.as_ref() == Some(&fingerprint) {
            // Unchanged since read.
            continue;
        }
        #[cfg(test)]
        outline_hooks::scanning(&path, ctl);
        // A reader that panics costs that panel its outline, not the
        // search.
        let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match frame {
            ReferenceFrame::Aligned { .. } => aligned_bbox(&path, ctl),
            ReferenceFrame::Solved { .. } => footprint(&path, frame),
        }))
        .unwrap_or_else(|_| Err(mmm_core::error::Error::format(&path, "reading it panicked")));
        let rect = match found {
            Ok(r) => r,
            Err(mmm_core::error::Error::Cancelled) => break,
            Err(e) => {
                tracing::debug!("Plan: no outline for {}: {e}", path.display());
                None
            }
        };
        out.insert(path, Outline::Read { fingerprint, rect });
    }
    out
}

/// Read `path`'s header facts for its details: size, channels, filter,
/// sub-frames and solution, with one open (worker thread).
pub fn read_info(path: &Path) -> Result<PanelHeader, String> {
    read_panel_header(path).map_err(|e| e.to_string())
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
    /// Outline colour (the problem colour for a panel badged with a
    /// problem, [`outline_color`]).
    pub outline: Color32,
    /// The panel number shown.
    pub label: String,
    /// Highlighted ([`PlanView::set_highlight`]: its row is pointed at).
    pub highlighted: bool,
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
    analysed: Vec<(String, PathBuf)>,
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
    /// Texture identity and summary key (group name, or `comp:<id>`).
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
    /// The panel under the pointer in the last frame drawn.
    hovered: Option<PathBuf>,
    /// The panel to highlight (pointed at elsewhere).
    highlight: Option<PathBuf>,
    /// What the window has drawn over the plan (the ROIs).
    overlays: Vec<Overlay>,
    /// Image patches (ROI previews) drawn over the canvas.
    patches: Vec<mega_ui::canvas_layers::Patch>,
    /// The lines last handed to the canvas (tests; the canvas keeps them).
    #[cfg(test)]
    lines: Vec<Polyline>,
    /// The view's problems, one per place, to badge ([`PlanView::set_marks`]).
    marks: Vec<ProblemMark>,
    /// The badges the last frame drew (tests; the canvas keeps them).
    #[cfg(test)]
    badges: Vec<CanvasBadge>,
    /// A badge clicked, until taken ([`PlanView::take_badge_clicked`]).
    badge_clicked: Option<u64>,
    /// A click landed on no panel, until taken
    /// ([`PlanView::take_clicked_empty`]).
    clicked_empty: bool,
    /// A rectangle just drawn, with the canvas it is on: see
    /// [`PlanView::take_drawn`].
    drawn: Option<([f64; 4], (u64, u64))>,
    /// The press held on the canvas in the last frame drawn.
    hold: Option<mega_ui::canvas_layers::Hold>,
    /// A canvas rect where a click selects nothing ([`PlanView::set_quiet`]).
    quiet: Option<[u64; 4]>,
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
    /// The panels' outlines in `outlines_frame`, kept across previews
    /// generations (see the module docs).
    footprints: BTreeMap<PathBuf, Outline>,
    /// The frame the outlines are in.
    outlines_frame: Option<ReferenceFrame>,
    /// The outline search ([`find_outlines`]), while it runs.
    footprint_task: Option<Task<BTreeMap<PathBuf, Outline>>>,
    /// Cancels the running outline search (its frame went, or the plan).
    footprint_cancel: CancelToken,
    /// The panels the running search still needs, shared with it.
    outlines_wanted: Arc<Mutex<BTreeSet<PathBuf>>>,
    /// The previews generation the outlines read were last checked for
    /// (their files may have changed since).
    outlines_checked: Option<u64>,
    /// The previews generation the header facts belong to.
    info_generation: u64,
    /// When the view was last drawn (textures are dropped once hidden).
    last_shown: Option<Instant>,
    info: BTreeMap<PathBuf, Result<PanelHeader, String>>,
    info_task: Option<Task<(PathBuf, Result<PanelHeader, String>)>>,
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

impl Drop for PlanView {
    /// Stop the outline search: nothing reads its outlines once the plan
    /// is gone (the session was left, or another opened), and it must not
    /// keep reading panel files (or hold the decode lock) for the next
    /// session.
    fn drop(&mut self) {
        self.footprint_cancel.cancel();
    }
}

impl PlanView {
    /// A view showing the first group once drawn.
    pub fn new() -> Self {
        PlanView {
            view: None,
            canvas: PlanCanvas::new(),
            selected: None,
            hovered: None,
            highlight: None,
            overlays: Vec::new(),
            patches: Vec::new(),
            #[cfg(test)]
            lines: Vec::new(),
            marks: Vec::new(),
            #[cfg(test)]
            badges: Vec::new(),
            badge_clicked: None,
            clicked_empty: false,
            drawn: None,
            hold: None,
            quiet: None,
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
            outlines_frame: None,
            footprint_task: None,
            footprint_cancel: CancelToken::new(),
            outlines_wanted: Arc::default(),
            outlines_checked: None,
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

    /// Draw these over the panels (canvas coordinates) from the next frame
    /// on: the ROIs, when the look has them.
    pub fn set_overlays(&mut self, overlays: Vec<Overlay>) {
        self.overlays = overlays;
    }

    /// Draw these image patches (ROI previews) over the panels from the next
    /// frame on, under the overlays.
    pub fn set_patches(&mut self, patches: Vec<mega_ui::canvas_layers::Patch>) {
        self.patches = patches;
    }

    /// Draw these polylines (the seams, canvas coordinates) over the panels
    /// from the next frame on, with the overlays (handed straight to the
    /// canvas).
    pub fn set_lines(&mut self, lines: Vec<Polyline>) {
        #[cfg(test)]
        {
            self.lines = lines.clone();
        }
        self.canvas.set_lines(lines);
    }

    /// Badge these problems (the view's, one mark per place,
    /// [`crate::marks`]) from the next frame on: a panel's at the top-right
    /// corner of its outline as the plan draws it, its outline in the
    /// problem colour; any other where the analysis places it, and only
    /// while the plan draws in the analysed frame
    /// ([`PlanView::frame_matches`]). A click on one is taken with
    /// [`PlanView::take_badge_clicked`].
    pub fn set_marks(&mut self, marks: Vec<ProblemMark>) {
        self.marks = marks;
    }

    /// The badge clicked (or activated from the keyboard) since the last
    /// call, once: its mark's id ([`ProblemMark::id`]).
    pub fn take_badge_clicked(&mut self) -> Option<u64> {
        self.badge_clicked.take()
    }

    /// Whether a click landed on no panel since the last call (it cleared
    /// the plan's selection; the window clears its own too), once. Not a
    /// click in the quiet rect ([`PlanView::set_quiet`]), nor one with the
    /// draw tool armed.
    pub fn take_clicked_empty(&mut self) -> bool {
        std::mem::take(&mut self.clicked_empty)
    }

    /// Arm (or disarm) the draw tool: the next drag draws a rectangle
    /// ([`PlanView::take_drawn`]) instead of panning, and clicks select
    /// nothing.
    pub fn set_drawing(&mut self, on: bool) {
        self.canvas
            .set_tool(if on { Tool::DrawRect } else { Tool::Navigate });
    }

    /// Whether the draw tool is armed.
    pub fn drawing(&self) -> bool {
        self.canvas.tool() == Tool::DrawRect
    }

    /// The rectangle the user finished drawing since the last call, once,
    /// as `[x0, y0, x1, y1]` in canvas coordinates with the size of the
    /// canvas it was drawn on.
    pub fn take_drawn(&mut self) -> Option<([f64; 4], (u64, u64))> {
        self.drawn.take()
    }

    /// From the next frame drawn, a click inside canvas rect `rect` (if any)
    /// selects no panel and clears no selection: a press there is the
    /// window's (compare's peek on the ROI compared).
    pub fn set_quiet(&mut self, rect: Option<[u64; 4]>) {
        self.quiet = rect;
    }

    /// The screen rect the canvas's own buttons took in its last frame
    /// ([`PlanCanvas::controls_rect`]).
    pub fn controls_rect(&self) -> Option<egui::Rect> {
        self.canvas.controls_rect()
    }

    /// The primary button held on the canvas in the last frame drawn (the
    /// navigate tool), in canvas coordinates.
    pub fn hold(&self) -> Option<mega_ui::canvas_layers::Hold> {
        self.hold
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

    /// Select the panel `path` (or none), as a click on it does (e.g. its
    /// row was clicked in the panel list).
    pub fn select(&mut self, path: Option<PathBuf>) {
        self.selected = path;
    }

    /// The panel under the pointer in the last frame drawn (one of the
    /// view's), if any.
    pub fn hovered(&self) -> Option<&Path> {
        self.hovered.as_deref()
    }

    /// Highlight panel `path`'s outline (or none) from the next frame drawn,
    /// if the view shows it (e.g. its row is pointed at in the panel list).
    pub fn set_highlight(&mut self, path: Option<PathBuf>) {
        self.highlight = path;
    }

    /// The automatic display stretch in use, once known.
    pub fn stretch_for_view(&self) -> Option<&[StretchParams]> {
        self.stretch.as_deref()
    }

    /// What the last frame drew, item by item.
    pub fn items(&self) -> &[ItemSnapshot] {
        &self.last_items
    }

    /// The badges the last frame drew (canvas coordinates).
    #[cfg(test)]
    pub fn badges(&self) -> &[CanvasBadge] {
        &self.badges
    }

    /// The lines handed to the canvas (canvas coordinates).
    #[cfg(test)]
    pub fn lines(&self) -> &[Polyline] {
        &self.lines
    }

    /// The frame and what the last analyses say about the panels, as last
    /// read ([`load_layout`]).
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Keep the layout current whether or not the plan is drawn (the merged
    /// mosaic places its outlines and badges with it): take a finished
    /// read, and read again (on a worker) when the previews or analyses may
    /// have moved the panels, as drawing does. Meant to be called every
    /// frame, and cheap: it compares a small key (previews generation,
    /// analysed groups, provenance length) and does no I/O unless the key
    /// changed.
    pub fn follow_layout(
        &mut self,
        ctx: &egui::Context,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
    ) {
        self.poll_tasks();
        self.start_layout(ctx, session, feed);
    }

    /// Panels of the view with no place to draw them yet (no preview, no
    /// analysis and no outline), last frame.
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

    /// Whether the plan draws in the frame the analysis left in the session
    /// (the regions of interest and the patches are anchored in that one), or
    /// there is no analysed frame yet. A canvas-changing panel edit makes the
    /// previews' frame differ until the re-analysis ends; the window draws
    /// no ROIs on the plan meanwhile and takes no rectangle from it.
    pub fn frame_matches(&self, session: &OpenSession) -> bool {
        match (&session.frame, &self.layout.frame) {
            (None, _) => true,
            (Some(analysed), Some(drawn)) => analysed == drawn,
            (Some(_), None) => false,
        }
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
    /// a panel, a click on no panel clears the selection (Esc is the
    /// window's: [`PlanView::select`]), and the panel under the pointer is
    /// noted ([`PlanView::hovered`]).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        session: &OpenSession,
        feed: &PlanFeed<'_>,
        look: &CanvasLook,
    ) {
        let ctx = ui.ctx().clone();
        self.last_shown = Some(Instant::now());
        // Set again if the canvas is drawn and the pointer is on a panel.
        self.hovered = None;
        self.hold = None;
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
            Some(ViewKind::Composition(id)) => compositions.iter().any(|c| c.id == *id),
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
                #[cfg(test)]
                self.badges.clear();
                self.canvas.set_badges(Vec::new());
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
        self.canvas.set_overlays(self.overlays.clone());
        self.canvas.set_patches(self.patches.clone());
        let r = self
            .canvas
            .show(ui, extent, items, look.outlines, look.numbers);
        self.hold = r.held;
        if r.badge_clicked.is_some() {
            self.badge_clicked = r.badge_clicked;
        }
        if let Some(rect) = r.drawn_rect {
            let size = (
                extent[2].ceil().max(0.0) as u64,
                extent[3].ceil().max(0.0) as u64,
            );
            self.drawn = Some((rect, size));
        }
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
        // A click in the quiet rect selects nothing.
        let quiet = self.quiet.is_some_and(|q| {
            let at = ui.input(|i| i.pointer.interact_pos());
            let frame = self.canvas.last_frame();
            at.zip(frame).is_some_and(|(p, (rect, t))| {
                let (x, y) = self.canvas.view().screen_to_image(p, rect, t);
                x >= q[0] as f64 && x < q[2] as f64 && y >= q[1] as f64 && y < q[3] as f64
            })
        });
        match r.clicked {
            _ if quiet => {}
            Some(id) => self.selected = path_of(id),
            None if r.clicked_empty => {
                self.selected = None;
                self.clicked_empty = true;
            }
            None => {}
        }
        self.hovered = r.hovered.and_then(path_of);
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
            ViewKind::Composition(id) => {
                let c = compositions.iter().find(|c| c.id == *id)?;
                let mut drawn: Vec<DrawnGroup> = Vec::new();
                for (slot, ch) in c.channels.iter().enumerate() {
                    // The weight is not applied (always 1; the export
                    // ignores it).
                    let Some((g, _)) = ch else { continue };
                    let tint = CHANNEL_COLORS[slot];
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
                    key: composition_stretch_key(c.id),
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
        self.follow_outlines(ctx, session, feed);
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
        let analysed: Vec<(String, PathBuf)> = cfg
            .group_dirs
            .keys()
            .filter(|g| !cfg.needs_analyze.contains(*g))
            .map(|g| (g.clone(), session.mosaic.group_session_dir(g)))
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

    /// Keep the panels' outlines (see the module docs): take a finished
    /// search; on a new frame drop them all (and stop the search); keep
    /// each arrived preview's rect; and search (on a worker) for the
    /// panels with no place, no preview and no outline yet, and once per
    /// previews generation for those whose outline was read (only a file
    /// changed since is read again). The running search passes over panels
    /// placed meanwhile.
    fn follow_outlines(&mut self, ctx: &egui::Context, session: &OpenSession, feed: &PlanFeed<'_>) {
        if let Some(task) = &self.footprint_task {
            match task.poll() {
                Poll::Pending => {}
                Poll::Done(found) => {
                    self.footprint_task = None;
                    self.footprints.extend(found);
                }
                Poll::Lost => {
                    self.footprint_task = None;
                    tracing::warn!("Plan: the outline search stopped unexpectedly");
                    // Not tried again before the next generation: no
                    // outline for those still without one.
                    for p in self.wanted_outlines(session) {
                        let none = Outline::Read {
                            fingerprint: None,
                            rect: None,
                        };
                        self.footprints.entry(p).or_insert(none);
                    }
                }
            }
        }
        let Some(frame) = self.layout.frame.as_ref() else {
            return;
        };
        if self.outlines_frame.as_ref() != Some(frame) {
            // Outlines belong to one frame.
            self.footprint_cancel.cancel();
            self.footprint_task = None;
            self.footprints.clear();
            self.outlines_frame = Some(frame.clone());
            self.outlines_checked = None;
        }
        // A preview's rect outlines its panel while the previews are
        // dropped and read again (after an edit).
        for (path, p) in feed.previews {
            let outline = Outline::Preview(p.rect);
            if self.footprints.get(path) != Some(&outline) {
                self.footprints.insert(path.clone(), outline);
            }
        }
        if self.footprint_task.is_some() {
            let wanted = self.wanted_outlines(session);
            if let Ok(mut shared) = self.outlines_wanted.lock()
                && *shared != wanted
            {
                *shared = wanted;
            }
            return;
        }
        // A panel with no place at all yet (new, or the analysis that
        // placed it is out of date).
        let fresh = session.mosaic.config.panels.iter().any(|p| {
            !self.layout.rects.contains_key(&p.path) && !self.footprints.contains_key(&p.path)
        });
        if !fresh && self.outlines_checked == Some(feed.generation) {
            return;
        }
        self.outlines_checked = Some(feed.generation);
        let wanted = self.wanted_outlines(session);
        if wanted.is_empty() {
            return;
        }
        let panels: Vec<(PathBuf, Option<Option<Fingerprint>>)> = wanted
            .iter()
            .map(|p| {
                let read_at = match self.footprints.get(p) {
                    Some(Outline::Read { fingerprint, .. }) => Some(fingerprint.clone()),
                    _ => None,
                };
                (p.clone(), read_at)
            })
            .collect();
        self.footprint_cancel = CancelToken::new();
        self.outlines_wanted = Arc::new(Mutex::new(wanted));
        let (frame, cancel) = (frame.clone(), self.footprint_cancel.clone());
        let shared = Arc::clone(&self.outlines_wanted);
        self.footprint_task = Task::spawn("mmm-plan-outlines", ctx, move || {
            find_outlines(&frame, panels, &shared, &JobCtl::new(cancel, None))
        });
    }

    /// The panels whose outline must be read from their file: those the
    /// last analysis did not place, with no preview's outline.
    fn wanted_outlines(&self, session: &OpenSession) -> BTreeSet<PathBuf> {
        session
            .mosaic
            .config
            .panels
            .iter()
            .map(|p| &p.path)
            .filter(|p| {
                !self.layout.rects.contains_key(*p)
                    && !matches!(self.footprints.get(*p), Some(Outline::Preview(_)))
            })
            .cloned()
            .collect()
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

    /// The items of the view, the snapshot of what they show, and the
    /// badges of the view's problems ([`PlanView::set_marks`]).
    fn build_items(
        &mut self,
        session: &OpenSession,
        spec: &ViewSpec,
        feed: &PlanFeed<'_>,
    ) -> Vec<PlacedItem> {
        let cfg = &session.mosaic.config;
        let badged: BTreeSet<PathBuf> = self
            .marks
            .iter()
            .flat_map(ProblemMark::panels)
            .map(Path::to_path_buf)
            .collect();
        // Where each panel is drawn, for its badge.
        let mut placed: BTreeMap<PathBuf, [f64; 4]> = BTreeMap::new();
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
                // Preview, else where analysis put it, else its outline
                // (an earlier preview's, or read from the file); dashed
                // until the preview.
                let known = self
                    .layout
                    .rects
                    .get(&path)
                    .or_else(|| self.footprints.get(&path).and_then(Outline::rect));
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
                let outline = outline_color(badged.contains(&path));
                let highlighted = self.highlight.as_ref() == Some(&path);
                snaps.push(ItemSnapshot {
                    path: path.clone(),
                    rect,
                    layers: drawn.iter().map(|l| (l.tint, l.additive)).collect(),
                    outline,
                    label: number.to_string(),
                    highlighted,
                });
                placed.insert(path.clone(), rect);
                items.push(PlacedItem {
                    id: id as u64,
                    rect,
                    layers: drawn,
                    outline,
                    label: number.to_string(),
                    selected: self.selected.as_ref() == Some(&path),
                    highlighted,
                });
            }
        }
        self.last_items = snaps;
        // A panel's badge at its outline as drawn here (its first panel's
        // drawn); the others where the analysis places them, only in the
        // analysed frame.
        let analysed = self.frame_matches(session);
        let badges: Vec<CanvasBadge> = self
            .marks
            .iter()
            .filter_map(|m| {
                let at = match &m.place {
                    Place::Panel(_) => {
                        let drawn = m.panels().find_map(|p| placed.get(p))?;
                        badge_point(&m.place, *drawn)
                    }
                    _ if analysed => badge_point(&m.place, m.rect?),
                    _ => return None,
                };
                Some(m.badge(at))
            })
            .collect();
        #[cfg(test)]
        {
            self.badges = badges.clone();
        }
        self.canvas.set_badges(badges);
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

    /// The selected panel's details: its name (a heading) and path,
    /// **Show in folder** and **Copy path**, its header facts (read on a
    /// worker: size, channels, filter, exposure, frames, integration,
    /// solution and scale; "—" for what the header does not say) and the
    /// panels it overlaps in its analysis, each a row; a hint when none is
    /// selected. Returns the panel whose Overlaps row was clicked: the
    /// window selects it and frames the overlap.
    pub fn selection_ui(&mut self, ui: &mut egui::Ui, session: &OpenSession) -> Option<PathBuf> {
        let Some(path) = self.selected.clone().filter(|p| is_panel(session, p)) else {
            ui.label(text(Type::Small, "Click a panel to select it."));
            return None;
        };
        if !self.info.contains_key(&path) && self.info_task.is_none() {
            let p = path.clone();
            self.info_task = Task::spawn("mmm-plan-panel-info", ui.ctx(), move || {
                let info = read_info(&p);
                (p, info)
            });
        }
        overline(ui, "Panel");
        details_heading(ui, &file_name(&path));
        ui.add(egui::Label::new(text(Type::Small, path.display().to_string())).wrap());
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            if ui
                .add(MegaButton::new(ButtonKind::Secondary, "Show in folder"))
                .on_hover_text("Open the folder holding this file")
                .clicked()
            {
                show_in_folder(&path);
            }
            if ui
                .add(MegaButton::new(ButtonKind::Secondary, "Copy path"))
                .on_hover_text("Copy the full path to the clipboard")
                .clicked()
            {
                ui.ctx().copy_text(path.display().to_string());
            }
        });
        ui.add_space(space::S);
        property_grid(ui, "plan-panel-props", |ui| match self.info.get(&path) {
            None => property_row(ui, "Header", "Reading…"),
            Some(Err(e)) => property_row(ui, "Header", &format!("Unreadable: {e}")),
            Some(Ok(h)) => header_rows(ui, h),
        });
        ui.add_space(space::S);
        overline(ui, "Overlaps");
        let mut clicked = None;
        match self.layout.overlaps.get(&path) {
            None => {
                ui.label(text(Type::Small, "Shown once analysed."));
            }
            Some(o) if o.is_empty() => {
                // Muted: its problem (if a check flags it) says so, with
                // its icon, in the panel's checks.
                ui.label(text(Type::Small, "No other panel"));
            }
            Some(o) => {
                for p in o {
                    let name = file_name(p);
                    let does = format!("Select {name} and go to the overlap");
                    let row = MegaButton::new(ButtonKind::Quiet, &name)
                        .trailing_icon(icons::CHEVRON_RIGHT);
                    let r = ui.add(row);
                    // Said to assistive technology too, as a checks row's
                    // detail is.
                    let said = does.clone();
                    ui.ctx()
                        .accesskit_node_builder(r.id, |node| node.set_description(said));
                    if r.on_hover_text(does).clicked() {
                        clicked = Some(p.clone());
                    }
                }
            }
        }
        clicked
    }
}

/// An exposure, rounded as the integration is: "300 s"; below a second, to
/// a hundredth ("0.25 s").
fn exposure_text(seconds: f64) -> String {
    if seconds >= 1.0 {
        format!("{} s", seconds.round() as u64)
    } else {
        format!("{seconds:.2} s")
    }
}

/// The header facts of a panel's details, as property rows.
fn header_rows(ui: &mut egui::Ui, h: &PanelHeader) {
    let or_unknown = |v: Option<String>| v.unwrap_or_else(|| UNKNOWN.into());
    property_row_numeric(ui, "Size", &format_canvas(h.width, h.height));
    property_row_numeric(ui, "Channels", &h.channels.to_string());
    property_row(ui, "Filter", &or_unknown(h.filter.clone()));
    property_row_numeric(ui, "Exposure", &or_unknown(h.exposure_s.map(exposure_text)));
    property_row_numeric(ui, "Frames", &or_unknown(h.frames.map(|n| n.to_string())));
    property_row_numeric(
        ui,
        "Integration",
        &or_unknown(h.integration_s().map(integration_text)),
    );
    property_row(ui, "Solution", solution_text(h.solution));
    if let Some(s) = h.scale_arcsec {
        property_row_numeric(ui, "Scale", &format!("{s:.3}″/px"));
    }
}

/// The heading of what the inspector details (a panel, an overlap, a
/// seam), wrapped, and a heading to assistive technology.
pub(crate) fn details_heading(ui: &mut egui::Ui, s: &str) {
    let r = ui.add(egui::Label::new(text(Type::Heading, s)).wrap());
    ui.ctx().accesskit_node_builder(r.id, |node| {
        node.set_role(egui::accesskit::Role::Heading);
        node.set_label(s);
    });
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

/// Plain text of a solution kind.
fn solution_text(k: SolutionKind) -> &'static str {
    match k {
        SolutionKind::None => "None (registered)",
        SolutionKind::Linear => "Linear (TAN)",
        SolutionKind::Distorted => "With distortion",
    }
}

/// Test-only record of the outline searches: how many times each panel's
/// outline was read, and panels whose read waits for a cancel ([`hold`]).
#[cfg(test)]
pub(crate) mod outline_hooks {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use mmm_core::job::JobCtl;

    static SCANS: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());
    static HELD: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
    static CANCELLED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

    /// The outline of `path` is about to be read under `ctl`: counted, and
    /// for a held panel, wait (at most [`PATIENCE`](crate::testutil::PATIENCE))
    /// until `ctl` is cancelled, noting that it was.
    pub(crate) fn scanning(path: &Path, ctl: &JobCtl) {
        *SCANS.lock().unwrap().entry(path.to_path_buf()).or_default() += 1;
        if !HELD.lock().unwrap().contains(path) {
            return;
        }
        let deadline = Instant::now() + crate::testutil::PATIENCE;
        while Instant::now() < deadline {
            if ctl.check().is_err() {
                CANCELLED.lock().unwrap().insert(path.to_path_buf());
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// How many times `path`'s outline has been read.
    pub(crate) fn scans(path: &Path) -> usize {
        SCANS.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    /// Make `path`'s outline reads wait for their search to be cancelled.
    pub(crate) fn hold(path: &Path) {
        HELD.lock().unwrap().insert(path.to_path_buf());
    }

    /// Whether a held read of `path` saw its search cancelled.
    pub(crate) fn cancelled(path: &Path) -> bool {
        CANCELLED.lock().unwrap().contains(path)
    }
}

#[cfg(test)]
#[path = "plan_view_tests.rs"]
pub(crate) mod tests;
