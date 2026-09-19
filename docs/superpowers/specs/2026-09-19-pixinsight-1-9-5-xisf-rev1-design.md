# PixInsight 1.9.5 / XISF 1.0 revision 1 support — design

Date: 2026-09-19. Status: approved (user), implementation pending.

## 1. Why

PixInsight 1.9.5 (PCL 2.10.8) adopts revision 1 of the XISF 1.0
specification. The part that affects this project is the new **standard
astrometric solution** (`AstrometricSolution:*` properties, spec §11.5.3.7).
When 1.9.5 solves or regenerates a solution it deletes every legacy
`PCL:AstrometricSolution:*` property, so a panel solved or registered in
1.9.5 carries none of the ids `mmm-core` reads today. Both the CLI and the
module then treat the panel as unsolved. The ImageSolver 6.5.0 changelog
states the same: solutions are stored only in the standard format, and
pre-1.9.5 cores load such images without a solution.

## 2. Policy (decided)

- New module builds target **PixInsight ≥ 1.9.5 only**, built against PCL
  2.10.8. The API version compiled in (`0x0188`) means 1.9.4 and older cores
  refuse the module, which is intended.
- Modules for 1.9.4 and earlier stay at v1.4.2 and are never updated.
- **The file readers keep reading 1.9.4-era files.** Old data will be mixed
  with new; the CLI and the worker must accept both the legacy block and the
  standard block.
- Update repositories are managed by the tools website; the repository
  packaging scripts in this repo are removed.
- XISF block compression (zstd/lz4/zlib) stays unsupported; readers keep
  refusing compressed files up front. Not in scope.

## 3. Verified facts

Source: the local 1.9.5 install (`/opt/PixInsight`), its PCL sources, and
the gitlab.com/pixinsight/PCL history.

| Item | Value |
|---|---|
| PCL | 2.10.8, build 1068 |
| PCL commit | `201860a364c05308cde8271f5ca517b69b487fa1` ("Update to PixInsight 1.9.5 Lockhart / PCL 2.10.8", 2026-09-17) |
| `PCL_API_Version` | `0x0188` |
| Core sources | 192 `src/pcl/*.cpp`; the install's `windows/vc17/PCL.vcxproj` (Makefile Generator v1.153) lists all 192 |
| 3rd-party libs | unchanged: cminpack, lcms, lz4, RFC6234, zlib, zstd |
| XISF defaults | `XISF::DefaultCompression = None`, `DefaultChecksum = None` |

What 1.9.5 writes for a solved image (`AstrometricMetadata::ToProperties` +
`SplineWorldTransformation::ToProperties`):

- Standard layer 1 (`AstrometricSolution:` prefix): `Version` = `"1.0"`,
  `ProjectionSystem`, `ReferenceCelestialCoordinates` (F64Vector[2]),
  `ReferenceImageCoordinates` (F64Vector[2]), `ReferenceNativeCoordinates`,
  `CelestialPoleNativeCoordinates`, `LinearTransformationMatrix`
  (F64Matrix 2×2), `CelestialReferenceSystem`; provenance `CreationTime`,
  `Catalog`, `CreatorOS`, `CreatorApplication`, `CreatorModule`.
- Standard layer 2: `ProjectiveTransformation:ImageToProjection` and
  `:ProjectionToImage` (F64Matrix 3×3), always present for spline
  solutions.
- Standard layer 3: `DistortionModel:{ImageToProjection,ProjectionToImage}:*`
  as defined in spec §11.5.3.7.4 (see §5 below).
- Standard layer 4: `ControlPoints:Celestial`, `ControlPoints:Image`,
  `Weights`, `ControlPoints:Rejected`.
- PixInsight-private extensions (ignored by us):
  `PCL:AstrometricSolution:Generation:*` (solver parameters) and
  `PCL:AstrometricSolution:Grid:{Fingerprint,ImageToProjection:*,
  ProjectionToImage:*}` (an evaluation cache, present only when the core
  has computed it).
- `Observation:CelestialReferenceSystem`, `Observation:Center:RA/Dec`,
  `Observation:Equinox` as before.

Coordinate conventions are unchanged: image coordinates are PixInsight's
(0-based, pixel k spans [k, k+1], y down); "projection plane coordinates"
are the gnomonic tangent-plane offsets (ξ, η) in degrees that the current
code calls "native".

