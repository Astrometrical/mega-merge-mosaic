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
}

/// A check result's identity: the check and, when panel-specific, the panel.
/// Used to remember which warnings the user has accepted.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckKey {
    /// Which check.
    pub code: CheckCode,
    /// The panel concerned, if any.
    pub panel: Option<PathBuf>,
}

/// One check result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Check {
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
    /// The identities of this result: one key per panel, or one panel-less
    /// key when it names no panels.
    pub fn keys(&self) -> Vec<CheckKey> {
        if self.panels.is_empty() {
            return vec![CheckKey {
                code: self.code,
                panel: None,
            }];
        }
        self.panels
            .iter()
            .map(|p| CheckKey {
                code: self.code,
                panel: Some(p.clone()),
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

/// Checks over the panels' headers and their grouping.
pub fn input_checks(cfg: &MosaicConfig, facts: &[PanelFacts], groups: &Groups) -> Vec<Check> {
    let mut out = Vec::new();
    let find = |p: &Path| facts.iter().find(|f| f.path == p);

    for f in facts {
        let name = file_name(&f.path);
        if f.solution == SolutionKind::None {
            out.push(Check {
                code: CheckCode::SolutionPresent,
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
            severity: Severity::Passed,
            panels: vec![f.path.clone()],
            groups: vec![],
            message: format!("{name} has an astrometric solution."),
            remedy: None,
        });
        if f.solution == SolutionKind::Linear {
            out.push(Check {
                code: CheckCode::DistortionModel,
                severity: Severity::Problem,
                panels: vec![f.path.clone()],
                groups: vec![],
                message: format!("{name}: missing distortion model."),
                remedy: Some("Re-solve the panel with distortion correction enabled.".into()),
            });
        } else {
            out.push(Check {
                code: CheckCode::DistortionModel,
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

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mmm-checks-{tag}-{}", std::process::id()));
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
        input_checks(cfg, &facts_of(paths), &groups)
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
        let checks = input_checks(&cfg, &facts_of(&paths), &groups);
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
        };
        let mut cfg = MosaicConfig::new(vec![]);
        assert!(!is_accepted(&cfg, &check));
        cfg.accepted_checks.push(CheckKey {
            code: CheckCode::MixedChannels,
            panel: Some(a),
        });
        assert!(!is_accepted(&cfg, &check));
        cfg.accepted_checks.push(CheckKey {
            code: CheckCode::MixedChannels,
            panel: Some(b),
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
                | CheckCode::SkyLevelMismatch => {}
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
    }
}
