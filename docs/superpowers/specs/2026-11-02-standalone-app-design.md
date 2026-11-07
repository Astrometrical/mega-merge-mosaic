# Mega Merge Mosaic — standalone desktop application

Status: design, 2026-11-02. Brainstormed with the user; mockups from the
session are in `.superpowers/brainstorm/` (gitignored, local only).

## 1. Intent

Mega Merge Mosaic aims to be *the* premier mosaic assembly tool: loved for
its performance, its exceptional blending, and a stunning user experience.
The engine already delivers the first two. This application delivers the
third.

The PixInsight module is no longer the way users reach mmm: its development
is eased off and shelving is expected (DESIGN.md "PixInsight module
status"). The standalone application becomes the canonical frontend.

What hurts users today, and what this app must never reintroduce:

- **Old-school stitching:** long waits, manual babysitting, and rework that
  means starting over (or resuming from a partial mosaic you happened to
  save). mmm's speed and cached sessions already fix this; the app must not
  bring it back through a blocking or clumsy UI.
- **The invisible plan:** no tool shows how the panels line up *before*
  stitching. With 100 panels (a real, current user case) you cannot see
  that every panel is loaded, in the right place, in the right group, or
  that one has a defect you would not otherwise notice. **Seeing the plan
  and verifying input quality** is the app's headline capability.

Technical excellence where it really matters is the Astrometrical brand.
Astrometrical (the publishing platform) hosts multi-gigapixel images,
including Vera C. Rubin Observatory images, without appearing to approach
any resource limit. This app must feel the same with massive mosaics: that
is both good UX and brand consistency.

## 2. Goals, non-goals, anti-goals

### Goals (v1)

1. A non-blocking UI that shows clear status and progress at all times.
2. Highly interactive assembly: a spatial view of the plan, previews, ROI
   blending.
3. Complex mosaics (many panels × several filters) are intuitive.
4. Checks that flag input problems (and confirm what passed).
5. Massive mosaics: tiled, pyramid-based viewing whose cost does not grow
   with canvas size.
6. A display stretch with automatic and manual control.
7. Human-readable mosaic health stats, plus technical stats (memory,
   timings, CPU).
8. Sessions that persist everything: revisit, add panels, tweak, re-blend;
   a provenance record.
9. Astrometrical branding.

### Non-goals for v1 (planned later)

- Panel preprocessing before the merge: gradient correction, SPCC-type
  colour calibration, aberration correction. These may call external CLIs
  (GraXpert, and RC-Astro's BlurXTerminator and siblings, which now have
  CLIs; the user supplies any licences) or own implementations.
- Session purge options (borrowed from Mega Image Integrator).
- Before/after comparison (blink or swipe) when blend settings change.
- In-memory (non-persisted) ROI blending as a performance optimisation.
- Composition colour tools beyond per-channel scaling (e.g. a
  luminance-masked saturation adjustment to tame sky colour casts).
- A "shared with Plan" option for the Blend tab's display stretch.
- Sharing the session cache on a separate (scratch) drive.
- "Add folder" when adding files.
- Undo (probably wanted for grouping and settings; not planned around yet).
- macOS builds (Windows and Linux first; see §12).
- Remote processing, servers, web clients, multi-user. These belong to
  Photometrical and workflow-platform, not this app.
- **Processing-scale rework** (user ruling 2026-11-02: no significant
  changes to the core algorithms in v1). Each panel's L8 summary is a dense
  canvas-sized grid (`summary.rs`), roughly 1.3 GB per OSC panel on a
  3-gigapixel canvas, and the overlap graph is built pairwise over those
  grids. That breaks "never dense over the canvas" at that scale. A later
  iteration makes summaries bbox-local per panel (touching analyze,
  photometry, seams and blend) and adds a synthetic multi-gigapixel,
  multi-group mosaic to test it. v1 processes what mmm processes today,
  which already includes a user's 80-panel OSC mosaic.
- **Seams shared across groups.** Each group solves its own seams today;
  in compositions, differing seams per filter can show as fringing or
  steps. A joint seam solve is a future engine feature.
- **Per-group blend settings in the UI.** The configuration and CLI
  support them in v1 (§7.2); the app edits only the mosaic-wide set.
- **Disk budget, tile compression, reduced-precision result, a separate
  cache location.** v1 stores everything uncompressed at full precision
  in the session folder.

### Anti-goals (failure even if it technically works)

- **Lack of interactivity.** A GUI that blocks, or handles large images
  poorly, is the single biggest miss the user can imagine. §8 turns this
  into rules and measurable targets.
- Anything that feels like old-school stitching (see §1).
- A spreadsheet-like UI.
- PixInsight terminology: say "display stretch", never "STF".

## 3. Decisions already made

- **In-process Rust desktop app on egui/eframe**, linking `mmm-core`
  directly. No worker process, no server. (`mmm-ipc-worker` remains the
  ready-made out-of-process path if job isolation is ever needed; see
  DESIGN.md "IPC transport".)
- **Name:** "Mega Merge Mosaic" for the app; `mmm` stays the CLI.
- **Reuse:** UI components are written so generic ones can later be lifted
  into workflow-platform or Photometrical. Photometrical will dynamically
  load modules that supply graph nodes and custom UI; its current rule of
  not depending on egui is deliberate. See §10.
- **One session per app instance** (a document model). mmm is
  interactive, not batch; there is no session list or job queue as in
  mii's PixInsight module.
- **All result tiles are disk-backed** in a memory-mapped tiled file in the
  session (§7). No in-memory ROI mode in v1.
- **Pan/zoom feel copies OpenSeadragon's spring model**, for familiarity
  with Astrometrical's OSD viewer.
- **Exports are explicit.** The session holds a tiled result, not a
  deliverable; FITS/XISF/TIFF are produced by Export.

## 4. Branding and look

> Superseded by `2026-11-07-ui-standard-and-start-screen-design.md`.

- Dark theme and colours from `../astrometrical/apps/web/src/styles/app.css`:
  - Surfaces: `#050505` (astro-900), `#0e0e0e` (800), `#1a1a1a` (700),
    borders `#2a2a2a` (600).
  - Text: strong `#eeeeee`, body `#aaaaaa`, muted `#999999`, subtle
    `#8a8a8a`. These meet WCAG AA on the dark surfaces; keep that.
  - Primary accent OIII: `#4EBCD4` / `#2A95AB` / `#1D7588`: buttons,
    active states, selection, progress.
  - Hα `#C83050`: problems and alerts (on the web it is badges only; here
    it marks failed checks).
  - OIII green `#4ED090` / `#2AB875` (and darker `#165E3E`, `#0C3825`):
    success and passed checks.
- The Angora typeface is used only in the logo and wordmark, brought in as
  images (`../astrometrical/apps/web/src/assets/Logo-White.png` and kin),
  not as a font. UI text uses egui's default sans.
- A small "by Astrometrical" alongside the wordmark ties the Mega family
  together.
- The theme lives in the generic UI crate (§10), so a future mii app and
  Photometrical-hosted components share it.

## 5. The application

Three tabs: **Panels**, **Plan**, **Blend**. They are not a wizard: the
user can move between them at any time and iterate (back to Panels to
regroup, back to Plan to recheck, and so on).

A persistent **status bar** on every tab shows what is running and its
progress, check totals (problems, notes, passed), canvas and image sizes
**with megapixels** (e.g. "Canvas 70 000 × 48 000 (3 360 MP)"), and a
compact technical readout (memory, CPU) that expands into a panel with
per-stage timings.

### 5.1 Start screen

> Superseded by `2026-11-07-ui-standard-and-start-screen-design.md`.

On launch, with no session open: **New mosaic…**, **Open…**, and a list of
recent sessions (thumbnail from the session's preview pyramid, panel
count, last opened, location). Opening another session opens a second
window.

### 5.2 Panels tab

Selecting files and grouping them. No image previews. It must look good,
not like a spreadsheet: file cards, and groups as columns of cards.

- **Add files…** opens a multi-select file dialog; drag and drop files
  anywhere too, if eframe supports it on the platform. No "add folder" in
  v1.
- **Grouping is a switch, off by default.** Off: a plain list of files,
  no mention of groups. This is the common OSC RGB case. Behind the
  scenes the mosaic is then a single group, and Plan and Blend hide group
  and composition controls.
- **Grouping on:** the user picks a rule:
  1. a FITS/XISF **header keyword** (FILTER by default), or
  2. a **filename token**: given the keyword `filter`, the token after it
     becomes the group, so `xxx_filter_Ha_yyy` → `Ha`.
- Files the rule cannot place go to **"No group"**, shown **first (left)**
  as work to do ("3 to sort"). It may also be a valid group in its own
  right; the user decides.
- **Manual placement:** multi-select files, then **Move to** a group, or
  **Add to new group…** (which is also how groups are created by hand),
  or **Remove**.
- Changing the rule regroups automatically placed files; files placed by
  hand stay where the user put them. (Builder's call on presentation.)
- If grouping is off but the files' FILTER headers differ, suggest
  turning grouping on (a check, §6).
- Each group gets a colour, carried through to Plan and Blend and used as
  its default composition channel colour.
- Reading headers runs in the background with progress; the same pass
  queues preview pyramid builds for Plan.
- **Analysis runs by itself** (builder's call, made in review): once files
  are in, the background job system reads headers, builds previews, then
  analyses (summaries, overlap graph, photometric solve, reprojection for
  raw solved panels) without the user pressing anything. Edits in Panels
  reschedule what they affect. Checks appear as the work that produces them
  finishes.

### 5.3 Plan tab

A spatial view of every panel in place, drawn from low-resolution previews
(built in the background as tile pyramids, appearing on the canvas as each
completes), with panel outlines and numbers. Rough is fine: gradients will
show. Raw solved panels are placed by a cheap low-resolution warp from
their header WCS, not by waiting for full reprojection (builder's call,
made in review). Its job is confidence that the right panels are in the right places,
the groups are right, and flagged issues are understood, before investing
in a blend.

- **View selector:** each group, plus **user-defined compositions**.
  A composition maps groups to display channels with a weight each (e.g.
  HOO: R ← Hα, G ← OIII, B ← OIII; or SHO, RGB from mono R/G/B). It is a
  layer mix in the renderer over the per-group preview pyramids: no pixels
  are written. Users may define several; they are saved in the session and
  are available in Blend too. For mono imagers this is the first colour
  view they ever get before all mosaics are stacked.
- **Preview overlap handling:** how overlapping previews combine (e.g.
  topmost or average) is a Plan setting. Builder picks the default.
- **Overlays:** panel outlines, panel numbers, overlap heatmap (toggles).
- **Selection:** click selects a panel; double-click (or a "zoom to
  selection" button) zooms to it; Esc deselects. The side panel shows its
  properties: file name and **full path with "Show in folder" and "Copy
  path"** (so the user can find and fix the file), dimensions with MP,
  channels, centre, scale, rotation, astrometric solution type, and which
  panels it overlaps.
- **Viewer controls:** zoom in/out, home (view all), zoom to selection;
  more will emerge in the building.
- **Checks list** (§6) beside the canvas; selecting a panel filters it to
  that panel's checks, "show all" clears the filter. Flagged panels carry
  a marker on the canvas.
- **Display stretch** for Plan, independent of Blend's (§5.5).

### 5.4 Blend tab

Looks like Plan (same spatial view, groups, compositions, viewer
controls) but shows the real blended result, read from the session's
tiled result (§7).

- **Blend settings** panel: mode, photometry, star lock, flatten, seam
  transition, and the rest of mmm's blend/analyze options. Settings
  changed since the last blend are marked.
- **Two ways to blend:**
  1. **Blend whole mosaic** (the batch path): every group.
  2. **Blend ROI**: just the region of interest (the interactive path),
     for the groups in the current view (one group, or a composition's
     groups).
- The UI edits one mosaic-wide set of settings. Per-group overrides exist
  in the configuration (§7.2) and are respected, but the v1 UI does not
  edit them; it shows that a group has overrides.
- **ROIs:** drawn on the canvas, named, with an optional note ("faint kink
  in the trapezium spike"); several per session; saved in the session so
  the user can come back to a problem spot. Each shows its size with MP
  and whether it is fresh or stale.
- **Staleness:** when a setting changes, affected tiles are flagged
  invalid, not deleted. Stale tiles stay visible, dimmed and hatched, until
  a later blend overwrites and re-validates them. The UI says what a change
  costs before the user commits ("312 of 340 tiles go stale; your ROI
  reblends in about 4 s").
- Settings come in two kinds, and the UI says which the user is touching:
  - **blend-local** (seam transition, mode, …): only the ROI needs
    reblending;
  - **global** (photometric solve, flatten, …): every tile goes stale and
    analysis may rerun first.
- An ROI blend reads a margin around the ROI so its result matches a full
  blend there.
- **Views:** groups, compositions, and a seam map overlay.
- **Health** panel (§6.2).
- **Export…** (§7.3).
- The intended loop: set options → blend all (or a big ROI) → inspect →
  narrow an ROI around a problem → adjust and reblend → repeat → final full
  blend → export. If blend settings cannot fix a problem, the user goes back
  to the input data (or, post-v1, preprocessing).

### 5.5 Display stretch

The term is "display stretch" everywhere.

- **One stretch for all panels in a view**, never per panel, so a panel
  with a dim sky *looks* dim. (Per-panel auto-stretching in PixInsight
  hides exactly the mismatch the Plan view should reveal.)
- **Automatic by default.** Algorithm, written independently from the
  public description of PixInsight's auto-stretch: shadows clip at median −
  2.8 × normalised MAD; midtones balance chosen so the median background
  lands at 0.25; highlights at 1.0. Statistics ignore no-data zeros.
  Photometrical already has a Midtone Stretch node; share the maths rather
  than duplicate it if practical.
- **Unlinked (per channel) by default** for colour, which neutralises the
  usual OSC green cast and background.
- **Auto from selection:** with a panel selected, the auto button computes
  the stretch from that panel's statistics and applies it to all panels.
  With nothing selected, auto uses the whole view's statistics.
- **Manual:** black point, white point, stretch factor (midtones), plus any
  other parameters the stretch needs; a small histogram with draggable
  black and white points.
- **Compositions** are treated like RGB initially, plus a scale per
  channel.
- Each view (group or composition) keeps its own stretch, **separately for
  Plan and for Blend**, saved in the session.
- Applied only at draw time (GPU shader), so slider changes appear on the
  next frame and data stays linear. Exports are always linear.

## 6. Checks and health

### 6.1 Checks

mmm-core emits checks as **structured data**: a stable code, severity
(problem / note / passed), the panels and groups involved, a human-readable
message, and a suggested remedy. The app renders them, the CLI prints them,
and the session records them (with the user's acceptance of any).

- **Passed checks are shown too** (green), so the list reads as a health
  report, not only complaints.
- **Checks never block** blend or export (builder's call, made in
  review). The user can accept a problem check per check and panel;
  acceptance resets if that panel's input file changes. Checks need a
  stable identity (code + subjects) for this.
- Remedies range from a notice, to guidance, to (post-v1) pointing at
  built-in preprocessing. The set of checks and remedies grows over time.
- v1 starting set (builder may add): astrometric solution present; missing
  distortion model (linear-only solution); panel far from / not overlapping
  its neighbours; plate scale mismatch; group panel counts differ (e.g. RGB
  missing panels 42 and 77); sky-level mismatch with neighbours; mixed
  mono/colour inputs; FILTER headers differ while grouping is off; source
  file changed or missing since last analysis. Existing analyze refusals
  and notices (e.g. mixed mono/colour, decode notices) become checks.

### 6.2 Mosaic health

A placeholder that grows: v1 shows what mmm already computes, i.e. the
per-seam step (seam Δ, with outliers flagged as `mmm report` does today)
and the photometric gain/offset spread. Residual errors and further
photometric measures come later, as we learn what is useful.

## 7. Sessions and the tiled result

### 7.1 Session folder

A session is the existing `<name>.mmm-session/` folder, which the app
adopts. Its contents split into two parts:

- **Configuration** (small, the "document"): panels by path plus a
  fingerprint, grouping switch, rule and manual placements, analyze and
  blend settings, compositions, display stretches, ROIs with notes,
  accepted checks, and a provenance log (mmm version, input fingerprints,
  settings of every blend run).
- **Cache** (large, always rebuildable): headers, preview pyramids, L8
  summaries, analysis artifacts, decode cache, the tiled result, stage
  stamps.

Today `session.json` mixes the two; it is split. Both parts are defined in
mmm-core so the app and CLI cannot drift.

- **No Save button.** Every change is written as it happens; closing never
  asks to save.
- The user chooses the folder's location at New.
- **Reopening** lands where the user left off, drawing previews and result
  tiles straight from the cache.
- **Changed or moved inputs** are checks on reopening ("Panel 42 changed on
  disk; it will be reanalysed"; "3 panels missing — locate folder…").
- **Adding panels later:** they pass through the grouping rule (or into No
  group), their previews build, they appear in Plan, and Blend marks the
  affected tiles stale (more if the photometric solve changes; the app says
  how much).
- **Panels outside the current frame** (builder's call, made in review):
  for solved input, the shared reference frame is re-derived, which
  invalidates reprojection caches, previews and result tiles; the app
  shows that cost before applying it. ROIs are stored in sky coordinates
  so they survive a frame change.
- **Multiple groups:** today one `.mmm-session` holds one group and
  `mmm batch` writes one session per group under a parent directory. The
  app's session holds all groups on one shared reference frame; the
  layout inside the folder (e.g. per-group subfolders under one
  configuration) is the builder's call, shared with the CLI.
- **No migration** (builder's call, made in review): sessions written by
  earlier versions are re-analysed, since their cache can be rebuilt. CLI
  flags keep their current meaning.

### 7.2 Sharing the session with the CLI (nice to have)

The configuration format is shared with the CLI so a session made in the
app can be re-run headless: `analyze`/`blend --session S` take their
settings from the configuration, with flags overriding, and a new
`mmm run S` re-runs a whole multi-group session (as `mmm batch` does
today). The CLI ignores what it does not use (compositions, ROIs,
display stretches). Expected to come almost free once the split exists;
not a v1 blocker.

**Per-group settings are in v1, configuration and CLI only** (user
ruling). The configuration holds one mosaic-wide set of analyze and blend
settings plus optional per-group overrides; the CLI can set them (e.g. a
per-group form of the existing `mmm batch --group` options). Staleness is
computed per group, so changing one group's override only stales that
group's tiles. The UI for editing overrides comes later.

### 7.3 The tiled result and export

- An **mmm-owned tiled pyramid format**, designed for memory-mapped
  access: the viewer mmaps the file and serves tiles from it, leaving
  memory management to the OS. One per group (compositions are composed at
  draw time).
- Each tile carries a **validity flag**: a settings change invalidates
  tiles in place; later blends overwrite and re-validate them. Partial
  blends (ROIs) fill in just their tiles.
- Preview pyramids for Plan use the same format (or a close relative), so
  one viewer reads both.
- **Export** produces deliverables from the tiled result: FITS, XISF,
  TIFF, linear, per group, with provenance in the headers (FITS `HISTORY`,
  XISF properties). Export of a stale or partial result warns first.
  Formats come from mpp-core as today.
- **Full-precision f32, uncompressed, in the session folder** (user
  ruling: no disk budget in v1; keep it simple). A 3-gigapixel RGB result
  is about 36 GB per group before pyramid levels; that is accepted.
- Remaining format details (tile size, levels, header layout, how
  validity is stored) are the builder's call, held to §8.

## 8. Interactivity: rules and targets

Responsiveness is the requirement the app succeeds or fails on.

Rules:

- **The UI thread never waits:** no file I/O, decoding or processing on
  it. Every stage (header reads, previews, analysis, blending, export)
  runs in the background; the UI draws state and sends commands.
- **Everything shows progress and is cancellable.** Cancel responds
  promptly and leaves the session consistent: finished tiles stay valid,
  unfinished ones stay stale.
- **The viewer reads only what is visible,** at the right pyramid level.
  Canvas size never affects frame rate. Missing tiles show a coarser level
  until the sharp one arrives (as OpenSeadragon does).
- **Edits take effect immediately.** A display stretch change appears on
  the next frame. A blend setting change marks tiles stale at once and work
  starts; no modal waits.
- **Newer work wins.** Changing a setting mid-blend cancels or
  reprioritises outdated work instead of queueing behind it.

Targets:

| Interaction | Target |
| --- | --- |
| Pan/zoom on a 100-panel, ~3-gigapixel canvas | steady 60 fps |
| Reopen a session | first image on screen in under 1 s |
| Display stretch slider | visible on the next frame |
| Cancel | acknowledged immediately; work stops within ~1 s |

A performance test exercises these on a large synthetic mosaic (mmm-core's
`synth` module) so regressions show in CI, not in users' hands.

The 3-gigapixel pan/zoom target is a **viewer** target: the viewer must
hold it on a canvas that size (a synthetic tiled result is enough to
prove it). End-to-end *processing* at that scale waits for the
processing-scale rework (§2 non-goals); v1 processes at today's scale.

## 9. mmm-core groundwork

What the app needs from the engine that does not exist today:

- **Progress and cancellation in every stage.** Analyze has a per-panel
  progress callback (`analyze_full`'s `AnalyzeProgress`); blend has none,
  and cancellation exists only on the IPC path (the worker turns host
  cancellation into errors). Both must be first-class, for every stage
  including header reads, previews and export.
- **ROI blending into the tiled result**, with the margin rule (§5.4) and
  tile-granular validity. Today `blend` streams rows to a `RowSink`
  (`--roi` exists on the CLI); the tiled result is a new sink or a new
  path.
- **Settings classified as blend-local or global**, with a staleness
  calculation (which tiles a change invalidates).
- **Preview pyramids** per panel and group, built in the background,
  available incrementally.
- **Structured checks** (§6.1) and health metrics as data.
- **The session configuration/cache split** (§7.1), with input
  fingerprints and provenance, multi-group layout, and per-group setting
  overrides (§7.2).

None of this changes the core algorithms (user ruling): analyze,
photometry, seams and blend keep their maths; the work is plumbing around
them.
- **Display stretch statistics** (median, MAD per channel, ignoring
  zeros), cheap enough to compute from preview levels.

The existing conventions hold: linear data end to end; zero is the no-data
sentinel; never process dense over the canvas; per-channel photometry,
shared seams.

## 10. Code structure and reuse

Proposed workspace additions (names are the builder's call):

- **A generic UI crate** that does **not** depend on mmm-core: theme and
  branding, the tiled pyramid viewer (OSD-style springs, level selection,
  coarse-to-fine fill), display stretch and histogram widgets, the
  job/progress panel, the checks list, the status bar. It reads data
  through small traits (tile source, job status, check list) that a future
  workflow-platform host could also implement. This is the crate meant for
  a later lift into workflow-platform or Photometrical.
- **`mmm-ui`:** mmm-specific widgets (Panels tab, panel layout and
  selection, compositions, ROI tools, blend settings, health), built on
  the generic crate and mmm-core.
- **The app crate:** the eframe shell, file dialogs, windows, background
  job execution, session lifecycle.

Cargo enforces the boundary: if the generic crate ever needs mmm-core,
that is a design smell to fix, mirroring Photometrical's "no egui" rule.

**Pin egui/eframe to the versions workflow-platform uses** (currently
0.36) from day one, or shared widgets will not be shareable.

## 11. Rendering, GPU and WSL

The user develops under WSL2. The GPU is exposed there (`/dev/dxg`, the
D3D12 libraries in `/usr/lib/wsl/lib`): OpenGL works through Mesa's d3d12
driver; Vulkan through Mesa's "dozen" driver is less mature.
workflow-platform already runs eframe on its OpenGL (`glow`) backend. Open
item for the builder: confirm the chosen backend under WSLg early (a spike
before the viewer is built), and pick between glow and wgpu (wgpu with a GL
fallback under WSL, DX12/Vulkan on native Windows) based on that and on
the viewer's needs.

## 12. Platforms and distribution

- **Windows and Linux first**; the user can test both locally. macOS
  later.
- Code signing and packaging get their own design, but the constraints are
  known: Windows signing via **Azure Artifact Signing** as an organisation
  (the legal entity is an Australian corporate trustee, trading since 2016;
  eligibility to be confirmed), from GitHub runners (no hardware tokens).
  Linux has no OS-level signing: checksums plus signatures, GitHub artifact
  attestations, and Flathub as a likely channel. macOS (Developer ID +
  notarisation, needs a D-U-N-S number) is deferred.

## 13. Left to the builder

- Tiled format internals (§7.3) and the pyramid level scheme.
- The background job system's design (threads, priorities, cancellation
  mechanism), held to §8.
- glow vs wgpu (§11).
- Exact layout, iconography and wording, within §4 and the mockups.
- Default preview overlap mode in Plan; which further checks to add.
- How the rule-change-versus-manual-placement behaviour is presented.
- Phasing. Suggested: (1) mmm-core groundwork (§9); (2) app shell, theme,
  viewer and job system, with the WSL backend spike first; (3) Panels,
  Plan, Blend; (4) CLI session sharing (§7.2).
