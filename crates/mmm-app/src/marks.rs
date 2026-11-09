//! Where the checks' problems are on the canvas, and what both canvases
//! draw for them (spec §7.2–§7.4). One builder for the plan and the merged
//! mosaic, so a problem is badged at the same canvas place on both.
//!
//! - **Places.** A check tied to a panel, an overlap, a seam or an area of
//!   the canvas (a coverage hole) has a [`Place`] ([`Place::of`]); a
//!   session- or group-wide one has none: it counts on the group chips and
//!   in the Problems list only. The unaccepted problems of the groups shown
//!   are gathered one [`ProblemMark`] per badge place ([`problem_marks`],
//!   [`Place::badged`]): a panel, a pair of panels (its overlap's problems
//!   and its seam's share one badge, spec §7.4), an area. One badge,
//!   counting them ("{n} problem(s) here"), with the problem icon.
//! - **Where.** A place's rect comes from the analysis (the plan's
//!   [`Layout`], in the analysed frame) and the checks' seam lines
//!   ([`place_rect`], [`target_rect`]): a panel's analysed bbox, the
//!   intersection of two panels' bboxes, the bbox of a seam's lines (else
//!   its overlap's), an area's rect. Going to a seam frames its lines; its
//!   badge is its pair's, at the overlap (at its lines only where the
//!   overlap does not resolve). A panel's badge sits at its outline's
//!   top-right corner, any other at its rect's centre ([`badge_point`]). A
//!   panel or group no longer in the session places nothing.
//! - **Seams** ([`seam_lines`]): the shown groups' seam lines in a neutral
//!   colour; one with an unaccepted problem of its own (the seam shows after
//!   a merge) in the warning colour and thicker, so colour is not the only
//!   signal, and badged with its pair. An accepted one goes neutral.
//! - **Outlines over the mosaic** ([`panel_outlines`]): each shown panel's
//!   analysed bbox, numbered as on the plan, in the problem colour when it
//!   is badged ([`outline_color`], as the plan's).
//! - **Chips** ([`problem_count`]): a group's unaccepted problems, placed or
//!   not.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use egui::Color32;
use mega_ui::canvas_layers::{CanvasBadge, Overlay, Polyline};
use mega_ui::theme::tokens::{LINE_WIDTH, LINE_WIDTH_STRONG};
use mega_ui::theme::{Tone, palette};
use mmm_core::checks::{Check, CheckTarget, Severity, is_accepted};
use mmm_core::health::SeamLine;
use mmm_core::mosaic::MosaicConfig;

use crate::checks::ChecksReport;
use crate::plan_view::{Layout, group_members};
use crate::session::OpenSession;
use crate::util::group_names;

/// Each analysed group's seam lines (as in [`ChecksReport::seams`]).
pub type Seams = BTreeMap<String, Vec<SeamLine>>;

/// A place on the canvas a problem is tied to: its badge sits there.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    /// A panel (its user-facing file).
    Panel(PathBuf),
    /// The overlap of two panels, the pair in path order.
    Overlap(PathBuf, PathBuf),
    /// The seam between two panels in a group's merge, the pair in path
    /// order.
    Seam {
        /// The group merged.
        group: String,
        /// The lesser panel of the pair.
        a: PathBuf,
        /// The greater panel of the pair.
        b: PathBuf,
    },
    /// An area of the canvas, `[x0, y0, x1, y1]` (a coverage hole).
    Area([u64; 4]),
}

impl Place {
    /// Where `check` is, if it is tied to a place on the canvas: its panel,
    /// overlap or seam, or the area it names; none for a session- or
    /// group-wide check without one.
    pub fn of(check: &Check) -> Option<Place> {
        let pair = |a: &PathBuf, b: &PathBuf| {
            if a <= b {
                (a.clone(), b.clone())
            } else {
                (b.clone(), a.clone())
            }
        };
        match &check.target {
            CheckTarget::Panel(p) => Some(Place::Panel(p.clone())),
            CheckTarget::Overlap(a, b) => {
                let (a, b) = pair(a, b);
                Some(Place::Overlap(a, b))
            }
            CheckTarget::Seam { group, a, b } => {
                let (a, b) = pair(a, b);
                Some(Place::Seam {
                    group: group.clone(),
                    a,
                    b,
                })
            }
            CheckTarget::Session | CheckTarget::Group(_) => check.rect.map(Place::Area),
        }
    }

