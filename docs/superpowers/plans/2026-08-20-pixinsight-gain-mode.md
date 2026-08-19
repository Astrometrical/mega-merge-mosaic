# PixInsight Gain-Mode Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the v1.3.1 photometric gain mode (`fit`/`unity`, commit d8d3aab) through the PixInsight integration: a `gain` field on the wire, threaded through the IPC worker, a Gain Mode control in the module UI, and updated CLI + module documentation.

**Architecture:** `BlendParamsWire` grows a `gain: String` field (serde-default `"fit"`, mirroring how `mode` travels as a lenient string and how `seam_map` was added compatibly in v1.1.0 — no `IPC_PROTOCOL_VERSION` bump; the exact `worker_version` handshake keeps module/worker paired). The worker maps it to `mmm_core::photometry::GainMode` and passes it into all three job modes. The C++ module adds a `gainMode` MetaEnumeration parameter (persisted in PSM icons/scripts), a ComboBox next to the surface-order control, and serializes `"gain"` into the job JSON. Release version bumps 1.3.1 → 1.4.0 across the three synced sources.

**Tech Stack:** Rust (serde, workspace crates `mmm-core`/`mmm-ipc-worker`), C++20 PCL module (PixInsight SDK), CMake host tests, Markdown/HTML docs.

**Spec:** `docs/DESIGN.md` §Photometric solve (the `--gain fit|unity` paragraph, lines ~84–96); `integration/pixinsight/PROTOCOL.md` (authoritative wire reference); background: `docs/superpowers/specs/2026-08-19-masked-base-detail-reference-design.md` and commit d8d3aab.

## Global Constraints

- `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`, `cargo doc` must all be warning-free (`missing_docs` is warned in `mmm-core`: every new public item needs a doc comment).
- Tests must NOT depend on `test_data/` — synthesize inputs (existing helpers in `end_to_end.rs` already do).
- Any wire change updates `integration/pixinsight/PROTOCOL.md` **in the same commit** (PROTOCOL.md §12).
- Release-version triple must stay in exact sync (enforced by `crates/mmm-ipc-worker/tests/version_sync.rs`): workspace `Cargo.toml` `version`, `integration/pixinsight/module/MmmVersion.h` `MMM_VERSION_STRING`, `integration/pixinsight/host/mmm_protocol.h` `kExpectedWorkerVersion`.
- C++ string literals must be pure ASCII (`pcl::String(const char*)` decodes narrow literals as ISO-8859-1).
- Missing wire field must mean `fit`; default is `fit` everywhere.
- Rust via rustup: `source ~/.cargo/env` if PATH lacks cargo.

---

### Task 1: `gain` field on `BlendParamsWire` + PROTOCOL.md (same commit)

**Files:**
- Modify: `crates/mmm-core/src/ipc/protocol.rs` (struct ~line 255–319, tests ~line 747)
- Modify: `crates/mmm-ipc-worker/tests/end_to_end.rs` (literal sites: lines ~198, ~437, ~840, ~931 — compile fix only)
- Modify: `integration/pixinsight/PROTOCOL.md` (params example ~line 242–253, `BlendParamsWire` table ~line 317–329)

**Interfaces:**
- Produces: `BlendParamsWire.gain: String` (serde default `"fit"`), `BlendParamsWire::gain_mode(&self) -> GainMode` (`"unity"` → `GainMode::Unity`, anything else → `GainMode::Fit`). Task 2 consumes `gain_mode()`.

- [ ] **Step 1: Write the failing test**

In the `tests` module of `crates/mmm-core/src/ipc/protocol.rs`, after `blend_params_seam_map_defaults_off_and_round_trips` (~line 771):

