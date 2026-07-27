# PixInsight Plan 2b — PCL Process/ProcessInterface module + packaging

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the native PixInsight PCL module `mmm-pxm.so` — one global-context Process + one C++ ProcessInterface — that lets a user blend selected in-memory views (or files) with mmm, driving the already-tested Plan-2a host transport library and the `mmm-ipc-worker`, and building the result into a new ImageWindow. No JavaScript/PJSR.

**Architecture:** The module is a **thin PCL wrapper over the Plan-2a `host/` library** (which is fully tested and PixInsight-free). The module's only new logic is: PCL parameter/UI plumbing, enumerating selected views, reading their pixels (an `ImageVariant` → `host::PanelSource` adapter), extracting astrometric properties for solved mode, building the output `ImageWindow` (a `host::OutputCollector` adapter), and wiring `ExecuteGlobal` to `host::Host`. The wire protocol, shm, fault isolation, and byte-identity are already proven in Plan 2a — this plan adds no protocol code.

**Tech Stack:** C++20 (g++ 13.3); the PixInsight PCL (`libPCL-pxi.a`, built locally); a PI-MakefileGenerator-style makefile; the Plan-2a `host/` library (compiled in directly); `dladdr` for the module's own path; POSIX. Verification is **compile+link warning-free** plus a **manual PixInsight smoke test** (the module cannot be behaviorally tested without the PixInsight GUI — the transport underneath already is).

## Global Constraints

