# PixInsight Multi-Filter Groups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the PixInsight module merge several named groups of panels (one per filter) onto one automatically derived reference frame, producing one co-registered output window per group, with group boundaries shown in the process console.

**Architecture:** The engine learns nothing new about groups. The wire gains an optional `InitJob.reference` (the stage-1 `ReferenceFrame` JSON) that all three worker job modes honour, plus a reference and a FILTER name in the probe replies and a new `--probe-reference` for views. The module gains a flat `group` column on both panel tables, a Filter / Set group / Group by FILTER control row, and an execution loop that derives one reference over every panel and runs one worker job per group. Group partitioning, name sanitisation and wildcard matching live in a PCL-free header so the host CTest suite can exercise them.

**Tech Stack:** Rust (mmm-core, mmm-ipc-worker; serde, rayon), C++20 host library (`nlohmann::json`, CMake/CTest), PCL 2.10.8 module (`make` in `integration/pixinsight/module`), PixInsight ≥ 1.9.5.

**Spec:** `docs/superpowers/specs/2026-10-03-pixinsight-multi-filter-groups-design.md` (stage 1: `docs/superpowers/specs/2026-10-03-shared-reference-frame-design.md`)

## Global Constraints

- Every wire addition is an optional field with `#[serde(default)]` (precedent: `seam_map`, `gain`); `IPC_PROTOCOL_VERSION` / `kProtocolVersion` stay `3`.
- Module and worker go to `1.6.0` together: workspace `Cargo.toml`, `MmmVersion.h` (`MMM_VERSION_STRING`, `MMM_VERSION_MINOR 6`), `mmm_protocol.h` (`kExpectedWorkerVersion`), HTML doc subtitle `Version 1.6.0 &mdash;`; enforced by `crates/mmm-ipc-worker/tests/version_sync.rs`.
- A single-group run (every panel in the default group) must be byte-identical to today: no reference imposed, window `MegaMergeMosaic`; the existing golden CTests must not change.
- Window ids: `MegaMergeMosaic_<sanitised>` where every character outside `[A-Za-z0-9_]` becomes `_`; the default group keeps `MegaMergeMosaic`; seam maps `seam_map` / `seam_map_<sanitised>`. Two groups sanitising to the same id are refused before any work.
- Group names are trimmed of surrounding whitespace at assignment; empty = default group; compared exactly (case-sensitive).
- "Set group", "Group by FILTER" and "Clear groups" act on the selected rows when any are selected, otherwise on every currently displayed (filtered) row.
- Console group headers are written only between stages (after `CommitLine`), never while a progress line is live; style per spec §4.
- No reference-frame file import/export in the module (spec non-goal).
- C++ string literals in the module stay pure ASCII (`pcl::String(const char*)` decodes ISO-8859-1); the `°` in frame descriptions therefore comes from the worker's `describe()` text passed through `UTF8ToUTF16`, never from a C++ literal.
- Every public Rust item documented (`missing_docs`); `cargo fmt --check`, `cargo clippy --all-targets --workspace`, `cargo doc --no-deps -p mmm-core` warning-free; module `make` warning-free (`-Wall`).
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## Review Focus

1. A process icon saved by 1.5.x (no `group` column) must load with every panel in the default group and run exactly as before — pinned in Task 6 (`AllocateParameter` resizes both arrays; test `test_groups` covers partitioning of an all-empty vector) and by the unchanged golden CTests.
2. A view in a multi-group aligned run that carries no astrometric solution must still work (geometry-only check) rather than fail on a missing WCS — pinned in Task 2 (`ipc_aligned_adopts_reference_without_wcs`).
3. Group names like `R-1` and `R_1`, or `Ha` and `ha`, which sanitise to the same or distinct window ids, must be reported before any worker spawns — pinned in Task 5 (`collision_is_detected`, `case_is_preserved`).
4. A group with a single panel in solved mode must succeed and land on the shared frame — pinned in Task 4 (`test_golden_groups`: group B has one panel).
5. The FILTER value may carry quotes and padding (`'Ha      '`) or live only in the XISF `Instrument:Filter:Name` property — pinned in Task 1 (`filter_name_strips_quotes_and_padding`, `filter_name_falls_back_to_xisf_property`).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/mmm-core/src/reference.rs` | `derive_from_descs`, shared `derive_items` |
| `crates/mmm-core/src/formats/mod.rs` | `InputPanel::filter_name()` |
| `crates/mmm-core/src/synth.rs` | `write_xisf_with_header_xml` (test writer) |
| `crates/mmm-core/src/ipc/protocol.rs` | `InitJob.reference`, `PanelProbeGeom.filter`, `PanelProbeReply.reference` |
| `crates/mmm-core/src/analyze.rs` | `analyze_ipc_aligned/solved(.., reference)`, `probe_panels` fills `filter`/`reference` |
| `crates/mmm-ipc-worker/src/main.rs` | passes `init.reference`; `--probe-reference` |
| `crates/mmm-ipc-worker/tests/end_to_end.rs`, `crates/mmm-core/tests/probe_panels.rs` | worker-level tests |
| `integration/pixinsight/host/mmm_host.{h,cpp}` | `ProbedPanel.filter`, `PanelProbeResult.reference`, `Host::probe_reference`, `reference_canvas` |
| `integration/pixinsight/host/test/test_probe_panels.cpp`, `test_golden_groups.cpp` (new), `test_groups.cpp` (new), `CMakeLists.txt` | host tests |
| `integration/pixinsight/module/MmmGroups.h` (new) | PCL-free partition / sanitise / wildcard helpers |
| `integration/pixinsight/module/MmmParameters.{h,cpp}`, `mmm.cpp` | `group` columns |
| `integration/pixinsight/module/MmmProcess.{h,cpp}` | `p_viewGroups`, `p_fileGroups`, hooks |
| `integration/pixinsight/module/MmmInterface.{h,cpp}` | Group column, Filter / Set group / Group by FILTER / Clear groups |
| `integration/pixinsight/module/ImageWindowCollector.{h,cpp}` | window id parameter |
| `integration/pixinsight/module/MmmExecution.cpp` | multi-group run loop, console headers |
| `integration/pixinsight/PROTOCOL.md`, `doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`, `docs/DESIGN.md` | docs |
| `Cargo.toml`, `MmmVersion.h`, `mmm_protocol.h` | 1.6.0 |

---

### Task 1: `derive_from_descs` and `InputPanel::filter_name`

**Files:**
- Modify: `crates/mmm-core/src/reference.rs` (`derive` refactor + new fn)
- Modify: `crates/mmm-core/src/formats/mod.rs:105-175` (new method)
- Modify: `crates/mmm-core/src/synth.rs:685` (expose a header-XML writer)
- Modify: `crates/mmm-core/tests/reference.rs` (tests)

**Interfaces:**
- Consumes: `ipc::protocol::PanelDesc { panel_id, width, height, channels, properties }`, `astrometry::{wcs_from_properties, describe_unsolved}`, `WcsModel::from_properties`.
- Produces:
  - `pub fn reference::derive_from_descs(panels: &[PanelDesc], input: InputSelect) -> Result<ReferenceFrame>`
  - `pub fn InputPanel::filter_name(&self) -> Option<String>`
  - `pub fn synth::write_xisf_with_header_xml(path: &Path, w: u64, h: u64, ch: u64, planes: &[f32], extra_xml: &str) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/mmm-core/tests/reference.rs` (add `use mmm_core::formats::{FitsKeyword, InputPanel}; use mmm_core::formats::xisf::XisfPanel; use mmm_core::ipc::protocol::PanelDesc; use mmm_core::reference::derive_from_descs; use mmm_core::synth::{write_fits, write_xisf_with_header_xml};` to the imports):

```rust
// ---------------------------------------------------------------------------
// stage 2: descriptors and FILTER names

/// Wire descriptors for `paths`, exactly as a Views-mode host builds them
/// (geometry from the header, the astrometric properties verbatim).
fn descs_for(paths: &[PathBuf]) -> Vec<PanelDesc> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let x = XisfPanel::open(p).unwrap();
            PanelDesc {
                panel_id: i as u32,
                width: x.width(),
                height: x.height(),
                channels: x.channels(),
                properties: x.header().properties.clone(),
            }
        })
        .collect()
}