```rust
#[test]
fn blend_params_gain_defaults_fit_and_maps() {
    use crate::photometry::GainMode;
    // Params JSON from a host that predates the gain field (no key) must
    // still parse, selecting the default fit solve.
    let legacy = serde_json::json!({
        "feather_px": 256.0,
        "downsample": 1,
        "band_rows": 256,
        "mode": "pyramid",
        "roi": null,
        "defect_veto": true,
        "flatten": null,
        "surface_order": 2
    });
    let p: BlendParamsWire = serde_json::from_value(legacy).unwrap();
    assert_eq!(p.gain, "fit");
    assert_eq!(p.gain_mode(), GainMode::Fit);

    let unity = BlendParamsWire {
        gain: "unity".to_string(),
        ..Default::default()
    };
    assert_eq!(unity.gain_mode(), GainMode::Unity);
    let back: BlendParamsWire =
        serde_json::from_str(&serde_json::to_string(&unity).unwrap()).unwrap();
    assert_eq!(back.gain_mode(), GainMode::Unity);

    // Unrecognized strings degrade to fit (mirrors mode -> "pyramid").
    let odd = BlendParamsWire {
        gain: "warp".to_string(),
        ..Default::default()
    };
    assert_eq!(odd.gain_mode(), GainMode::Fit);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mmm-core blend_params_gain -- --nocapture`
Expected: FAIL to compile with "no field `gain`" / "no method named `gain_mode`" — that is the correct TDD failure for a new field.

- [ ] **Step 3: Implement the field, default fn, and accessor**

In `crates/mmm-core/src/ipc/protocol.rs`:

Add to the imports block (after `use crate::formats::...`, ~line 16):

```rust
use crate::photometry::GainMode;
```

Add a free function near the struct (above `impl Default for BlendParamsWire`):

```rust
/// Serde default for [`BlendParamsWire::gain`]: params JSON from hosts that
/// predate the field still parses, and means the default fit solve.
fn default_gain() -> String {
    "fit".to_string()
}
```

Append to `struct BlendParamsWire` (after `seam_map`):

```rust
    /// Photometric gain handling for the analyze stage: `"fit"` or
    /// `"unity"`. Not part of [`BlendParams`]; consumed by the worker via
    /// [`Self::gain_mode`]. Defaults to `"fit"` so params JSON from hosts
    /// that predate this field still parses.
    #[serde(default = "default_gain")]
    pub gain: String,
```

In `impl Default for BlendParamsWire`, add `gain: "fit".to_string(),` after `seam_map: false,`.

In `impl BlendParamsWire`, add (after `to_params`):

```rust
    /// Maps the wire `gain` string to a [`GainMode`]: `"unity"` selects
    /// [`GainMode::Unity`]; `"fit"` and any unrecognized value map to
    /// [`GainMode::Fit`] (lenient like `mode`'s unrecognized → pyramid).
    pub fn gain_mode(&self) -> GainMode {
        match self.gain.as_str() {
            "unity" => GainMode::Unity,
            _ => GainMode::Fit,
        }
    }
```

- [ ] **Step 4: Fix the four struct-literal sites in `end_to_end.rs`**

`crates/mmm-ipc-worker/tests/end_to_end.rs` builds `BlendParamsWire` literally at ~lines 198, 437, 840, 931. Add to each, after `seam_map: false,` (keep field order matching the struct):

```rust
            gain: "fit".to_string(),
```

