//! Mosaic-session commands: `init`, `set`, `check` and `run` over a mosaic
//! directory (`mosaic.json`, see [`mmm_core::mosaic`]), plus the guard that
//! keeps the legacy single-session commands off such a directory.

use std::path::{Path, PathBuf};

use anyhow::Context;
use mmm_core::checks::{
    Check, Severity, analysis_checks, file_checks, input_checks, is_accepted, panel_facts,
};
use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::grouping::Groups;
use mmm_core::mosaic::{
    CONFIG_FILE, FRAME_FILE, GROUPS_DIR, GroupRule, Grouping, Mosaic, MosaicConfig, analyze_all,
    is_mosaic_dir, set_analyze_settings, set_blend_settings, set_group_override,
};
use mmm_core::overlap::OverlapGraph;
use mmm_core::session::Session;
use mmm_core::settings::{AnalyzeOverride, BlendOverride, ChangeImpact, GroupOverride};

use crate::{
    BlendConfig, blend_cmd, parse_defect_veto, parse_flatten, parse_gain, parse_input, parse_mode,
    parse_surface, parse_wcs_frame, print_outputs_summary,
};

/// Analyze and blend settings of a mosaic session. Unlike the `analyze` /
/// `blend` flags these have no defaults: a flag not given leaves its
/// setting unchanged.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct SettingsOpts {
    /// Residual surface correction: off, 0 (constant), 1 (plane), 2 (quadratic)
    #[arg(long)]
    surface: Option<String>,

    /// Input kind: auto (detect), aligned (registered full-canvas
    /// frames), solved (unaligned panels with astrometric solutions)
    #[arg(long)]
    input: Option<String>,

    /// Photometric gain handling: fit (measure per-panel gains from
    /// overlaps), unity (pin every gain at 1, match levels with offsets
    /// only — for same-rig/same-exposure mosaics)
    #[arg(long)]
    gain: Option<String>,

    /// Blend mode: pyramid (multiband base + star-safe seams),
    /// twoband (feathered base + star-safe seams) or feather (phase-1)
    #[arg(long)]
    mode: Option<String>,

    /// Feather ramp length in canvas pixels
    #[arg(long)]
    feather: Option<f32>,

    /// Cross-panel defect veto in overlaps (twoband mode): on or off
    #[arg(long)]
    defect_veto: Option<String>,

    /// Opt-in global background flatten: off, 1 (plane) or 2 (quadratic)
    #[arg(long)]
    flatten: Option<String>,
}

impl SettingsOpts {
    /// The flags given, as an override: set fields are the flags passed.
    fn resolve(&self) -> anyhow::Result<GroupOverride> {
        Ok(GroupOverride {
            analyze: AnalyzeOverride {
                input: self.input.as_deref().map(parse_input).transpose()?,
                surface: self.surface.as_deref().map(parse_surface).transpose()?,
                gain: self.gain.as_deref().map(parse_gain).transpose()?,
            },
            blend: BlendOverride {
                mode: self.mode.as_deref().map(parse_mode).transpose()?,
                feather_px: self.feather,
                defect_veto: self
                    .defect_veto
                    .as_deref()
                    .map(parse_defect_veto)
                    .transpose()?,
                flatten: self.flatten.as_deref().map(parse_flatten).transpose()?,
            },
        })
    }
}

/// `mmm init` arguments.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct InitArgs {
    /// Input panel files (XISF or FITS)
    #[arg(required = true)]
    panels: Vec<PathBuf>,

    /// Mosaic directory to create (holds mosaic.json and all cached analysis)
    #[arg(short, long)]
    pub(crate) session: PathBuf,

    /// Split the panels into groups, blended separately on one shared
    /// frame: header:KEY (by a header keyword's value, e.g. header:FILTER)
    /// or token:KEYWORD (by the file-name token after KEYWORD, e.g.
    /// token:FILTER for "..._FILTER-Ha_...")
    #[arg(long, value_name = "RULE")]
    group_by: Option<String>,

    /// Replace a mosaic already in the directory, discarding its cached
    /// group analysis
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    settings: SettingsOpts,
}