#[test]
fn descriptors_derive_the_same_frame_as_files() {
    let dir = tempdir("descs");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let b = write_solved_group(&dir.join("B"), "b", (40.0, 25.0), 140);
    let all: Vec<PathBuf> = a.iter().chain(&b).cloned().collect();
    let from_files = derive(&all, InputSelect::Auto).unwrap();
    let from_descs = derive_from_descs(&descs_for(&all), InputSelect::Auto).unwrap();
    assert_eq!(from_files, from_descs);

    let reg = write_aligned_group(&dir.join("R"), "r", STAR, 10, 20);
    let from_files = derive(&reg, InputSelect::Auto).unwrap();
    let from_descs = derive_from_descs(&descs_for(&reg), InputSelect::Auto).unwrap();
    assert_eq!(from_files, from_descs);
    assert!(matches!(from_descs, ReferenceFrame::Aligned { wcs: Some(_), .. }));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn descriptors_without_solution_are_named_by_panel_id() {
    let dir = tempdir("descs-unsolved");
    let a = write_solved_group(&dir.join("A"), "a", (0.0, 0.0), 150);
    let mut descs = descs_for(&a);
    descs[1].properties.clear();
    let err = derive_from_descs(&descs, InputSelect::Solved).unwrap_err().to_string();
    assert!(err.contains("panel 1"), "{err}");
    assert!(!err.contains("panel 0"), "{err}");
    let err = derive_from_descs(&[], InputSelect::Auto).unwrap_err().to_string();
    assert!(err.contains("no input panels"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn filter_name_strips_quotes_and_padding() {
    let dir = tempdir("filter-fits");
    let path = dir.join("ha.fits");
    let card = FitsKeyword {
        name: "FILTER".into(),
        value: "'Ha      '".into(),
        comment: String::new(),
    };
    write_fits(&path, 8, 6, 1, &vec![0.2f32; 48], -32, &[card]).unwrap();
    assert_eq!(InputPanel::open(&path).unwrap().filter_name().as_deref(), Some("Ha"));

    let plain = dir.join("plain.fits");
    write_fits(&plain, 8, 6, 1, &vec![0.2f32; 48], -32, &[]).unwrap();
    assert_eq!(InputPanel::open(&plain).unwrap().filter_name(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn filter_name_falls_back_to_xisf_property() {
    let dir = tempdir("filter-xisf");
    let by_card = dir.join("card.xisf");
    write_xisf_with_header_xml(
        &by_card, 8, 6, 1, &vec![0.2f32; 48],
        r#"<FITSKeyword name="FILTER" value="'R'" comment=""/>"#,
    )
    .unwrap();
    assert_eq!(InputPanel::open(&by_card).unwrap().filter_name().as_deref(), Some("R"));

    let by_prop = dir.join("prop.xisf");
    write_xisf_with_header_xml(
        &by_prop, 8, 6, 1, &vec![0.2f32; 48],
        r#"<Property id="Instrument:Filter:Name" type="String">OIII</Property>"#,
    )
    .unwrap();
    assert_eq!(InputPanel::open(&by_prop).unwrap().filter_name().as_deref(), Some("OIII"));

    let none = dir.join("none.xisf");
    write_xisf(&none, 8, 6, 1, &vec![0.2f32; 48]).unwrap();
    assert_eq!(InputPanel::open(&none).unwrap().filter_name(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core --test reference 2>&1 | grep -E "^error" | sort -u`
Expected: unresolved `derive_from_descs`, `write_xisf_with_header_xml`, and no method `filter_name`.

- [ ] **Step 3: Implement**

In `crates/mmm-core/src/synth.rs`, directly above the private `fn write_xisf_impl(` (line 685):

```rust
/// [`write_xisf`] with arbitrary extra XML spliced into the `<Image>`
/// element — `<FITSKeyword …/>` cards or `<Property …>` elements — so tests
/// can exercise header-driven behaviour (FILTER names, custom properties)
/// without a dedicated writer per case.
pub fn write_xisf_with_header_xml(
    path: &Path,
    w: u64,
    h: u64,
    ch: u64,
    planes: &[f32],
    extra_xml: &str,
) -> Result<()> {
    write_xisf_impl(path, w, h, ch, planes, extra_xml)
}
```

(`write_xisf_impl` takes the extra XML as its last argument today — `wcs_property_xml(wcs)` in `write_xisf_solved`; match its exact parameter type, `&str` or `String`.)

In `crates/mmm-core/src/formats/mod.rs`, inside `impl InputPanel` after `linear_wcs`:

```rust
    /// The panel's filter name, for grouping UIs: the FITS `FILTER` card
    /// (either format; quotes and padding stripped), else — XISF only — the
    /// `Instrument:Filter:Name` property. `None` when neither is present or
    /// the value is empty.
    pub fn filter_name(&self) -> Option<String> {
        let clean = |s: &str| {
            let t = s.trim().trim_matches('\'').trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        if let Some(k) = self.fits_keywords().iter().find(|k| k.name == "FILTER")
            && let Some(name) = clean(&k.value)
        {
            return Some(name);
        }
        match self {
            Self::Xisf(p) => p
                .header()
                .properties
                .iter()
                .find(|pr| pr.id == "Instrument:Filter:Name")
                .and_then(|pr| match &pr.value {
                    PropertyValue::Str(s) => clean(s),
                    _ => None,
                }),
            Self::Fits(_) => None,
        }
    }
```

In `crates/mmm-core/src/reference.rs`, replace the body of `derive` with the shared item pipeline and add `derive_from_descs`:

```rust
use crate::astrometry::{describe_unsolved, wcs_from_properties};
use crate::ipc::protocol::PanelDesc;

/// One panel's header-level facts for frame derivation, from a file or a
/// wire descriptor — the two sources must never drift, so both feed
/// [`derive_items`].
struct DeriveItem {
    label: String,
    width: u64,
    height: u64,
    wcs: Option<LinearWcs>,
    model: std::result::Result<WcsModel, String>,
}

fn derive_items(items: Vec<DeriveItem>, input: InputSelect) -> Result<ReferenceFrame> {
    if items.is_empty() {
        return Err(Error::compute("no input panels given"));
    }
    let same_geometry = items
        .iter()
        .all(|i| (i.width, i.height) == (items[0].width, items[0].height));
    let aligned = match input {
        InputSelect::Aligned => true,
        InputSelect::Solved => false,
        InputSelect::Auto => items.len() >= 2 && same_geometry,
    };
    if aligned {
        if let Some(k) = items
            .iter()
            .position(|i| (i.width, i.height) != (items[0].width, items[0].height))
        {
            return Err(Error::compute(format!(
                "aligned input needs one canvas geometry, but {} is {}x{} and {} is {}x{}",
                items[0].label, items[0].width, items[0].height,
                items[k].label, items[k].width, items[k].height
            )));
        }
        let first = items.into_iter().next().expect("non-empty");
        return Ok(ReferenceFrame::Aligned {
            width: first.width,
            height: first.height,
            wcs: first.wcs,
        });
    }
    let mut models: Vec<WcsModel> = Vec::with_capacity(items.len());
    let mut errors: Vec<String> = Vec::new();
    for item in items {
        match item.model {
            Ok(m) => models.push(m),
            Err(reason) => errors.push(format!("{}: {reason}", item.label)),
        }
    }
    if !errors.is_empty() {
        return Err(Error::compute(format!(
            "solved input requires an astrometric solution in every panel:\n  {}",
            errors.join("\n  ")
        )));
    }
    Ok(ReferenceFrame::Solved {
        frame: choose_frame(&models),
    })
}

pub fn derive(paths: &[PathBuf], input: InputSelect) -> Result<ReferenceFrame> {
    if paths.is_empty() {
        return Err(Error::compute("no input panels given"));
    }
    let opened: Vec<InputPanel> = paths
        .iter()
        .map(|p| InputPanel::open(p))
        .collect::<Result<_>>()?;
    let items = opened
        .iter()
        .zip(paths)
        .map(|(x, p)| DeriveItem {
            label: p.display().to_string(),
            width: x.width(),
            height: x.height(),
            wcs: x.linear_wcs(),
            model: x.wcs_model(),
        })
        .collect();
    derive_items(items, input)
}

/// [`derive`] over wire panel descriptors (a Views-mode host's `PanelDesc`s
/// with the astrometric properties attached): the same kind rule and frame
/// choice, so a frame derived from views equals one derived from the same
/// panels saved to disk. Labels in errors are `panel <id>`.
pub fn derive_from_descs(panels: &[PanelDesc], input: InputSelect) -> Result<ReferenceFrame> {
    let items = panels
        .iter()
        .map(|p| DeriveItem {
            label: format!("panel {}", p.panel_id),
            width: p.width,
            height: p.height,
            wcs: wcs_from_properties(&p.properties),
            model: WcsModel::from_properties(&p.properties, p.width, p.height)
                .ok_or_else(|| describe_unsolved(&p.properties)),
        })
        .collect();
    derive_items(items, input)
}
```

Keep `derive`'s existing doc comment. The existing `derive_*` tests must keep passing unchanged.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mmm-core --test reference && cargo clippy -p mmm-core --all-targets`
Expected: 20 tests pass (16 existing + 4 new); no warnings.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/mmm-core/src/reference.rs crates/mmm-core/src/formats/mod.rs crates/mmm-core/src/synth.rs crates/mmm-core/tests/reference.rs
git commit -m "feat(reference): derive a frame from wire descriptors; InputPanel::filter_name

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `InitJob.reference` honoured by the shared-memory analyze paths

**Files:**
- Modify: `crates/mmm-core/src/ipc/protocol.rs:348-377` (`InitJob`)
- Modify: `crates/mmm-core/src/analyze.rs:699-860` (`analyze_ipc_aligned`, `analyze_ipc_solved`) and its `mod tests`
- Modify: `crates/mmm-core/src/ipc/testhost.rs:190-205` (`job` builder sets `reference: None`)
- Modify: `crates/mmm-ipc-worker/src/main.rs:166-195`, `crates/mmm-ipc-worker/tests/end_to_end.rs` (every `InitJob {` literal gains `reference: None`)

**Interfaces:**
- Consumes: `reference::{ReferenceFrame, check_aligned, check_footprints}`, `Session::frame_imposed`.
- Produces:
  - `InitJob.reference: Option<ReferenceFrame>` (`#[serde(default)]`)
  - `pub fn analyze_ipc_aligned(link, session_dir, band_rows, surface_order, gain, reference: Option<&ReferenceFrame>) -> Result<Session>`
  - `pub fn analyze_ipc_solved(link, session_dir, band_rows, surface_order, gain, reference: Option<&ReferenceFrame>) -> Result<Session>`
  - `HostLink::reference(&self) -> Option<&ReferenceFrame>` (accessor on the stored `InitJob`, next to `link.panels()` / `link.canvas()`).

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/mmm-core/src/analyze.rs` (imports: `use crate::reference::ReferenceFrame; use crate::align::MosaicFrame; use crate::synth::write_xisf_solved; use crate::synth::SynthWcs; use crate::formats::xisf::XisfPanel; use crate::ipc::protocol::PanelDesc;`):

```rust
    /// Two raw solved mono panels (64×48 at RA 10°, 60×52 offset ~55 % east
    /// and 8 px north, rotated 6°), as files plus planar pixels plus wire
    /// descriptors carrying their solutions.
    struct SolvedPair {
        dir: PathBuf,
        descs: Vec<PanelDesc>,
        planar: Vec<Vec<f32>>,
    }

    fn synth_solved_pair(tag: &str) -> SolvedPair {
        let dir = tmpdir(tag);
        let s = 1.0e-3_f64;
        let specs = [
            (64u64, 48u64, [10.0f64, 0.0f64], 0.0f64),
            (60, 52, [10.0 + 64.0 * s * 0.55, 8.0 * s], 6.0),
        ];
        let mut descs = Vec::new();
        let mut planar = Vec::new();
        for (k, (w, h, crval, rot)) in specs.iter().enumerate() {
            let (sr, cr) = rot.to_radians().sin_cos();
            let cd = [[-s * cr, -s * sr], [-s * sr, s * cr]];
            let mut planes = vec![0f32; (w * h) as usize];
            for j in 0..*h {
                for i in 0..*w {
                    planes[(j * w + i) as usize] =
                        (((i * 7 + j * 13 + k as u64 * 41) % 97) as f32) / 97.0 + 0.01;
                }
            }
            let path = dir.join(format!("solved_{k}.xisf"));
            write_xisf_solved(
                &path, *w, *h, 1, &planes,
                &SynthWcs { crval: *crval, refimg: [*w as f64 / 2.0, *h as f64 / 2.0], cd },
            )
            .unwrap();
            let props = XisfPanel::open(&path).unwrap().header().properties.clone();
            descs.push(PanelDesc { panel_id: k as u32, width: *w, height: *h, channels: 1, properties: props });
            planar.push(planes);
        }
        SolvedPair { dir, descs, planar }
    }

    fn solved_job(descs: &[PanelDesc], slot_width: u64) -> crate::ipc::protocol::InitJob {
        let mut job = MockHost::aligned_job(0, 0, 1, 0, 8, slot_width * 8 * 4);
        job.panels = descs.to_vec();
        job.mode = crate::ipc::protocol::JobMode::Solved;
        job
    }

    #[test]
    fn ipc_solved_adopts_an_imposed_reference() {
        let f = synth_solved_pair("ipc-ref-solved");
        // A reference wider than either panel's own frame: the union of the
        // pair shifted 30 px east, as a second filter's panels would give.
        let own = crate::reference::derive_from_descs(&f.descs, InputSelect::Solved).unwrap();
        let ReferenceFrame::Solved { frame: own_frame } = &own else { unreachable!() };
        let imposed = ReferenceFrame::Solved {
            frame: MosaicFrame { width: own_frame.width + 30, ..own_frame.clone() },
        };
        let mut job = solved_job(&f.descs, own_frame.width + 30);
        job.reference = Some(imposed.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let sess = analyze_ipc_solved(link.clone(), &dir, 8, Some(2), GainMode::Fit, Some(&imposed)).unwrap();
        link.finish_ok().unwrap();
        host.join();
        assert!(sess.frame_imposed);
        assert_eq!(sess.canvas.0, own_frame.width + 30);
        let ReferenceFrame::Solved { frame } = imposed else { unreachable!() };
        assert_eq!(sess.frame, Some(frame));
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_solved_refuses_a_panel_outside_the_reference() {
        let f = synth_solved_pair("ipc-ref-footprint");
        let own = crate::reference::derive_from_descs(&f.descs, InputSelect::Solved).unwrap();
        let ReferenceFrame::Solved { frame: own_frame } = &own else { unreachable!() };
        // Narrower than the panels' union: panel 1 overhangs the right edge.
        let narrow = ReferenceFrame::Solved {
            frame: MosaicFrame { width: own_frame.width - 40, ..own_frame.clone() },
        };
        let mut job = solved_job(&f.descs, own_frame.width);
        job.reference = Some(narrow.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let err = analyze_ipc_solved(link.clone(), &dir, 8, Some(2), GainMode::Fit, Some(&narrow))
            .unwrap_err()
            .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(err.contains("beyond the"), "{err}");
        assert!(err.contains("panel 1") || err.contains("panel 0"), "{err}");
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_solved_refuses_an_aligned_reference() {
        let f = synth_solved_pair("ipc-ref-kind");
        let aligned = ReferenceFrame::Aligned { width: 64, height: 48, wcs: None };
        let mut job = solved_job(&f.descs, 128);
        job.reference = Some(aligned.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let err = analyze_ipc_solved(link.clone(), &dir, 8, Some(2), GainMode::Fit, Some(&aligned))
            .unwrap_err()
            .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(err.contains("aligned canvas") && err.contains("--input aligned"), "{err}");
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_aligned_adopts_reference_without_wcs() {
        let f = synth_two_full_canvas_panels("ipc-ref-aligned");
        let (w, h, ch) = (32u64, 24u64, 3u64);
        let reference = ReferenceFrame::Aligned { width: w, height: h, wcs: None };
        let mut job = MockHost::aligned_job(w, h, ch, 2, 8, w * ch * 32 * 4);
        job.reference = Some(reference.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let sess = analyze_ipc_aligned(link.clone(), &dir, 32, Some(2), GainMode::Fit, Some(&reference)).unwrap();
        link.finish_ok().unwrap();
        host.join();
        assert!(sess.frame_imposed);
        assert!(sess.frame.is_none());
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn ipc_aligned_refuses_a_mismatched_canvas_reference() {
        let f = synth_two_full_canvas_panels("ipc-ref-aligned-size");
        let (w, h, ch) = (32u64, 24u64, 3u64);
        let reference = ReferenceFrame::Aligned { width: w + 8, height: h, wcs: None };
        let mut job = MockHost::aligned_job(w, h, ch, 2, 8, w * ch * 32 * 4);
        job.reference = Some(reference.clone());
        let (host, r, wr) = MockHost::spawn(job.clone(), f.planar.clone());
        let link = HostLink::start(job, r, wr).unwrap();
        let dir = f.dir.join("ipc.mmm-session");
        let err = analyze_ipc_aligned(link.clone(), &dir, 32, Some(2), GainMode::Fit, Some(&reference))
            .unwrap_err()
            .to_string();
        link.finish_ok().unwrap();
        host.join();
        assert!(err.contains("does not match the reference frame"), "{err}");
        assert!(err.contains("align every group to one common reference"), "{err}");
        std::fs::remove_dir_all(&f.dir).unwrap();
    }

    #[test]
    fn init_job_without_reference_still_parses() {
        let mut job = MockHost::aligned_job(4, 4, 1, 1, 1, 64);
        job.reference = None;
        let mut v = serde_json::to_value(&job).unwrap();
        v.as_object_mut().unwrap().remove("reference");
        let back: crate::ipc::protocol::InitJob = serde_json::from_value(v).unwrap();
        assert_eq!(back.reference, None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core --lib analyze::tests 2>&1 | grep -E "^error" | sort -u | head`
Expected: no field `reference` on `InitJob`; `analyze_ipc_*` take 5 arguments.

- [ ] **Step 3: Implement the wire field and accessor**

`crates/mmm-core/src/ipc/protocol.rs`, in `InitJob` after `params`:

```rust
    /// Shared reference frame the job must adopt instead of deriving its own
    /// (multi-filter runs: every group's job carries the same value). Same
    /// JSON as a `*.mmm-frame.json` file. `None` (or absent — hosts that
    /// predate the field) keeps today's behaviour.
    #[serde(default)]
    pub reference: Option<crate::reference::ReferenceFrame>,
```

In `crates/mmm-core/src/ipc/client.rs`, next to the existing `pub fn panels(&self)` accessor on `HostLink`:

```rust
    /// The job's shared reference frame, if the host imposed one.
    pub fn reference(&self) -> Option<&crate::reference::ReferenceFrame> {
        self.init.reference.as_ref()
    }
```

(Use whatever field name `HostLink` stores its `InitJob` under; `panels()` shows it.) Add `reference: None,` to the `InitJob` literal in `testhost.rs::job` and to every `InitJob {` literal in `crates/mmm-ipc-worker/tests/end_to_end.rs` (`cargo build --all-targets` lists them).

- [ ] **Step 4: Implement the analyze changes**

`analyze_ipc_aligned`: add the trailing parameter `reference: Option<&ReferenceFrame>`; directly before `session.canvas = canvas;` at its end insert:

```rust
    if let Some(r) = reference {
        match r {
            ReferenceFrame::Aligned { width, height, wcs } => {
                let panel_wcs = link
                    .panels()
                    .first()
                    .and_then(|p| crate::astrometry::wcs_from_properties(&p.properties));
                crate::reference::check_aligned(
                    (canvas.0, canvas.1),
                    panel_wcs.as_ref(),
                    *width,
                    *height,
                    wcs.as_ref(),
                )
                .map_err(|reason| Error::format(session_dir, reason))?;
                session.frame_imposed = true;
            }
            ReferenceFrame::Solved { .. } => {
                return Err(Error::format(session_dir, SOLVED_REFERENCE_ON_ALIGNED));
            }
        }
    }
```

Also run the geometry half of that check *before* the scan loop (the canvas is known from `link.canvas()` up front): right after the per-panel canvas agreement loop, `if let Some(ReferenceFrame::Aligned { width, height, .. }) = reference && (canvas.0, canvas.1) != (*width, *height) { return Err(Error::format(session_dir, crate::reference::check_aligned((canvas.0, canvas.1), None, *width, *height, None).unwrap_err())); }` so a wrong canvas never requests a band.

`analyze_ipc_solved`: add the trailing parameter; replace

```rust
    let (models, frame, ch) =
        solved_frame(panels).map_err(|reason| Error::format(session_dir, reason))?;
```

with

```rust
    let (models, own_frame, ch) =
        solved_frame(panels).map_err(|reason| Error::format(session_dir, reason))?;
    let frame = match reference {
        None => own_frame,
        Some(ReferenceFrame::Solved { frame }) => {
            crate::reference::check_footprints(
                panels
                    .iter()
                    .zip(models.iter())
                    .map(|(p, m)| (format!("panel {}", p.panel_id), m)),
                frame,
            )
            .map_err(|reason| Error::format(session_dir, reason))?;
            frame.clone()
        }
        Some(ReferenceFrame::Aligned { .. }) => {
            return Err(Error::format(session_dir, ALIGNED_REFERENCE_ON_SOLVED));
        }
    };
```

and set `session.frame_imposed = reference.is_some();` beside `session.frame = Some(frame);`. Hoist the solved-path kind-mismatch text used in `analyze_solved` into `const ALIGNED_REFERENCE_ON_SOLVED: &str` next to `SOLVED_REFERENCE_ON_ALIGNED` and use it in both places.

`crates/mmm-ipc-worker/src/main.rs::run`: capture `let reference = init.reference.clone();` with the other pre-move captures, then pass `reference.as_ref()` to `analyze_ipc_aligned`, `analyze_ipc_solved`, and in place of the final `None` to `analyze_full`.

- [ ] **Step 5: Run the tests**

Run: `cargo build --all-targets && cargo test -p mmm-core --lib analyze::tests && cargo test -p mmm-ipc-worker`
Expected: the 6 new tests pass alongside the existing analyze tests; worker end-to-end suite green (its `InitJob` literals compile with `reference: None`).

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/mmm-core/src crates/mmm-ipc-worker
git commit -m "feat(ipc): InitJob.reference adopted by the shared-memory analyze paths

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: Worker probes — `filter`, `reference`, `--probe-reference`

**Files:**
- Modify: `crates/mmm-core/src/ipc/protocol.rs:227-245` (`PanelProbeGeom`, `PanelProbeReply`)
- Modify: `crates/mmm-core/src/analyze.rs:532-590` (`probe_panels`)
- Modify: `crates/mmm-ipc-worker/src/main.rs` (new flag)
- Modify: `crates/mmm-core/tests/probe_panels.rs`, `crates/mmm-ipc-worker/tests/end_to_end.rs`

**Interfaces:**
- Produces:
  - `PanelProbeGeom.filter: Option<String>` (`#[serde(default)]`; the struct drops `Copy`, keeps `Clone`)
  - `PanelProbeReply.reference: Option<ReferenceFrame>` (`#[serde(default)]`)
  - worker flag `--probe-reference`: stdin = bare `InitJob` JSON (panels with properties; `mode` is read for the kind override: `"Aligned"` → `InputSelect::Aligned`, `"Solved"` → `Solved`, `{"Files":{..,"input_select"}}` → that select), stdout = one line of `ReferenceFrame` JSON, nonzero exit with the derive error on stderr.

- [ ] **Step 1: Write the failing tests**

Append to `crates/mmm-core/tests/probe_panels.rs` (it already has `write_solved(dir)` producing two solved panels and `tmpdir`):

```rust
#[test]
fn probe_reports_filter_names_and_the_reference() {
    use mmm_core::formats::FitsKeyword;
    use mmm_core::reference::{ReferenceFrame, derive};
    use mmm_core::synth::write_fits;
    let dir = tmpdir("filter-ref");
    let paths = write_solved(&dir);
    let reply = probe_panels(&paths, InputSelect::Auto).unwrap();
    assert!(reply.panels.iter().all(|p| p.filter.is_none()));
    assert_eq!(reply.reference, Some(derive(&paths, InputSelect::Auto).unwrap()));
    assert!(matches!(reply.reference, Some(ReferenceFrame::Solved { .. })));

    let ha = dir.join("ha.fits");
    write_fits(&ha, 8, 6, 1, &vec![0.2f32; 48], -32, &[FitsKeyword {
        name: "FILTER".into(), value: "'Ha'".into(), comment: String::new(),
    }]).unwrap();
    let plain = dir.join("plain.fits");
    write_fits(&plain, 8, 6, 1, &vec![0.2f32; 48], -32, &[]).unwrap();
    let reply = probe_panels(&[ha, plain], InputSelect::Aligned).unwrap();
    assert_eq!(reply.panels[0].filter.as_deref(), Some("Ha"));
    assert_eq!(reply.panels[1].filter, None);
    assert!(matches!(reply.reference, Some(ReferenceFrame::Aligned { width: 8, height: 6, .. })));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn probe_reference_is_none_when_underivable() {
    let dir = tmpdir("no-ref");
    let a = dir.join("a.xisf");
    let b = dir.join("b.xisf");
    write_xisf(&a, 10, 8, 1, &vec![0.1f32; 80]).unwrap();
    write_xisf(&b, 12, 8, 1, &vec![0.1f32; 96]).unwrap();
    // Mixed geometry, no solutions, Auto: neither aligned nor solvable.
    let reply = probe_panels(&[a, b], InputSelect::Auto).unwrap();
    assert_eq!(reply.frame, None);
    assert_eq!(reply.reference, None);
    std::fs::remove_dir_all(&dir).unwrap();
}
```

Append to `crates/mmm-ipc-worker/tests/end_to_end.rs`:

```rust
/// `--probe-reference` derives the shared frame from Views-style
/// descriptors and prints it as the same JSON a `.mmm-frame.json` holds.
#[test]
fn probe_reference_prints_reference_frame_json() {
    use std::io::Write;
    let dir = tmpdir("probe-reference");
    let (paths, _planars) = write_two_solved_panels(&dir);
    let descs: Vec<PanelDesc> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let xp = XisfPanel::open(p).unwrap();
            PanelDesc {
                panel_id: i as u32,
                width: xp.width(),
                height: xp.height(),
                channels: xp.channels(),
                properties: xp.header().properties.clone(),
            }
        })
        .collect();
    let expected = mmm_core::reference::derive_from_descs(&descs, mmm_core::analyze::InputSelect::Solved).unwrap();

    let mut job = InitJob {
        protocol_version: IPC_PROTOCOL_VERSION,
        worker_version: env!("CARGO_PKG_VERSION").to_string(),
        shm_name: String::new(),
        slot_bytes: 0,
        input_slots: 0,
        output_slots: 0,
        canvas: [0, 0, 1],
        panels: descs,
        mode: JobMode::Solved,
        session_dir: String::new(),
        params: BlendParamsWire::default(),
        reference: None,
    };
    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let run = |job: &InitJob| {
        let mut child = std::process::Command::new(exe)
            .arg("--probe-reference")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(serde_json::to_string(job).unwrap().as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    let out = run(&job);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let got: mmm_core::reference::ReferenceFrame = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(got, expected);

    // A panel without a solution under Solved fails naming it on stderr.
    job.panels[1].properties.clear();
    let out = run(&job);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("panel 1"));
    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mmm-core --test probe_panels 2>&1 | grep -E "^error" | sort -u; cargo test -p mmm-ipc-worker --test end_to_end probe_reference 2>&1 | grep -E "^test |FAILED|unrecognized" | head -3`
Expected: no field `filter` / `reference`; the worker test fails at the exit status (unknown flag falls through to `run()` and errors).

- [ ] **Step 3: Implement**

`protocol.rs`: in `PanelProbeGeom` change the derive to `#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]` and add

```rust
    /// The panel's filter name (FITS `FILTER` card or XISF
    /// `Instrument:Filter:Name`), for a host's "group by filter" UI; `None`
    /// when the header carries none. Hosts that predate the field ignore it.
    #[serde(default)]
    pub filter: Option<String>,
```

In `PanelProbeReply` add

```rust
    /// The shared reference frame `reference::derive` yields over `paths`
    /// with the request's `input_select`; `None` when it cannot be derived
    /// (mixed geometry without solutions under `Auto`). Multi-group hosts
    /// impose it on every group's job via `InitJob.reference`.
    #[serde(default)]
    pub reference: Option<crate::reference::ReferenceFrame>,
```

`analyze.rs::probe_panels`: extend the parallel probe tuple with `x.filter_name()`; fill `filter` into each `PanelProbeGeom`; after `frame` compute

```rust
    let reference = match input {
        InputSelect::Solved => Some(crate::reference::derive(paths, input)?),
        _ => crate::reference::derive(paths, input).ok(),
    };
```

(`Solved` propagates the error exactly as the existing `frame` computation does; otherwise `None`.) Return `PanelProbeReply { panels, frame, reference }`. Fix the `PanelProbeGeom` copy in `probed.iter().map(|(g, _)| *g)` to `.cloned()`/`clone()` now that it is not `Copy`.

`crates/mmm-ipc-worker/src/main.rs`: add `--probe-reference` to `main`'s dispatch and

```rust
/// `--probe-reference`: read an `InitJob`-shaped JSON object on stdin (Views-
/// style `PanelDesc`s with their astrometric properties) and print the
/// shared [`ReferenceFrame`] `reference::derive_from_descs` yields as one
/// line of JSON — the same shape as a `.mmm-frame.json`. The kind override
/// comes from `mode`: `Aligned`/`Solved` force that kind, `Files` carries its
/// own `input_select`. Lets a GUI host derive one frame over every group's
/// views before running the groups' jobs (PROTOCOL.md §11).
fn probe_reference() -> mmm_core::Result<()> {
    use mmm_core::analyze::InputSelect;
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .map_err(|e| mmm_core::Error::compute(format!("reading probe JSON from stdin: {e}")))?;
    let job: mmm_core::ipc::protocol::InitJob = serde_json::from_str(&buf)
        .map_err(|e| mmm_core::Error::compute(format!("parsing probe InitJob JSON: {e}")))?;
    if job.worker_version != WORKER_VERSION {
        return Err(version_mismatch_error(&job.worker_version));
    }
    let input = match &job.mode {
        JobMode::Aligned => InputSelect::Aligned,
        JobMode::Solved => InputSelect::Solved,
        JobMode::Files { input_select, .. } => input_select.to_input_select(),
    };
    let reference = mmm_core::reference::derive_from_descs(&job.panels, input)?;
    let text = serde_json::to_string(&reference)
        .map_err(|e| mmm_core::Error::compute(format!("encoding ReferenceFrame: {e}")))?;
    writeln!(std::io::stdout(), "{text}")
        .map_err(|e| mmm_core::Error::compute(format!("probe-reference: writing stdout: {e}")))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p mmm-core --test probe_panels && cargo test -p mmm-ipc-worker --test end_to_end probe && cargo clippy --all-targets --workspace`
Expected: all probe tests pass (old and new); no warnings.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates
git commit -m "feat(worker): probe replies carry filter names and the reference; --probe-reference for views

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Host library — probe additions and the two-group golden test

**Files:**
- Modify: `integration/pixinsight/host/mmm_host.h:40-60, 108-130`, `integration/pixinsight/host/mmm_host.cpp:762-820`
- Modify: `integration/pixinsight/host/test/test_probe_panels.cpp`
- Create: `integration/pixinsight/host/test/test_golden_groups.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt`

**Interfaces:**
- Produces (namespace `mmm`):
  - `ProbedPanel.filter: std::string` (empty when none)
  - `PanelProbeResult.reference: nlohmann::json` (`null` when none)
  - `static nlohmann::json Host::probe_reference(const std::string& worker_path, const nlohmann::json& init_obj, ProgressCallback* prog = nullptr)`
  - `bool reference_canvas(const nlohmann::json& reference, uint64_t& w, uint64_t& h)` — reads `frame.width/height` (solved) or `width/height` (aligned); false if neither shape.
  - `std::string reference_kind(const nlohmann::json& reference)` — the `kind` string or empty.

- [ ] **Step 1: Write the failing tests**

Append to `test_probe_panels.cpp`'s `main` before the final `fprintf`:

```cpp
  // ---- 5. The reply carries the reference frame (solved kind for raw
  // panels) and FILTER names (none in these fixtures). ----
  CHECK(!res.reference.is_null());
  CHECK(mmm::reference_kind(res.reference) == "solved");
  uint64_t rw = 0, rh = 0;
  CHECK(mmm::reference_canvas(res.reference, rw, rh));
  CHECK(rw == frame_w && rh == frame_h);
  for (const auto& p : res.panels) CHECK(p.filter.empty());
  CHECK(mmm::reference_kind(ares.reference) == "aligned");
  CHECK(mmm::reference_canvas(ares.reference, rw, rh));
  CHECK(rw == ameta.at("canvas")[0].get<uint64_t>());

  // ---- 6. probe_reference over Views-style descriptors equals the
  // file-derived reference. ----
  json ref = mmm::Host::probe_reference(worker_path, probe_init, &idle);
  CHECK(ref == res.reference);
  json bad_init = probe_init;
  bad_init["panels"][1]["properties"] = json::array();
  threw = false;
  try {
    (void)mmm::Host::probe_reference(worker_path, bad_init, nullptr);
  } catch (const mmm::HostError& e) {
    threw = true;
    CHECK(std::string(e.what()).find("panel 1") != std::string::npos);
  }
  CHECK(threw);
```

Create `test_golden_groups.cpp` — two groups (A = both solved fixtures, B = the first solved fixture alone) run as `Solved` jobs under one reference derived over all three descriptors; both outputs must have the reference's geometry:

```cpp
// Two-group run through the host: the reference derived over every group's
// panels is imposed on each group's Solved job, so both outputs land on the
// same grid even though group B is a single panel whose own frame would be
// much smaller. Mirrors what the PixInsight module does per group.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#include "mmm_host.h"
#include "test/golden_harness.h"
#include "test/test_util.h"
#include "third_party/json.hpp"

namespace {
using nlohmann::json;
using mmm_test::make_params;
using mmm_test::MemSource;
using mmm_test::run_job;
using mmm_test::RunResult;

std::string read_text(const std::string& path) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  return std::string((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
}

std::vector<float> read_floats(const std::string& path, uint64_t count) {
  std::ifstream f(path, std::ios::binary);
  CHECK(f.good());
  std::vector<float> out(count);
  f.read(reinterpret_cast<char*>(out.data()), count * sizeof(float));
  CHECK(f.gcount() == static_cast<std::streamsize>(count * sizeof(float)));
  return out;
}

json desc(const json& meta_panel, const json& props, uint32_t id) {
  json pd;
  pd["panel_id"] = id;
  pd["width"] = meta_panel.at("w").get<uint64_t>();
  pd["height"] = meta_panel.at("h").get<uint64_t>();
  pd["channels"] = meta_panel.at("ch").get<uint64_t>();
  pd["properties"] = props;
  return pd;
}
}  // namespace

int main(int argc, char** argv) {
  CHECK(argc >= 3);
  const std::string fixtures = argv[1];
  const std::string worker_path = argv[2];
  json meta = json::parse(read_text(fixtures + "/solved_meta.json"));
  json props = json::parse(read_text(fixtures + "/solved_props.json"));
  const uint64_t ch = meta.at("ch").get<uint64_t>();
  const uint32_t band_rows = meta.at("band_rows").get<uint32_t>();
  json params = make_params(band_rows, meta.at("feather_px").get<double>());
  const int pid = mmm_test_getpid();

  // Groups: A = {solved0, solved1}, B = {solved0}.
  std::vector<std::vector<size_t>> groups = {{0, 1}, {0}};

  // Reference over every panel of every group (ids renumbered 0..n).
  json all = json::array();
  uint32_t id = 0;
  for (const auto& g : groups)
    for (size_t k : g) all.push_back(desc(meta.at("panels")[k], props[k], id++));
  json probe_init;
  probe_init["shm_name"] = "";
  probe_init["slot_bytes"] = 0;
  probe_init["input_slots"] = 0;
  probe_init["output_slots"] = 0;
  probe_init["canvas"] = {0, 0, ch};
  probe_init["panels"] = all;
  probe_init["mode"] = "Solved";
  probe_init["session_dir"] = "";
  probe_init["params"] = params;
  json reference = mmm::Host::probe_reference(worker_path, probe_init);
  uint64_t rw = 0, rh = 0;
  CHECK(mmm::reference_canvas(reference, rw, rh));

  uint64_t max_w = 0;
  for (const auto& p : meta.at("panels")) max_w = std::max<uint64_t>(max_w, p.at("w").get<uint64_t>());
  const uint64_t slot_bytes = std::max<uint64_t>(max_w, rw) * ch * band_rows * 4;
  mmm::SlotLayout layout{slot_bytes, 8, 2};

  std::vector<RunResult> outputs;
  for (size_t gi = 0; gi < groups.size(); gi++) {
    MemSource mem;
    json panels = json::array();
    uint32_t pid_in_group = 0;
    for (size_t k : groups[gi]) {
      const auto& pj = meta.at("panels")[k];
      uint64_t pw = pj.at("w"), ph = pj.at("h"), pc = pj.at("ch");
      mem.w.push_back(pw);
      mem.h.push_back(ph);
      mem.ch.push_back(pc);
      mem.panels.push_back(read_floats(fixtures + "/solved" + std::to_string(k) + ".bin", pw * ph * pc));
      panels.push_back(desc(pj, props[k], pid_in_group++));
    }
    json init;
    init["slot_bytes"] = slot_bytes;
    init["input_slots"] = layout.input_slots;
    init["output_slots"] = layout.output_slots;
    init["canvas"] = {0, 0, ch};
    init["panels"] = panels;
    init["mode"] = "Solved";
    init["session_dir"] = fixtures + "/groups_" + std::to_string(gi) + "_" + std::to_string(pid) + ".mmm-session";
    init["params"] = params;
    init["reference"] = reference;
    const std::string shm = "/mmm-golden-groups-" + std::to_string(gi) + "-" + std::to_string(pid);
    init["shm_name"] = shm;
    outputs.push_back(run_job(worker_path, init, layout, shm, mem));
  }

  // Both outputs are the whole reference frame (canvas extent), hence equal.
  for (const auto& o : outputs) {
    CHECK(o.w == rw);
    CHECK(o.h == rh);
    CHECK(o.ch == ch);
  }
  // Group B (one panel) is non-empty and differs from group A.
  bool any_nonzero = false;
  for (float v : outputs[1].data) any_nonzero |= (v != 0.0f);
  CHECK(any_nonzero);
  CHECK(outputs[0].data != outputs[1].data);

  std::printf("test_golden_groups OK: 2 groups on a %llu x %llu reference grid\n",
              static_cast<unsigned long long>(rw), static_cast<unsigned long long>(rh));
  return 0;
}
```

Register in `CMakeLists.txt` after `test_golden_solved`:

```cmake
# Two-group run: one reference derived over every group's panels, imposed on
# each group's Solved job; both outputs share the reference geometry.
add_executable(test_golden_groups test/test_golden_groups.cpp)
target_link_libraries(test_golden_groups PRIVATE mmm_host)
add_dependencies(test_golden_groups gen_fixtures build_worker)
add_test(NAME test_golden_groups COMMAND test_golden_groups ${CMAKE_BINARY_DIR}/fixtures ${MMM_WORKER})
```

- [ ] **Step 2: Build to verify failure**

Run: `cmake -S integration/pixinsight/host -B integration/pixinsight/host/build && cmake --build integration/pixinsight/host/build 2>&1 | grep -E "error" | head -5`
Expected: compile errors for `reference_kind`, `reference_canvas`, `probe_reference`, `ProbedPanel::filter`.

- [ ] **Step 3: Implement**

`mmm_host.h`: in `ProbedPanel` add `std::string filter;  ///< FILTER name from the header; empty when none.`; in `PanelProbeResult` add `nlohmann::json reference;  ///< ReferenceFrame JSON the worker derived over the paths; null when none.`; in `class Host` after `probe_panels` declare

```cpp
  /// Multi-group helper: spawn `worker_path --probe-reference`, write
  /// `init_obj` (an `InitJob`-shaped object whose `panels` carry every
  /// group's descriptors with their astrometric properties; `mode` selects
  /// the kind override) and return the `ReferenceFrame` JSON it prints —
  /// the value to put in each group's `InitJob.reference`. Throws
  /// `HostError` (worker stderr in the message) or `HostCancelled`.
  static nlohmann::json probe_reference(const std::string& worker_path,
                                        const nlohmann::json& init_obj,
                                        ProgressCallback* prog = nullptr);
```

and, as free functions after the class:

```cpp
/// The `kind` of a ReferenceFrame JSON (`"solved"` / `"aligned"`), or "".
inline std::string reference_kind(const nlohmann::json& reference) {
  if (!reference.is_object() || !reference.contains("kind")) return "";
  return reference.at("kind").get<std::string>();
}

/// Canvas size a ReferenceFrame JSON prescribes: `frame.width/height` for
/// the solved kind, `width/height` for the aligned kind. False if the value
/// has neither shape.
inline bool reference_canvas(const nlohmann::json& reference, uint64_t& w, uint64_t& h) {
  try {
    const std::string kind = reference_kind(reference);
    const nlohmann::json& geom = (kind == "solved") ? reference.at("frame") : reference;
    if (kind != "solved" && kind != "aligned") return false;
    w = geom.at("width").get<uint64_t>();
    h = geom.at("height").get<uint64_t>();
    return true;
  } catch (const nlohmann::json::exception&) {
    return false;
  }
}
```

`mmm_host.cpp`: in `probe_panels` parse `pp.filter = p.contains("filter") && !p.at("filter").is_null() ? p.at("filter").get<std::string>() : std::string();` and `res.reference = reply.contains("reference") ? reply.at("reference") : nlohmann::json();`. Add

```cpp
nlohmann::json Host::probe_reference(const std::string& worker_path, const nlohmann::json& init_obj,
                                     ProgressCallback* prog) {
  nlohmann::json obj = init_obj;
  obj["protocol_version"] = kProtocolVersion;
  obj["worker_version"] = kExpectedWorkerVersion;
  const std::string out = run_probe_process(worker_path, "--probe-reference", obj.dump(), prog);
  try {
    nlohmann::json ref = nlohmann::json::parse(out);
    uint64_t w = 0, h = 0;
    if (!reference_canvas(ref, w, h)) {
      throw HostError("probe-reference: reply is not a ReferenceFrame: " + out);
    }
    return ref;
  } catch (const nlohmann::json::exception& e) {
    throw HostError(std::string("probe-reference: could not parse worker reply: ") + e.what());
  }
}
```

- [ ] **Step 4: Build and run the host suite**

Run: `cmake --build integration/pixinsight/host/build && ctest --test-dir integration/pixinsight/host/build --output-on-failure`
Expected: all tests pass including `test_probe_panels` (extended) and the new `test_golden_groups`; `test_golden_aligned` / `test_golden_solved` unchanged and green.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/host
git commit -m "feat(host): probe_reference, filter names and reference in probe results; two-group golden test

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: `MmmGroups.h` — PCL-free partition, sanitise, wildcard

**Files:**
- Create: `integration/pixinsight/module/MmmGroups.h`
- Create: `integration/pixinsight/host/test/test_groups.cpp`
- Modify: `integration/pixinsight/host/CMakeLists.txt`

**Interfaces:**
- Produces (namespace `mmm_groups`, header-only, C++20, no PCL):
  - `struct Group { std::string name; std::vector<size_t> rows; }`
  - `std::vector<Group> partition(const std::vector<std::string>& group_of_row)` — order of first appearance; `""` is the default group.
  - `std::string sanitize(const std::string& name)` — `[A-Za-z0-9_]` kept, everything else → `_`.
  - `std::string window_id(const std::string& group, const std::string& base)` — `base` for the default group, else `base + "_" + sanitize(group)`.
  - `bool find_window_collision(const std::vector<Group>&, const std::string& base, std::string& a, std::string& b, std::string& id)`.
  - `bool wildcard_match(const std::string& pattern, const std::string& text)` — `*`, `?`, case-insensitive ASCII.
  - `std::string display_name(const std::string& group)` — `"(default)"` for `""`.
  - `std::string trim(const std::string&)`.

- [ ] **Step 1: Write the failing test**

Create `test_groups.cpp`:

```cpp
#include <string>
#include <vector>

#include "MmmGroups.h"
#include "test/test_util.h"

int main() {
  using namespace mmm_groups;

  // Partition: first-appearance order, default group is "".
  auto g = partition({"L", "", "R", "L", "", "Ha"});
  CHECK(g.size() == 4);
  CHECK(g[0].name == "L" && g[0].rows == std::vector<size_t>({0, 3}));
  CHECK(g[1].name == "" && g[1].rows == std::vector<size_t>({1, 4}));
  CHECK(g[2].name == "R" && g[2].rows == std::vector<size_t>({2}));
  CHECK(g[3].name == "Ha" && g[3].rows == std::vector<size_t>({5}));
  auto single = partition({"", "", ""});
  CHECK(single.size() == 1 && single[0].name.empty() && single[0].rows.size() == 3);
  CHECK(partition({}).empty());

  // Names and window ids.
  CHECK(sanitize("R-1") == "R_1");
  CHECK(sanitize("S II") == "S_II");
  CHECK(sanitize("Ha") == "Ha");
  CHECK(window_id("", "MegaMergeMosaic") == "MegaMergeMosaic");
  CHECK(window_id("Ha", "MegaMergeMosaic") == "MegaMergeMosaic_Ha");
  CHECK(window_id("O-III", "seam_map") == "seam_map_O_III");
  CHECK(display_name("") == "(default)");
  CHECK(display_name("L") == "L");
  CHECK(trim("  Ha \t") == "Ha");

  // Collisions: distinct names, same id; case is preserved (Ha != ha).
  std::string a, b, id;
  CHECK(find_window_collision(partition({"R-1", "R_1"}), "MegaMergeMosaic", a, b, id));
  CHECK(a == "R-1" && b == "R_1" && id == "MegaMergeMosaic_R_1");
  CHECK(!find_window_collision(partition({"Ha", "ha", ""}), "MegaMergeMosaic", a, b, id));
  // A group literally named "MegaMergeMosaic"'s id would collide with
  // nothing (its id is MegaMergeMosaic_MegaMergeMosaic).
  CHECK(!find_window_collision(partition({"", "MegaMergeMosaic"}), "MegaMergeMosaic", a, b, id));

  // Wildcards.
  CHECK(wildcard_match("*_Ha*", "M42_Ha_p01.xisf"));
  CHECK(wildcard_match("*_ha*", "M42_Ha_p01.xisf"));
  CHECK(!wildcard_match("*_Ha*", "M42_OIII_p01.xisf"));
  CHECK(wildcard_match("p?.xisf", "p1.xisf"));
  CHECK(!wildcard_match("p?.xisf", "p12.xisf"));
  CHECK(wildcard_match("", "anything"));
  CHECK(wildcard_match("*", ""));
  CHECK(wildcard_match("Ha", "ha"));

  std::fprintf(stderr, "test_groups: OK\n");
  return 0;
}
```

Register in `CMakeLists.txt`:

```cmake
# Group partitioning / window-id sanitisation / wildcard matching used by the
# module's multi-filter UI and run loop. Header-only and PCL-free so it can be
# tested here (the module has no runtime harness).
add_executable(test_groups test/test_groups.cpp)
target_include_directories(test_groups PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../module ${CMAKE_CURRENT_SOURCE_DIR})
add_test(NAME test_groups COMMAND test_groups)
```

- [ ] **Step 2: Build to verify failure**

Run: `cmake -S integration/pixinsight/host -B integration/pixinsight/host/build && cmake --build integration/pixinsight/host/build --target test_groups 2>&1 | grep -c "MmmGroups.h"`
Expected: a non-zero count (fatal error: no such file).

- [ ] **Step 3: Implement the header**

```cpp
// MmmGroups.h -- PCL-free helpers for the multi-filter "groups" feature:
// partition panel rows by group name, derive PixInsight-safe window ids,
// and match the Target Frames filter pattern. Header-only so the host CTest
// suite (test_groups) can exercise it without PCL.
#pragma once

#include <cctype>
#include <string>
#include <vector>

namespace mmm_groups
{

struct Group
{
   std::string         name;   // "" = the default group
   std::vector<size_t> rows;   // panel rows, in list order
};

inline std::string trim( const std::string& s )
{
   size_t b = 0, e = s.size();
   while ( b < e && std::isspace( (unsigned char)s[b] ) ) ++b;
   while ( e > b && std::isspace( (unsigned char)s[e - 1] ) ) --e;
   return s.substr( b, e - b );
}

inline std::vector<Group> partition( const std::vector<std::string>& groupOfRow )
{
   std::vector<Group> out;
   for ( size_t row = 0; row < groupOfRow.size(); ++row )
   {
      const std::string& name = groupOfRow[row];
      Group* g = nullptr;
      for ( Group& existing : out )
         if ( existing.name == name )
         {
            g = &existing;
            break;
         }
      if ( g == nullptr )
      {
         out.push_back( Group{ name, {} } );
         g = &out.back();
      }
      g->rows.push_back( row );
   }
   return out;
}

inline std::string sanitize( const std::string& name )
{
   std::string out;
   out.reserve( name.size() );
   for ( unsigned char c : name )
      out += ( std::isalnum( c ) || c == '_' ) ? char( c ) : '_';
   return out;
}

inline std::string window_id( const std::string& group, const std::string& base )
{
   return group.empty() ? base : base + "_" + sanitize( group );
}

inline std::string display_name( const std::string& group )
{
   return group.empty() ? std::string( "(default)" ) : group;
}

inline bool find_window_collision( const std::vector<Group>& groups, const std::string& base,
                                   std::string& a, std::string& b, std::string& id )
{
   for ( size_t i = 0; i < groups.size(); ++i )
      for ( size_t j = i + 1; j < groups.size(); ++j )
         if ( window_id( groups[i].name, base ) == window_id( groups[j].name, base ) )
         {
            a = groups[i].name;
            b = groups[j].name;
            id = window_id( groups[i].name, base );
            return true;
         }
   return false;
}

// Case-insensitive glob: '*' matches any run, '?' one character. Iterative
// with backtracking to the last '*' (no recursion, linear in practice).
inline bool wildcard_match( const std::string& pattern, const std::string& text )
{
   auto lower = []( unsigned char c ) { return char( std::tolower( c ) ); };
   size_t p = 0, t = 0, star = std::string::npos, mark = 0;
   while ( t < text.size() )
   {
      if ( p < pattern.size() && ( pattern[p] == '?' || lower( pattern[p] ) == lower( text[t] ) ) )
      {
         ++p;
         ++t;
      }
      else if ( p < pattern.size() && pattern[p] == '*' )
      {
         star = p++;
         mark = t;
      }
      else if ( star != std::string::npos )
      {
         p = star + 1;
         t = ++mark;
      }
      else
         return false;
   }
   while ( p < pattern.size() && pattern[p] == '*' )
      ++p;
   return p == pattern.size();
}

} // namespace mmm_groups
```

- [ ] **Step 4: Run the test**

Run: `cmake --build integration/pixinsight/host/build --target test_groups && ctest --test-dir integration/pixinsight/host/build -R test_groups --output-on-failure`
Expected: `test_groups: OK`, 1/1 passed.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/module/MmmGroups.h integration/pixinsight/host/test/test_groups.cpp integration/pixinsight/host/CMakeLists.txt
git commit -m "feat(module): PCL-free group partition, window-id sanitisation and wildcard helpers

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: Module parameters — the `group` columns

**Files:**
- Modify: `integration/pixinsight/module/MmmParameters.h:58-71, 90-103`, `MmmParameters.cpp`, `mmm.cpp:95-110`
- Modify: `integration/pixinsight/module/MmmProcess.h:85-100`, `MmmProcess.cpp:111-133, 177-265`

**Interfaces:**
- Produces: `TheMmmViewGroupParameter` (`MetaString` column `"group"` of `inputViews`), `TheMmmFileGroupParameter` (column `"group"` of `filePaths`); instance members `Array<String> p_viewGroups`, `Array<String> p_fileGroups` kept the same length as `p_viewIds` / `p_filePaths`.

There is no runtime test harness for the module; the verification for this task is a warning-free module build plus the source-text CTests. The behavioural guarantee (old icons → default group) comes from `AllocateParameter` resizing both arrays of a table to the row count, so a missing column leaves empty strings.

- [ ] **Step 1: Declare the parameters**

`MmmParameters.h`, after `MmmViewIdParameter`:

```cpp
/*!
 * \brief The "group" string column of the inputViews table: the filter group
 * a view belongs to ("" = the default group). Rows sharing a group are merged
 * into one output; several groups share one reference frame (spec: PixInsight
 * multi-filter groups).
 */
class MmmViewGroupParameter : public MetaString
{
public:

   MmmViewGroupParameter( MetaTable* T ) : MetaString( T ) {}
   IsoString Id() const override { return "group"; }
};

extern MmmViewGroupParameter* TheMmmViewGroupParameter;
```

and after `MmmPathParameter` the same for files:

```cpp
/*!
 * \brief The "group" string column of the filePaths table (see
 * MmmViewGroupParameter).
 */
class MmmFileGroupParameter : public MetaString
{
public:

   MmmFileGroupParameter( MetaTable* T ) : MetaString( T ) {}
   IsoString Id() const override { return "group"; }
};

extern MmmFileGroupParameter* TheMmmFileGroupParameter;
```

Update the header's parameter↔wire comment: `inputViews/group, filePaths/group -> one worker job per distinct group; InitJob.reference shared`.

`MmmParameters.cpp`: add `MmmViewGroupParameter* TheMmmViewGroupParameter = nullptr;` and `MmmFileGroupParameter* TheMmmFileGroupParameter = nullptr;`.

`mmm.cpp`: after `TheMmmViewIdParameter = …` add `TheMmmViewGroupParameter = new MmmViewGroupParameter( TheMmmInputImagesParameter );` and after `TheMmmPathParameter = …` add `TheMmmFileGroupParameter = new MmmFileGroupParameter( TheMmmFilePathsParameter );` (columns append to their table in construction order, so the group column is second).

- [ ] **Step 2: Instance storage and hooks**

`MmmProcess.h`: after `p_filePaths` add

```cpp
   Array<String> p_viewGroups;      // inputViews "group" column, parallel to p_viewIds
   Array<String> p_fileGroups;      // filePaths "group" column, parallel to p_filePaths
```

`MmmProcess.cpp::Assign`: add `p_viewGroups = x->p_viewGroups; p_fileGroups = x->p_fileGroups;`.

`LockParameter`: add

```cpp
   if ( p == TheMmmViewGroupParameter )
      return p_viewGroups[tableRow].Begin();
   if ( p == TheMmmFileGroupParameter )
      return p_fileGroups[tableRow].Begin();
```

`AllocateParameter`: in the `TheMmmInputImagesParameter` branch also `p_viewGroups.Clear(); if ( sizeOrLength > 0 ) p_viewGroups.Add( String(), sizeOrLength );` and likewise `p_fileGroups` in the `TheMmmFilePathsParameter` branch; add the string-column branches

```cpp
   if ( p == TheMmmViewGroupParameter )
   {
      p_viewGroups[tableRow].Clear();
      if ( sizeOrLength > 0 )
         p_viewGroups[tableRow].SetLength( sizeOrLength );
      return true;
   }
   if ( p == TheMmmFileGroupParameter )
   {
      p_fileGroups[tableRow].Clear();
      if ( sizeOrLength > 0 )
         p_fileGroups[tableRow].SetLength( sizeOrLength );
      return true;
   }
```

`ParameterLength`: add `if ( p == TheMmmViewGroupParameter ) return p_viewGroups[tableRow].Length(); if ( p == TheMmmFileGroupParameter ) return p_fileGroups[tableRow].Length();`.

- [ ] **Step 3: Build the module**

Run: `cd integration/pixinsight/module && make 2>&1 | grep -E "warning|error|mmm-pxm.so" | head`
Expected: links `mmm-pxm.so` with no warnings. (Requires `~/.local/pcl-build/lib/libPCL-pxi.a` per `module/README.md`; the existing `mmm-pxm.so` in the tree shows the local prerequisite is in place.)

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(module): group column on the view and file panel tables

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: Module UI — Group column, Filter / Set group / Group by FILTER / Clear groups

**Files:**
- Modify: `integration/pixinsight/module/MmmInterface.h:98-215`, `MmmInterface.cpp` (Target Frames construction ≈ lines 245-290, `e_ToggleSection`, `UpdateInputModeControls`, `Populate*TreeBox`, add/remove handlers, new handlers)

**Interfaces:**
- Consumes: `MmmGroups.h` (`wildcard_match`, `trim`), `mmm::Host::probe_panels` (via a small module helper for FILTER names), `View::Window().Keywords()`.
- Produces: GUI members `GroupTools_Sizer`, `Filter_Label`, `Filter_Edit`, `Group_Label`, `Group_Edit`, `SetGroup_PushButton`, `GroupByFilter_PushButton`, `ClearGroups_PushButton`; interface state `String m_filterPattern; bool m_filterHintShown = false; Array<int> m_visibleRows;` and handlers `e_FilterTextUpdated`, `e_FilterGetFocus`, `e_FilterLoseFocus`, `e_SetGroupClick`, `e_GroupByFilterClick`, `e_ClearGroupsClick`; helper `Array<int> TargetRows() const`.

- [ ] **Step 1: Declarations**

In `GUIData`, after `RemoveFile_PushButton`:

```cpp
      // Group tools (shared by both lists): filter the displayed rows by a
      // wildcard, set/clear the group of the selection (or of every displayed
      // row), or fill groups from the FILTER keyword.
      HorizontalSizer GroupTools_Sizer;
      Label           Filter_Label;
      Edit            Filter_Edit;
      Label           Group_Label;
      Edit            Group_Edit;
      PushButton      SetGroup_PushButton;
      PushButton      GroupByFilter_PushButton;
      PushButton      ClearGroups_PushButton;
```

In the interface class after `bool m_viewsMode = true;`:

```cpp
   // Target Frames filter: the live wildcard pattern (empty = show all), the
   // emulated placeholder state of Filter_Edit (PCL Edit has no native
   // placeholder), and the instance rows currently displayed, in TreeBox
   // node order (so node index i maps to instance row m_visibleRows[i]).
   String     m_filterPattern;
   bool       m_filterHintShown = false;
   Array<int> m_visibleRows;

   static constexpr const char* kFilterHint = "e.g. *_Ha*";

   Array<int> TargetRows() const;          // selection if any, else all displayed rows
   Array<String>& ActiveGroups();          // p_viewGroups or p_fileGroups
   Array<String>& ActiveItems();           // p_viewIds or p_filePaths
   void ShowFilterHint( bool show );
   void PopulateActiveTreeBox();
```

and the handlers:

```cpp
   void e_FilterTextUpdated( Edit& sender, const String& text );
   void e_FilterGetFocus( Control& sender );
   void e_FilterLoseFocus( Control& sender );
   void e_SetGroupClick( Button& sender, bool checked );
   void e_GroupByFilterClick( Button& sender, bool checked );
   void e_ClearGroupsClick( Button& sender, bool checked );
```

Replace `PopulateViewsTreeBox`/`PopulateFilesTreeBox` declarations with the single `PopulateActiveTreeBox` (both trees are rebuilt by it; the inactive one is hidden anyway).

- [ ] **Step 2: Construction**

In `GUIData::GUIData`, make both trees two-column: `Views_TreeBox.SetNumberOfColumns( 2 ); Views_TreeBox.SetHeaderText( 0, "View Id" ); Views_TreeBox.SetHeaderText( 1, "Group" );` and the same for `Files_TreeBox` with `"File Path"`. After `FileButtons_Sizer` construction:

```cpp
   Filter_Label.SetText( "Filter:" );
   Filter_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
   Filter_Edit.SetToolTip( "<p>Show only the panels whose view id or file name matches this "
      "wildcard pattern (* = any run, ? = one character, case-insensitive), e.g. <b>*_Ha*</b>. "
      "Then press <b>Set group</b> to assign every displayed panel to a group at once.</p>" );
   Filter_Edit.OnTextUpdated( (Edit::text_event_handler)&MmmBlendInterface::e_FilterTextUpdated, w );
   Filter_Edit.OnGetFocus( (Control::event_handler)&MmmBlendInterface::e_FilterGetFocus, w );
   Filter_Edit.OnLoseFocus( (Control::event_handler)&MmmBlendInterface::e_FilterLoseFocus, w );

   Group_Label.SetText( "Group:" );
   Group_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
   Group_Edit.SetToolTip( "<p>Group name to assign with <b>Set group</b>. Each group is merged into "
      "its own output window (MegaMergeMosaic_&lt;group&gt;); all groups share one reference frame "
      "so the outputs can be combined directly. Leave empty for the default group.</p>" );

   SetGroup_PushButton.SetText( "Set group" );
   SetGroup_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SetGroupClick, w );
   SetGroup_PushButton.SetToolTip( "<p>Assign the group name to the selected panels, or to every "
      "displayed panel when nothing is selected.</p>" );

   GroupByFilter_PushButton.SetText( "Group by FILTER" );
   GroupByFilter_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_GroupByFilterClick, w );
   GroupByFilter_PushButton.SetToolTip( "<p>Fill each panel's group from its FILTER keyword "
      "(selected panels, or every displayed panel when nothing is selected). Panels without a "
      "FILTER keyword keep their current group.</p>" );

   ClearGroups_PushButton.SetText( "Clear groups" );
   ClearGroups_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ClearGroupsClick, w );
   ClearGroups_PushButton.SetToolTip( "<p>Move the selected panels (or every displayed panel when "
      "nothing is selected) back to the default group.</p>" );

   GroupTools_Sizer.SetSpacing( 6 );
   GroupTools_Sizer.Add( Filter_Label );
   GroupTools_Sizer.Add( Filter_Edit, 100 );
   GroupTools_Sizer.Add( Group_Label );
   GroupTools_Sizer.Add( Group_Edit, 100 );
   GroupTools_Sizer.Add( SetGroup_PushButton );
   GroupTools_Sizer.Add( GroupByFilter_PushButton );
   GroupTools_Sizer.Add( ClearGroups_PushButton );
```

and `TargetFrames_Sizer.Add( GroupTools_Sizer );` after `FileButtons_Sizer`. Check the exact handler typedef names in `/opt/PixInsight/include/pcl/Edit.h` (`text_event_handler` for `OnTextUpdated`) and `Control.h` (`event_handler` for `OnGetFocus`/`OnLoseFocus`) and use those.

- [ ] **Step 3: Behaviour**

```cpp
Array<String>& MmmBlendInterface::ActiveGroups()
{
   return m_viewsMode ? m_instance.p_viewGroups : m_instance.p_fileGroups;
}

Array<String>& MmmBlendInterface::ActiveItems()
{
   return m_viewsMode ? m_instance.p_viewIds : m_instance.p_filePaths;
}

// Display text matched by the filter: the view id, or the file NAME (no dir).
static String DisplayText( bool viewsMode, const String& item )
{
   return viewsMode ? item : File::ExtractNameAndSuffix( item );
}

void MmmBlendInterface::PopulateActiveTreeBox()
{
   TreeBox& tree = m_viewsMode ? GUI->Views_TreeBox : GUI->Files_TreeBox;
   tree.Clear();
   m_visibleRows.Clear();
   Array<String>& items  = ActiveItems();
   Array<String>& groups = ActiveGroups();
   // Keep the group array in lockstep with the item array (icons from older
   // versions, or Add/Remove paths, can leave it short).
   while ( groups.Length() < items.Length() )
      groups.Add( String() );
   while ( groups.Length() > items.Length() )
      groups.Remove( groups.At( groups.Length() - 1 ) );
   const std::string pattern( m_filterPattern.ToUTF8().c_str() );
   for ( size_type i = 0; i < items.Length(); ++i )
   {
      const String text = DisplayText( m_viewsMode, items[i] );
      if ( !pattern.empty() && !mmm_groups::wildcard_match( pattern, std::string( text.ToUTF8().c_str() ) ) )
         continue;
      TreeBox::Node* node = new TreeBox::Node( tree );
      node->SetText( 0, items[i] );
      node->SetText( 1, groups[i] );
      m_visibleRows.Add( int( i ) );
   }
   tree.AdjustColumnWidthToContents( 0 );
}

Array<int> MmmBlendInterface::TargetRows() const
{
   const TreeBox& tree = m_viewsMode ? GUI->Views_TreeBox : GUI->Files_TreeBox;
   Array<int> rows;
   IndirectArray<TreeBox::Node> selected = const_cast<TreeBox&>( tree ).SelectedNodes();
   if ( selected.IsEmpty() )
      return m_visibleRows;   // nothing selected: every displayed row
   for ( TreeBox::Node* node : selected )
   {
      int idx = const_cast<TreeBox&>( tree ).ChildIndex( node );
      if ( idx >= 0 && size_type( idx ) < m_visibleRows.Length() )
         rows.Add( m_visibleRows[idx] );
   }
   return rows;
}

void MmmBlendInterface::ShowFilterHint( bool show )
{
   m_filterHintShown = show;
   if ( show )
   {
      GUI->Filter_Edit.SetText( kFilterHint );
      GUI->Filter_Edit.SetStyleSheet( "QLineEdit { color: #808080; font-style: italic; }" );
   }
   else
   {
      if ( GUI->Filter_Edit.Text() == kFilterHint )
         GUI->Filter_Edit.Clear();
      GUI->Filter_Edit.SetStyleSheet( String() );
   }
}

void MmmBlendInterface::e_FilterGetFocus( Control& )
{
   if ( m_filterHintShown )
      ShowFilterHint( false );
}

void MmmBlendInterface::e_FilterLoseFocus( Control& )
{
   if ( GUI->Filter_Edit.Text().IsEmpty() )
      ShowFilterHint( true );
}

void MmmBlendInterface::e_FilterTextUpdated( Edit&, const String& text )
{
   if ( m_filterHintShown )
      return;   // programmatic hint text, not a pattern
   m_filterPattern = text.Trimmed();
   PopulateActiveTreeBox();
}

void MmmBlendInterface::e_SetGroupClick( Button&, bool )
{
   const String name = GUI->Group_Edit.Text().Trimmed();
   Array<String>& groups = ActiveGroups();
   for ( int row : TargetRows() )
      if ( row >= 0 && size_type( row ) < groups.Length() )
         groups[row] = name;
   PopulateActiveTreeBox();
}

void MmmBlendInterface::e_ClearGroupsClick( Button&, bool )
{
   Array<String>& groups = ActiveGroups();
   for ( int row : TargetRows() )
      if ( row >= 0 && size_type( row ) < groups.Length() )
         groups[row].Clear();
   PopulateActiveTreeBox();
}

// FILTER value of an open view: the FITS keyword array, quotes/padding
// stripped; empty when absent.
static String ViewFilterName( const String& viewId )
{
   View v = View::ViewById( viewId );
   if ( v.IsNull() )
      return String();
   FITSKeywordArray keywords = v.Window().Keywords();
   for ( const FITSHeaderKeyword& k : keywords )
      if ( k.name == "FILTER" )
         return String( k.StripValueDelimiters() ).Trimmed();
   return String();
}

void MmmBlendInterface::e_GroupByFilterClick( Button&, bool )
{
   Array<int> rows = TargetRows();
   Array<String>& items  = ActiveItems();
   Array<String>& groups = ActiveGroups();
   int missing = 0;
   if ( m_viewsMode )
   {
      for ( int row : rows )
      {
         String f = ViewFilterName( items[row] );
         if ( f.IsEmpty() )
            ++missing;
         else
            groups[row] = f;
      }
   }
   else
   {
      // Header pass in the worker process (never on this thread): one
      // --probe-panels over the targeted files, pumped like a run's probe.
      std::vector<std::string> paths;
      for ( int row : rows )
         paths.push_back( std::string( items[row].ToUTF8().c_str() ) );
      try
      {
         Console().EnableAbort();
         mmm::PanelProbeResult probe = probe_filter_names( paths );
         for ( size_t i = 0; i < rows.Length() && i < probe.panels.size(); ++i )
         {
            if ( probe.panels[i].filter.empty() )
               ++missing;
            else
               groups[rows[i]] = String( IsoString( probe.panels[i].filter.c_str() ).UTF8ToUTF16() );
         }
      }
      catch ( const mmm::HostCancelled& )
      {
         return;
      }
      catch ( const mmm::HostError& e )
      {
         throw Error( String( "MegaMergeMosaic: could not read FILTER keywords: " ) + e.what() );
      }
   }
   if ( missing > 0 )
      Console().WarningLn( String().Format( "<end><cbr>** MegaMergeMosaic: %d panel(s) carry no FILTER "
                                            "keyword; their group was not changed.", missing ) );
   PopulateActiveTreeBox();
}
```

`probe_filter_names(paths)` is a new free function declared in `MmmExecution.h` and implemented in `MmmExecution.cpp` (Task 8 lands the rest of that file; implement this one now):

```cpp
/*!
 * \brief FILTER names of panel files, read header-only by the worker's
 * --probe-panels (pumped so the GUI stays responsive). Returns the probe
 * result; `panels[i].filter` is empty when a file has no FILTER.
 */
mmm::PanelProbeResult probe_filter_names( const std::vector<std::string>& pathsUtf8 );
```

```cpp
mmm::PanelProbeResult probe_filter_names( const std::vector<std::string>& pathsUtf8 )
{
   ConsoleProgress prog;
   return mmm::Host::probe_panels( ResolveWorkerPath(), pathsUtf8, "Auto", &prog );
}
```

(`ConsoleProgress` and `ResolveWorkerPath` live in `MmmExecution.cpp`'s anonymous namespace; define `probe_filter_names` after them, in namespace `pcl`.) Note `--probe-panels` with `Auto` on a mixed set returns no frame but still returns panels and filters, and refuses a mixed channel count — acceptable for a grouping helper (the run would refuse too).

Update the existing code paths: `UpdateControls` calls `PopulateActiveTreeBox()` instead of the two populate functions; `e_AddViewsClick`/`e_AddFilesClick` push `String()` onto the matching group array for each added item; `e_RemoveViewClick`/`e_RemoveFileClick` map node indices through `m_visibleRows` and remove from both arrays; `e_ModeClick` clears the other side's group array too; `ImportProcess` clears the inactive side's group array along with its items; `Launch` calls `ShowFilterHint( true )` once after `UpdateControls()`; `UpdateInputModeControls` leaves the group tools always visible.

Check the exact PCL spellings before building: `FITSKeywordArray`/`FITSHeaderKeyword` and `StripValueDelimiters()` in `/opt/PixInsight/include/pcl/FITSHeaderKeyword.h`, `ImageWindow::Keywords()` in `ImageWindow.h`, `File::ExtractNameAndSuffix` in `File.h`, `String::Trimmed()` in `String.h`.

- [ ] **Step 4: Build and run the source-text test**

Run: `cd integration/pixinsight/module && make 2>&1 | grep -E "warning|error" ; cd ../host && cmake --build build --target test_sectionbar_order && ctest --test-dir build -R test_sectionbar_order --output-on-failure`
Expected: warning-free build; SectionBar order test passes (no new SectionBars were added, the three existing pairs still hold).

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(module): Group column with filter / set group / group by FILTER tools

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: Module execution — one job per group on a shared reference

**Files:**
- Modify: `integration/pixinsight/module/ImageWindowCollector.h/.cpp` (window id)
- Modify: `integration/pixinsight/module/MmmExecution.cpp` (`Params`, `DriveHost`, `RunViews`, `RunFiles`, `ShowSeamMap`, `run_blend`)

**Interfaces:**
- Consumes: `mmm_groups::{partition, window_id, find_window_collision, display_name}`, `mmm::Host::probe_reference`, `mmm::reference_canvas`, `mmm::reference_kind`, `PanelProbeResult.reference`, `InitJob.reference`.
- Produces: `explicit ImageWindowCollector( const IsoString& windowId = "MegaMergeMosaic" )`; `ShowSeamMap( sessionDir, const IsoString& windowId )`; `RunViews( const Params&, const std::string& worker, const GroupJob& )`, `RunFiles(...)` likewise, where

```cpp
struct GroupJob
{
   std::string      name;        // "" = default group
   IsoString        windowId;    // MegaMergeMosaic[_x]
   IsoString        seamMapId;   // seam_map[_x]
   Array<size_type> rows;        // instance rows in this group
   json             reference;   // null for a single-group run
   std::string      sessionDir;  // this group's session dir
};
```

- [ ] **Step 1: Window ids**

`ImageWindowCollector.h`: replace the defaulted constructor with

```cpp
   explicit ImageWindowCollector( const IsoString& windowId = "MegaMergeMosaic" )
      : m_windowId( windowId )
   {
   }
```

and a `IsoString m_windowId;` member; `.cpp` `begin` passes `m_windowId` as the last `ImageWindow` constructor argument instead of the literal.

`ShowSeamMap( const std::string& sessionDirUtf8, const IsoString& windowId )` uses `windowId` in its `ImageWindow` constructor. `DriveHost` gains a `const IsoString& windowId` parameter and constructs `ImageWindowCollector collector( windowId );`.

- [ ] **Step 2: Group-aware Run functions**

Both `RunViews` and `RunFiles` take `const GroupJob& group` and operate only on `group.rows` (the subset of `in.viewIds` / `in.filePaths`), use `group.sessionDir` for `init_body["session_dir"]`, attach `init_body["reference"] = group.reference;` when `!group.reference.is_null()`, and pass `group.windowId` to `DriveHost`. Two further changes:

Views: when a reference is present, (a) the mode comes from the reference kind, not geometry — `solved = ( mmm::reference_kind( group.reference ) == "solved" );`, and (b) `extract_astrometry_props( views[i] )` is attached for every view in both modes (it yields an empty array for a view without a solution), so the worker can run `check_aligned` with a WCS; (c) solved slot sizing uses the reference width: `uint64_t rw, rh; mmm::reference_canvas( group.reference, rw, rh ); width = max( rw, max_panel_w );` instead of `probe_frame`. Without a reference, behaviour is exactly today's.

Files: when a reference is present, slot sizing uses `max( rw, max_w )` from the reference instead of `probe.frame_w`.

- [ ] **Step 3: `run_blend` orchestration**

```cpp
void run_blend( MmmBlendInstance& in )
{
   WriteBanner();
   // … existing validation and Params snapshot unchanged …

   // Partition into groups (first-appearance order; "" = default).
   const Array<String>& groupsArr = haveViews ? in.p_viewGroups : in.p_fileGroups;
   const size_type n = haveViews ? in.p_viewIds.Length() : in.p_filePaths.Length();
   std::vector<std::string> groupOfRow( n );
   for ( size_type i = 0; i < n; ++i )
      if ( i < groupsArr.Length() )
         groupOfRow[i] = mmm_groups::trim( std::string( groupsArr[i].ToUTF8().c_str() ) );
   std::vector<mmm_groups::Group> groups = mmm_groups::partition( groupOfRow );
   {
      std::string a, b, id;
      if ( mmm_groups::find_window_collision( groups, "MegaMergeMosaic", a, b, id ) )
         throw Error( String( "MegaMergeMosaic: groups '" ) + a.c_str() + "' and '" + b.c_str() +
                      "' both map to window id " + id.c_str() + "; rename one of them." );
   }
   p.viewIds / p.filePaths / p.inputSelect … as before

   // Single group: today's path, untouched (no reference, plain window ids).
   if ( groups.size() == 1 )
   {
      GroupJob single;
      single.windowId  = "MegaMergeMosaic";
      single.seamMapId = "seam_map";
      for ( size_t r : groups[0].rows ) single.rows.Add( r );
      single.sessionDir = p.sessionDir;   // (temp guard logic as today)
      … run exactly as before with `single` …
      return;
   }

   // Several groups: one reference over every panel, then one job per group.
   Console console;
   console.EnableAbort();
   ConsoleProgress prog;
   json reference = haveViews ? DeriveReferenceFromViews( p, prog, worker_path )
                              : DeriveReferenceFromFiles( p, prog, worker_path );
   uint64_t rw = 0, rh = 0;
   mmm::reference_canvas( reference, rw, rh );
   console.WriteLn( "<end><cbr><br><b>Reference frame</b>  " + DescribeReference( reference ) );
   console.WriteLn( String().Format( "  groups          %u (", unsigned( groups.size() ) ) +
                    U( GroupNameList( groups ) ) + String().Format( ")   panels %u", unsigned( n ) ) );

   std::vector<std::pair<std::string, IsoString>> outputs;
   for ( const mmm_groups::Group& g : groups )
   {
      GroupJob job;
      job.name       = g.name;
      job.windowId   = IsoString( mmm_groups::window_id( g.name, "MegaMergeMosaic" ).c_str() );
      job.seamMapId  = IsoString( mmm_groups::window_id( g.name, "seam_map" ).c_str() );
      for ( size_t r : g.rows ) job.rows.Add( r );
      job.reference  = reference;
      AutoSessionDirGuard guard;          // per-group temp dir (or <user dir>/<sanitised group>)
      job.sessionDir = GroupSessionDir( p.sessionDir, g.name, guard );

      console.WriteLn( "<end><cbr><br><b>== Group " + U( mmm_groups::display_name( g.name ) ) +
                       String().Format( " (%u panels)</b>", unsigned( g.rows.size() ) ) );
      try
      {
         if ( haveViews ) RunViews( p, worker_path, job ); else RunFiles( p, worker_path, job );
      }
      catch ( const ProcessAborted& ) { throw; }
      catch ( const Error& e )
      {
         throw Error( "MegaMergeMosaic: group " + U( mmm_groups::display_name( g.name ) ) + ": " + e.Message() );
      }
      if ( p.seamMap )
         ShowSeamMap( job.sessionDir, job.seamMapId );
      PrintSummary( job.sessionDir );
      outputs.emplace_back( g.name, job.windowId );
   }

   console.WriteLn( String().Format( "<end><cbr><br><b>Groups complete</b>  %u outputs on the shared %llu x %llu grid",
                                     unsigned( outputs.size() ), (unsigned long long)rw, (unsigned long long)rh ) );
   for ( const auto& o : outputs )
   {
      String name = U( mmm_groups::display_name( o.first ) );
      if ( name.Length() < 15 )
         name.Append( ' ', 15 - name.Length() );
      console.WriteLn( "  " + name + " " + String( o.second ) );
   }
}
```

Helpers to add in the anonymous namespace (group names are UTF-8 `std::string`s; never pass them through `%s` in `String::Format`, which decodes narrow text as ISO-8859-1 — convert with `U()` instead):

- `String U( const std::string& utf8 ) { return String( IsoString( utf8.c_str() ).UTF8ToUTF16() ); }`

- `DeriveReferenceFromViews( const Params&, ConsoleProgress&, worker )`: resolve and lock every view (same loop as `RunViews`, unlock in all paths), build descriptors for *all* rows with `extract_astrometry_props` attached, `init_body["mode"] = json{ { "Files", { { "paths", json::array() }, { "input_select", InputSelectWireString( p.inputSelect ) } } } }` (the probe reads only the kind override from `mode`; the Files shape carries all three overrides uniformly and its `paths` are ignored), then `mmm::Host::probe_reference( worker, init_body, &prog )`; map `HostCancelled` to `ProcessAborted`.
- `DeriveReferenceFromFiles(...)`: `mmm::Host::probe_panels( worker, all_paths, InputSelectWireString(p.inputSelect), &prog )`, throw `Error( "MegaMergeMosaic: could not derive a shared reference frame over the panel set: the files are neither registered to one canvas nor all plate-solved" )` when `.reference.is_null()`.
- `DescribeReference( const json& )`: `kind == "solved"` → `String().Format( "solved frame %llu x %llu px, %.3f\"/px, center RA %.4f Dec %+.4f", w, h, scale_deg*3600, crval[0], crval[1] )` (no degree sign — ASCII only); aligned → `"aligned canvas %llu x %llu px"` plus `" with canvas WCS"` when `wcs` is non-null.
- `GroupNameList( groups )`: comma-separated `display_name`s.
- `GroupSessionDir( userDir, groupName, guard )`: user dir empty → a fresh temp dir per group (same scheme as today, `RemoveSessionDir` first, guard active); user dir set → `userDir + "/" + sanitize(groupName or "default")`, preserved.

Cancellation inside a group propagates `ProcessAborted` out of the loop (earlier groups' windows stay shown).

- [ ] **Step 4: Build**

Run: `cd integration/pixinsight/module && make 2>&1 | grep -E "warning|error"; nm -D -u mmm-pxm.so | grep -c "_ZN3mmm"`
Expected: warning-free; `0` undefined `mmm::` symbols.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(module): one worker job per group on an auto-derived shared reference frame

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 9: Version 1.6.0, protocol and user documentation, final verification

**Files:**
- Modify: `Cargo.toml:12`, `integration/pixinsight/module/MmmVersion.h:13,17`, `integration/pixinsight/host/mmm_protocol.h:33`, `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html:91` (+ new section before `<h2>Usage</h2>` list end), `integration/pixinsight/PROTOCOL.md` (§6 Init table, §10 history, §11 probes), `docs/DESIGN.md` (stage-2 paragraph), the stage-2 spec status line.

- [ ] **Step 1: Bump versions**

`Cargo.toml` workspace `version = "1.6.0"`; `MmmVersion.h` `MMM_VERSION_MINOR 6`, `MMM_VERSION_STRING "1.6.0"`; `mmm_protocol.h` `kExpectedWorkerVersion = "1.6.0"`; HTML subtitle `Version 1.6.0 &mdash; Category: Mosaic`.

Run: `cargo test -p mmm-ipc-worker --test version_sync`
Expected: 3 passed.

- [ ] **Step 2: PROTOCOL.md**

§6 `Init` field table: add row `| reference | ReferenceFrame or null | optional (default null): shared reference frame to adopt — the same JSON as a *.mmm-frame.json (kind "solved": {frame:{crval,scale_deg,width,height,rotation_deg}}; kind "aligned": {width,height,wcs}); honoured by all three modes; the worker's blend then covers the whole frame |`. §6 `PanelProbeGeom`/reply text in §11: add `filter` (string or null) to each panel and `reference` (ReferenceFrame or null) to the reply; add the **Reference probe** paragraph for `--probe-reference` (stdin bare `InitJob`, `mode` as kind override, stdout one line of ReferenceFrame JSON, exit 1 with stderr on error). §10: append `1.6.0 adds the optional `reference` fields and `--probe-reference` (additive, protocol stays 3).`

- [ ] **Step 3: HTML doc**

Add before the `<h2>Usage</h2>` block a new section:

```html
<h2>Merging several filters onto one grid</h2>

<p>Mono imagers stack one panel set per filter and want one mosaic per
filter that can be combined directly (LRGBCombination, PixelMath). Add
<b>every</b> panel of every filter to the Target Frames list, then assign
each panel a <b>Group</b>:</p>
<ul>
  <li><b>Group by FILTER</b> fills the Group column from each panel's
  FILTER keyword in one click.</li>
  <li>Or type a wildcard in <b>Filter</b> (e.g. <code>*_Ha*</code>) so only
  matching panels are shown, enter a name in <b>Group</b> and press
  <b>Set group</b>; repeat for the next filter. With a selection, the
  buttons act on the selected panels only.</li>
</ul>
<p>When more than one group is present, MegaMergeMosaic derives one
reference frame over every panel and merges each group onto it, producing
one window per group (<code>MegaMergeMosaic_L</code>,
<code>MegaMergeMosaic_Ha</code>, ...) with identical dimensions and
astrometric solution. The Process Console shows a header for each group
and a final list of the output windows. Panels left in the default group
produce the plain <code>MegaMergeMosaic</code> window.</p>
<p>Registered (pre-aligned) panels must share one canvas across all
groups: filters registered separately get separate canvases, so either
align every group against one common reference or use the raw plate-solved
panels instead.</p>
```

Add a `<tr>` to the Parameters table describing `group` (both tables).

- [ ] **Step 4: DESIGN.md and spec status**

Under "Shared reference frame across groups", replace the PixInsight bullet with: `**PixInsight (stage 2, 1.6.0)**: flat group column on both panel tables; multi-group runs derive one reference via --probe-reference (views) or the probe-panels reply (files), run one worker job per group with InitJob.reference, name windows MegaMergeMosaic_<group>, and print group headers; single-group runs are unchanged. Spec: [PixInsight multi-filter groups](superpowers/specs/2026-10-03-pixinsight-multi-filter-groups-design.md).` Set the stage-2 spec's `Status:` to `implemented 2026-10-03 (module 1.6.0).`

- [ ] **Step 5: Full verification**

```bash
cargo fmt --check
cargo clippy --all-targets --workspace
cargo doc --no-deps -p mmm-core 2>&1 | grep -ci warning
cargo test --workspace
cmake --build integration/pixinsight/host/build && ctest --test-dir integration/pixinsight/host/build --output-on-failure
(cd integration/pixinsight/module && make 2>&1 | grep -E "warning|error"; true)
```

Expected: no diffs, no warnings, `0`, all Rust tests pass, all CTests pass (golden aligned/solved unchanged, groups + probe + sectionbar + groups helpers green), warning-free module build.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: module and worker 1.6.0; protocol, user and design docs for multi-filter groups

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Self-review notes

- **Spec coverage**: §1 → Tasks 5–6 (column, default group, sanitised ids, collision refusal); §2 → Task 7 (column, multi-select, Filter with emulated hint, Set group / Group by FILTER / Clear groups with the selection-else-displayed rule, worker-side FILTER probe); §3 → Task 8 (single group untouched, reference derived once, mode from the reference kind, per-group jobs and session dirs, earlier windows kept, slot sizing from the reference); §4 → Task 8 console strings; §5 → Tasks 2–4 (`InitJob.reference`, both shm analyze paths, `filter`/`reference` probe fields, `--probe-reference`, `derive_from_descs`, host probe); §6 → tests in Tasks 1–5; §7 → Task 9.
- **Type consistency**: `derive_from_descs(&[PanelDesc], InputSelect)` (T1) used by T3 worker and T2 tests; `InitJob.reference: Option<ReferenceFrame>` (T2) used by T3 test, T4 host JSON, T8 module; `ProbedPanel.filter` / `PanelProbeResult.reference` (T4) used by T7/T8; `mmm_groups` API (T5) used by T7/T8; `GroupJob` (T8) internal to `MmmExecution.cpp`.
- **Review Focus**: items 1–5 pinned as listed; the module-only behaviours (Task 7 UI, Task 8 loop) have no automated harness and are verified by warning-free build plus Daniel's manual smoke per spec §6.
- **Known uncertainty, handled in-step**: exact PCL event typedef names (Task 7 names where to check); `HostLink`'s `InitJob` field name (Task 2 points at `panels()`); the `write_xisf_impl` extra-XML parameter type (Task 1).