(Verify no other literal sites exist: `grep -rn "BlendParamsWire {" crates/` — only `protocol.rs` impls/tests and these four should appear; `testhost.rs` uses `BlendParamsWire::default()`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mmm-core && cargo test -p mmm-ipc-worker`
Expected: PASS (end-to-end tests take a couple of minutes; they spawn the real worker).

- [ ] **Step 6: Update PROTOCOL.md (same commit — §12 rule)**

In `integration/pixinsight/PROTOCOL.md`:

(a) In the `Init` example's `params` block (~line 242–252), add after `"seam_map": false`:

```jsonc
    "seam_map": false,
    "gain": "fit"
```

(b) In the `BlendParamsWire` table (~line 317–329), add a row after `seam_map`:

```markdown
| `gain` | string | photometric gain handling for the analyze stage: `"fit"` (measure per-panel gains from overlap edges — the default) or `"unity"` (pin every gain at 1, solve offsets only; for photometrically homogeneous same-rig/same-exposure mosaics). `#[serde(default)]`: absent means `"fit"`, so pre-field hosts are unaffected; unrecognized values map to `"fit"` |
```

(c) No `IPC_PROTOCOL_VERSION` bump: the field is serde-defaulted and additive, exactly the `seam_map` precedent (v1.1.0, commit f7d714c). The frame layout, tags, and existing fields are untouched. The module/worker pairing is enforced by the exact `worker_version` handshake, bumped in Task 3.

- [ ] **Step 7: Full verification**

Run: `cargo fmt && cargo clippy --all-targets && cargo doc --no-deps 2>&1 | grep -i warn; cargo test`
Expected: no warnings, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/mmm-core/src/ipc/protocol.rs crates/mmm-ipc-worker/tests/end_to_end.rs integration/pixinsight/PROTOCOL.md
git commit -m "feat(ipc): gain field on BlendParamsWire (serde-default fit)"
```

---

### Task 2: Thread the gain mode through the worker

**Files:**
- Modify: `crates/mmm-core/src/analyze.rs` (`analyze_ipc_aligned` ~line 532, `analyze_ipc_solved` ~line 582)
- Modify: `crates/mmm-ipc-worker/src/main.rs` (imports ~line 9, dispatch ~line 141–195)
- Test: `crates/mmm-ipc-worker/tests/end_to_end.rs`

**Interfaces:**
- Consumes: `BlendParamsWire::gain_mode()` from Task 1.
- Produces: `analyze_ipc_aligned(link, session_dir, band_rows, surface_order, gain: GainMode)` and `analyze_ipc_solved(...)` with the same appended parameter. `Session.gain_mode` (already exists, `session.rs:87`, serde lowercase) records the value; `session.json` shows `"gain_mode": "unity"`.

- [ ] **Step 1: Write the failing test**

In `crates/mmm-ipc-worker/tests/end_to_end.rs`, after `worker_blend_is_byte_identical_to_file_blend` (model its harness; reuse `synth_two_panels`, `SlotLayout`, `MockHost::serve_over`):

```rust
/// The wire `gain` field must reach the analyze stage: a `"unity"` job's
/// session records `gain_mode: unity` and its photometry pins every gain
/// at exactly 1.
#[test]
fn unity_gain_mode_reaches_the_worker_session() {
    let dir = tmpdir("unitygain");
    let (w, h, ch, _paths, planar) = synth_two_panels(&dir);
    let band_rows_u32 = 16u32;

    let slot_bytes = w * ch * band_rows_u32 as u64 * 4;
    let layout = SlotLayout {
        slot_bytes,
        input_slots: 8,
        output_slots: 2,
    };
    let shm_name = format!("/mmm-e2e-ug-{}", std::process::id());
    let shm = ShmSegment::create(&shm_name, layout.total_bytes()).unwrap();

    let panel_descs: Vec<PanelDesc> = (0..planar.len() as u32)
        .map(|panel_id| PanelDesc {
            panel_id,
            width: w,
            height: h,
            channels: ch,
            properties: vec![],
        })
        .collect();

    let worker_session_dir = dir.join("worker.mmm-session");
    let job = InitJob {
        protocol_version: IPC_PROTOCOL_VERSION,
        worker_version: env!("CARGO_PKG_VERSION").to_string(),
        shm_name: shm_name.clone(),
        slot_bytes,
        input_slots: layout.input_slots,
        output_slots: layout.output_slots,
        canvas: [w, h, ch],
        panels: panel_descs,
        mode: JobMode::Aligned,
        session_dir: worker_session_dir.to_string_lossy().into_owned(),
        params: BlendParamsWire {
            band_rows: band_rows_u32,
            gain: "unity".to_string(),
            ..Default::default()
        },
    };

    let exe = env!("CARGO_BIN_EXE_mmm-ipc-worker");
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mmm-ipc-worker");
    let mut child_stdin = child.stdin.take().expect("child stdin");
    let child_stdout = child.stdout.take().expect("child stdout");
    write_frame(&mut child_stdin, &HostMsg::Init(job.clone()))
        .expect("write Init frame to child stdin");
    let host = MockHost::serve_over(job, planar, shm, child_stdout, child_stdin);

    let status = child.wait().expect("wait on mmm-ipc-worker");
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        panic!("mmm-ipc-worker exited with {status}: {stderr}");
    }
    host.join();

    // The session must record the wire's gain mode…
    let session_json =
        std::fs::read_to_string(worker_session_dir.join("session.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&session_json).unwrap();
    assert_eq!(v["gain_mode"], "unity", "session.json: {session_json}");

    // …and the solve must have pinned every gain at exactly 1.
    let phot = Photometry::load(
        &worker_session_dir.join("analysis").join("photometry.json"),
    )
    .unwrap();
    for per_channel in &phot.gains {
        for &g in per_channel {
            assert_eq!(g, 1.0);
        }
    }
}
```

Notes for the implementer: `serde_json` is already a dev-dependency of the tests via `mmm-core` re-exports — if `serde_json::Value` is not in scope, add `serde_json.workspace = true` to `[dev-dependencies]` in `crates/mmm-ipc-worker/Cargo.toml` (check the workspace `Cargo.toml` `[workspace.dependencies]` first; follow existing style). `Photometry` is already imported by this test file.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mmm-ipc-worker unity_gain_mode -- --nocapture`
Expected: FAIL on the `assert_eq!(v["gain_mode"], "unity")` with `"fit"` — the worker still hardcodes `GainMode::Fit`.

- [ ] **Step 3: Implement the threading**

In `crates/mmm-core/src/analyze.rs`:

(a) `analyze_ipc_aligned` (~line 532): append a parameter and use it; extend the doc comment with one sentence: `/// \`gain\` selects the photometric solve's gain handling (see [\`GainMode\`]).`

```rust
pub fn analyze_ipc_aligned(
    link: Arc<HostLink>,
    session_dir: &Path,
    band_rows: usize,
    surface_order: Option<u32>,
    gain: GainMode,
) -> Result<Session> {
```

and change its tail (~line 574) from `finish_session(session, scans, surface_order, GainMode::Fit)` to:

```rust
    finish_session(session, scans, surface_order, gain)
```

(b) `analyze_ipc_solved` (~line 582): identical treatment — append `gain: GainMode` to the signature, extend the doc comment, and change its tail (~line 674) to `finish_session(session, scans, surface_order, gain)`. Remove any leftover "hardcoded" marker comments at either site.

In `crates/mmm-ipc-worker/src/main.rs`:

(c) In the import list (~line 9), replace `analyze_input_progress` with `analyze_full`.

(d) In the capture block (~line 144), after `let surface_order = init.params.surface_order;` add:

```rust
    let gain = init.params.gain_mode();
```

(e) In the dispatch `match mode` (~line 168–195):

```rust
            JobMode::Aligned => {
                analyze_ipc_aligned(link.clone(), &session_dir, band_rows, surface_order, gain)?
            }
            JobMode::Solved => {
                analyze_ipc_solved(link.clone(), &session_dir, band_rows, surface_order, gain)?
            }
```

and in the `Files` arm replace the `analyze_input_progress(...)` call with:

```rust
                analyze_full(
                    &paths,
                    &session_dir,
                    surface_order,
                    gain,
                    input_select.to_input_select(),
                    Some(&progress),
                )?
```

(`analyze_full` is the existing full-parameter entry point — `analyze_input_progress` is just `analyze_full` with `GainMode::Fit`, `analyze.rs:149-164` — so Files-mode IPC jobs honor the wire field too.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mmm-ipc-worker && cargo test -p mmm-core`
Expected: PASS, including `unity_gain_mode_reaches_the_worker_session` and the unchanged byte-identical test (its jobs say `gain: "fit"`, preserving behavior).

- [ ] **Step 5: Full verification**

Run: `cargo fmt && cargo clippy --all-targets && cargo doc --no-deps 2>&1 | grep -i warn; cargo test`
Expected: no warnings (watch for `missing_docs` on the changed public signatures), all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/mmm-core/src/analyze.rs crates/mmm-ipc-worker/src/main.rs crates/mmm-ipc-worker/tests/end_to_end.rs crates/mmm-ipc-worker/Cargo.toml
git commit -m "feat(ipc): thread wire gain mode into the worker analyze paths"
```

(Drop `Cargo.toml` from the add list if Step 1 needed no dev-dependency change.)

---

### Task 3: Release version bump 1.3.1 → 1.4.0

New wire field + new UI control = minor version, matching the v1.1.0 seam-map precedent (feature commit carries the bump). The exact-match `worker_version` handshake is what protects a mixed-version install: a 1.3.1 host never speaks to a 1.4.0 worker and vice versa.

**Files:**
- Modify: `Cargo.toml` (workspace `version = "1.3.1"` → `"1.4.0"`, line 12)
- Modify: `integration/pixinsight/module/MmmVersion.h` (lines 12–17)
- Modify: `integration/pixinsight/host/mmm_protocol.h` (`kExpectedWorkerVersion`, line 33)
- Modify: `integration/pixinsight/PROTOCOL.md` (example `"worker_version": "1.3.1"` strings, ~lines 225 and 648)

**Interfaces:**
- Consumes: nothing. Produces: version string `"1.4.0"` that Task 4's C++ build and the packaged module rely on. `crates/mmm-ipc-worker/tests/version_sync.rs` is the tripwire.

- [ ] **Step 1: Run the tripwire to see it green first**

Run: `cargo test -p mmm-ipc-worker --test version_sync`
Expected: PASS at 1.3.1 (baseline).

- [ ] **Step 2: Bump all three sources + PROTOCOL.md examples**

- `Cargo.toml` line 12: `version = "1.4.0"`
- `MmmVersion.h`: `MMM_VERSION_MAJOR 1`, `MMM_VERSION_MINOR 4`, `MMM_VERSION_REVISION 0`, `MMM_VERSION_BUILD 1`, `MMM_VERSION_STRING "1.4.0"`
- `mmm_protocol.h` line 33: `kExpectedWorkerVersion = "1.4.0";`
- `PROTOCOL.md`: both example strings `"1.3.1"` → `"1.4.0"` (`grep -n '1\.3\.1' integration/pixinsight/PROTOCOL.md` must come back empty afterwards; also `grep -rn '1\.3\.1' integration/pixinsight/ docs/ README.md --include='*.md' --include='*.h'` to catch stragglers — historical release notes/memory files excepted).

- [ ] **Step 3: Verify**

Run: `cargo test -p mmm-ipc-worker --test version_sync && cargo build -p mmm-ipc-worker`
Expected: PASS; `Cargo.lock` refreshes to 1.4.0 (commit it).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock integration/pixinsight/module/MmmVersion.h integration/pixinsight/host/mmm_protocol.h integration/pixinsight/PROTOCOL.md
git commit -m "chore: bump release version to 1.4.0 (gain-mode wire field + UI)"
```

---

### Task 4: C++ module — gainMode parameter, UI control, JSON serialization

Follows the `blendMode` MetaEnumeration pattern end-to-end. All new literals pure ASCII.

**Files:**
- Modify: `integration/pixinsight/module/MmmParameters.h` (mapping comment ~line 31; new class after `MmmSurfaceOrderParameter`, ~line 297)
- Modify: `integration/pixinsight/module/MmmParameters.cpp` (~line 27)
- Modify: `integration/pixinsight/module/mmm.cpp` (~line 107)
- Modify: `integration/pixinsight/module/MmmProcess.h` (~line 105)
- Modify: `integration/pixinsight/module/MmmProcess.cpp` (ctor ~line 99, `Assign` ~line 127, `LockParameter` ~line 192)
- Modify: `integration/pixinsight/module/MmmExecution.cpp` (`Params` ~line 79, wire-string helper ~line 344, `BuildParams` ~line 370, snapshot ~line 832)
- Modify: `integration/pixinsight/module/MmmInterface.h` (~lines 123–126, ~line 176)
- Modify: `integration/pixinsight/module/MmmInterface.cpp` (setup after SurfaceOrder block ~line 337, sizer ~line 396, `UpdateControls` ~line 523, handler ~line 731)
- Modify: `integration/pixinsight/host/test/golden_harness.h` (`make_params`, ~line 83)

**Interfaces:**
- Consumes: wire field `"gain"` (`"fit"`/`"unity"`) from Task 1.
- Produces: PSM-persisted process parameter `gainMode` (enumeration `Fit`=0/`Unity`=1, default `Fit`); instance member `pcl_enum p_gainMode`.

- [ ] **Step 1: Parameter metadata (`MmmParameters.h` / `.cpp` / `mmm.cpp`)**

`MmmParameters.h` — extend the mapping comment (~line 31) with:

```cpp
//   gainMode           -> params.gain ("fit"/"unity")
```

and add after the `MmmSurfaceOrderParameter` section (~line 297), before bandRows:

```cpp
// ----------------------------------------------------------------------------
// gainMode: photometric gain handling for the analyze stage (params.gain).
// Element ORDER matters for wire serialization; values equal indices.
// ----------------------------------------------------------------------------

/*!
 * \brief Enumeration: photometric gain handling (Fit/Unity).
 */
class MmmGainModeParameter : public MetaEnumeration
{
public:

   enum { Fit, Unity, NumberOfItems, Default = Fit };

   MmmGainModeParameter( MetaProcess* P ) : MetaEnumeration( P ) {}
   IsoString Id() const override { return "gainMode"; }
   size_type NumberOfElements() const override { return NumberOfItems; }
   IsoString ElementId( size_type i ) const override
   {
      switch ( i )
      {
      case Unity: return "Unity";
      default:    return "Fit";
      }
   }
   int ElementValue( size_type i ) const override { return int( i ); }
   size_type DefaultValueIndex() const override { return Default; }
};

extern MmmGainModeParameter* TheMmmGainModeParameter;
```

`MmmParameters.cpp` — after the `TheMmmSurfaceOrderParameter` definition (~line 27):

```cpp
MmmGainModeParameter*      TheMmmGainModeParameter      = nullptr;
```

`mmm.cpp` — after the `TheMmmSurfaceOrderParameter` instantiation (~line 107):

```cpp
      TheMmmGainModeParameter      = new MmmGainModeParameter( TheMmmBlendProcess );
```

- [ ] **Step 2: Instance member (`MmmProcess.h` / `.cpp`)**

`MmmProcess.h` after `p_surfaceOrder` (~line 105):

```cpp
   pcl_enum      p_gainMode;        // Fit / Unity (analyze photometric gains)
```

`MmmProcess.cpp` ctor initializer list, after the `p_surfaceOrder` entry (~line 99):

```cpp
   , p_gainMode( TheMmmGainModeParameter->ElementValue( TheMmmGainModeParameter->DefaultValueIndex() ) )
```

`Assign` (~line 127), after `p_surfaceOrder`:

```cpp
      p_gainMode      = x->p_gainMode;
```

`LockParameter` scalars block (~line 192), after the SurfaceOrder line:

```cpp
   if ( p == TheMmmGainModeParameter )      return &p_gainMode;
```

- [ ] **Step 3: Job JSON (`MmmExecution.cpp`)**

`Params` struct (~line 79), after `surfaceOrder`:

```cpp
   pcl_enum      gainMode;
```

After `BlendModeWireString` (~line 344–352), add:

```cpp
// Wire string for the gain-mode enum (PROTOCOL.md section 6 BlendParamsWire).
const char* GainModeWireString( pcl_enum v )
{
   switch ( v )
   {
   case MmmGainModeParameter::Unity: return "unity";
   default:                          return "fit";
   }
}
```

`BuildParams` (~line 370), after the `surface_order` line:

```cpp
   p["gain"]          = GainModeWireString( in.gainMode );
```

Snapshot in `run_blend` (~line 832), after `p.surfaceOrder`:

```cpp
   p.gainMode       = in.p_gainMode;
```

- [ ] **Step 4: UI (`MmmInterface.h` / `.cpp`)**

`MmmInterface.h` — after the SurfaceOrder members (~line 126):

```cpp
      HorizontalSizer GainMode_Sizer;
      Label           GainMode_Label;
      ComboBox        GainMode_ComboBox;
```

and after `e_SurfaceOrderValueUpdated` (~line 176):

```cpp
   void e_GainModeItemSelected( ComboBox& sender, int itemIndex );
```

`MmmInterface.cpp` — after the SurfaceOrder_Sizer setup block (~line 337):

```cpp
   GainMode_Label.SetText( "Gain mode:" );
   GainMode_Label.SetFixedWidth( labelWidth1 );
   GainMode_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   // Element order MUST match MmmGainModeParameter (Fit=0/Unity=1).
   GainMode_ComboBox.AddItem( "Fit" );
   GainMode_ComboBox.AddItem( "Unity" );
   GainMode_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_GainModeItemSelected, w );
   GainMode_ComboBox.SetMinWidth( editWidth1*2 );
   GainMode_ComboBox.SetToolTip( "<p>How the photometric solve treats per-panel brightness gains.</p>"
      "<p><b>Fit</b> (default) - measure a gain factor for each panel from the overlap regions, "
      "correcting real transparency and exposure differences between panels.</p>"
      "<p><b>Unity</b> - force every gain to 1 and match panels with offsets only. Choose this for "
      "mosaics known to be photometrically homogeneous (same rig, exposure and filter, stable "
      "skies), where a fitted gain could only chase noise.</p>" );

   GainMode_Sizer.SetSpacing( 4 );
   GainMode_Sizer.Add( GainMode_Label );
   GainMode_Sizer.Add( GainMode_ComboBox );
   GainMode_Sizer.AddStretch();
```

Sizer order (~line 396) — insert after the SurfaceOrder entry so the control sits alongside it:

```cpp
   Parameters_Sizer.Add( SurfaceOrder_Sizer );
   Parameters_Sizer.Add( GainMode_Sizer );
```

`UpdateControls` (~line 523), after the SurfaceOrder line:

```cpp
   GUI->GainMode_ComboBox.SetCurrentItem( m_instance.p_gainMode );
```

Handlers (~line 731), after `e_SurfaceOrderValueUpdated`:

```cpp
void MmmBlendInterface::e_GainModeItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_gainMode = pcl_enum( itemIndex );
}
```

- [ ] **Step 5: Golden harness params (`golden_harness.h`)**

In `make_params` (~line 83), after `p["surface_order"] = 2;`:

```cpp
  p["gain"] = "fit";
