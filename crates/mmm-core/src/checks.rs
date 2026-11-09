//! Structured pre-flight, staleness and analysis checks. Each check has a
//! stable [`CheckCode`], a [`Severity`], plain-English text and, where there
//! is one, a remedy. Results the user has accepted are remembered by
//! [`CheckKey`] in the mosaic configuration.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::mosaic::grouping::Groups;
use crate::mosaic::{MosaicConfig, fingerprint};
use crate::overlap::OverlapGraph;
use crate::session::Session;

/// How much a check result matters.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Likely to harm the result; needs attention or acceptance.
    Problem,
    /// Worth knowing, not necessarily wrong.
    Note,
    /// The check ran and found nothing wrong.
    Passed,
}

/// Identifies a kind of check. Persisted in `mosaic.json`: the serialised
/// names are stable and must never be renamed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CheckCode {
    /// A panel has no astrometric solution.
    SolutionPresent,
    /// A panel's distortion model is unsupported or inconsistent.
    DistortionModel,
    /// Panel pixel scales differ.
    ScaleMismatch,
    /// Panels mix colour and mono channels.
    MixedChannels,
    /// Groups hold different panel counts.
    GroupCountMismatch,
    /// Filters differ but grouping is off.
    FiltersDifferUngrouped,
    /// Some panels could not be placed in a group.
    Ungrouped,
    /// A panel file changed since it was added.
    FileChanged,
    /// A panel file is missing.
    FileMissing,
    /// A panel overlaps no other panel.
    NoOverlap,
    /// Panel sky levels differ strongly.
    SkyLevelMismatch,
    /// Two panels are the same file or cover nearly the same sky.
    DuplicatePanels,
    /// A gap in the coverage, enclosed by panels.
    CoverageHole,
    /// Two panels overlap by too little to be matched reliably.
    ThinOverlap,
    /// A panel's footprint is largely empty.
    EmptyArea,
}

/// The overlap's short side, in canvas pixels, below which two panels are
/// too thinly joined to match reliably ([`CheckCode::ThinOverlap`]).
pub const THIN_OVERLAP_PX: u64 = 128;

/// The intersection-over-union of two panels' footprints at or above which
/// they are taken to cover the same sky ([`CheckCode::DuplicatePanels`]).
pub const DUPLICATE_IOU: f64 = 0.9;

/// The share of a panel's bounding box that may be empty before
/// [`CheckCode::EmptyArea`] notes it (uncovered share, 0 to 1).
pub const EMPTY_AREA_FRACTION: f64 = 0.25;

/// The smallest enclosed gap that counts as a hole in the coverage
/// ([`CheckCode::CoverageHole`]), as a fraction of the smallest panel's
/// footprint area. Below 1 because a missing panel leaves a gap smaller than
/// itself: its neighbours overlap into it.
pub const HOLE_MIN_PANEL_FRACTION: f64 = 0.25;

/// Where in the mosaic a check applies. Lets the interface put a badge in
/// the right place and zoom to it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum CheckTarget {
    /// The session as a whole (or several groups).
    #[default]
    Session,
    /// One group.
    Group(String),
    /// One panel (user-facing file).
    Panel(PathBuf),
    /// The overlap of two panels.
    Overlap(PathBuf, PathBuf),
    /// The seam between two panels in a group's merge.
    Seam {
        /// The group the seam belongs to.
        group: String,
        /// One panel of the pair.
        a: PathBuf,
        /// The other panel of the pair.
        b: PathBuf,
    },
}

/// A check result's identity: the check and, when panel-specific, the panel
/// (and, for a pair such as an overlap, the other panel). Used to remember
/// which warnings the user has accepted.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckKey {
    /// Which check.
    pub code: CheckCode,
    /// The panel concerned, if any.
    pub panel: Option<PathBuf>,
    /// The second panel of a pair target. Absent from keys saved before
    /// pairs existed, which serialise exactly as they did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<PathBuf>,
}

/// One check result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Check {
    /// Where it applies.
    #[serde(default)]
    pub target: CheckTarget,
    /// For an area target (a coverage hole), the canvas rectangle
    /// `[x0, y0, x1, y1]` concerned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[u64; 4]>,
    /// Which check produced this result.
    pub code: CheckCode,
    /// How much it matters.
    pub severity: Severity,
    /// The panels concerned (user-facing files).
    pub panels: Vec<PathBuf>,
    /// The groups concerned.
    pub groups: Vec<String>,
    /// Plain-English description, leading with the panel's file name.
    pub message: String,
    /// What to do about it, if anything.
    pub remedy: Option<String>,
}

impl Check {
    /// The identities of this result: one key for a pair target (an overlap
    /// or a seam), else one key per panel, or one panel-less key when it
    /// names no panels.
    pub fn keys(&self) -> Vec<CheckKey> {
        if let CheckTarget::Overlap(a, b) | CheckTarget::Seam { a, b, .. } = &self.target {
            return vec![CheckKey {
                code: self.code,
                panel: Some(a.clone()),
                other: Some(b.clone()),
            }];
        }
        if self.panels.is_empty() {
            return vec![CheckKey {
                code: self.code,
                panel: None,
                other: None,
            }];
        }
        self.panels
            .iter()
            .map(|p| CheckKey {
                code: self.code,
                panel: Some(p.clone()),
                other: None,
            })
            .collect()
    }
}

/// True when every key of `check` is in the configuration's accepted list.
pub fn is_accepted(cfg: &MosaicConfig, check: &Check) -> bool {
    check.keys().iter().all(|k| cfg.accepted_checks.contains(k))
}

/// How a panel is plate-solved.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolutionKind {
    /// No usable astrometric solution.
    None,
    /// A linear (TAN) solution only.
    Linear,
    /// A solution with a distortion model.
    Distorted,
}

/// Header-level facts about one panel, enough for the input checks.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelFacts {
    /// The panel file.
    pub path: PathBuf,
    /// Channel count.
    pub channels: u64,
    /// Filter name from the header, if any.
    pub filter: Option<String>,
    /// What kind of solution the panel carries.
    pub solution: SolutionKind,
    /// Pixel scale in arcseconds per pixel, when solved.
    pub scale_arcsec: Option<f64>,
}