/// `mmm set` arguments.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct SetArgs {
    /// Mosaic directory created by `mmm init`
    #[arg(short, long)]
    pub(crate) session: PathBuf,

    /// Set an override for this group only (instead of mosaic-wide)
    #[arg(long, value_name = "NAME")]
    group: Option<String>,

    #[command(flatten)]
    settings: SettingsOpts,
}

/// `mmm check` arguments.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct CheckArgs {
    /// Mosaic directory created by `mmm init`
    #[arg(short, long)]
    pub(crate) session: PathBuf,
}

/// `mmm run` arguments.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct RunArgs {
    /// Mosaic directory created by `mmm init`
    #[arg(short, long)]
    pub(crate) session: PathBuf,

    /// Directory for the per-group output FITS files (<group dir>.fits)
    #[arg(short, long)]
    output: PathBuf,

    /// Also write an autostretched PNG preview per group (<group dir>.png)
    #[arg(long)]
    png: bool,

    /// WCS card convention: topdown (PI display-space, default) or
    /// flipped (reflected bottom-up) for readers that mirror annotations
    #[arg(long, default_value = "topdown")]
    wcs_frame: String,
}

/// Fail with directions when `session` is a mosaic directory: the legacy
/// single-session commands (`analyze`, `blend`, `report`) do not apply.
pub(crate) fn refuse_mosaic_dir(session: &Path) -> anyhow::Result<()> {
    if is_mosaic_dir(session) {
        let s = session.display();
        anyhow::bail!(
            "{s} is a mosaic session ({CONFIG_FILE}), not an analyze session: \
             use `mmm run -s {s} -o OUTDIR` to analyze and blend it, \
             and `mmm check -s {s}` to review it"
        );
    }
    Ok(())
}

fn open_mosaic(dir: &Path) -> anyhow::Result<Mosaic> {
    anyhow::ensure!(
        is_mosaic_dir(dir),
        "{} is not a mosaic session (no {CONFIG_FILE}) — create one with `mmm init`",
        dir.display()
    );
    Ok(Mosaic::open(dir)?)
}

fn parse_group_by(rule: &str) -> anyhow::Result<GroupRule> {
    match rule.split_once(':') {
        Some(("header", key)) if !key.trim().is_empty() => Ok(GroupRule::Header {
            key: key.trim().to_string(),
        }),
        Some(("token", keyword)) if !keyword.trim().is_empty() => Ok(GroupRule::FilenameToken {
            keyword: keyword.trim().to_string(),
        }),
        _ => anyhow::bail!("--group-by must be header:KEY or token:KEYWORD (got {rule})"),
    }
}

fn describe_grouping(g: &Grouping) -> String {
    match (&g.enabled, &g.rule) {
        (false, _) => "grouping off".to_string(),
        (true, GroupRule::Header { key }) => format!("grouped by header {key}"),
        (true, GroupRule::FilenameToken { keyword }) => {
            format!("grouped by file-name token after {keyword}")
        }
    }
}

/// Print the mosaic's headline and its processing groups with panel counts.
fn print_groups(m: &Mosaic, groups: &Groups) {
    println!(
        "mosaic: {} panels, {}   session: {}",
        m.config.panels.len(),
        describe_grouping(&m.config.grouping),
        m.dir.display()
    );
    let pg = groups.processing_groups();
    println!("groups ({}):", pg.len());
    for (name, panels) in &pg {
        println!("  {name}: {} panels", panels.len());
    }
}