```

- [ ] **Step 6: Build + run the host test suite**

```bash
source ~/.cargo/env
cargo build -p mmm-ipc-worker
cmake -S integration/pixinsight/host -B integration/pixinsight/host/build
cmake --build integration/pixinsight/host/build -j
ctest --test-dir integration/pixinsight/host/build --output-on-failure
```

Expected: all host tests PASS (golden tests drive the real 1.4.0 worker; version handshake must agree — Task 3 done first).

- [ ] **Step 7: Build the module (if the PCL SDK is present)**

Per `integration/pixinsight/module/README.md`: `cd integration/pixinsight/module && make`. Requires `/opt/PixInsight/include` and `~/.local/pcl-build/lib/libPCL-pxi.a`. If the SDK is not on this machine, note it and rely on CI (`.github/workflows/module.yml`) — do not claim the module compiled.

- [ ] **Step 8: Commit**

```bash
git add integration/pixinsight/module/ integration/pixinsight/host/test/golden_harness.h
git commit -m "feat(pixinsight): Gain Mode process parameter + UI (fit/unity)"
```

---

### Task 5: Documentation

**Files:**
- Modify: `README.md` (`mmm analyze` flags table ~line 133; `mmm report` intro ~line 138)
- Modify: `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html` (parameters table, after the "Gradient fit order" row ~line 160)
- Modify: `docs/DESIGN.md` (one wire sentence in the `--gain` paragraph, ~line 84–87)

**Interfaces:** none (prose only). Terminology fixed by earlier tasks: control label "Gain mode", wire field `gain`, values `fit`/`unity`.

- [ ] **Step 1: README — analyze flag row**

Add to the `mmm analyze` flags table after the `--input` row:

```markdown
| `--gain fit\|unity` | `fit` | Photometric gain handling: `fit` measures per-panel gains from the overlaps and corrects real transparency/exposure differences; `unity` pins every gain at 1 and solves offsets only (same-rig/same-exposure mosaics) |
```

- [ ] **Step 2: README — report diagnostics note**

Extend the `mmm report` section intro ("Print the overlap-graph edge table, …, with ⚠ flags on outliers.") with:

```markdown
In the photometric table a gain shown as `-` means that overlap had too
little shared structure to support a gain measurement (the pair is matched
by level only), and a closing ⚠ warning lists any panels whose solved gains
fall outside [0.5, 2] — on same-rig/same-exposure data that usually means
the overlaps cannot constrain gains, and re-running `mmm analyze` with
`--gain unity` is the fix.
```

- [ ] **Step 3: Module HTML doc — Gain mode row**

In `MegaMergeMosaic.html`, insert after the "Gradient fit order" `</tr>` (~line 160), matching UI order:

```html
<tr>
<td class="name">Gain mode</td>
<td>How the photometric solve treats per-panel brightness gains.
<b>Fit</b> (default) measures a gain factor for each panel from the
overlap regions, correcting real transparency and exposure differences
between panels &mdash; on one 25-panel narrowband survey this recovered
genuine per-panel transparency variation of up to ~2.4&times;.
<b>Unity</b> forces every gain to 1 and matches panels with offsets only;
choose it for mosaics known to be photometrically homogeneous (same rig,
exposure and filter, shot under stable skies), where a fitted gain could
only chase noise. The <code>mmm report</code> CLI command shows the
fitted per-panel corrections, prints <code>-</code> for overlaps without
enough shared structure to measure a gain, and flags
(<code>&#9888;</code>) solved gains outside [0.5,&nbsp;2].</td>
</tr>
```

- [ ] **Step 4: DESIGN.md — wire sentence**

In the `--gain fit|unity` paragraph (~line 84–87), after "…recorded in `session.json` and shown by `report`, which also warns when solved gains leave [0.5, 2].", append:

```markdown
On the PixInsight wire it travels as `BlendParamsWire.gain` (`"fit"`/`"unity"`,
serde-default `"fit"` so pre-field hosts keep working); the module exposes it
as the **Gain mode** control (PROTOCOL.md §6 is the authoritative reference).
```

- [ ] **Step 5: Verify + commit**

Run: `cargo test` (docs shouldn't break anything; cheap sanity). Visually check the HTML renders (open in a browser or inspect the table).

```bash
git add README.md docs/DESIGN.md integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html
git commit -m "docs: --gain fit|unity CLI reference + PixInsight Gain mode doc"
```

---

### Optional manual smoke (not a task gate)

`test_data/rickjay.mmm-session` holds fit-mode photometry from the Barnard's Loop investigation. After `cargo build --release`, re-running `mmm analyze` on that data with `--gain unity` and comparing `mmm report` output (gains all 1, offsets-only note in the header) is a good real-data sanity check — manual only; no test may depend on `test_data/`.