/// Read a panel's facts from its header only (no pixel data).
pub fn panel_facts(path: &Path) -> Result<PanelFacts> {
    let h = crate::panel_header::read_panel_header(path)?;
    Ok(PanelFacts {
        path: path.to_path_buf(),
        channels: h.channels,
        filter: h.filter,
        solution: h.solution,
        scale_arcsec: h.scale_arcsec,
    })
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |f| f.to_string_lossy().into_owned(),
    )
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Checks over the panels' headers and their grouping. `aligned`: the
/// panels are pre-aligned (registered) input, which carry no distortion
/// model by design, so a linear solution passes the distortion check.
pub fn input_checks(
    cfg: &MosaicConfig,
    facts: &[PanelFacts],
    groups: &Groups,
    aligned: bool,
) -> Vec<Check> {
    let mut out = Vec::new();
    let find = |p: &Path| facts.iter().find(|f| f.path == p);

    for f in facts {
        let name = file_name(&f.path);
        if f.solution == SolutionKind::None {
            out.push(Check {
                code: CheckCode::SolutionPresent,
                target: CheckTarget::Panel(f.path.clone()),
                rect: None,
                severity: Severity::Note,
                panels: vec![f.path.clone()],
                groups: vec![],
                message: format!("{name} has no astrometric solution."),
                remedy: Some(
                    "Fine for registered panels; raw panels need a plate solution.".into(),
                ),
            });
            continue;
        }
        out.push(Check {
            code: CheckCode::SolutionPresent,
            target: CheckTarget::Panel(f.path.clone()),
            rect: None,
            severity: Severity::Passed,
            panels: vec![f.path.clone()],
            groups: vec![],
            message: format!("{name} has an astrometric solution."),
            remedy: None,
        });
        if f.solution == SolutionKind::Linear && aligned {
            // Registered panels carry a plain linear solution by design.
            out.push(Check {
                code: CheckCode::DistortionModel,
                target: CheckTarget::Panel(f.path.clone()),
                rect: None,
                severity: Severity::Passed,
                panels: vec![f.path.clone()],
                groups: vec![],
                message: "No distortion model: accepted for pre-aligned panels".into(),
                remedy: None,
            });
        } else if f.solution == SolutionKind::Linear {
            out.push(Check {
                code: CheckCode::DistortionModel,
                target: CheckTarget::Panel(f.path.clone()),
                rect: None,
                severity: Severity::Problem,
                panels: vec![f.path.clone()],
                groups: vec![],
                message: format!("{name}: missing distortion model."),
                remedy: Some("Re-solve the panel with distortion correction enabled.".into()),
            });
        } else {
            out.push(Check {
                code: CheckCode::DistortionModel,
                target: CheckTarget::Panel(f.path.clone()),
                rect: None,
                severity: Severity::Passed,
                panels: vec![f.path.clone()],
                groups: vec![],
                message: format!("{name} has a distortion model."),
                remedy: None,
            });
        }
    }

    // Per-group checks; ungrouped panels are processed together too.
    let mut sets: Vec<(String, &Vec<PathBuf>)> =
        groups.groups.iter().map(|(n, v)| (n.clone(), v)).collect();
    if !groups.ungrouped.is_empty() {
        sets.push((crate::mosaic::UNGROUPED.to_string(), &groups.ungrouped));
    }
    for (gname, paths) in &sets {
        let members: Vec<&PanelFacts> = paths.iter().filter_map(|p| find(p)).collect();

        let scales: Vec<(&PanelFacts, f64)> = members
            .iter()
            .filter_map(|f| f.scale_arcsec.map(|s| (*f, s)))
            .collect();
        if scales.len() > 1 {
            let med = median(&mut scales.iter().map(|(_, s)| *s).collect::<Vec<_>>());
            for (f, s) in &scales {
                let name = file_name(&f.path);
                let off = (s - med).abs() / med > 0.01;
                out.push(Check {
                    code: CheckCode::ScaleMismatch,
target: CheckTarget::Panel(f.path.clone()),                    rect: None,
                    severity: if off { Severity::Problem } else { Severity::Passed },
                    panels: vec![f.path.clone()],
                    groups: vec![gname.clone()],
                    message: if off {
                        format!(
                            "{name} has a pixel scale of {s:.3} arcsec/px, which differs by more than 1% from the group median of {med:.3}."
                        )
                    } else {
                        format!("{name} matches the group's pixel scale ({s:.3} arcsec/px).")
                    },
                    remedy: off.then(|| {
                        "Check that every panel in the group came from the same optical setup, or resample it first.".into()
                    }),
                });
            }
        }

        let chans: BTreeSet<u64> = members.iter().map(|f| f.channels).collect();
        if chans.len() > 1 {
            out.push(Check {
                code: CheckCode::MixedChannels,
                target: CheckTarget::Group(gname.clone()),
                rect: None,
                severity: Severity::Problem,
                panels: members.iter().map(|f| f.path.clone()).collect(),
                groups: vec![gname.clone()],
                message: format!(
                    "{}: panels have different channel counts ({}).",
                    gname,
                    chans
                        .iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                remedy: Some(
                    "Put colour and mono panels in separate groups, or remove the odd ones out."
                        .into(),
                ),
            });
        }
    }

    if cfg.grouping.enabled {
        let counts: BTreeSet<usize> = groups.groups.values().map(Vec::len).collect();
        if counts.len() > 1 {
            let list = groups
                .groups
                .iter()
                .map(|(n, v)| format!("{n} ({})", v.len()))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(Check {
                code: CheckCode::GroupCountMismatch,
                target: CheckTarget::Session,
                rect: None,
                severity: Severity::Note,
                panels: vec![],
                groups: groups.groups.keys().cloned().collect(),
                message: format!("Groups hold different numbers of panels: {list}."),
                remedy: Some("Check that no panel is missing from a group.".into()),
            });
        }
        if !groups.ungrouped.is_empty() {
            out.push(Check {
                code: CheckCode::Ungrouped,
                target: CheckTarget::Session,
                rect: None,
                severity: Severity::Note,
                panels: groups.ungrouped.clone(),
                groups: vec![],
                message: format!(
                    "{} panel(s) could not be placed in a group, starting with {}.",
                    groups.ungrouped.len(),
                    file_name(&groups.ungrouped[0])
                ),
                remedy: Some("Assign them to a group by hand, or change the grouping rule.".into()),
            });
        }
    } else {
        let filters: BTreeSet<&str> = facts.iter().filter_map(|f| f.filter.as_deref()).collect();
        if filters.len() > 1 {
            out.push(Check {
                code: CheckCode::FiltersDifferUngrouped,
                target: CheckTarget::Session,
                rect: None,
                severity: Severity::Note,
                panels: vec![],
                groups: vec![],
                message: format!(
                    "Panels use different filters ({}) but grouping is off.",
                    filters.into_iter().collect::<Vec<_>>().join(", ")
                ),
                remedy: Some("Group by FILTER header to merge each filter separately.".into()),
            });
        }
    }
    out
}

/// Compare each panel's stored fingerprint with the file on disk.
pub fn file_checks(cfg: &MosaicConfig) -> Vec<Check> {
    let mut out = Vec::new();
    for e in &cfg.panels {
        let name = file_name(&e.path);
        if !e.path.exists() {
            out.push(Check {
                code: CheckCode::FileMissing,
                target: CheckTarget::Panel(e.path.clone()),
                rect: None,
                severity: Severity::Problem,
                panels: vec![e.path.clone()],
                groups: vec![],
                message: format!("{name} is missing from {}.", e.path.display()),
                remedy: Some("Restore the file, or remove the panel from the mosaic.".into()),
            });
            continue;
        }
        if let (Some(old), Ok(now)) = (&e.fingerprint, fingerprint(&e.path))
            && *old != now
        {
            out.push(Check {
                code: CheckCode::FileChanged,
                target: CheckTarget::Panel(e.path.clone()),
                rect: None,
                severity: Severity::Note,
                panels: vec![e.path.clone()],
                groups: vec![],
                message: format!(
                    "{name} has changed since it was analysed; it will be reanalysed."
                ),
                remedy: None,
            });
        }
    }
    out
}

/// Checks over one analysed group's overlap structure and sky levels.
/// Panels are reported by their user-facing file (`source` when present,
/// since `path` may be a reprojection cache).
pub fn analysis_checks(group: &str, session: &Session, graph: &OverlapGraph) -> Vec<Check> {
    let mut out = Vec::new();
    let user_file =
        |m: &crate::session::PanelMeta| m.source.clone().unwrap_or_else(|| m.path.clone());
    let neighbours = |id: usize| -> Vec<usize> {
        graph
            .edges
            .iter()
            .filter_map(|e| {
                if e.a == id {
                    Some(e.b)
                } else if e.b == id {
                    Some(e.a)
                } else {
                    None
                }
            })
            .collect()
    };
    let mean_of = |id: usize| -> Option<f64> {
        let m = session.panels.iter().find(|p| p.id == id)?;
        (!m.ch_mean.is_empty()).then(|| m.ch_mean.iter().sum::<f64>() / m.ch_mean.len() as f64)
    };

    for m in &session.panels {
        let name = file_name(&user_file(m));
        let nb = neighbours(m.id);
        if session.panels.len() > 1 && nb.is_empty() {
            out.push(Check {
                code: CheckCode::NoOverlap,
                target: CheckTarget::Panel(user_file(m)),
                rect: None,
                severity: Severity::Problem,
                panels: vec![user_file(m)],
                groups: vec![group.to_string()],
                message: format!("{name} does not overlap any other panel in {group}."),
                remedy: Some(
                    "Check the panel's placement or group; it cannot be matched to its neighbours."
                        .into(),
                ),
            });
            continue;
        }
        let mut nm: Vec<f64> = nb.iter().filter_map(|&i| mean_of(i)).collect();
        if let (Some(mine), false) = (mean_of(m.id), nm.is_empty()) {
            let med = median(&mut nm);
            if med > 0.0 && (mine > 2.0 * med || mine < 0.5 * med) {
                out.push(Check {
                    code: CheckCode::SkyLevelMismatch,
target: CheckTarget::Panel(user_file(m)),                    rect: None,
                    severity: Severity::Note,
                    panels: vec![user_file(m)],
                    groups: vec![group.to_string()],
                    message: format!(
                        "{name} has a mean level of {mine:.4}, against {med:.4} for its overlapping neighbours."
                    ),
                    remedy: Some(
                        "Check for a gradient or a different exposure; the merge will match levels, but large differences can leave residuals.".into(),
                    ),
                });
            }
        }
    }
    out
}

/// Area of a `[x0, y0, x1, y1]` rectangle (0 when empty).
fn rect_area(r: &[u64; 4]) -> u64 {
    r[2].saturating_sub(r[0]) * r[3].saturating_sub(r[1])
}

/// Area of the intersection of two rectangles.
fn intersection_area(a: &[u64; 4], b: &[u64; 4]) -> u64 {
    let r = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    rect_area(&r)
}

/// Uncovered regions fully enclosed by `rects`, as `(bounding rect, area)`.
///
/// The rectangles' edges cut the plane into a small grid (one cell per
/// pair of neighbouring edge coordinates), so this is exact and costs
/// nothing in canvas pixels. Cells outside every rectangle that connect to
/// the grid's border are open sky; the rest, grouped by 4-connectivity,
/// are enclosed.
fn enclosed_gaps(rects: &[[u64; 4]]) -> Vec<([u64; 4], u64)> {
    let rects: Vec<&[u64; 4]> = rects.iter().filter(|r| rect_area(r) > 0).collect();
    let mut xs: Vec<u64> = rects.iter().flat_map(|r| [r[0], r[2]]).collect();
    let mut ys: Vec<u64> = rects.iter().flat_map(|r| [r[1], r[3]]).collect();
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();
    if xs.len() < 2 || ys.len() < 2 {
        return Vec::new();
    }
    let (nx, ny) = (xs.len() - 1, ys.len() - 1);
    let at = |i: usize, j: usize| j * nx + i;
    let mut covered = vec![false; nx * ny];
    for r in &rects {
        let i0 = xs.partition_point(|&x| x < r[0]);
        let i1 = xs.partition_point(|&x| x < r[2]);
        let j0 = ys.partition_point(|&y| y < r[1]);
        let j1 = ys.partition_point(|&y| y < r[3]);
        for j in j0..j1 {
            for i in i0..i1 {
                covered[at(i, j)] = true;
            }
        }
    }
    // Flood the open sky in from the border, then collect what is left.
    let mut seen = covered.clone();
    let neighbours = |i: usize, j: usize| {
        let mut n = Vec::with_capacity(4);
        if i > 0 {
            n.push((i - 1, j));
        }
        if j > 0 {
            n.push((i, j - 1));
        }
        if i + 1 < nx {
            n.push((i + 1, j));
        }
        if j + 1 < ny {
            n.push((i, j + 1));
        }
        n
    };
    let flood = |seed: Vec<(usize, usize)>, seen: &mut [bool]| {
        let mut stack = seed;
        let (mut lo, mut hi) = ([u64::MAX; 2], [0u64; 2]);
        let mut area = 0u64;
        while let Some((i, j)) = stack.pop() {
            if seen[at(i, j)] {
                continue;
            }
            seen[at(i, j)] = true;
            let (w, h) = (xs[i + 1] - xs[i], ys[j + 1] - ys[j]);
            area += w * h;
            lo = [lo[0].min(xs[i]), lo[1].min(ys[j])];
            hi = [hi[0].max(xs[i + 1]), hi[1].max(ys[j + 1])];
            stack.extend(neighbours(i, j));
        }
        ([lo[0], lo[1], hi[0], hi[1]], area)
    };
    let border: Vec<(usize, usize)> = (0..nx)
        .flat_map(|i| [(i, 0), (i, ny - 1)])
        .chain((0..ny).flat_map(|j| [(0, j), (nx - 1, j)]))
        .collect();
    flood(border, &mut seen);
    let mut gaps = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            if !seen[at(i, j)] {
                gaps.push(flood(vec![(i, j)], &mut seen));
            }
        }
    }
    gaps
}