## 4. Build, CI and repository changes

### 4.1 Pin

`integration/pixinsight/ci/pcl-pin.env` becomes a single pin:

```
PCL_SHA="201860a364c05308cde8271f5ca517b69b487fa1"   # PCL 2.10.8 / PixInsight 1.9.5
PCL_VER_MAJOR=2  PCL_VER_MINOR=10  PCL_VER_RELEASE=8
PCL_API_VERSION_HEX=0188
```

The `PCL_ARM64_*` variables are deleted. The policy comment is rewritten:
the pin tracks the **minimum supported core, which is now 1.9.5**; the API
history table gains `1.9.5 (PCL 2.10.8) -> 0x0188`.

### 4.2 Scripts

- `build-pcl-macos.sh`: remove the arm64 pin override block and the zlib
  `fdopen` sed (dead at this pin); keep the SDK retarget and the hand-rolled
  3rd-party makes (both arches share the pin now). Update header comments.
- `build-pcl.sh`, `build-pcl-windows.ps1`: no logic change; comments.
- `win/PCL.vcxproj`: replace with the file lifted from
  `/opt/PixInsight/src/pcl/windows/vc17/PCL.vcxproj` (no trimming; the
  drift guard must pass with 192 sources).
- `module.yml`: read one SHA for macOS; drop the "Generate + validate
  repository package" and "Run repo generator tests" steps and the
  `repo-out/**` upload path; refresh the header comment (macOS still ships
  both arches: Intel Macs exist, arm64 is native).
- Delete `integration/pixinsight/repo/` entirely (scripts, tests, README).
  Fix the one cross-reference in `module/README.md`.
- `module/makefile-x64` and `module/CMakeLists.txt`: the `-isystem`/`SYSTEM`
  treatment of PCL headers stays (harmless); the 2.8.3-era comments are
  updated. Remove the two `noexcept` destructor workarounds
  (`ImageWindowCollector.h`, `ViewPanelSource.h`) — 2.10.x headers declare
  `UIObject::~UIObject()` noexcept.

### 4.3 Version

Release is **v1.5.0** in all four synced places (`Cargo.toml`,
`MmmVersion.h`, `mmm_protocol.h`, `MegaMergeMosaic.html`).

### 4.4 Docs

`README.md`, `docs/DESIGN.md`, `ci/README.md`, `PCL_API_REFERENCE.md`,
`module/README.md`, the user doc HTML: state "PixInsight ≥ 1.9.5", describe
the standard block, and note that legacy 1.9.4 files remain readable.

## 5. mmm-core: standard astrometric solution decoder

### 5.1 Module layout

`crates/mmm-core/src/astrometry.rs` (1651 lines) becomes a directory:

| File | Contents |
|---|---|
| `astrometry/mod.rs` | `LinearWcs`, `WcsModel`, `Grid2D`, TAN math, `wcs_cards*`, `wcs_from_properties`, dispatch between legacy and standard |
| `astrometry/legacy.rs` | the existing `PCL:AstrometricSolution:*` linear + `SplineWorldTransformation` grid reader, unchanged in behaviour |
| `astrometry/standard.rs` | XISF rev 1 decoder: version gate, layer 1–3 parsing and validation, projective transform, model sampling onto `Grid2D` |
| `astrometry/spline.rs` | RBF surface spline evaluation: kernels, polynomial part, scalar/vector splines, term model (Global/Local/Fallback) |

Public API additions in `lib.rs`: nothing new beyond what already exists
(`WcsModel`, `LinearWcs`, `wcs_from_properties`); the standard decoder is
reached through `WcsModel::from_properties`. `standard` and `spline` are
`pub(crate)`.

### 5.2 Dispatch

`WcsModel::from_properties(props, w, h)`:

1. If `AstrometricSolution:Version` is present: parse `major.minor`. Major
   ≠ 1 ⇒ `None` (the spec forbids interpreting any layer). Otherwise decode
   the standard block (§5.3). A standard block is used exclusively; legacy
   ids are ignored even if present.