/// `mmm init`: create a mosaic session.
pub(crate) fn init(a: &InitArgs) -> anyhow::Result<()> {
    // Validate every flag before touching the disk.
    let rule = a.group_by.as_deref().map(parse_group_by).transpose()?;
    let edit = a.settings.resolve()?;
    if is_mosaic_dir(&a.session) {
        anyhow::ensure!(
            a.force,
            "{} already holds a mosaic ({CONFIG_FILE}); pass --force to replace it",
            a.session.display()
        );
        // The old groups' sessions and results belong to the old panel set.
        for stale in [a.session.join(GROUPS_DIR), a.session.join(FRAME_FILE)] {
            let res = if stale.is_dir() {
                std::fs::remove_dir_all(&stale)
            } else {
                std::fs::remove_file(&stale)
            };
            match res {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("cannot remove {}", stale.display()));
                }
                _ => {}
            }
        }
    }
    // Absolute paths: the session must not depend on the current directory.
    let panels = a
        .panels
        .iter()
        .map(|p| std::path::absolute(p).with_context(|| format!("bad path {}", p.display())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut cfg = MosaicConfig::new(panels);
    if let Some(rule) = rule {
        cfg.grouping = Grouping {
            enabled: true,
            rule,
        };
    }
    cfg.analyze = cfg.analyze.with(&edit.analyze);
    cfg.blend = cfg.blend.with(&edit.blend);
    let m = Mosaic::create(&a.session, cfg)?;
    print_groups(&m, &m.groups()?);
    let s = m.dir.display();
    println!("next: `mmm check -s {s}`, then `mmm run -s {s} -o OUTDIR`");
    Ok(())
}

/// Copy every field set in `edit` into `o`.
fn merge_override(o: &mut GroupOverride, edit: &GroupOverride) {
    let (a, ea) = (&mut o.analyze, &edit.analyze);
    a.input = ea.input.or(a.input);
    a.surface = ea.surface.or(a.surface);
    a.gain = ea.gain.or(a.gain);
    let (b, eb) = (&mut o.blend, &edit.blend);
    b.mode = eb.mode.or(b.mode);
    b.feather_px = eb.feather_px.or(b.feather_px);
    b.defect_veto = eb.defect_veto.or(b.defect_veto);
    b.flatten = eb.flatten.or(b.flatten);
}

/// `mmm set`: change mosaic-wide settings or one group's override.
pub(crate) fn set(a: &SetArgs) -> anyhow::Result<()> {
    let edit = a.settings.resolve()?;
    anyhow::ensure!(
        !edit.is_empty(),
        "nothing to set: pass at least one of --surface, --input, --gain, --mode, \
         --feather, --defect-veto, --flatten"
    );
    let mut m = open_mosaic(&a.session)?;
    let (scope, impact) = match &a.group {
        Some(group) => {
            let names: Vec<String> = m
                .groups()?
                .processing_groups()
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            anyhow::ensure!(
                names.contains(group),
                "no group '{group}' in this mosaic (groups: {})",
                names.join(", ")
            );
            let mut o = m
                .config
                .group_overrides
                .get(group)
                .cloned()
                .unwrap_or_default();
            merge_override(&mut o, &edit);
            (
                format!("group {group}"),
                set_group_override(&mut m, group, o)?,
            )
        }
        None => {
            let analyze = m.config.analyze.with(&edit.analyze);
            let blend = m.config.blend.with(&edit.blend);
            let ia = set_analyze_settings(&mut m, analyze)?;
            let ib = set_blend_settings(&mut m, blend)?;
            ("mosaic-wide".to_string(), ia.max(ib))
        }
    };
    let what = match impact {
        ChangeImpact::None => "none — nothing changed",
        ChangeImpact::Reblend => "reblend — analysis is kept; the next `mmm run` reblends",
        ChangeImpact::Reanalyze => "reanalyze — the next `mmm run` reruns analysis",
    };
    println!("{scope}: impact {what}");
    Ok(())
}

fn print_check(c: &Check, accepted: bool) {
    let mark = if accepted { "  (accepted)" } else { "" };
    let groups = if c.groups.is_empty() {
        String::new()
    } else {
        format!("[{}] ", c.groups.join(", "))
    };
    println!("  - {groups}{}{mark}", c.message);
    if let Some(r) = &c.remedy {
        println!("      {r}");
    }
}

/// `mmm check`: the groups, then every check by severity.
pub(crate) fn check(a: &CheckArgs) -> anyhow::Result<()> {
    let m = open_mosaic(&a.session)?;
    let groups = m.groups()?;
    print_groups(&m, &groups);

    let mut facts = Vec::new();
    for p in &m.config.panels {
        match panel_facts(&p.path) {
            Ok(f) => facts.push(f),
            // A missing file is reported by the file checks below.
            Err(e) if p.path.exists() => println!("cannot read {}: {e}", p.path.display()),
            Err(_) => {}
        }
    }
    let mut checks = input_checks(&m.config, &facts, &groups);
    checks.extend(file_checks(&m.config));
    for (group, _) in groups.processing_groups() {
        if !m.config.group_dirs.contains_key(&group) {
            continue;
        }
        let dir = m.group_session_dir(&group);
        if !dir.join("session.json").is_file() {
            continue;
        }
        let session = Session::open(&dir)?;
        let graph = OverlapGraph::load(&session.overlap_graph_path())
            .with_context(|| format!("group {group}: cannot load its analysis"))?;
        checks.extend(analysis_checks(&group, &session, &graph));
    }

    for (severity, title) in [(Severity::Problem, "problems"), (Severity::Note, "notes")] {
        let of: Vec<&Check> = checks.iter().filter(|c| c.severity == severity).collect();
        println!("\n{title} ({}):", of.len());
        for c in of {
            print_check(c, is_accepted(&m.config, c));
        }
    }
    let passed = checks
        .iter()
        .filter(|c| c.severity == Severity::Passed)
        .count();
    println!("\npassed: {passed} checks");
    Ok(())
}

/// `mmm run`: analyze every group, then blend each with the streaming
/// `blend` path to `OUTDIR/<group dir>.fits`.
pub(crate) fn run(a: &RunArgs) -> anyhow::Result<()> {
    let wcs_flip = parse_wcs_frame(&a.wcs_frame)?;
    let mut m = open_mosaic(&a.session)?;
    std::fs::create_dir_all(&a.output)
        .with_context(|| format!("cannot create {}", a.output.display()))?;

    println!("== analyze ({} panels)", m.config.panels.len());
    let progress = |stage: &str, done: u64, total: u64| println!("  {stage} {done}/{total}");
    let ctl = JobCtl::new(CancelToken::new(), Some(&progress));
    let groups = analyze_all(&mut m, &ctl)?;
    let processing = groups.processing_groups();
    let frame_path = m.dir.join(FRAME_FILE);
    let reference = mmm_core::reference::ReferenceFrame::load(&frame_path)?;
    println!(
        "reference: {} over {} panels in {} groups → {}",
        reference.describe(),
        m.config.panels.len(),
        processing.len(),
        frame_path.display()
    );

    let mut outputs = Vec::with_capacity(processing.len());
    for (name, _) in &processing {
        println!("\n== group {name}: blend");
        let b = m.config.settings_for(name).1;
        let cfg = BlendConfig {
            downsample: 1,
            feather: b.feather_px,
            mode: b.mode,
            roi: None,
            defect_veto: b.defect_veto,
            flatten: b.flatten,
            wcs_flip,
            extent: None,
        };
        let file = &m.config.group_dirs[name];
        let out = a.output.join(format!("{file}.fits"));
        let png = a.png.then(|| a.output.join(format!("{file}.png")));
        blend_cmd(&m.group_session_dir(name), &out, png.as_deref(), &cfg)
            .with_context(|| format!("group {name}: blend failed"))?;
        outputs.push((name.clone(), out));
    }
    print_outputs_summary("run", &reference, &outputs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_by_parses_both_rules_and_rejects_others() {
        assert_eq!(
            parse_group_by("header:FILTER").unwrap(),
            GroupRule::Header {
                key: "FILTER".into()
            }
        );
        assert_eq!(
            parse_group_by("token:FILTER").unwrap(),
            GroupRule::FilenameToken {
                keyword: "FILTER".into()
            }
        );
        for bad in ["FILTER", "header:", "name:FILTER"] {
            assert!(parse_group_by(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn merge_keeps_unset_fields_of_the_existing_override() {
        let mut o = GroupOverride {
            blend: BlendOverride {
                feather_px: Some(50.0),
                ..Default::default()
            },
            ..Default::default()
        };
        let edit = GroupOverride {
            blend: BlendOverride {
                mode: Some(mmm_core::blend::BlendMode::Feather),
                ..Default::default()
            },
            analyze: AnalyzeOverride {
                surface: Some(None),
                ..Default::default()
            },
        };
        merge_override(&mut o, &edit);
        assert_eq!(o.blend.feather_px, Some(50.0));
        assert_eq!(o.blend.mode, Some(mmm_core::blend::BlendMode::Feather));
        assert_eq!(o.analyze.surface, Some(None));
        assert_eq!(o.analyze.gain, None);
    }
}