/// Cheap checks over one analysed group's layout, from the session's
/// panel footprints and overlap graph alone (no pixels): duplicate panels,
/// gaps in the coverage, overlaps too thin to match, and mostly empty
/// panels. Panels are reported by their user-facing file.
pub fn layout_checks(group: &str, session: &Session, graph: &OverlapGraph) -> Vec<Check> {
    let mut out = Vec::new();
    let user_file =
        |m: &crate::session::PanelMeta| m.source.clone().unwrap_or_else(|| m.path.clone());
    let metas = &session.panels;

    // Duplicates: the same file twice, or nearly the same footprint.
    for (i, a) in metas.iter().enumerate() {
        for b in &metas[i + 1..] {
            let (fa, fb) = (user_file(a), user_file(b));
            let same_file = fa == fb;
            let union = rect_area(&a.bbox) + rect_area(&b.bbox);
            let inter = intersection_area(&a.bbox, &b.bbox);
            let same_sky = union > inter
                && inter > 0
                && inter as f64 / (union - inter) as f64 >= DUPLICATE_IOU;
            if !(same_file || same_sky) {
                continue;
            }
            let (na, nb) = (file_name(&fa), file_name(&fb));
            out.push(Check {
                code: CheckCode::DuplicatePanels,
                target: CheckTarget::Overlap(fa.clone(), fb.clone()),
                rect: None,
                severity: Severity::Problem,
                panels: vec![fa, fb],
                groups: vec![group.to_string()],
                message: if same_file {
                    format!("{na} is added twice.")
                } else {
                    format!("{na} and {nb} cover nearly the same sky.")
                },
                remedy: Some("Remove one of the two panels.".into()),
            });
        }
    }

    // Gaps in the coverage: enclosed by panels, and a sizeable part of a
    // panel (a missing panel leaves a gap smaller than itself, since its
    // neighbours overlap it).
    let rects: Vec<[u64; 4]> = metas.iter().map(|m| m.bbox).collect();
    let smallest = rects.iter().map(rect_area).filter(|&a| a > 0).min();
    if let Some(smallest) = smallest {
        for (rect, area) in enclosed_gaps(&rects) {
            if (area as f64) < HOLE_MIN_PANEL_FRACTION * smallest as f64 {
                continue;
            }
            out.push(Check {
                code: CheckCode::CoverageHole,
                target: CheckTarget::Group(group.to_string()),
                rect: Some(rect),
                severity: Severity::Problem,
                panels: vec![],
                groups: vec![group.to_string()],
                message: format!("A gap in {group}'s coverage: a panel may be missing."),
                remedy: Some("Check that every panel of the mosaic has been added.".into()),
            });
        }
    }

    // Overlaps too thin to match.
    for e in &graph.edges {
        let (Some(a), Some(b)) = (
            metas.iter().find(|m| m.id == e.a),
            metas.iter().find(|m| m.id == e.b),
        ) else {
            continue;
        };
        let w = u64::from(e.bbox8[2].saturating_sub(e.bbox8[0]));
        let h = u64::from(e.bbox8[3].saturating_sub(e.bbox8[1]));
        let short = w.min(h) * u64::from(crate::summary::BLOCK);
        if short >= THIN_OVERLAP_PX {
            continue;
        }
        let (fa, fb) = (user_file(a), user_file(b));
        out.push(Check {
            code: CheckCode::ThinOverlap,
            target: CheckTarget::Overlap(fa.clone(), fb.clone()),
            rect: None,
            severity: Severity::Problem,
            message: format!(
                "{} and {} overlap by only {short} px: too thin to match them reliably.",
                file_name(&fa),
                file_name(&fb)
            ),
            panels: vec![fa, fb],
            groups: vec![group.to_string()],
            remedy: Some("Add more overlap between the two panels.".into()),
        });
    }

    // Mostly empty panels: the share of the bounding box not covered.
    let canvas_area = session.canvas.0 as f64 * session.canvas.1 as f64;
    for m in metas {
        let bbox_area = rect_area(&m.bbox) as f64;
        if bbox_area <= 0.0 || canvas_area <= 0.0 {
            continue;
        }
        let empty = 1.0 - m.nonzero_frac * canvas_area / bbox_area;
        if empty > EMPTY_AREA_FRACTION {
            let f = user_file(m);
            out.push(Check {
                code: CheckCode::EmptyArea,
                target: CheckTarget::Panel(f.clone()),
                rect: None,
                severity: Severity::Note,
                message: format!("{} has large empty areas.", file_name(&f)),
                panels: vec![f],
                groups: vec![group.to_string()],
                remedy: Some(
                    "Fine for a tilted or rotated panel; otherwise check the panel for missing data."
                        .into(),
                ),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::InputPanel;
    use crate::mosaic::grouping::resolve_groups;
    use crate::overlap::OverlapEdge;
    use crate::session::PanelMeta;
    use crate::synth::{
        SynthSip, SynthWcs, write_fits_solved, write_xisf, write_xisf_solved,
        write_xisf_with_header_xml,
    };

    /// A fresh directory per call (tests run in parallel, several calling
    /// with the same tag).
    fn tmp(tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mmm-checks-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn wcs(scale_deg: f64) -> SynthWcs {
        SynthWcs {
            crval: [84.0, -5.0],
            refimg: [8.0, 6.0],
            cd: [[-scale_deg, 0.0], [0.0, scale_deg]],
        }
    }

    fn planes(ch: u64) -> Vec<f32> {
        vec![0.5; (16 * 12 * ch) as usize]
    }

    fn solved_fits(dir: &Path, name: &str, scale: f64, sip: bool) -> PathBuf {
        let p = dir.join(name);
        let s = SynthSip {
            order: 2,
            a: vec![(2, 0, 1.0e-4)],
            b: vec![(0, 2, 1.0e-4)],
            ap: vec![(2, 0, -1.0e-4)],
            bp: vec![(0, 2, -1.0e-4)],
        };
        write_fits_solved(
            &p,
            16,
            12,
            1,
            &planes(1),
            &wcs(scale),
            sip.then_some(&s),
            -32,
        )
        .unwrap();
        p
    }

    fn facts_of(paths: &[PathBuf]) -> Vec<PanelFacts> {
        paths.iter().map(|p| panel_facts(p).unwrap()).collect()
    }

    fn run(cfg: &MosaicConfig, paths: &[PathBuf]) -> Vec<Check> {
        let groups = resolve_groups(cfg, &|_| None);
        input_checks(cfg, &facts_of(paths), &groups, false)
    }

    fn of(checks: &[Check], code: CheckCode) -> Vec<&Check> {
        checks.iter().filter(|c| c.code == code).collect()
    }

    #[test]
    fn distortion_missing_is_problem() {
        let d = tmp("dmiss");
        let p = solved_fits(&d, "lin.fits", 1e-3, false);
        let cfg = MosaicConfig::new(vec![p.clone()]);
        let checks = run(&cfg, &[p]);
        let c = of(&checks, CheckCode::DistortionModel);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].severity, Severity::Problem);
        assert!(c[0].message.starts_with("lin.fits"));
        assert!(c[0].message.contains("missing distortion model"));
        assert!(c[0].remedy.as_deref().unwrap().contains("distortion"));
    }

    #[test]
    fn distortion_present_passes() {
        let d = tmp("dpres");
        let p = solved_fits(&d, "sip.fits", 1e-3, true);
        let cfg = MosaicConfig::new(vec![p.clone()]);
        let checks = run(&cfg, &[p]);
        assert_eq!(
            of(&checks, CheckCode::DistortionModel)[0].severity,
            Severity::Passed
        );
        assert_eq!(
            of(&checks, CheckCode::SolutionPresent)[0].severity,
            Severity::Passed
        );
    }

    #[test]
    fn unsolved_panel_is_note_without_distortion_check() {
        let d = tmp("unsolved");
        let p = d.join("raw.xisf");
        write_xisf(&p, 16, 12, 1, &planes(1)).unwrap();
        let cfg = MosaicConfig::new(vec![p.clone()]);
        let checks = run(&cfg, &[p]);
        assert_eq!(
            of(&checks, CheckCode::SolutionPresent)[0].severity,
            Severity::Note
        );
        assert!(of(&checks, CheckCode::DistortionModel).is_empty());
    }

    #[test]
    fn scale_mismatch_flagged() {
        let d = tmp("scale");
        let a = solved_fits(&d, "a.fits", 1.0e-3, true);
        let b = solved_fits(&d, "b.fits", 1.0e-3, true);
        let c = solved_fits(&d, "c.fits", 1.05e-3, true);
        let paths = vec![a, b, c.clone()];
        let cfg = MosaicConfig::new(paths.clone());
        let checks = run(&cfg, &paths);
        let sm = of(&checks, CheckCode::ScaleMismatch);
        assert_eq!(sm.len(), 3);
        let bad: Vec<_> = sm
            .iter()
            .filter(|x| x.severity == Severity::Problem)
            .collect();
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].panels, vec![c]);
        assert_eq!(
            sm.iter().filter(|x| x.severity == Severity::Passed).count(),
            2
        );
    }

    #[test]
    fn mixed_channels_flagged() {
        let d = tmp("mixch");
        let a = d.join("a.xisf");
        let b = d.join("b.xisf");
        write_xisf_solved(&a, 16, 12, 1, &planes(1), &wcs(1e-3)).unwrap();
        write_xisf_solved(&b, 16, 12, 3, &planes(3), &wcs(1e-3)).unwrap();
        let paths = vec![a, b];
        let cfg = MosaicConfig::new(paths.clone());
        let checks = run(&cfg, &paths);
        let m = of(&checks, CheckCode::MixedChannels);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].severity, Severity::Problem);
        assert_eq!(m[0].panels.len(), 2);
    }

    fn filtered(dir: &Path, name: &str, filter: &str) -> PathBuf {
        let p = dir.join(name);
        let xml = format!(r#"<FITSKeyword name="FILTER" value="{filter}" comment=""/>"#);
        write_xisf_with_header_xml(&p, 16, 12, 1, &planes(1), &xml).unwrap();
        p
    }

    #[test]
    fn group_count_mismatch_note() {
        let d = tmp("gcount");
        let paths = vec![
            filtered(&d, "a.xisf", "Ha"),
            filtered(&d, "b.xisf", "Ha"),
            filtered(&d, "c.xisf", "OIII"),
        ];
        let mut cfg = MosaicConfig::new(paths.clone());
        cfg.grouping.enabled = true;
        cfg.grouping.rule = crate::mosaic::GroupRule::Header {
            key: "FILTER".into(),
        };
        let header = |p: &Path| {
            crate::mosaic::grouping::header_value(&InputPanel::open(p).unwrap(), "FILTER")
        };
        let groups = resolve_groups(&cfg, &header);
        let checks = input_checks(&cfg, &facts_of(&paths), &groups, false);
        let g = of(&checks, CheckCode::GroupCountMismatch);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].severity, Severity::Note);
        assert!(g[0].message.contains("Ha (2)") && g[0].message.contains("OIII (1)"));
    }

    #[test]
    fn filters_differ_while_ungrouped_note() {
        let d = tmp("filt");
        let paths = vec![filtered(&d, "a.xisf", "Ha"), filtered(&d, "b.xisf", "OIII")];
        let cfg = MosaicConfig::new(paths.clone());
        let checks = run(&cfg, &paths);
        let f = of(&checks, CheckCode::FiltersDifferUngrouped);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Note);
        assert_eq!(
            f[0].remedy.as_deref(),
            Some("Group by FILTER header to merge each filter separately.")
        );
    }

    #[test]
    fn ungrouped_note_when_grouping_on() {
        let d = tmp("ungr");
        let paths = vec![filtered(&d, "a.xisf", "Ha")];
        let mut cfg = MosaicConfig::new(paths.clone());
        cfg.grouping.enabled = true;
        cfg.grouping.rule = crate::mosaic::GroupRule::FilenameToken {
            keyword: "zzz".into(),
        };
        let checks = run(&cfg, &paths);
        assert_eq!(of(&checks, CheckCode::Ungrouped).len(), 1);
    }

    #[test]
    fn moved_and_modified_inputs_are_checks() {
        let d = tmp("files");
        let a = d.join("a.xisf");
        let b = d.join("b.xisf");
        let c = d.join("c.xisf");
        for p in [&a, &b, &c] {
            write_xisf(p, 16, 12, 1, &planes(1)).unwrap();
        }
        let mut cfg = MosaicConfig::new(vec![a.clone(), b.clone(), c.clone()]);
        for e in &mut cfg.panels {
            e.fingerprint = Some(fingerprint(&e.path).unwrap());
        }
        assert!(file_checks(&cfg).is_empty());
        std::fs::remove_file(&a).unwrap();
        write_xisf(&b, 20, 12, 1, &vec![0.5; 20 * 12]).unwrap();
        let checks = file_checks(&cfg);
        let missing = of(&checks, CheckCode::FileMissing);
        let changed = of(&checks, CheckCode::FileChanged);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].panels, vec![a]);
        assert_eq!(missing[0].severity, Severity::Problem);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].panels, vec![b]);
        assert_eq!(changed[0].severity, Severity::Note);
        assert!(changed[0].message.contains("reanalysed"));
        // A panel without a stored fingerprint is never "changed".
        cfg.panels[1].fingerprint = None;
        assert!(of(&file_checks(&cfg), CheckCode::FileChanged).is_empty());
    }

    #[test]
    fn accepted_requires_every_key() {
        let a = PathBuf::from("a.xisf");
        let b = PathBuf::from("b.xisf");
        let check = Check {
            code: CheckCode::MixedChannels,
            severity: Severity::Problem,
            panels: vec![a.clone(), b.clone()],
            groups: vec![],
            message: String::new(),
            remedy: None,
            target: CheckTarget::Session,
            rect: None,
        };
        let mut cfg = MosaicConfig::new(vec![]);
        assert!(!is_accepted(&cfg, &check));
        cfg.accepted_checks.push(CheckKey {
            code: CheckCode::MixedChannels,
            panel: Some(a),
            other: None,
        });
        assert!(!is_accepted(&cfg, &check));
        cfg.accepted_checks.push(CheckKey {
            code: CheckCode::MixedChannels,
            panel: Some(b),
            other: None,
        });
        assert!(is_accepted(&cfg, &check));
        // Panel-less checks use a single panel-less key.
        let none = Check {
            panels: vec![],
            ..check
        };
        assert_eq!(none.keys().len(), 1);
        assert!(!is_accepted(&cfg, &none));
    }

    fn meta(id: usize, mean: f64) -> PanelMeta {
        PanelMeta {
            id,
            path: PathBuf::from(format!("/cache/{id}/aligned.bin")),
            source: Some(PathBuf::from(format!("/in/p{id}.xisf"))),
            bbox: [0, 0, 1, 1],
            nonzero_frac: 0.5,
            ch_min: vec![0.0],
            ch_max: vec![1.0],
            ch_mean: vec![mean],
            storage: Default::default(),
        }
    }

    fn edge(a: usize, b: usize) -> OverlapEdge {
        OverlapEdge {
            a,
            b,
            bbox8: [0, 0, 1, 1],
            n_cells: 100,
        }
    }

    fn session(panels: Vec<PanelMeta>) -> Session {
        let mut s = Session::create(&tmp("sess")).unwrap();
        s.panels = panels;
        s
    }

    #[test]
    fn no_overlap_flagged() {
        let s = session(vec![meta(0, 0.1), meta(1, 0.1), meta(2, 0.1)]);
        let g = OverlapGraph {
            edges: vec![edge(0, 1)],
        };
        let checks = analysis_checks("Ha", &s, &g);
        let n = of(&checks, CheckCode::NoOverlap);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].severity, Severity::Problem);
        assert_eq!(n[0].panels, vec![PathBuf::from("/in/p2.xisf")]);
        assert_eq!(n[0].groups, vec!["Ha".to_string()]);
        // A single-panel group has nothing to overlap.
        let one = session(vec![meta(0, 0.1)]);
        assert!(analysis_checks("Ha", &one, &OverlapGraph::default()).is_empty());
    }

    #[test]
    fn sky_level_mismatch_noted() {
        let s = session(vec![
            meta(0, 0.1),
            meta(1, 0.1),
            meta(2, 0.1),
            meta(3, 0.35),
        ]);
        let g = OverlapGraph {
            edges: vec![
                edge(0, 1),
                edge(0, 2),
                edge(1, 2),
                edge(0, 3),
                edge(1, 3),
                edge(2, 3),
            ],
        };
        let checks = analysis_checks("g", &s, &g);
        let sl = of(&checks, CheckCode::SkyLevelMismatch);
        assert_eq!(sl.len(), 1);
        assert_eq!(sl[0].severity, Severity::Note);
        assert_eq!(sl[0].panels, vec![PathBuf::from("/in/p3.xisf")]);
    }

    /// Every check the engine makes, of every code: the scenarios of the
    /// tests above, run together.
    fn every_check() -> Vec<Check> {
        let d = tmp("every");
        let mut checks = Vec::new();
        // Solution, distortion (missing and present), scale.
        let solved = vec![
            solved_fits(&d, "lin.fits", 1.0e-3, false),
            solved_fits(&d, "sip.fits", 1.0e-3, true),
            solved_fits(&d, "odd.fits", 1.05e-3, true),
        ];
        checks.extend(run(&MosaicConfig::new(solved.clone()), &solved));
        // Unsolved; mixed channels.
        let raw = d.join("raw.xisf");
        write_xisf(&raw, 16, 12, 1, &planes(1)).unwrap();
        let rgb = d.join("rgb.xisf");
        write_xisf_solved(&rgb, 16, 12, 3, &planes(3), &wcs(1e-3)).unwrap();
        let mono = d.join("mono.xisf");
        write_xisf_solved(&mono, 16, 12, 1, &planes(1), &wcs(1e-3)).unwrap();
        let mixed = vec![raw, rgb, mono];
        checks.extend(run(&MosaicConfig::new(mixed.clone()), &mixed));
        // Filters differ while ungrouped.
        let filters = vec![filtered(&d, "a.xisf", "Ha"), filtered(&d, "b.xisf", "OIII")];
        checks.extend(run(&MosaicConfig::new(filters.clone()), &filters));
        // Group counts differ; a panel no rule places.
        let mut filters = filters;
        filters.push(filtered(&d, "c.xisf", "Ha"));
        let mut cfg = MosaicConfig::new(filters.clone());
        cfg.grouping.enabled = true;
        cfg.grouping.rule = crate::mosaic::GroupRule::Header {
            key: "FILTER".into(),
        };
        let header = |p: &Path| {
            crate::mosaic::grouping::header_value(&InputPanel::open(p).unwrap(), "FILTER")
        };
        checks.extend(input_checks(
            &cfg,
            &facts_of(&filters),
            &resolve_groups(&cfg, &header),
            false,
        ));
        cfg.grouping.rule = crate::mosaic::GroupRule::FilenameToken {
            keyword: "zzz".into(),
        };
        checks.extend(run(&cfg, &filters));
        // A file missing, a file changed.
        let (gone, changed) = (d.join("gone.xisf"), d.join("changed.xisf"));
        for p in [&gone, &changed] {
            write_xisf(p, 16, 12, 1, &planes(1)).unwrap();
        }
        let mut cfg = MosaicConfig::new(vec![gone.clone(), changed.clone()]);
        for e in &mut cfg.panels {
            e.fingerprint = Some(fingerprint(&e.path).unwrap());
        }
        std::fs::remove_file(&gone).unwrap();
        write_xisf(&changed, 20, 12, 1, &vec![0.5; 20 * 12]).unwrap();
        checks.extend(file_checks(&cfg));
        // No overlap; a sky level apart.
        let s = session(vec![
            meta(0, 0.1),
            meta(1, 0.1),
            meta(2, 0.1),
            meta(3, 0.35),
        ]);
        let g = OverlapGraph {
            edges: vec![edge(0, 1), edge(0, 3), edge(1, 3)],
        };
        checks.extend(analysis_checks("Ha", &s, &g));
        // Layout: a thin overlap, a duplicate, a hole, an empty area.
        checks.extend(layout_checks("Ha", &layout_session(), &thin_graph()));
        checks.extend(layout_checks(
            "Ha",
            &ring_session(),
            &OverlapGraph::default(),
        ));
        // Pre-aligned: the distortion check passes by acceptance.
        let lin = vec![solved_fits(&d, "lin2.fits", 1.0e-3, false)];
        let cfg = MosaicConfig::new(lin.clone());
        let groups = resolve_groups(&cfg, &|_| None);
        checks.extend(input_checks(&cfg, &facts_of(&lin), &groups, true));
        checks
    }

    #[test]
    fn no_check_says_blend() {
        let checks = every_check();
        // Every code is made (a new code must join the scenarios).
        for code in [
            CheckCode::SolutionPresent,
            CheckCode::DistortionModel,
            CheckCode::ScaleMismatch,
            CheckCode::MixedChannels,
            CheckCode::GroupCountMismatch,
            CheckCode::FiltersDifferUngrouped,
            CheckCode::Ungrouped,
            CheckCode::FileChanged,
            CheckCode::FileMissing,
            CheckCode::NoOverlap,
            CheckCode::SkyLevelMismatch,
            CheckCode::DuplicatePanels,
            CheckCode::CoverageHole,
            CheckCode::ThinOverlap,
            CheckCode::EmptyArea,
        ] {
            // Exhaustive: a code added to the enum fails to compile here
            // until it is listed (and made) above.
            match code {
                CheckCode::SolutionPresent
                | CheckCode::DistortionModel
                | CheckCode::ScaleMismatch
                | CheckCode::MixedChannels
                | CheckCode::GroupCountMismatch
                | CheckCode::FiltersDifferUngrouped
                | CheckCode::Ungrouped
                | CheckCode::FileChanged
                | CheckCode::FileMissing
                | CheckCode::NoOverlap
                | CheckCode::SkyLevelMismatch
                | CheckCode::DuplicatePanels
                | CheckCode::CoverageHole
                | CheckCode::ThinOverlap
                | CheckCode::EmptyArea => {}
            }
            assert!(!of(&checks, code).is_empty(), "no {code:?} check made");
        }
        // The app shows each message and remedy: the product says merge.
        for c in &checks {
            for t in std::iter::once(&c.message).chain(&c.remedy) {
                assert!(!t.to_lowercase().contains("blend"), "{:?}: {t}", c.code);
            }
        }
    }

    #[test]
    fn keys_are_stable_json() {
        assert_eq!(
            serde_json::to_string(&CheckCode::DistortionModel).unwrap(),
            "\"distortion_model\""
        );
        assert_eq!(
            serde_json::to_string(&Severity::Passed).unwrap(),
            "\"passed\""
        );
        for (code, name) in [
            (CheckCode::DuplicatePanels, "duplicate_panels"),
            (CheckCode::CoverageHole, "coverage_hole"),
            (CheckCode::ThinOverlap, "thin_overlap"),
            (CheckCode::EmptyArea, "empty_area"),
        ] {
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{name}\""));
        }
        // An old key (no `other`) serialises exactly as before and reads back.
        let old = CheckKey {
            code: CheckCode::NoOverlap,
            panel: Some(PathBuf::from("a.xisf")),
            other: None,
        };
        let json = serde_json::to_string(&old).unwrap();
        assert_eq!(json, r#"{"code":"no_overlap","panel":"a.xisf"}"#);
        assert_eq!(serde_json::from_str::<CheckKey>(&json).unwrap(), old);
        let pair = CheckKey {
            other: Some(PathBuf::from("b.xisf")),
            ..old
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert!(json.contains(r#""other":"b.xisf""#));
        assert_eq!(serde_json::from_str::<CheckKey>(&json).unwrap(), pair);
    }

    // ---- targets, aligned mode, layout checks ----

    /// A session on a 1000 x 1000 canvas with the given panels.
    fn canvas_session(panels: Vec<PanelMeta>) -> Session {
        let mut s = session(panels);
        s.canvas = (1000, 1000, 1);
        s
    }

    /// A fully covered panel on the 1000 x 1000 canvas.
    fn boxed(id: usize, bbox: [u64; 4]) -> PanelMeta {
        let area = ((bbox[2] - bbox[0]) * (bbox[3] - bbox[1])) as f64;
        PanelMeta {
            bbox,
            nonzero_frac: area / 1.0e6,
            ..meta(id, 0.1)
        }
    }

    fn edge_at(a: usize, b: usize, bbox8: [u32; 4]) -> OverlapEdge {
        OverlapEdge {
            a,
            b,
            bbox8,
            n_cells: 100,
        }
    }

    /// Panels 0 and 1 side by side, 2 a copy of 0, 3 half empty.
    fn layout_session() -> Session {
        let mut sparse = boxed(3, [500, 500, 700, 700]);
        sparse.nonzero_frac /= 2.0;
        canvas_session(vec![
            boxed(0, [0, 0, 400, 400]),
            boxed(1, [390, 0, 800, 400]),
            boxed(2, [0, 0, 400, 400]),
            sparse,
        ])
    }

    /// Four panels around a gap at x 200..400, y 300..500.
    fn ring_session() -> Session {
        canvas_session(vec![
            boxed(0, [0, 0, 400, 300]),
            boxed(1, [400, 0, 800, 500]),
            boxed(2, [200, 500, 800, 800]),
            boxed(3, [0, 300, 200, 800]),
        ])
    }

    /// One overlap 8 cells (64 px) wide.
    fn thin_graph() -> OverlapGraph {
        OverlapGraph {
            edges: vec![edge_at(0, 1, [48, 0, 56, 50])],
        }
    }

    /// A clean 2 x 2 tiling with generous overlaps.
    fn clean_session() -> (Session, OverlapGraph) {
        let s = canvas_session(vec![
            boxed(0, [0, 0, 600, 600]),
            boxed(1, [400, 0, 1000, 600]),
            boxed(2, [0, 400, 600, 1000]),
            boxed(3, [400, 400, 1000, 1000]),
        ]);
        let g = OverlapGraph {
            edges: vec![
                edge_at(0, 1, [50, 0, 75, 75]),
                edge_at(0, 2, [0, 50, 75, 75]),
                edge_at(1, 3, [50, 50, 125, 75]),
                edge_at(2, 3, [50, 50, 75, 125]),
                edge_at(0, 3, [50, 50, 75, 75]),
                edge_at(1, 2, [50, 50, 75, 75]),
            ],
        };
        (s, g)
    }

    fn no_edges() -> OverlapGraph {
        OverlapGraph::default()
    }

    #[test]
    fn existing_checks_name_their_place() {
        let d = tmp("targets");
        let a = solved_fits(&d, "a.fits", 1.0e-3, true);
        let b = d.join("raw.xisf");
        write_xisf(&b, 16, 12, 1, &planes(1)).unwrap();
        let paths = vec![a, b];
        let checks = run(&MosaicConfig::new(paths.clone()), &paths);
        for c in of(&checks, CheckCode::SolutionPresent) {
            assert_eq!(c.target, CheckTarget::Panel(c.panels[0].clone()));
        }
        let s = session(vec![meta(0, 0.1), meta(1, 0.1), meta(2, 0.1)]);
        let g = OverlapGraph {
            edges: vec![edge(0, 1)],
        };
        let checks = analysis_checks("Ha", &s, &g);
        assert_eq!(
            of(&checks, CheckCode::NoOverlap)[0].target,
            CheckTarget::Panel(PathBuf::from("/in/p2.xisf"))
        );
        // A mixed-channel group is the group's problem.
        let rgb = d.join("rgb.xisf");
        write_xisf_solved(&rgb, 16, 12, 3, &planes(3), &wcs(1e-3)).unwrap();
        let mono = d.join("mono.xisf");
        write_xisf_solved(&mono, 16, 12, 1, &planes(1), &wcs(1e-3)).unwrap();
        let mixed = vec![rgb, mono];
        let checks = run(&MosaicConfig::new(mixed.clone()), &mixed);
        assert!(matches!(
            of(&checks, CheckCode::MixedChannels)[0].target,
            CheckTarget::Group(_)
        ));
    }

    #[test]
    fn aligned_mode_passes_missing_distortion() {
        let d = tmp("aligned");
        let p = solved_fits(&d, "lin.fits", 1e-3, false);
        let cfg = MosaicConfig::new(vec![p.clone()]);
        let groups = resolve_groups(&cfg, &|_| None);
        let facts = facts_of(std::slice::from_ref(&p));
        let checks = input_checks(&cfg, &facts, &groups, true);
        let c = of(&checks, CheckCode::DistortionModel);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].severity, Severity::Passed);
        assert_eq!(
            c[0].message,
            "No distortion model: accepted for pre-aligned panels"
        );
        // Solved input still has the problem.
        let checks = input_checks(&cfg, &facts, &groups, false);
        assert_eq!(
            of(&checks, CheckCode::DistortionModel)[0].severity,
            Severity::Problem
        );
    }

    #[test]
    fn old_accepted_keys_still_match() {
        let old: CheckKey =
            serde_json::from_str(r#"{"code":"no_overlap","panel":"/in/p2.xisf"}"#).unwrap();
        assert_eq!(old.other, None);
        let s = session(vec![meta(0, 0.1), meta(1, 0.1), meta(2, 0.1)]);
        let g = OverlapGraph {
            edges: vec![edge(0, 1)],
        };
        let checks = analysis_checks("Ha", &s, &g);
        let mut cfg = MosaicConfig::new(vec![]);
        cfg.accepted_checks.push(old);
        assert!(is_accepted(&cfg, of(&checks, CheckCode::NoOverlap)[0]));
    }

    #[test]
    fn pair_keys_accept_one_overlap() {
        let s = canvas_session(vec![
            boxed(0, [0, 0, 400, 400]),
            boxed(1, [390, 0, 800, 400]),
            boxed(2, [0, 390, 400, 800]),
        ]);
        let g = OverlapGraph {
            edges: vec![
                edge_at(0, 1, [48, 0, 56, 50]),
                edge_at(0, 2, [0, 48, 50, 56]),
            ],
        };
        let checks = layout_checks("Ha", &s, &g);
        let thin = of(&checks, CheckCode::ThinOverlap);
        assert_eq!(thin.len(), 2);
        let keys = thin[0].keys();
        assert_eq!(keys.len(), 1);
        assert!(keys[0].panel.is_some() && keys[0].other.is_some());
        let mut cfg = MosaicConfig::new(vec![]);
        cfg.accepted_checks.extend(keys);
        assert!(is_accepted(&cfg, thin[0]));
        assert!(!is_accepted(&cfg, thin[1]));
    }

    #[test]
    fn thin_overlap_fires_below_the_limit() {
        let checks = layout_checks("Ha", &layout_session(), &thin_graph());
        let t = of(&checks, CheckCode::ThinOverlap);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].severity, Severity::Problem);
        assert_eq!(
            t[0].target,
            CheckTarget::Overlap(PathBuf::from("/in/p0.xisf"), PathBuf::from("/in/p1.xisf"))
        );
        assert_eq!(
            t[0].message,
            "p0.xisf and p1.xisf overlap by only 64 px: too thin to match them reliably."
        );
        // 16 cells x 8 = 128 px is not thin.
        let wide = OverlapGraph {
            edges: vec![edge_at(0, 1, [48, 0, 64, 50])],
        };
        let checks = layout_checks("Ha", &layout_session(), &wide);
        assert!(of(&checks, CheckCode::ThinOverlap).is_empty());
    }

    #[test]
    fn duplicate_panels_by_path_and_by_footprint() {
        let checks = layout_checks("Ha", &layout_session(), &no_edges());
        let d = of(&checks, CheckCode::DuplicatePanels);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Problem);
        assert!(matches!(d[0].target, CheckTarget::Overlap(..)));
        assert_eq!(
            d[0].message,
            "p0.xisf and p2.xisf cover nearly the same sky."
        );
        // The same file twice.
        let mut twice = boxed(1, [500, 500, 600, 600]);
        twice.source = Some(PathBuf::from("/in/p0.xisf"));
        let s = canvas_session(vec![boxed(0, [0, 0, 100, 100]), twice]);
        let checks = layout_checks("Ha", &s, &no_edges());
        let d = of(&checks, CheckCode::DuplicatePanels);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].message, "p0.xisf is added twice.");
        // Merely overlapping panels are not duplicates.
        let (s, g) = clean_session();
        assert!(of(&layout_checks("Ha", &s, &g), CheckCode::DuplicatePanels).is_empty());
    }

    #[test]
    fn coverage_hole_is_found_with_its_rect() {
        let s = ring_session();
        let checks = layout_checks("Ha", &s, &no_edges());
        let h = of(&checks, CheckCode::CoverageHole);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].severity, Severity::Problem);
        assert_eq!(h[0].target, CheckTarget::Group("Ha".into()));
        assert_eq!(h[0].rect, Some([200, 300, 400, 500]));
        assert_eq!(
            h[0].message,
            "A gap in Ha's coverage: a panel may be missing."
        );
        // Close the gap: no hole.
        let mut closed = s.clone();
        closed.panels.push(boxed(4, [200, 300, 400, 500]));
        let checks = layout_checks("Ha", &closed, &no_edges());
        assert!(of(&checks, CheckCode::CoverageHole).is_empty());
        // An open notch (not enclosed) is not a hole.
        let mut notch = s.clone();
        notch.panels.remove(3);
        let checks = layout_checks("Ha", &notch, &no_edges());
        assert!(of(&checks, CheckCode::CoverageHole).is_empty());
        // A sliver smaller than any panel is ignored.
        let mut tiny = s;
        tiny.panels.push(boxed(4, [200, 300, 400, 490]));
        let checks = layout_checks("Ha", &tiny, &no_edges());
        assert!(of(&checks, CheckCode::CoverageHole).is_empty());
    }

    #[test]
    fn empty_area_is_a_note() {
        let checks = layout_checks("Ha", &layout_session(), &no_edges());
        let e = of(&checks, CheckCode::EmptyArea);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].severity, Severity::Note);
        assert_eq!(
            e[0].target,
            CheckTarget::Panel(PathBuf::from("/in/p3.xisf"))
        );
        assert_eq!(e[0].message, "p3.xisf has large empty areas.");
        // 20% empty is under the limit, 30% over it.
        let mut s = canvas_session(vec![boxed(0, [0, 0, 400, 400])]);
        s.panels[0].nonzero_frac *= 0.8;
        let n = |s: &Session| of(&layout_checks("Ha", s, &no_edges()), CheckCode::EmptyArea).len();
        assert_eq!(n(&s), 0);
        s.panels[0].nonzero_frac *= 0.875;
        assert_eq!(n(&s), 1);
    }

    #[test]
    fn a_clean_session_raises_no_new_problems() {
        let (s, g) = clean_session();
        let checks = layout_checks("Ha", &s, &g);
        assert!(checks.is_empty(), "{checks:?}");
        let all = analysis_checks("Ha", &s, &g);
        assert!(
            all.iter().all(|c| c.severity != Severity::Problem),
            "{all:?}"
        );
    }
}