- **Branch:** continue on `pixinsight-integration`. Do NOT create a new branch.
- **The authoritative API reference is `integration/pixinsight/PCL_API_REFERENCE.md`** (committed). Every PCL class/method/macro this plan names is documented there with `header:line` cites — implementers MUST read the relevant section before writing each piece. The design contract is spec §10 (UI/params/execution) and §12 (packaging); the wire contract is `integration/pixinsight/PROTOCOL.md`; the host API is `integration/pixinsight/host/mmm_host.h`.
- **`libPCL-pxi.a` location:** built at `~/.local/pcl-build/lib/libPCL-pxi.a` (45 MB, already built). Headers at `/opt/PixInsight/include`. A makefile variable (`PCLLIBDIR`, `PCLINCDIR`) must point there; document defaults and allow override.
- **Compiler flags (mirror the PCL makefile-x64):** `g++ -std=c++20 -fPIC -pthread -D_REENTRANT -D__PCL_LINUX -D__PCL_AVX2 -D__PCL_FMA -mavx2 -mfma -minline-all-stringops -O3 -fvisibility=hidden -fvisibility-inlines-hidden -fnon-call-exceptions -Wall`. Output is a shared object **`mmm-pxm.so`** (the `-pxm.so` module suffix). Link `libPCL-pxi.a` + the compiled `host/` objects; the module's undefined PixInsight-core symbols are resolved by the core at load time, so do **not** pass `-Wl,--no-undefined`.
- **The `host/` library is compiled straight in** (its three `.cpp` are PCL-free): the module makefile compiles `../host/mmm_shm.cpp ../host/mmm_protocol.cpp ../host/mmm_host.cpp` alongside the module sources, with `-I../host -I../host/third_party` on the include path. No PCL header may leak into those files (Plan 2a invariant — unchanged here).
- **ABI types (from PCL_API_REFERENCE.md §3 gotchas):** booleans/enums stored for the core MUST be `pcl_bool`/`pcl_enum` (int32-based), never C++ `bool`/`enum class`. Parameter hooks dispatch by **pointer identity** of the `MetaParameter*` singleton.
- **Global-context process:** override `CanExecuteGlobal(String&)`→true, `ExecuteGlobal()`, `MetaProcess::CanProcessViews()`→false, `PrefersGlobalExecution()`→true, and `ProcessImplementation::CanExecuteOn(const View&,String&)`→false (reference §2).
- **Fault isolation:** any `host::HostError` (worker crash/exit/cancel/protocol) → a clean PixInsight error (message box / `throw Error`), the user's views intact, **no partial output window created**. The host library already guarantees no hang and shm cleanup.
- **Verification per task:** each code task ends with a **warning-free `make`** producing (or updating) `mmm-pxm.so`, plus any stated static checks (`readelf`/`nm`). Tasks 1 and 5 additionally define a **manual PixInsight smoke test** the user runs (the reviewer/controller cannot run the GUI). Do NOT claim behavioral success from compile-only evidence — state exactly what was and wasn't verified.
- Keep `cargo test --workspace` green and the `host/` CTest suite green (Plan 2b touches neither, but don't regress them).

---

## File Structure

`integration/pixinsight/module/` (all new):
- `mmm.cpp` — `MmmModule : MetaModule` + `InstallPixInsightModule` entry point (reference §1).
- `MmmProcess.h`/`.cpp` — `MmmBlendProcess : MetaProcess` (identity, global flags, `Create`/`Clone`) and `MmmBlendInstance : ProcessImplementation` (parameter storage members, the five parameter hooks, `Assign`, `ExecuteGlobal`). Reference §2.
- `MmmParameters.h`/`.cpp` — the `MetaParameter` singleton subclasses (the `inputImages` table + `viewId` column, `filePaths` table + `path` column, `inputSelect`/`blendMode` enums, `sessionDir`, `feather`, `flatten`+`flattenEnabled`, `roi`+`roiEnabled`, `downsample`, `defectVeto`, `surfaceOrder`, `bandRows`). Reference §3.
- `MmmInterface.h`/`.cpp` — `MmmBlendInterface : ProcessInterface` (the UI). Reference §4.
- `MmmExecution.h`/`.cpp` — the `ExecuteGlobal` implementation body (enumerate views, build the host job, run it, build the window), factored out of the instance for readability.
- `ViewPanelSource.h`/`.cpp` — `class ViewPanelSource : public mmm::PanelSource` over `ImageVariant`. Reference §5.
- `ImageWindowCollector.h`/`.cpp` — `class ImageWindowCollector : public mmm::OutputCollector` building a new `ImageWindow`. Reference §7.
- `AstrometryProps.h`/`.cpp` — extract `PCL:AstrometricSolution:*` view properties into an `nlohmann::json` array matching the Rust `XisfProperty` JSON (PROTOCOL.md §6). Reference §6.
- `Makefile` + `makefile-x64` — PI-MakefileGenerator-style; builds `mmm-pxm.so`.
- `README.md` — one-time `libPCL-pxi.a` build, module build, dev install, worker-beside-module, and the manual smoke-test checklist.

---

## Task 1: Build chain + minimal loadable module (de-risk)

**Why first:** PCL_API_REFERENCE.md flags that the module-registration skeleton and parameter dispatch are *synthesized from headers* (no complete example module exists in the install). A minimal module that actually compiles, links against `libPCL-pxi.a`, and **loads in PixInsight** validates the entire skeleton + build chain before any real functionality is built on it.

**Files:**
- Create: `integration/pixinsight/module/mmm.cpp`, `MmmProcess.{h,cpp}`, `MmmInterface.{h,cpp}` (trivial versions), `Makefile`, `makefile-x64`.

**Interfaces:**
- Produces: `mmm-pxm.so` exporting `InstallPixInsightModule` (+ the other two entry points from reference §1). A global-context process `MosaicMerge` (pick the final Id; `Categories()` e.g. `"Geometry"` or `"Mosaic"`) whose `ExecuteGlobal()` currently just returns `true` (no-op), and a bare `ProcessInterface`.

- [ ] **Step 1: Write `makefile-x64`** mirroring `/opt/PixInsight/src/pcl/linux/g++/makefile-x64`'s flags (see Global Constraints), with: `PCLINCDIR ?= /opt/PixInsight/include`, `PCLLIBDIR ?= $(HOME)/.local/pcl-build/lib`, `HOSTDIR = ../host`. Compile each `module/*.cpp` and `$(HOSTDIR)/{mmm_shm,mmm_protocol,mmm_host}.cpp` to objects with `-I$(PCLINCDIR) -I$(HOSTDIR) -I$(HOSTDIR)/third_party`, then link:
  `g++ -shared -fPIC -o mmm-pxm.so $(OBJS) -L$(PCLLIBDIR) -lPCL-pxi -lpthread -lrt`.
  Add a `Makefile` that just `$(MAKE) -f makefile-x64`. (Consult the real makefile-x64 for the exact link line PixInsight uses — match it; do not add `--no-undefined`.)

- [ ] **Step 2: Write the trivial `MmmProcess.{h,cpp}`** — `MmmBlendProcess : public pcl::MetaProcess` with `Id()`, `Category()`/`Categories()`, `Version()`, `Description()`, `Create()` (returns `new MmmBlendInstance(this)`), `Clone()`, `CanProcessViews()`→false, `PrefersGlobalExecution()`→true, `DefaultInterface()`→`TheMmmBlendInterface`. `MmmBlendInstance : public pcl::ProcessImplementation` with the two ctors, `CanExecuteGlobal(String&)`→true, `CanExecuteOn(const View&,String&)`→false, `ExecuteGlobal()`→`return true;`, and minimal `Assign`. Follow reference §2 exactly (these methods `throw` at runtime if wrong, so match signatures precisely). Declare `extern MmmBlendProcess* TheMmmBlendProcess;`.

- [ ] **Step 3: Write the trivial `MmmInterface.{h,cpp}`** — `MmmBlendInterface : public pcl::ProcessInterface` with `Id()`, `Process()` (returns `*TheMmmBlendProcess`), `NewProcess()`, a minimal `Launch`/`ProcessInterface` overrides per reference §4, and a stub `ClientData`/control that just creates an empty `Control`. Declare `extern MmmBlendInterface* TheMmmBlendInterface;`.

- [ ] **Step 4: Write `mmm.cpp`** — the `MmmModule : pcl::MetaModule` (Version/Name/Description) and `InstallPixInsightModule(int32 mode)` that `new`s the module then (on `FullInstall`) `new`s the process + interface singletons, per reference §1's skeleton.

- [ ] **Step 5: Build**

Run: `cd integration/pixinsight/module && make 2>&1 | tail -20`
Expected: compiles + links **warning-free** into `mmm-pxm.so`. Fix any error (the most likely: a `throw`-on-purpose default not overridden won't fail the *compile* — but a missing pure-virtual `Id()`/`Create()`/`Clone()` will; and undefined PCL symbols at link that are NOT core-provided indicate a missing source file).

- [ ] **Step 6: Static verification**

Run: `readelf -d integration/pixinsight/module/mmm-pxm.so | head; nm -D integration/pixinsight/module/mmm-pxm.so | grep -i InstallPixInsightModule`
Expected: `mmm-pxm.so` is a valid shared object exporting `InstallPixInsightModule`. Note in the report which PCL/core symbols remain undefined (expected — the core resolves them at load).

- [ ] **Step 7: Commit + write the manual load-test procedure**

```bash
git add integration/pixinsight/module && git commit -m "feat(pxm): minimal loadable PCL module skeleton + build chain

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
In the report, write the exact manual steps for the user: copy `mmm-pxm.so` (and later the worker) into a folder, PixInsight → Process → Modules → Install Modules → select the folder → confirm the `MosaicMerge` process appears in its category without error. **Flag clearly that this manual load is the real validation of the synthesized skeleton — the controller cannot run it.**

---

## Task 2: Process parameters

**Files:**
- Create: `integration/pixinsight/module/MmmParameters.{h,cpp}`
- Modify: `MmmProcess.{h,cpp}` (add storage members + the five parameter hooks + `Assign`).

**Interfaces:**
- Produces: the parameter singletons (`TheMmmInputImagesParameter`/`TheMmmViewIdParameter`, `TheMmmFilePathsParameter`/`TheMmmPathParameter`, `TheMmmInputSelectParameter`, `TheMmmSessionDirParameter`, `TheMmmFeatherParameter`, `TheMmmBlendModeParameter`, `TheMmmFlattenParameter`/`TheMmmFlattenEnabledParameter`, `TheMmmRoiX0..Y1`/`TheMmmRoiEnabledParameter`, `TheMmmDownsampleParameter`, `TheMmmDefectVetoParameter`, `TheMmmSurfaceOrderParameter`, `TheMmmBandRowsParameter`). Instance storage: `Array<String> p_viewIds; Array<String> p_filePaths; pcl_enum p_inputSelect; String p_sessionDir; float p_feather; pcl_enum p_blendMode; int32 p_flatten; pcl_bool p_flattenEnabled; int32 p_roi[4]; pcl_bool p_roiEnabled; int32 p_downsample; pcl_bool p_defectVeto; int32 p_surfaceOrder; int32 p_bandRows;` (map to spec §10.1 / PROTOCOL.md `BlendParamsWire`).

- [ ] **Step 1: Write `MmmParameters.{h,cpp}`.** Declare each `MetaParameter` subclass per reference §3: the two `MetaTable`s each own one `MetaString` column (reference §3 "MetaTable — the view-id LIST mechanism", copy that pattern verbatim); `inputSelect` and `blendMode` are `MetaEnumeration` (elements: inputSelect = Auto/Aligned/Solved with `DefaultValueIndex`→Auto; blendMode = Feather/TwoBand/Pyramid with default Pyramid — match the CLI mapping and PROTOCOL.md `params.mode` strings); `feather` a `MetaFloat`; `flatten`/`roi*`/`downsample`/`surfaceOrder`/`bandRows` `MetaInt32`; `flattenEnabled`/`roiEnabled`/`defectVeto` `MetaBoolean`; `sessionDir` a `MetaString`. Give each a stable `Id()` and sensible defaults (feather 256, downsample 1, bandRows 256, surfaceOrder 2, defectVeto true, blendMode Pyramid, inputSelect Auto, flattenEnabled/roiEnabled false).

- [ ] **Step 2: Add instance storage + register singletons.** Add the members above to `MmmBlendInstance`; construct the parameter singletons in `InstallPixInsightModule` in dependency order (tables before their columns), each `new`'d with the process (or table) as parent (reference §3).

- [ ] **Step 3: Implement the five parameter hooks** on `MmmBlendInstance`: `LockParameter`, `AllocateParameter`, `ParameterLength`, and (default-ok) `ValidateParameter`/`UnlockParameter`, dispatching by `p ==` pointer identity per reference §3 "How ProcessImplementation actually stores/exposes the list" — including the table-vs-column duality for `inputImages`/`viewId` and `filePaths`/`path` (table `p` → row count / resize; string column `p` → `p_viewIds[tableRow].Begin()` / char length). Implement `Assign` to copy all members.

- [ ] **Step 4: Build warning-free**

Run: `cd integration/pixinsight/module && make 2>&1 | tail -20`
Expected: clean build of `mmm-pxm.so`.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/module && git commit -m "feat(pxm): process parameters (view/file lists, mode, blend params)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
(Behavioral check of parameter round-tripping is part of Task 5's manual smoke test / optional PJSR inspection; note this in the report.)

---

## Task 3: ProcessInterface UI

**Files:**
- Modify: `integration/pixinsight/module/MmmInterface.{h,cpp}` (build the real UI).

**Interfaces:**
- Consumes: the parameter storage on `MmmBlendInstance` (Task 2); reference §4 control classes.
- Produces: a working non-modal interface bound to the instance via `ImportProcess`/`ExportProcess`-style sync (reference §4 `ProcessInterface` overrides).

- [ ] **Step 1: Build the control tree** in a `GUIData`-style struct (reference §4 idiom), using `VerticalSizer`/`HorizontalSizer`: a **view multi-selector** (a `TreeBox` or list populated from `ImageWindow::AllWindows()`, plus an "Add views…" `PushButton` opening `MultiViewSelectionDialog` — reference §4 says use that dialog, don't build one) with a **Files toggle** (a `PushButton`/`RadioButton` pair switching between the view list and a files list); a **session-dir** `Edit` + a "…" `PushButton` opening `GetDirectoryDialog` (reference §4 FileDialog family); an **Input** `ComboBox` (Auto/Aligned/Solved); `NumericControl`s for feather / downsample / surfaceOrder / bandRows; a **blend-mode** `ComboBox`; `CheckBox`es for defectVeto / flattenEnabled (+ its order `SpinBox`) / roiEnabled (+ four ROI `NumericEdit`s); and a **progress** `Label` + **Cancel** `PushButton` area.

- [ ] **Step 2: Wire events** using the direct `OnXxx(handler, receiver)` pattern (reference §4 "Event-wiring idiom — no `__CLASS_HANDLER` macro"): each control's change handler writes into `TheMmmBlendInstance`'s members (via the interface's current instance pointer) and calls `UpdateControls()`; enable/disable the view-list vs files-list per the toggle; enable the flatten order / ROI edits per their checkboxes.

- [ ] **Step 3: Implement the `ProcessInterface` sync overrides** — `Launch`, `NewProcess` (returns a `new MmmBlendInstance`), `ImportProcess`/`ValidateProcess` (adopt an instance's values into the controls), and the `InterfaceFeatures` needed for a global process (reference §4). Populate the view list on launch.

- [ ] **Step 4: Build warning-free**

Run: `cd integration/pixinsight/module && make 2>&1 | tail -20`
Expected: clean `mmm-pxm.so`.

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/module && git commit -m "feat(pxm): ProcessInterface UI (view/files selection, params, progress+cancel)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: PCL↔host adapters (pixels, output, astrometric props)

**Files:**
- Create: `integration/pixinsight/module/ViewPanelSource.{h,cpp}`, `ImageWindowCollector.{h,cpp}`, `AstrometryProps.{h,cpp}`.

**Interfaces:**
- Consumes: `mmm::PanelSource`, `mmm::OutputCollector` (from `../host/mmm_host.h`); PCL `View`/`ImageVariant`/`ImageWindow`/`Property` (reference §5–§7).
- Produces:
  - `class ViewPanelSource : public mmm::PanelSource` — ctor takes the ordered `Array<View>` (or their locked `ImageVariant`s); `fill_band(panel_id, y0, y1, float* dst)` copies planar rows of every channel into `dst` (native-endian `index(c,r,x)=c*rows*w+r*w+x`), converting to Float32 if the view's sample format differs (branch on `IsFloatSample()`/`BitsPerSample()` — reference §5). Returns false on out-of-range.
  - `class ImageWindowCollector : public mmm::OutputCollector` — `begin(w,h,ch)` creates one `ImageWindow(w,h,ch,32,true,ch>=3,...)` (reference §7); `band(y0,rows,planar,width,ch)` writes into `MainView().Image()`; exposes the finished `ImageWindow` for the caller to `Show()`.
  - `nlohmann::json extract_astrometry_props(const View&)` — returns the `properties` JSON array for a solved panel: `View::Properties()` filtered to ids starting `PCL:AstrometricSolution:` (reference §6 has the exact id list), each mapped to the Rust `XisfProperty` JSON shape `{id, type_, value, location}` with `value` a `PropertyValue` variant (`{"F64":..}`/`{"Str":..}`/`{"F64Vec":..}`/`{"F64Mat":{rows,cols,data}}`/`{"I64":..}`) — match PROTOCOL.md §6 exactly (the worker parses these with serde). Enforce the finite-float precondition is left to `encode_init`, but prefer to skip/emit finite values.

- [ ] **Step 1: Write `ViewPanelSource`** per reference §5 (planar access via `ImageVariant`/`pcl::Image`, per-channel `memcpy` where Float32, element convert otherwise). Doc-comment the sample-format handling.

- [ ] **Step 2: Write `ImageWindowCollector`** per reference §7 (create window in `begin`, fill in `band`, hold the `ImageWindow`).

- [ ] **Step 3: Write `AstrometryProps`** per reference §6 + PROTOCOL.md §6. Map each PCL `Variant` property value to the matching `PropertyValue` JSON variant; get the type string (`type_`) from the property's declared type. Cross-check the JSON shape against `crates/mmm-core/src/ipc/protocol.rs`'s `XisfProperty`/`PropertyValue` serde (the worker must parse it) — this is the highest-risk mapping; document the field-by-field correspondence in the report.

- [ ] **Step 4: Build warning-free**

Run: `cd integration/pixinsight/module && make 2>&1 | tail -20`
Expected: clean `mmm-pxm.so` (adapters compiled in; not yet called until Task 5).

- [ ] **Step 5: Commit**

```bash
git add integration/pixinsight/module && git commit -m "feat(pxm): ImageVariant PanelSource, ImageWindow collector, astrometry props

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: ExecuteGlobal wiring + worker path + smoke test

**Files:**
- Create: `integration/pixinsight/module/MmmExecution.{h,cpp}`
- Modify: `MmmProcess.cpp` (`MmmBlendInstance::ExecuteGlobal` calls into `MmmExecution`).

**Interfaces:**
- Consumes: everything above + `mmm::Host`, `mmm::HostConfig`, `mmm::SlotLayout`, `mmm::Host::probe_frame` (from `../host/mmm_host.h`); the `host/` Init JSON builder path (`encode_init` takes an `{"Init":{...}}` nlohmann object — build it here from the instance's params + PanelDescs).

- [ ] **Step 1: Implement `run_blend(MmmBlendInstance&)`** in `MmmExecution.cpp`, following spec §10.1's execution flow:
  1. Validate the selection: non-empty and a single input type (all views or all files); else `throw Error(...)`.
  2. Resolve the effective `JobMode` from `p_inputSelect` + input type (views: geometry-uniform ⇒ Aligned else Solved, unless forced; files: pass `input_select` through to `JobMode::Files`) — see spec §10.1.
  3. Build ordered `PanelDesc` JSON (per-view width/height/channels; in Solved mode attach `extract_astrometry_props(view)`).
  4. Size `slot_bytes`: Aligned/Files → `canvas_width * ch * band_rows * 4`; Solved → `max(max_panel_width, probe_w) * ch * band_rows * 4` where `probe_w` comes from `mmm::Host::probe_frame(worker_path, initObjForProbe, ...)`.
  5. Resolve `worker_path` via `dladdr` on a module symbol → sibling `mmm-ipc-worker` (reference §8 — no PCL API for the module path).
  6. Build the full `{"Init":{...}}` object (shm_name unique, slot counts, canvas, panels, mode, session_dir from `p_sessionDir`, params from the instance). Construct `mmm::HostConfig`, a `ViewPanelSource`, an `ImageWindowCollector`, and a `ProgressCallback` that updates the interface's progress Label + checks an abort flag; run `mmm::Host(...).run()`.
  7. On success, `Show()` the collector's `ImageWindow`. On any `mmm::HostError`/exception → `throw Error(e.what())` (PixInsight surfaces it), and ensure **no** window was shown.

- [ ] **Step 2: Wire progress + cancel** — the interface's Cancel button calls `Host::cancel()` (thread-safe); the `ProgressCallback` maps `Progress{stage,done,total}` to the Label. If running `ExecuteGlobal` on the GUI thread blocks the UI, use `pcl::Thread` per reference §9 (document the choice; a modal progress dialog is an acceptable v1).

- [ ] **Step 3: `MmmBlendInstance::ExecuteGlobal()`** calls `run_blend(*this); return true;` (catching → rethrow as `Error`).

- [ ] **Step 4: Build warning-free → complete module**

Run: `cd integration/pixinsight/module && make 2>&1 | tail -20`
Expected: clean `mmm-pxm.so`.

- [ ] **Step 5: Commit + the manual smoke test**

```bash
git add integration/pixinsight/module && git commit -m "feat(pxm): ExecuteGlobal drives host over shm, builds output window

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```
In the report, write the **manual PixInsight smoke-test checklist** for the user (this is the real functional verification — the controller cannot run it): (a) build the worker (`cargo build --release -p mmm-ipc-worker`) and place `mmm-ipc-worker` beside `mmm-pxm.so`; (b) Install Modules; (c) open ≥2 aligned full-canvas views (MosaicByCoordinates output), select them, pick a session dir, Input=Auto, run → a new blended window appears; (d) repeat with raw solved panels (Input=Auto→Solved); (e) Files mode on on-disk panels; (f) induce a worker failure (e.g. rename the worker binary) → a clean PixInsight error, no partial window, PixInsight still alive. Ask the user to report results.

---

## Task 6: Build automation, packaging, README, spec update

**Files:**
- Create: `integration/pixinsight/module/README.md`
- Modify: `docs/superpowers/specs/2026-07-27-pixinsight-integration-design.md` (§12 status → implemented for dev).

- [ ] **Step 1: Write `module/README.md`** — the one-time `libPCL-pxi.a` build (copy `/opt/PixInsight/src/{pcl,3rdparty}` to a writable dir, set `PCLSRCDIR`/`PCLINCDIR`/`PCLLIBDIR64`, `make`; note it lands at `~/.local/pcl-build/lib`), the module build (`make` in `module/`), the `PCLINCDIR`/`PCLLIBDIR` overrides, placing `mmm-ipc-worker` beside `mmm-pxm.so`, the dev **Install Modules** flow, and a pointer to the Task-5 smoke-test checklist. Note the future repository/signing path (spec §12) is not implemented.

- [ ] **Step 2: Update spec §12** — mark the Linux/WSL dev-distribution path as implemented (module builds + loads via Install Modules; worker ships beside), leaving the repository/signing/cross-platform items as the remaining forward work.

- [ ] **Step 3: Final build sweep**

Run: `cd integration/pixinsight/module && make clean >/dev/null 2>&1; make 2>&1 | grep -iE "warning|error" || echo "(clean)"; ls -la mmm-pxm.so`
Also confirm the rest of the repo is untouched-green: `source ~/.cargo/env && cargo test --workspace 2>&1 | grep -c "test result: FAILED"` (expect 0) and the host CTest still passes.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "docs(pxm): module build/install README + spec §12 dev-distribution status

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review notes (for the executor)

- **Spec coverage:** Task 1 ↔ §14 (module wrapper) + reference §1–2; Task 2 ↔ §10.1 parameter table + reference §3; Task 3 ↔ §10 controls + reference §4; Task 4 ↔ §7 output / §11 solved props / reference §5–7; Task 5 ↔ §10.1 execution flow + §7 no-doubling + §9 isolation; Task 6 ↔ §12 packaging.
- **Verification honesty:** module-side tasks are **compile+link verified only**; behavioral proof is the manual PixInsight smoke test in Tasks 1 and 5 (the transport underneath is already automatically proven in Plan 2a). Every task report must state what was compiled/linked vs what still needs the manual GUI check. Do not mark a task "done, working" on compile evidence alone.
- **Highest risks (flag in reviews):** (a) the synthesized registration skeleton — Task 1's manual load is the gate; (b) the `MetaParameter` pointer-identity dispatch + `pcl_bool`/`pcl_enum` ABI types (reference §3 gotchas); (c) the astrometric `Property`→`XisfProperty` JSON mapping (Task 4 — must match the Rust serde shape the worker parses; cross-check against `protocol.rs`).
- **Type consistency:** the C++ Init JSON built in Task 5 must match `mmm::encode_init`'s expected `{"Init":{...}}` shape and PROTOCOL.md §6 field names exactly (snake_case, `type_` with trailing underscore, `protocol_version: 2`).
