//! Assigning panels to groups: header and file-name rules, manual overrides,
//! and safe directory names for the groups.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::{GroupRule, MosaicConfig, SINGLE_GROUP, UNGROUPED};
use crate::formats::InputPanel;

/// The token after `keyword` in the file stem split on `_`. The keyword
/// matches case-insensitively; the next non-empty token is returned.
pub fn token_after(file_name: &str, keyword: &str) -> Option<String> {
    let stem = Path::new(file_name).file_stem()?.to_str()?;
    let mut tokens = stem.split('_');
    tokens.find(|t| t.eq_ignore_ascii_case(keyword))?;
    tokens.find(|t| !t.is_empty()).map(str::to_string)
}

/// The value of header keyword `key` in `panel`. `FILTER` goes through
/// [`InputPanel::filter_name`]; other keys come from the FITS keywords with
/// quotes and whitespace trimmed. Empty values give `None`.
pub fn header_value(panel: &InputPanel, key: &str) -> Option<String> {
    if key.eq_ignore_ascii_case("FILTER") {
        return panel.filter_name();
    }
    let k = panel
        .fits_keywords()
        .iter()
        .find(|k| k.name.eq_ignore_ascii_case(key))?;
    let v = k.value.trim().trim_matches('\'').trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Panels split into named groups, plus those no rule could place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Groups {
    /// Group name to its panels, in configuration order.
    pub groups: BTreeMap<String, Vec<PathBuf>>,
    /// Panels that belong to no group.
    pub ungrouped: Vec<PathBuf>,
}

impl Groups {
    /// The groups to process: every named group, then [`UNGROUPED`] when
    /// non-empty (ungrouped panels are processed as their own group). Names
    /// are never duplicated: if a manual group is itself named [`UNGROUPED`],
    /// the ungrouped panels are merged into it.
    pub fn processing_groups(&self) -> Vec<(String, Vec<PathBuf>)> {
        let mut out: Vec<_> = self
            .groups
            .iter()
            .map(|(n, p)| (n.clone(), p.clone()))
            .collect();
        if !self.ungrouped.is_empty() {
            match out.iter_mut().find(|(n, _)| n == UNGROUPED) {
                Some((_, panels)) => panels.extend(self.ungrouped.iter().cloned()),
                None => out.push((UNGROUPED.to_string(), self.ungrouped.clone())),
            }
        }
        out
    }
}

/// Split the configuration's panels into groups. `header` looks up a
/// panel's header value (for [`GroupRule::Header`]). With grouping off every
/// panel is in [`SINGLE_GROUP`]; with it on, a manual group beats the rule.
pub fn resolve_groups(cfg: &MosaicConfig, header: &dyn Fn(&Path) -> Option<String>) -> Groups {
    let mut out = Groups::default();
    for p in &cfg.panels {
        if !cfg.grouping.enabled {
            out.groups
                .entry(SINGLE_GROUP.to_string())
                .or_default()
                .push(p.path.clone());
            continue;
        }
        let name = p.manual_group.clone().or_else(|| match &cfg.grouping.rule {
            GroupRule::Header { key } => header(&p.path).filter(|_| !key.is_empty()),
            GroupRule::FilenameToken { keyword } => p
                .path
                .file_name()
                .and_then(|f| f.to_str())
                .and_then(|f| token_after(f, keyword)),
        });
        match name {
            Some(n) => out.groups.entry(n).or_default().push(p.path.clone()),
            None => out.ungrouped.push(p.path.clone()),
        }
    }
    out
}

/// Windows reserved device names (with or without an extension).
fn is_reserved_device_name(s: &str) -> bool {
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Map each group name to a safe, unique directory name. Characters outside
/// `[A-Za-z0-9._-]` become `_`; empty or all-dot results become `group`; a
/// leading or trailing `.` becomes `_`; Windows reserved device names get a
/// trailing `_`. Uniqueness is case-insensitive (case-folding filesystems):
/// collisions get `-2`, `-3`, … in name order. Deterministic.
pub fn group_dirs(names: &[String]) -> BTreeMap<String, String> {
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort();
    sorted.dedup();
    let mut used = BTreeSet::new();
    let mut out = BTreeMap::new();
    for name in sorted {
        let mut base: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if base.chars().all(|c| c == '.') {
            base = "group".into();
        }
        if base.starts_with('.') {
            base.replace_range(..1, "_");
        }
        if base.ends_with('.') {
            base.pop();
            base.push('_');
        }
        if is_reserved_device_name(&base) {
            base.push('_');
        }
        let mut cand = base.clone();
        let mut n = 2;
        while used.contains(&cand.to_ascii_lowercase()) {
            cand = format!("{base}-{n}");
            n += 1;
        }
        used.insert(cand.to_ascii_lowercase());
        out.insert(name.clone(), cand);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mosaic::Grouping;

    fn cfg(paths: &[&str]) -> MosaicConfig {
        MosaicConfig::new(paths.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn token_after_examples() {
        assert_eq!(
            token_after("P11_filter_Ha_int.xisf", "filter").as_deref(),
            Some("Ha")
        );
        assert_eq!(
            token_after("P11_filter_Ha_int.xisf", "FILTER").as_deref(),
            Some("Ha")
        );
        assert_eq!(token_after("P11_filter_Ha_int.xisf", "nope"), None);
        assert_eq!(token_after("x_filter_.xisf", "filter"), None);
    }

    #[test]
    fn grouping_off_is_single_group() {
        let c = cfg(&["a.xisf", "b.xisf"]);
        let g = resolve_groups(&c, &|_| Some("Ha".into()));
        assert_eq!(g.groups.len(), 1);
        assert_eq!(g.groups[SINGLE_GROUP].len(), 2);
        assert!(g.ungrouped.is_empty());
    }

    #[test]
    fn manual_beats_rule_and_ungrouped_collects() {
        let mut c = cfg(&["a.xisf", "b.xisf", "c.xisf"]);
        c.grouping = Grouping {
            enabled: true,
            rule: GroupRule::default(),
        };
        c.panels[1].manual_group = Some("Manual".into());
        let header = |p: &Path| match p.to_str().unwrap() {
            "a.xisf" | "b.xisf" => Some("Ha".to_string()),
            _ => None,
        };
        let g = resolve_groups(&c, &header);
        assert_eq!(g.groups["Ha"], vec![PathBuf::from("a.xisf")]);
        assert_eq!(g.groups["Manual"], vec![PathBuf::from("b.xisf")]);
        assert_eq!(g.ungrouped, vec![PathBuf::from("c.xisf")]);
    }

    #[test]
    fn processing_groups_include_ungrouped_last() {
        let mut g = Groups::default();
        g.groups.insert("Z".into(), vec!["z.xisf".into()]);
        g.groups.insert("A".into(), vec!["a.xisf".into()]);
        g.ungrouped.push("u.xisf".into());
        let names: Vec<_> = g.processing_groups().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["A", "Z", UNGROUPED]);
        g.ungrouped.clear();
        assert_eq!(g.processing_groups().len(), 2);
    }

    #[test]
    fn processing_groups_never_duplicate_names() {
        let mut g = Groups::default();
        g.groups.insert(UNGROUPED.into(), vec!["m.xisf".into()]);
        g.ungrouped.push("u.xisf".into());
        let pg = g.processing_groups();
        assert_eq!(pg.len(), 1);
        assert_eq!(pg[0].1.len(), 2);
    }

    #[test]
    fn group_dirs_are_safe_and_unique() {
        let names: Vec<String> = ["Ha 3nm", "Ha_3nm", "R/G", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = group_dirs(&names);
        assert_eq!(m["Ha 3nm"], "Ha_3nm");
        assert_eq!(m["Ha_3nm"], "Ha_3nm-2");
        assert_eq!(m["R/G"], "R_G");
        assert_eq!(m[""], "group");
        let uniq: BTreeSet<_> = m.values().collect();
        assert_eq!(uniq.len(), m.len());
        assert_eq!(m, group_dirs(&names));

        let names: Vec<String> = [".", "..", "Ha", "ha", "CON", "nul.txt", "com1", "a."]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = group_dirs(&names);
        assert_eq!(m["."], "group");
        assert_eq!(m[".."], "group-2");
        assert_eq!(m["Ha"], "Ha");
        assert_eq!(m["ha"], "ha-2");
        assert_eq!(m["CON"], "CON_");
        assert_eq!(m["nul.txt"], "nul.txt_");
        assert_eq!(m["com1"], "com1_");
        assert_eq!(m["a."], "a_");
        let lower: BTreeSet<_> = m.values().map(|v| v.to_lowercase()).collect();
        assert_eq!(lower.len(), m.len());
    }

    #[test]
    fn header_value_reads_synth_filter() {
        let dir = std::env::temp_dir().join(format!("mmm-grouping-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("p.xisf");
        crate::synth::write_xisf_with_header_xml(
            &p,
            2,
            2,
            1,
            &[0.5; 4],
            r#"<FITSKeyword name="FILTER" value="'Ha  '" comment=""/><FITSKeyword name="OBJECT" value="'M42'" comment=""/><FITSKeyword name="EMPTY" value="''" comment=""/>"#,
        )
        .unwrap();
        let panel = InputPanel::open(&p).unwrap();
        assert_eq!(header_value(&panel, "FILTER").as_deref(), Some("Ha"));
        assert_eq!(header_value(&panel, "OBJECT").as_deref(), Some("M42"));
        assert_eq!(header_value(&panel, "EMPTY"), None);
        assert_eq!(header_value(&panel, "MISSING"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