    /// Where a problem at this place is badged: a seam with its pair's
    /// overlap (one badge per pair of panels), any other place at itself.
    pub fn badged(self) -> Place {
        match self {
            Place::Seam { a, b, .. } => Place::Overlap(a, b),
            other => other,
        }
    }

    /// An id for the place's badge: the same for the same place, so the
    /// badge keeps its widget (and keyboard focus) while problems come and
    /// go elsewhere.
    fn id(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// The unaccepted problems at one badge place: one badge.
#[derive(Clone, Debug, PartialEq)]
pub struct ProblemMark {
    /// The badge's id ([`CanvasBadge::id`]), the same while the place is
    /// (for a pair, whichever of its overlap's and seam's problems it has).
    pub id: u64,
    /// Where the badge sits ([`Place::badged`]): a panel, a pair of panels
    /// ([`Place::Overlap`], for its overlap's and its seam's problems), or
    /// an area.
    pub place: Place,
    /// The place's rect in the analysed frame ([`place_rect`]; for a pair
    /// whose overlap does not resolve, its seam's lines' bbox), if it
    /// resolves.
    pub rect: Option<[f64; 4]>,
    /// The problems there, in the report's order (the Problems list's).
    pub problems: Vec<Check>,
}

impl ProblemMark {
    /// Its badge at canvas point `at`: the problem icon and the count, said
    /// as "{n} problem(s) here".
    pub fn badge(&self, at: [f64; 2]) -> CanvasBadge {
        let n = u32::try_from(self.problems.len()).unwrap_or(u32::MAX);
        CanvasBadge {
            id: self.id,
            at,
            tone: Tone::Problem,
            count: n,
            description: format!("{n} problem{} here", if n == 1 { "" } else { "s" }),
        }
    }

    /// Its badge where the analysis places it ([`badge_point`] of its
    /// rect); none if its place does not resolve.
    pub fn placed_badge(&self) -> Option<CanvasBadge> {
        self.rect.map(|r| self.badge(badge_point(&self.place, r)))
    }
}

/// Whether `check` is a problem not accepted under `cfg`.
fn unaccepted_problem(cfg: &MosaicConfig, check: &Check) -> bool {
    check.severity == Severity::Problem && !is_accepted(cfg, check)
}

/// Whether `check` concerns group `group`, whose panels are `panels`: it
/// names the group (in its groups, else as its target's group), or names
/// no group and is about one of its panels.
fn concerns(check: &Check, group: &str, panels: &[PathBuf]) -> bool {
    if !check.groups.is_empty() {
        return check.groups.iter().any(|g| g == group);
    }
    let mine = |p: &PathBuf| panels.contains(p);
    let named = check.panels.iter().any(mine);
    match &check.target {
        CheckTarget::Group(g) | CheckTarget::Seam { group: g, .. } => g == group,
        CheckTarget::Panel(p) => named || mine(p),
        CheckTarget::Overlap(a, b) => named || mine(a) || mine(b),
        CheckTarget::Session => named,
    }
}

/// The unaccepted problems in `report` that concern any of `groups` (the
/// groups shown), one mark per badge place ([`Place::badged`]: a pair's
/// overlap and seam problems together), in place order, each with its rect
/// from `layout` and the report's seam lines: a pair's overlap, else its
/// seam's lines. None without a report.
pub fn problem_marks(
    session: &OpenSession,
    layout: &Layout,
    report: Option<&ChecksReport>,
    groups: &[String],
) -> Vec<ProblemMark> {
    let Some(report) = report else {
        return Vec::new();
    };
    let cfg = &session.mosaic.config;
    let members = group_members(session, groups);
    let mut at: BTreeMap<Place, Vec<Check>> = BTreeMap::new();
    for c in &report.checks {
        let shown = groups.iter().any(|g| {
            let panels = members.get(g).map_or(&[][..], Vec::as_slice);
            concerns(c, g, panels)
        });
        if !shown || !unaccepted_problem(cfg, c) {
            continue;
        }
        if let Some(place) = Place::of(c) {
            at.entry(place.badged()).or_default().push(c.clone());
        }
    }
    let seams = &report.seams;
    at.into_iter()
        .map(|(place, problems)| {
            // A pair whose overlap does not resolve: at its seam's lines.
            let rect = place_rect(session, layout, seams, &place).or_else(|| {
                problems.iter().find_map(|c| match Place::of(c)? {
                    seam @ Place::Seam { .. } => place_rect(session, layout, seams, &seam),
                    _ => None,
                })
            });
            ProblemMark {
                id: place.id(),
                place,
                rect,
                problems,
            }
        })
        .collect()
}

/// How many unaccepted problems in `report` concern group `group`, placed
/// or not (its chip's count).
pub fn problem_count(session: &OpenSession, report: &ChecksReport, group: &str) -> u32 {
    let cfg = &session.mosaic.config;
    let members = group_members(session, &[group.to_string()]);
    let panels = members.get(group).map_or(&[][..], Vec::as_slice);
    let n = report
        .checks
        .iter()
        .filter(|c| unaccepted_problem(cfg, c) && concerns(c, group, panels))
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The intersection of two rects, if it is not empty.
fn intersect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let r = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (r[0] < r[2] && r[1] < r[3]).then_some(r)
}

/// The ids of panels `a` and `b` in group `group`'s analysis (`layout`),
/// lesser first.
fn pair_ids(layout: &Layout, group: &str, a: &Path, b: &Path) -> Option<(usize, usize)> {
    let ids = layout.ids.get(group)?;
    let (x, y) = (*ids.get(a)?, *ids.get(b)?);
    Some((x.min(y), x.max(y)))
}

/// The bbox of every seam line of group `group` between panels `a` and
/// `b`: one seam however many lines it runs in.
fn seam_bbox(layout: &Layout, seams: &Seams, group: &str, a: &Path, b: &Path) -> Option<[f64; 4]> {
    let pair = pair_ids(layout, group, a, b)?;
    seams
        .get(group)?
        .iter()
        .filter(|l| line_ids(l) == pair)
        .flat_map(|l| &l.points)
        .fold(None, |acc: Option<[f64; 4]>, p| {
            Some(match acc {
                None => [p[0], p[1], p[0], p[1]],
                Some(r) => [
                    r[0].min(p[0]),
                    r[1].min(p[1]),
                    r[2].max(p[0]),
                    r[3].max(p[1]),
                ],
            })
        })
}

/// A seam line's pair of panel ids, lesser first.
fn line_ids(l: &SeamLine) -> (usize, usize) {
    (usize::from(l.a.min(l.b)), usize::from(l.a.max(l.b)))
}

/// The canvas rect of `place` in the analysed frame: a panel's analysed
/// bbox (`layout`), the intersection of two panels' bboxes, the bbox of a
/// seam's lines in `seams` (else its panels' overlap), an area's rect.
/// None for a panel no longer in `session`, a seam of a group it no longer
/// has, panels that do not overlap, or what the analysis does not place.
pub fn place_rect(
    session: &OpenSession,
    layout: &Layout,
    seams: &Seams,
    place: &Place,
) -> Option<[f64; 4]> {
    let cfg = &session.mosaic.config;
    let panel = |p: &Path| {
        cfg.panels
            .iter()
            .any(|e| e.path == p)
            .then(|| layout.rects.get(p).copied())
            .flatten()
    };
    match place {
        Place::Panel(p) => panel(p),
        Place::Overlap(a, b) => intersect(panel(a)?, panel(b)?),
        Place::Seam { group, a, b } => {
            let (ra, rb) = (panel(a)?, panel(b)?);
            if !group_names(session).contains(group) {
                return None;
            }
            seam_bbox(layout, seams, group, a, b).or_else(|| intersect(ra, rb))
        }
        Place::Area(r) => Some(r.map(|v| v as f64)),
    }
}

/// Where a badge for `place` sits given its rect: a panel's at the
/// outline's top-right corner, any other at the rect's centre.
pub fn badge_point(place: &Place, rect: [f64; 4]) -> [f64; 2] {
    match place {
        Place::Panel(_) => [rect[2], rect[1]],
        _ => [(rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0],
    }
}

/// The canvas rect (whole pixels, covering it) of `check`'s target: its
/// panel's analysed bbox, its overlap, its seam's lines' bbox (else the
/// overlap), or its area ([`place_rect`]). None for a check with no place
/// on the canvas, or one whose target no longer resolves.
pub fn target_rect(
    session: &OpenSession,
    layout: &Layout,
    seams: &Seams,
    check: &Check,
) -> Option<[u64; 4]> {
    let place = Place::of(check)?;
    Some(covering_px(place_rect(session, layout, seams, &place)?))
}

/// The whole-pixel canvas rect covering `r` (none of it below zero).
pub fn covering_px(r: [f64; 4]) -> [u64; 4] {
    let px = |v: f64| v.max(0.0) as u64;
    [
        px(r[0].floor()),
        px(r[1].floor()),
        px(r[2].ceil()),
        px(r[3].ceil()),
    ]
}

/// The seam lines of `groups` in `report` (canvas coordinates): neutral,
/// and those of a seam with an unaccepted problem of its own in `marks` (an
/// overlap problem of the same pair does not count) in the warning colour
/// and thicker, drawn last (on top). None without a report.
pub fn seam_lines(
    layout: &Layout,
    report: Option<&ChecksReport>,
    groups: &[String],
    marks: &[ProblemMark],
) -> Vec<Polyline> {
    let Some(report) = report else {
        return Vec::new();
    };
    let badged: BTreeSet<(&str, (usize, usize))> = marks
        .iter()
        .flat_map(|m| &m.problems)
        .filter_map(|c| match &c.target {
            CheckTarget::Seam { group, a, b } => {
                Some((group.as_str(), pair_ids(layout, group, a, b)?))
            }
            _ => None,
        })
        .collect();
    let (mut neutral, mut warned) = (Vec::new(), Vec::new());
    for g in groups {
        for l in report.seams.get(g).into_iter().flatten() {
            if badged.contains(&(g.as_str(), line_ids(l))) {
                warned.push(Polyline {
                    points: l.points.clone(),
                    color: Tone::Warning.color(),
                    width: LINE_WIDTH_STRONG,
                    dashed: false,
                });
            } else {
                neutral.push(Polyline {
                    points: l.points.clone(),
                    color: Tone::Neutral.color(),
                    width: LINE_WIDTH,
                    dashed: false,
                });
            }
        }
    }
    neutral.extend(warned);
    neutral
}

/// The ring a selected overlap or seam is highlighted with at its rect
/// (`[x0, y0, x1, y1]`, canvas coordinates): the selection colour, as the
/// plan outlines a selected panel, at the strong line width.
pub fn selection_ring(rect: [f64; 4]) -> Polyline {
    let [x0, y0, x1, y1] = rect;
    Polyline {
        points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]],
        color: palette::ACCENT_400,
        width: LINE_WIDTH_STRONG,
        dashed: false,
    }
}

/// A panel's outline colour: the problem colour when it is badged, else
/// the accent.
pub fn outline_color(badged: bool) -> Color32 {
    if badged {
        palette::HA_500
    } else {
        palette::ACCENT_500
    }
}

/// The outlines of the panels of `groups` (the groups shown, in order) at
/// their analysed bboxes (`layout`), for the merged mosaic: numbered across
/// the view as the plan numbers them (when `numbers`), a panel badged in
/// `marks` in the problem colour. A panel the analysis has not placed has
/// none (its number is still taken).
pub fn panel_outlines(
    session: &OpenSession,
    layout: &Layout,
    groups: &[String],
    marks: &[ProblemMark],
    numbers: bool,
) -> Vec<Overlay> {
    let members = group_members(session, groups);
    let badged: BTreeSet<&Path> = marks
        .iter()
        .filter_map(|m| match &m.place {
            Place::Panel(p) => Some(p.as_path()),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    let panels = groups.iter().filter_map(|g| members.get(g)).flatten();
    for (i, p) in panels.enumerate() {
        let Some(rect) = layout.rects.get(p) else {
            continue;
        };
        out.push(Overlay {
            rect: *rect,
            color: outline_color(badged.contains(p.as_path())),
            label: if numbers {
                (i + 1).to_string()
            } else {
                String::new()
            },
            dashed: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use mmm_core::checks::CheckCode;

    use super::*;
    use crate::start::tests::temp_dir;
    use crate::testutil::two_group_session;

    /// A problem at `target`, naming its panels (as the core's checks do).
    fn problem(target: CheckTarget, groups: &[&str]) -> Check {
        let panels = match &target {
            CheckTarget::Panel(p) => vec![p.clone()],
            CheckTarget::Overlap(a, b) | CheckTarget::Seam { a, b, .. } => {
                vec![a.clone(), b.clone()]
            }
            CheckTarget::Session | CheckTarget::Group(_) => Vec::new(),
        };
        Check {
            code: CheckCode::ThinOverlap,
            severity: Severity::Problem,
            panels,
            groups: groups.iter().map(|g| g.to_string()).collect(),
            message: "x".into(),
            remedy: None,
            target,
            rect: None,
        }
    }

    /// A two-group session (Ha's four panels first), its panels, and a
    /// layout placing Ha's first three (ids 0–2 in Ha's analysis); the
    /// fourth is not analysed.
    fn fixture(tag: &str) -> (PathBuf, OpenSession, Vec<PathBuf>, Layout) {
        let root = temp_dir(tag);
        let s = two_group_session(&root);
        let p: Vec<PathBuf> = s
            .mosaic
            .config
            .panels
            .iter()
            .map(|e| e.path.clone())
            .collect();
        let mut layout = Layout::default();
        layout.rects.insert(p[0].clone(), [0.0, 0.0, 100.0, 80.0]);
        layout.rects.insert(p[1].clone(), [60.0, 0.0, 160.0, 80.0]);
        layout.rects.insert(p[2].clone(), [200.0, 0.0, 300.0, 80.0]);
        layout.ids.insert(
            "Ha".into(),
            BTreeMap::from([(p[0].clone(), 0), (p[1].clone(), 1), (p[2].clone(), 2)]),
        );
        (root, s, p, layout)
    }

    #[test]
    fn target_rect_places_each_kind_of_target() {
        let (root, s, p, layout) = fixture("marks-target-rect");
        let none = Seams::new();
        let rect =
            |seams: &Seams, t: CheckTarget| target_rect(&s, &layout, seams, &problem(t, &["Ha"]));
        assert_eq!(
            rect(&none, CheckTarget::Panel(p[0].clone())),
            Some([0, 0, 100, 80])
        );
        // An overlap: the intersection, whichever way round.
        let overlap = Some([60, 0, 100, 80]);
        assert_eq!(
            rect(&none, CheckTarget::Overlap(p[1].clone(), p[0].clone())),
            overlap
        );
        assert_eq!(
            rect(&none, CheckTarget::Overlap(p[0].clone(), p[2].clone())),
            None,
            "apart"
        );
        // A seam: its lines' bbox (one seam in two lines), else its overlap.
        let seam = CheckTarget::Seam {
            group: "Ha".into(),
            a: p[1].clone(),
            b: p[0].clone(),
        };
        assert_eq!(rect(&none, seam.clone()), overlap);
        let line = |points: Vec<[f64; 2]>| SeamLine {
            a: 0,
            b: 1,
            points,
            flagged: true,
        };
        let seams = Seams::from([(
            "Ha".to_string(),
            vec![
                line(vec![[70.5, 10.0], [70.5, 30.0]]),
                line(vec![[72.0, 50.0], [90.0, 60.25]]),
                SeamLine {
                    a: 1,
                    b: 2,
                    points: vec![[0.0, 0.0], [300.0, 80.0]],
                    flagged: false,
                },
            ],
        )]);
        assert_eq!(rect(&seams, seam), Some([70, 10, 90, 61]));
        // A seam of a group the session no longer has places nothing.
        let gone = CheckTarget::Seam {
            group: "SII".into(),
            a: p[0].clone(),
            b: p[1].clone(),
        };
        assert_eq!(rect(&seams, gone), None);
        // An area: its rect; a group-wide check without one: nothing.
        let mut hole = problem(CheckTarget::Group("Ha".into()), &["Ha"]);
        hole.rect = Some([5, 6, 7, 8]);
        assert_eq!(target_rect(&s, &layout, &none, &hole), Some([5, 6, 7, 8]));
        assert_eq!(rect(&none, CheckTarget::Group("Ha".into())), None);
        assert_eq!(rect(&none, CheckTarget::Session), None);
        // A panel the session no longer has, or never analysed: nothing.
        assert_eq!(rect(&none, CheckTarget::Panel("gone.xisf".into())), None);
        assert_eq!(rect(&none, CheckTarget::Panel(p[3].clone())), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn marks_gather_the_shown_groups_unaccepted_problems_by_place() {
        let (root, mut s, p, layout) = fixture("marks-gather");
        let panel = |i: usize, groups: &[&str]| problem(CheckTarget::Panel(p[i].clone()), groups);
        let mut note = panel(0, &["Ha"]);
        note.severity = Severity::Note;
        let accepted = panel(2, &["Ha"]);
        s.mosaic.config.accepted_checks.extend(accepted.keys());
        let report = ChecksReport {
            checks: vec![
                panel(0, &["Ha"]),
                panel(0, &[]),
                note,
                accepted,
                panel(1, &["OIII"]),
                problem(CheckTarget::Overlap(p[1].clone(), p[0].clone()), &["Ha"]),
                problem(CheckTarget::Overlap(p[0].clone(), p[1].clone()), &["Ha"]),
                problem(CheckTarget::Group("Ha".into()), &["Ha"]),
            ],
            errors: vec!["unreadable".into()],
            seams: Seams::new(),
        };
        let groups = ["Ha".to_string()];
        let marks = problem_marks(&s, &layout, Some(&report), &groups);
        let got: Vec<(Place, usize, Option<[f64; 4]>)> = marks
            .iter()
            .map(|m| (m.place.clone(), m.problems.len(), m.rect))
            .collect();
        assert_eq!(
            got,
            [
                (Place::Panel(p[0].clone()), 2, Some([0.0, 0.0, 100.0, 80.0])),
                (
                    Place::Overlap(
                        p[0].clone().min(p[1].clone()),
                        p[0].clone().max(p[1].clone())
                    ),
                    2,
                    Some([60.0, 0.0, 100.0, 80.0])
                ),
            ]
        );
        let b = marks[0].placed_badge().unwrap();
        assert_eq!(
            (b.at, b.count, b.description.as_str()),
            ([100.0, 0.0], 2, "2 problems here")
        );
        let b = marks[1].placed_badge().unwrap();
        assert_eq!(
            (b.at, b.description.as_str()),
            ([80.0, 40.0], "2 problems here")
        );
        // The same place keeps its id.
        let again = problem_marks(&s, &layout, Some(&report), &groups);
        assert_eq!(again[0].id, marks[0].id);
        assert_ne!(marks[0].id, marks[1].id);
        // Chips: every unaccepted problem of the group, placed or not.
        assert_eq!(problem_count(&s, &report, "Ha"), 5);
        assert_eq!(problem_count(&s, &report, "OIII"), 1);
        assert!(problem_marks(&s, &layout, None, &groups).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pair_has_one_badge_for_its_overlap_and_its_seam() {
        let (root, s, p, layout) = fixture("marks-pair");
        let seam_of = |a: &PathBuf, b: &PathBuf| {
            let mut c = problem(
                CheckTarget::Seam {
                    group: "Ha".into(),
                    a: a.clone(),
                    b: b.clone(),
                },
                &["Ha"],
            );
            c.code = CheckCode::ResidualSeam;
            c
        };
        let thin = problem(CheckTarget::Overlap(p[1].clone(), p[0].clone()), &["Ha"]);
        let shows = seam_of(&p[0], &p[1]);
        // Lines off the overlap's centre, and a seam of two panels that do
        // not overlap (its badge falls back to its lines).
        let lines = |a: u16, b: u16, points: Vec<[f64; 2]>| SeamLine {
            a,
            b,
            points,
            flagged: true,
        };
        let seams = Seams::from([(
            "Ha".to_string(),
            vec![
                lines(0, 1, vec![[70.5, 10.0], [70.5, 30.0]]),
                lines(1, 2, vec![[170.0, 10.0], [170.0, 50.0]]),
            ],
        )]);
        let report = |checks: Vec<Check>| ChecksReport {
            checks,
            errors: Vec::new(),
            seams: seams.clone(),
        };
        let groups = ["Ha".to_string()];
        let both = report(vec![thin.clone(), shows.clone()]);
        let marks = problem_marks(&s, &layout, Some(&both), &groups);
        assert_eq!(marks.len(), 1, "one badge for the pair: {marks:?}");
        let b = marks[0].placed_badge().unwrap();
        assert_eq!(
            (b.at, b.count),
            ([80.0, 40.0], 2),
            "at the overlap's centre"
        );
        assert_eq!(
            marks[0].problems,
            [thin.clone(), shows.clone()],
            "in report order"
        );
        // The same id whichever of the pair's problems it has.
        for alone in [thin.clone(), shows.clone()] {
            let one = problem_marks(&s, &layout, Some(&report(vec![alone])), &groups);
            assert_eq!(one[0].id, marks[0].id);
            assert_eq!(one[0].placed_badge().unwrap().at, [80.0, 40.0]);
        }
        // Going to the seam still frames its lines.
        assert_eq!(
            target_rect(&s, &layout, &seams, &shows),
            Some([70, 10, 71, 30])
        );
        // The seam's line warns for its own problem only.
        let looks = |r: &ChecksReport| {
            let marks = problem_marks(&s, &layout, Some(r), &groups);
            seam_lines(&layout, Some(r), &groups, &marks)
                .iter()
                .map(|l| (l.points[0], l.color))
                .collect::<Vec<_>>()
        };
        let (warning, neutral) = (Tone::Warning.color(), Tone::Neutral.color());
        assert!(looks(&both).contains(&([70.5, 10.0], warning)));
        let thin_only = report(vec![thin]);
        assert!(looks(&thin_only).contains(&([70.5, 10.0], neutral)));
        // No overlap to place it at: the seam's lines' centre.
        let apart = report(vec![seam_of(&p[2], &p[1])]);
        let marks = problem_marks(&s, &layout, Some(&apart), &groups);
        assert_eq!(marks[0].placed_badge().unwrap().at, [170.0, 30.0]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seam_lines_warn_where_a_seam_is_badged() {
        let (root, mut s, p, layout) = fixture("marks-seams");
        let line = |a: u16, b: u16| SeamLine {
            a,
            b,
            points: vec![[f64::from(a), 0.0], [f64::from(b), 9.0]],
            flagged: true,
        };
        let shows = problem(
            CheckTarget::Seam {
                group: "Ha".into(),
                a: p[1].clone(),
                b: p[0].clone(),
            },
            &["Ha"],
        );
        let report = ChecksReport {
            checks: vec![shows.clone()],
            errors: Vec::new(),
            seams: Seams::from([
                ("Ha".to_string(), vec![line(0, 1), line(1, 2), line(0, 1)]),
                ("OIII".to_string(), vec![line(0, 1)]),
            ]),
        };
        let groups = ["Ha".to_string()];
        let marks = problem_marks(&s, &layout, Some(&report), &groups);
        let lines = seam_lines(&layout, Some(&report), &groups, &marks);
        let looks: Vec<(Color32, f32)> = lines.iter().map(|l| (l.color, l.width)).collect();
        let warning = (Tone::Warning.color(), LINE_WIDTH_STRONG);
        let neutral = (Tone::Neutral.color(), LINE_WIDTH);
        // Both lines of the badged seam warn, drawn last.
        assert_eq!(looks, [neutral, warning, warning]);
        assert_eq!(lines[0].points, line(1, 2).points);
        // Accepted: neutral like the rest.
        s.mosaic.config.accepted_checks.extend(shows.keys());
        let marks = problem_marks(&s, &layout, Some(&report), &groups);
        let lines = seam_lines(&layout, Some(&report), &groups, &marks);
        assert!(lines.iter().all(|l| (l.color, l.width) == neutral));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mosaic_outlines_number_the_view_and_flag_badged_panels() {
        let (root, s, p, layout) = fixture("marks-outlines");
        let marks = vec![ProblemMark {
            id: 1,
            place: Place::Panel(p[1].clone()),
            rect: None,
            problems: Vec::new(),
        }];
        let groups = ["Ha".to_string()];
        let outlines = panel_outlines(&s, &layout, &groups, &marks, true);
        let got: Vec<(&str, [f64; 4], Color32)> = outlines
            .iter()
            .map(|o| (o.label.as_str(), o.rect, o.color))
            .collect();
        // The fourth panel, not analysed, has no outline.
        let rect = |i: usize| layout.rects[&p[i]];
        assert_eq!(
            got,
            [
                ("1", rect(0), outline_color(false)),
                ("2", rect(1), outline_color(true)),
                ("3", rect(2), outline_color(false)),
            ]
        );
        let plain = panel_outlines(&s, &layout, &groups, &marks, false);
        assert!(plain.len() == 3 && plain.iter().all(|o| o.label.is_empty()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