2. Else fall back to the legacy reader (today's code).

`wcs_from_properties` (linear only, used by the registered-canvas path and
FITS card emission) follows the same rule: standard ids first, legacy ids
second. `Observation:CelestialReferenceSystem` remains the RADESYS source
for legacy files; for standard files `AstrometricSolution:CelestialReferenceSystem`
takes precedence, default ICRS.

### 5.3 Layer decoding

Layer 1 (required): `ProjectionSystem` must be a vocabulary identifier;
only `Gnomonic` is accepted for grid/spline work (as today), others map to
CTYPE codes for the linear path exactly as today. `ReferenceNativeCoordinates`
/ `CelestialPoleNativeCoordinates` must be absent or the standard zenithal
values, as today.

Layer 2 (optional): both 3×3 matrices present, else the layer is
unavailable. Applied as a homography: `w = h20·x + h21·y + h22`,
`x' = (h00·x + h01·y + h02)/w`, `y' = (h10·x + h11·y + h12)/w`.

Layer 3 (optional, requires layer 2): both directions present, else
unavailable. Each direction's complete transformation is
`projective(p) + residual(p)` where the residual field is the normalized
weighted sum of the term splines (§5.4). Any required property missing,
any dimension inconsistency, any unknown `BasisFunction` or `Terms`
identifier ⇒ layer 3 unavailable (never partially evaluated).

Layer availability follows the spec's fallback rule: unavailable layer 3
means layer 2 is used; unavailable layer 2 means the linear solution. The
resulting `WcsModel` carries `image_to_native`/`native_to_image` grids when
layer 2 or 3 is used, `None` when only layer 1 is available.

Note: with a standard block the "grid" is **our own sampling** of the
decoded model, not something read from the file. `Grid2D` and its
consumers are unchanged.

### 5.4 Spline evaluation (reference: PCL 2.10.8, `SurfaceSpline.cpp`)

Scalar spline record `{normalization (x0,y0,r0), nodes[n×2] (normalized),
coefficients[n+q], shape ε (optional)}` with basis function id, order `m`,
and `polynomial` flag; `q = m(m+1)/2` when `polynomial`, else 0. Value at
source point `(x, y)`:

```
u = r0·(x − x0),  v = r0·(y − y0)
s = Σ_i c_i · φ(r_i²)             r_i² = (u − X_i)² + (v − Y_i)²
  + Σ_k d_k · u^a v^b             monomials by total degree, then descending
                                  power of u: 1, u, v, u², uv, v², …
```

Kernels (r² in normalized units, `e2 = ε²`):

| Identifier | φ(r²) | order / parameter |
|---|---|---|
| `ThinPlateSpline` | `½·r²·ln(r²)` (= r² ln r) | order sets polynomial degree only; polynomial required |
| `VariableOrder` | `(r²)^(m−1)·ln(r²)` | m ≥ 3; polynomial required |
| `Gaussian` | `exp(−e2·r²)` | shape required; polynomial optional |
| `Multiquadric` | `sqrt(1 + e2·r²)` | shape required |
| `InverseMultiquadric` | `1/sqrt(1 + e2·r²)` | shape required |
| `InverseQuadratic` | `1/(1 + e2·r²)` | shape required |

`φ(0) = 0` for the logarithmic kernels (the limit; avoids 0·−∞).

Vector spline: X and Y component splines. Y may share X's nodes,
normalization and shape (only `Y:Coefficients` present) or carry its own.

Term model per direction, at source point `p`:

- Global term: weight 1.
- Local term k: `t = |p − center_k| / radius_k`; weight
  `W(t) = (1−t)⁴(4t+1)` for `t < 1`, else 0 (Wendland C2).
- Let `S = Σ w_k·spline_k(p)`, `ws = Σ w_k` over Global and Local terms.
- Fallback term with threshold `t0`: if `ws < t0`, `wc = W(ws/t0)`,
  residual = `(S + wc·fallback(p)) / (ws + wc)`.
- Else if `ws > 0`: residual = `S/ws`.
- Else (no Fallback, no covering disc): residual = the Local term with the
  smallest `t`, or 0 if there are no terms.

Local term packing follows the spec: `NodeOffsets` (N+1 ints, `[0]` = 0,
each term ≥ 3 nodes), `Nodes` rows `[o_k, o_{k+1})`, X coefficients for term
k at `[o_k + k·q, o_{k+1} + (k+1)·q)`; Y likewise over the Y offsets when
`Local:Y:Nodes` exists, else over the X offsets.

A uniform bucket index over the Local disc centers keeps per-point cost
proportional to the discs actually covering the point.

### 5.5 Sampling the model onto grids

- `image_to_native`: `Rect = [0, 0, w, h]`, `Delta = 8` px (PixInsight's
  1.9.4 spacing, which the existing consistency checks were tuned on).
- `native_to_image`: `Rect` = bounding box of the image→projection map of
  the image corners and edge midpoints, padded by one cell; `Delta = 8 px ×
  mean plate scale` (deg/px from the linear matrix).
- Sampling runs in parallel over rows (rayon). Node counts follow the
  existing `expected_nodes` rule (`1 + ⌈extent/Δ⌉`).
- After sampling, the existing `WcsModel::from_properties` validations run
  unchanged: grid domain equals the image bounds, `grid(refimg) ≈ (0, 0)`,
  inverse returns the reference pixel, and corner agreement with the linear
  solution within the distortion allowance. Any failure ⇒ `None` (treated
  as unsolved rather than solved wrongly).

### 5.6 XISF reader changes

`formats/xisf.rs` must decode the additional property types the standard
block uses:

- `Int32` / `Boolean` scalars: already decoded.
- `I32Vector` (`Local:X:NodeOffsets`, `ControlPoints:Rejected`) and the
  other integer/float vector and matrix types (`I8..UI64Vector`,
  `F32Vector`, and matrix equivalents): decoded into `PropertyValue::F64Vec`
  / `F64Mat` with `type_` preserved, via inline or attachment locations,
  element width taken from the type. This mirrors what the module already
  does over IPC (all vector types become `F64Vec`). Integer values up to
  2⁵³ are exact.
- `String` properties containing newlines (`Terms`) must keep the newlines
  (verify `quick-xml` text handling; the parser must not collapse them).

### 5.7 Error reporting

`analyze::describe_unsolved` learns the standard ids: reports the missing
required layer-1 properties, an unsupported major version, an unknown
projection/basis/term identifier, or "distortion model present but failed
validation", naming the layer.

## 6. Module (C++) changes

- `AstrometryProps.cpp`: forward properties whose id starts with
  `AstrometricSolution:` **or** `PCL:AstrometricSolution:` (legacy files
  opened in 1.9.5 keep their legacy ids until regenerated), plus
  `Observation:CelestialReferenceSystem` (today it is filtered out, so the
  worker's legacy RADESYS path silently defaults to ICRS).
- Type mapping already covers String, Bool, Int32, F64Vector, F64Matrix,
  I32Vector (→ F64Vec). Add `Terms` newline safety: strings pass through
  JSON unchanged.
- Minimum core version 1.9.5 in module docs; version 1.5.0.

## 7. Synthetic test fixtures

`synth.rs` gains `write_xisf_solved_standard` (linear-only standard block)
and a property-array builder for a standard spline solution, so unit and
integration tests can synthesize: a pure layer-1 file, a layer-2 file, a
layer-3 file with a Global TPS term, and a layer-3 file with Local +
Fallback terms. Spline coefficients for fixtures are produced by a small
in-test TPS fitter (solve the interpolation system for a handful of nodes
with a known analytic distortion), so the decoder is checked against a
known field rather than against itself.

Tests never depend on `test_data/`.

## 8. Verification against real 1.9.5 output (manual smoke test)

1. Run PixInsight 1.9.5 headlessly (`bin/PixInsight.sh`, automation mode,
   a small PJSR script) to open one raw Orion panel, call
   `regenerateAstrometricSolution()`, and save a copy into the scratchpad.
2. Confirm the property inventory matches §3 and record the actual
   `BasisFunction`, `Order`, `Terms` in `astrometry/standard.rs` docs.
3. `mmm info --stats` on that file must report a solved panel; the decoded
   image→projection grid must agree with PixInsight's own
   `PCL:AstrometricSolution:Grid:ImageToProjection` cache (read ad hoc in a
   test binary, not in product code) to well under 0.1″ across the field.
4. Re-run the solved-mode Orion alignment smoke test with the regenerated
   panels and compare the footprint placement against the 1.9.4 run.

## 9. Out of scope

- XISF block compression and checksum verification.
- Writing standard astrometric solutions into mmm's FITS/XISF output (mmm
  emits FITS WCS cards only; unchanged).
- Non-gnomonic projections in spline mode (as today).
- The website update repository and XRI generation.
