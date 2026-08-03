# PixInsight UI Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the MegaMergeMosaic PixInsight tool window up to PixInsight UI conventions: branding/attribution header, process icon, collapsible sections, tidy alignment, tooltips, console-driven progress, and corrected parameter ranges.

**Architecture:** All changes live in `integration/pixinsight/module/` (the PCL C++ module). The instance/parameter layer (`MmmParameters.*`, `MmmProcess.*`) is trimmed first (remove ROI + downsample, fix ranges, feather → integer), then the process/module identity is renamed, then the interface (`MmmInterface.*`) is rebuilt section by section, and finally the execution layer (`MmmExecution.cpp`) gets console-only progress. Process documentation is a hand-authored HTML file that PixInsight's integrated doc browser picks up by process Id.

**Tech Stack:** PCL 2.10.4 (headers at `/opt/PixInsight/include/pcl`), C++17/20, GNU make (`makefile-x64`).

## Global Constraints

- Process Id: `MegaMergeMosaic` (PixInsight ids cannot contain spaces); old id `MosaicMerge` kept via `MetaProcess::Aliases()`. Window title: `Mega Merge Mosaic`.
- Tagline (user-approved, verbatim): `Big mosaics, no big deal.`
- Copyright line: `Copyright (c) 2026 Astrometrical | astrometrical.com` (footer format on the website is "© 2026 Astrometrical"; PCL source files stay ASCII, so use `(c)` in code and `&copy;` in HTML).
- Brand accent teal: `#2a95ab` (mid), `#1d7588` (dark), `#4ebcd4` (light). Astrometrical logo = a chevron/peak "A" mark (triangle with a notch cut up into the bottom center).
- Feather: **integer, range 1–1024, default 256** (user chose 1 as the minimum because `mmm-core` `blend.rs:652` rejects `feather_px <= 0`).
- Surface fit order: range **0–2**, default 2 (`crates/mmm-core/src/surfaces.rs:60-66` saturates ≥2 to quadratic; 0 = constant is valid).
- Flatten order: range **1–2**, default **2** (`crates/mmm-core/src/flatten.rs:107-111` rejects anything else; the current default of 4 is a live bug).
- ROI and Downsample parameters are **removed entirely** (pre-release; no compatibility shim). The wire protocol still requires the fields: always send `"roi": null` and `"downsample": 1`.
- All user-visible literals in C++ sources must be plain ASCII (`...` not `…`) — PCL's `String(const char*)` treats narrow literals as ISO-8859-1, which is what produced the `â€¦` mojibake.
- Every task ends with a clean build: `cd integration/pixinsight/module && make -f makefile-x64 -j$(nproc)` (env defaults `PCLINCDIR=/opt/PixInsight/include`, `PCLLIBDIR=$HOME/.local/pcl-build/lib` already work on this machine). "Clean build" = exits 0, no new warnings.
- Do not touch `crates/` (the Rust engine) or `integration/pixinsight/host/`.
- Commit after every task with the message given in the task.

## File Structure

- Modify: `integration/pixinsight/module/MmmParameters.h` / `.cpp` — ranges, feather type, delete ROI/downsample singletons.
- Modify: `integration/pixinsight/module/MmmProcess.h` / `.cpp` — instance members, Id/alias/description, icon hook.
- Modify: `integration/pixinsight/module/mmm.cpp` — module metadata (Astrometrical), singleton construction list.
- Create: `integration/pixinsight/module/MmmVersion.h` — single source for the human-readable version string.
- Create: `integration/pixinsight/module/MmmIcon.h` — process-icon SVG + header-logo SVG (**this is the file the user edits to change the icon**).
- Modify: `integration/pixinsight/module/MmmInterface.h` / `.cpp` — full interface rebuild.
- Modify: `integration/pixinsight/module/MmmExecution.h` / `.cpp` — remove interface progress plumbing, console progress rewrite, wire constants.
- Create: `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html` — integrated-docs page.
- Modify: `integration/pixinsight/module/README.md` — doc-install step.

Headers added (`MmmVersion.h`, `MmmIcon.h`) need **no build-system change** — `makefile-x64` and `CMakeLists.txt` list only `.cpp` files.

---

### Task 1: Parameter cleanup (feather int 1–1024, order ranges, drop ROI + downsample)

**Files:**
- Modify: `integration/pixinsight/module/MmmParameters.h`
- Modify: `integration/pixinsight/module/MmmParameters.cpp`
- Modify: `integration/pixinsight/module/MmmProcess.h`
- Modify: `integration/pixinsight/module/MmmProcess.cpp`
- Modify: `integration/pixinsight/module/mmm.cpp`
- Modify: `integration/pixinsight/module/MmmExecution.cpp`
- Modify: `integration/pixinsight/module/MmmInterface.h` / `.cpp` (only what's needed to keep it compiling)

**Interfaces:**
- Consumes: existing `MmmBlendInstance` members.
- Produces: `int32 p_feather` (was `float`); members `p_roi`, `p_roiEnabled`, `p_downsample` and their parameter classes/singletons **no longer exist**. `MmmFeatherParameter` is a `MetaInt32`. Later tasks rely on exactly these names: `p_feather`, `p_surfaceOrder`, `p_flatten`, `p_flattenEnabled`, `p_bandRows`, `p_defectVeto`, `p_blendMode`, `p_inputSelect`, `p_sessionDir`, `p_viewIds`, `p_filePaths`.

- [ ] **Step 1: MmmParameters.h — retype feather, fix ranges, delete ROI/downsample classes**

In `MmmParameters.h`:

Replace the `MmmFeatherParameter` class (currently `MetaFloat`) with:

```cpp
/*!
 * \brief Int32 parameter: feather ramp length in canvas pixels.
 */
class MmmFeatherParameter : public MetaInt32
{
public:

   MmmFeatherParameter( MetaProcess* P ) : MetaInt32( P ) {}
   IsoString Id() const override { return "feather"; }
   double DefaultValue() const override { return 256; }
   double MinimumValue() const override { return 1; }      // mmm-core rejects feather_px <= 0
   double MaximumValue() const override { return 1024; }
};
```

In `MmmFlattenParameter`, change `DefaultValue` to `2` and `MinimumValue` to `1`, `MaximumValue` to `2` (mmm-core `flatten.rs` accepts only 1..=2).

In `MmmSurfaceOrderParameter`, change `MaximumValue` to `2` (mmm-core saturates ≥2 to quadratic; keep default 2, min 0).

Delete these classes and their `extern` declarations entirely: `MmmRoiX0Parameter`, `MmmRoiY0Parameter`, `MmmRoiX1Parameter`, `MmmRoiY1Parameter`, `MmmRoiEnabledParameter`, `MmmDownsampleParameter`. Also delete the ROI and downsample entries from the wire-mapping comment at the top of the file (roi maps to `null`, downsample to constant `1` — note that in the comment).

- [ ] **Step 2: MmmParameters.cpp — delete the six removed singleton definitions**

Remove the `TheMmmRoiX0Parameter` … `TheMmmRoiY1Parameter`, `TheMmmRoiEnabledParameter`, and `TheMmmDownsampleParameter` definition lines.

- [ ] **Step 3: MmmProcess.h — instance members**

Change `float p_feather` to `int32 p_feather`. Delete members `p_roi[4]`, `p_roiEnabled`, `p_downsample` (keep everything else).

- [ ] **Step 4: MmmProcess.cpp — constructor, Assign, LockParameter, AllocateParameter**

- Constructor init list: `p_feather( int32( TheMmmFeatherParameter->DefaultValue() ) )`; delete the `p_roiEnabled`/`p_downsample` initializers and the four `p_roi[i] = ...` body lines.
- `Assign()`: delete the `p_roi[0..3]`, `p_roiEnabled`, `p_downsample` copies.
- `LockParameter()`: delete the five ROI lines and the downsample line.
- (`AllocateParameter`/`ParameterLength` have no ROI/downsample entries — no change.)

- [ ] **Step 5: mmm.cpp — stop constructing removed singletons**

Delete the six `TheMmmRoi*Parameter = new ...` / `TheMmmDownsampleParameter = new ...` lines from `InstallPixInsightModule`.

- [ ] **Step 6: MmmExecution.cpp — wire constants for removed params, int feather**

- In the local `Params` struct: `feather` becomes `int32`; delete `roi[4]`, `roiEnabled`, `downsample`.
- In `BuildParams()`: delete the `isfinite` check (feather is an int now); set

```cpp
   p["feather_px"] = double( in.feather );
   p["downsample"] = uint32_t( 1 );   // parameter removed from UI; wire field is mandatory
   ...
   p["roi"] = nullptr;                // ROI removed from UI; wire field is mandatory
```

  (delete the `roiEnabled` conditional entirely).
- In `run_blend()`: delete the `p.roi[...]`, `p.roiEnabled`, `p.downsample` snapshot lines; `p.feather = in.p_feather;` stays (now int32).

- [ ] **Step 7: MmmInterface — minimal compile fixes only**

(The full interface rebuild is Tasks 4–6; here only keep it compiling.)

- `MmmInterface.h`: delete the `Roi_Sizer`, `RoiEnabled_CheckBox`, `RoiFields_Sizer`, `RoiX0/Y0/X1/Y1_NumericEdit`, `Downsample_Sizer`, `Downsample_Label`, `Downsample_SpinBox` GUIData members; delete `UpdateRoiControls()`, `e_RoiEnabledClick`, `e_RoiValueUpdated`, `e_DownsampleValueUpdated` declarations.
- `MmmInterface.cpp`: delete the corresponding construction blocks in `GUIData::GUIData` (Downsample rows, ROI rows, and their `BlendParams_Sizer.Add(...)` lines), the `UpdateRoiControls` definition + its call sites, the three deleted handlers, and the ROI/downsample lines in `UpdateControls()`.
- Feather control becomes integer: in `GUIData::GUIData`:

```cpp
   Feather_NumericControl.label.SetText( "Feather:" );
   Feather_NumericControl.SetInteger();
   Feather_NumericControl.SetRange( 1, 1024 );
```

  (drop `SetReal()`/`SetPrecision`). `e_FeatherValueUpdated` body becomes `m_instance.p_feather = int32( value );`.

- [ ] **Step 8: Build**

Run: `cd /home/dpaull/dev/MergeMosaic/integration/pixinsight/module && make -f makefile-x64 -j$(nproc)`
Expected: exit 0, no new warnings.

- [ ] **Step 9: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): integer feather 1-1024, valid order ranges, drop ROI + downsample"
```

---

### Task 2: Rename to Mega Merge Mosaic + Astrometrical module metadata

**Files:**
- Create: `integration/pixinsight/module/MmmVersion.h`
- Modify: `integration/pixinsight/module/mmm.cpp`
- Modify: `integration/pixinsight/module/MmmProcess.cpp`
- Modify: `integration/pixinsight/module/MmmInterface.cpp`
- Modify: `integration/pixinsight/module/MmmExecution.cpp`

**Interfaces:**
- Produces: `MMM_VERSION_STRING` macro (e.g. `"1.0.0"`) in `MmmVersion.h`; process Id `"MegaMergeMosaic"`; interface Id `"MegaMergeMosaic"`. Task 5's header pane consumes `MMM_VERSION_STRING`.

- [ ] **Step 1: Create MmmVersion.h**

```cpp
// MmmVersion.h -- single source for the module's human-readable version.
// mmm.cpp derives its PCL_MODULE_VERSION components from the same numbers;
// keep both in sync when bumping.

#ifndef __MmmVersion_h
#define __MmmVersion_h

#define MMM_VERSION_MAJOR     1
#define MMM_VERSION_MINOR     0
#define MMM_VERSION_REVISION  0
#define MMM_VERSION_BUILD     1

#define MMM_VERSION_STRING    "1.0.0"

#endif   // __MmmVersion_h
```

- [ ] **Step 2: mmm.cpp — use MmmVersion.h and Astrometrical metadata**

Replace the four `MMM_MODULE_VERSION_*` numeric defines with `#include "MmmVersion.h"` plus

```cpp
#define MMM_MODULE_VERSION_LANGUAGE  eng
```

and pass `MMM_VERSION_MAJOR/MINOR/REVISION/BUILD` to `PCL_MODULE_VERSION`. Update `MmmModule`:

```cpp
   IsoString Name() const override
   {
      return "MegaMergeMosaic";
   }

   String Description() const override
   {
      return "Mega Merge Mosaic: fast merge/blend for pre-aligned astro mosaic panels.";
   }

   String Company() const override
   {
      return "Astrometrical";
   }

   String Author() const override
   {
      return "Daniel Paull";
   }

   String Copyright() const override
   {
      return "Copyright (c) 2026 Astrometrical";
   }
```

(`Author()` and `Copyright()` are virtuals on `MetaModule` — verified at `/opt/PixInsight/include/pcl/MetaModule.h:271,284`.)

- [ ] **Step 3: MmmProcess.cpp — Id, alias, description**

```cpp
IsoString MmmBlendProcess::Id() const
{
   return "MegaMergeMosaic";
}

IsoString MmmBlendProcess::Aliases() const
{
   // Pre-rename id; lets any existing icons/scripts keep resolving.
   return "MosaicMerge";
}
```

(`Aliases()` is a `MetaProcess` virtual — `/opt/PixInsight/include/pcl/MetaProcess.h:142`; add the `override` in a class declaration if `MmmProcess.h` declares members explicitly — it does, so add `IsoString Aliases() const override;` there.)

Update `Description()`:

```cpp
String MmmBlendProcess::Description() const
{
   return "<html><p>Merges pre-aligned astro mosaic panels (e.g. MosaicByCoordinates output) "
          "into a single seamless image using overlap-band analysis, gain/offset matching and "
          "multiband blending.</p><p>Big mosaics, no big deal.</p></html>";
}
```

Also update the `CanExecuteOn` whyNot string and both error prefixes in this file from `MosaicMerge` to `MegaMergeMosaic`.

- [ ] **Step 4: MmmInterface.cpp — interface Id + window title**

`MmmBlendInterface::Id()` returns `"MegaMergeMosaic"`; `Launch()` calls `SetWindowTitle( "Mega Merge Mosaic" )`.

- [ ] **Step 5: MmmExecution.cpp — error-message prefixes**

Replace every `"MosaicMerge:"` message prefix with `"MegaMergeMosaic:"` (worker-not-found errors, module-path errors, validation errors in `run_blend`, null-window error). `grep -n "MosaicMerge" integration/pixinsight/module/*.cpp *.h` afterwards must return no hits except the `Aliases()` return and its comment.

- [ ] **Step 6: Build** (same command as Task 1). Expected: exit 0.

- [ ] **Step 7: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): rename process to MegaMergeMosaic; Astrometrical module metadata"
```

---

### Task 3: Process icon (SVG)

**Files:**
- Create: `integration/pixinsight/module/MmmIcon.h`
- Modify: `integration/pixinsight/module/MmmProcess.h` / `.cpp`
- Modify: `integration/pixinsight/module/MmmInterface.h` / `.cpp`

**Interfaces:**
- Produces: `MMM_PROCESS_ICON_SVG` (48×48 process icon, mosaic tiles + Astrometrical chevron) and `MMM_CHEVRON_SVG` (plain teal chevron, used by Task 5's header logo). Both are `R"svg(...)svg"` raw string literals.

- [ ] **Step 1: Create MmmIcon.h**

```cpp
// MmmIcon.h -- SVG artwork for the MegaMergeMosaic process.
//
// MMM_PROCESS_ICON_SVG is returned by MetaProcess::IconImageSVG() /
// ProcessInterface::IconImageSVG() and is what appears in the Process
// Explorer, process icons and the tool window's title bar.
//
// EDIT THIS FILE to change the icon. To preview outside PixInsight, copy the
// literal between the svg( )svg delimiters into a .svg file and open it in a
// browser.
//
// Motif: four offset mosaic panels in the Astrometrical teal ramp
// (#1d7588 / #2a95ab / #4ebcd4) behind the Astrometrical chevron mark.
//
// The chevron path is the actual Astrometrical "A" mark (the Angora font
// glyph): polygon vertices traced from apps/web/src/assets/Logo-Black.png in
// the astrometrical repo -- pointed apex, THIN left stroke, THICK right
// stroke, notch rising to about 60% height. Do not "fix" the asymmetry.

#ifndef __MmmIcon_h
#define __MmmIcon_h

#define MMM_PROCESS_ICON_SVG R"svg(<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="48" height="48">
  <!-- mosaic panels: four tiles, slightly offset like registered frames -->
  <rect x="3"  y="5"  width="21" height="21" rx="2" fill="#1d7588"/>
  <rect x="25" y="3"  width="20" height="21" rx="2" fill="#2a95ab"/>
  <rect x="5"  y="27" width="20" height="18" rx="2" fill="#2a95ab"/>
  <rect x="26" y="25" width="19" height="20" rx="2" fill="#4ebcd4"/>
  <!-- panel seams -->
  <rect x="3" y="3" width="42" height="42" rx="2" fill="none" stroke="#0b3541" stroke-opacity="0.35" stroke-width="1"/>
  <!-- Astrometrical chevron: the Angora "A" mark, traced from Logo-Black.png -->
  <path d="M 23.82 10.14 L 42 37.86 L 32.91 37.86 L 21.13 19.89 L 9.55 37.86 L 6 37.86 Z"
        fill="#ffffff" stroke="#0b3541" stroke-width="1.2" stroke-linejoin="round"/>
</svg>
)svg"

// Plain chevron mark for the interface header (no tiles), Astrometrical teal.
// Same traced Angora "A" glyph at full size.
#define MMM_CHEVRON_SVG R"svg(<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48" width="48" height="48">
  <path d="M 23.78 7.06 L 46 40.94 L 34.89 40.94 L 20.49 18.97 L 6.34 40.94 L 2 40.94 Z" fill="#2a95ab"/>
</svg>
)svg"

#endif   // __MmmIcon_h
```

- [ ] **Step 2: Wire into process and interface**

- `MmmProcess.h`: add `IsoString IconImageSVG() const override;` to `MmmBlendProcess`.
- `MmmProcess.cpp`: `#include "MmmIcon.h"` and

```cpp
IsoString MmmBlendProcess::IconImageSVG() const
{
   return MMM_PROCESS_ICON_SVG;
}
```

- `MmmInterface.h`: add `IsoString IconImageSVG() const override;`.
- `MmmInterface.cpp`: `#include "MmmIcon.h"` and the same one-line body (interface icon = process icon; `ProcessInterface::IconImageSVG` verified at `/opt/PixInsight/include/pcl/ProcessInterface.h:356`).

- [ ] **Step 3: Build.** Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): mosaic-tiles + Astrometrical chevron process icon (MmmIcon.h)"
```

---

### Task 4: Remove in-window progress UI and interface cancel plumbing

**Files:**
- Modify: `integration/pixinsight/module/MmmInterface.h` / `.cpp`
- Modify: `integration/pixinsight/module/MmmExecution.h` / `.cpp`

**Interfaces:**
- Consumes: nothing new.
- Produces: `MmmBlendInterface` no longer has `SetBlendRunning`, `SetProgressText`, `e_CancelClick`, or any `Progress_*`/`Cancel_*` GUIData members. `MmmExecution.h` no longer declares `request_cancel()`. Cancellation is Console-only (the Console's Pause/Abort button), already handled inside `ConsoleProgress::on_progress` via `AbortRequested()` → `host->cancel()`.

- [ ] **Step 1: MmmInterface — delete progress group**

Remove from `MmmInterface.h`: `Progress_GroupBox`, `Progress_Sizer`, `Progress_Label`, `Cancel_PushButton` members; `SetBlendRunning`, `SetProgressText`, `e_CancelClick` declarations.
Remove from `MmmInterface.cpp`: the whole "Progress + cancel" construction block, `Global_Sizer.Add( Progress_GroupBox )`, the `e_CancelClick`, `SetBlendRunning`, `SetProgressText` definitions.

- [ ] **Step 2: MmmExecution — drop interface hooks**

- `MmmExecution.h`: delete the `request_cancel()` declaration (and its doc comment).
- `MmmExecution.cpp`: delete the `request_cancel()` definition, the `SetRunningUI()` helper and its two call sites plus the calls in the catch/exit paths of `DriveHost`, and in `ConsoleProgress::on_progress` the `TheMmmBlendInterface->SetProgressText(...)` mirror (keep `Module->ProcessEvents()` and the `AbortRequested()` → `host->cancel()` path — that is the Console cancel button working). Remove the now-unused `#include "MmmInterface.h"` if nothing else in the file needs it (check: `TheMmmBlendInterface` should have no remaining uses).

- [ ] **Step 3: Build.** Expected: exit 0, no unused-variable warnings.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module
git commit -m "refactor(pixinsight): drop in-window progress/cancel; console owns progress + abort"
```

---

### Task 5: Interface restructure — header notice, SectionBars, mode-dependent lists, ellipsis fix

**Files:**
- Modify: `integration/pixinsight/module/MmmInterface.h`
- Modify: `integration/pixinsight/module/MmmInterface.cpp`

**Interfaces:**
- Consumes: `MMM_CHEVRON_SVG` (Task 3), `MMM_VERSION_STRING` (Task 2).
- Produces: GUIData members named exactly: `Notice_Control`, `TargetFrames_SectionBar`, `TargetFrames_Control`, `Parameters_SectionBar`, `Parameters_Control`, `SessionDir_ToolButton` (replaces `SessionDir_PushButton`). Handlers `e_ToggleSection( SectionBar&, Control&, bool )`, `e_NoticeMouseRelease( Control&, const pcl::Point&, int, unsigned, unsigned )`. Task 6 edits these same blocks in place.

- [ ] **Step 1: MmmInterface.h — new control tree**

Includes: add `<pcl/SectionBar.h>`, `<pcl/ToolButton.h>`, `<pcl/BitmapBox.h>` — **note**: PCL has no BitmapBox; do NOT add it. The logo is a plain `Control` painted via `OnPaint`. Final include additions: `<pcl/SectionBar.h>`, `<pcl/ToolButton.h>`.

Replace the GUIData layout members so the tree reads:

```cpp
      VerticalSizer   Global_Sizer;

      // --- Header notice: logo + title/tagline/copyright ---------------------
      Control         Notice_Control;
      HorizontalSizer Notice_Sizer;
      Control         Logo_Control;          // paints the chevron bitmap
      VerticalSizer   NoticeText_Sizer;
      Label           Title_Label;
      Label           Tagline_Label;
      Label           Copyright_Label;
      Bitmap          Logo_Bitmap;           // rendered from MMM_CHEVRON_SVG

      // --- Target Frames section --------------------------------------------
      SectionBar      TargetFrames_SectionBar;
      Control         TargetFrames_Control;
      VerticalSizer   TargetFrames_Sizer;
      HorizontalSizer InputMode_Sizer;
      RadioButton     ViewsMode_RadioButton;
      RadioButton     FilesMode_RadioButton;
      TreeBox         Views_TreeBox;
      HorizontalSizer ViewButtons_Sizer;
      PushButton      AddViews_PushButton;
      PushButton      RemoveView_PushButton;
      TreeBox         Files_TreeBox;
      HorizontalSizer FileButtons_Sizer;
      PushButton      AddFiles_PushButton;
      PushButton      RemoveFile_PushButton;

      // --- Parameters section ------------------------------------------------
      SectionBar      Parameters_SectionBar;
      Control         Parameters_Control;
      VerticalSizer   Parameters_Sizer;
      HorizontalSizer SessionDir_Sizer;
      Label           SessionDir_Label;
      Edit            SessionDir_Edit;
      ToolButton      SessionDir_ToolButton;
      HorizontalSizer InputSelect_Sizer;
      Label           InputSelect_Label;
      ComboBox        InputSelect_ComboBox;
      HorizontalSizer BlendMode_Sizer;
      Label           BlendMode_Label;
      ComboBox        BlendMode_ComboBox;
      NumericControl  Feather_NumericControl;
      HorizontalSizer SurfaceOrder_Sizer;
      Label           SurfaceOrder_Label;
      SpinBox         SurfaceOrder_SpinBox;
      HorizontalSizer BandRows_Sizer;
      Label           BandRows_Label;
      SpinBox         BandRows_SpinBox;
      HorizontalSizer DefectVeto_Sizer;
      CheckBox        DefectVeto_CheckBox;
      HorizontalSizer Flatten_Sizer;
      CheckBox        FlattenEnabled_CheckBox;
      SpinBox         FlattenOrder_SpinBox;
```

(Delete `InputSource_GroupBox`, `InputSource_Sizer`, `Views_Sizer`, `Files_Sizer`, `BlendParams_GroupBox`, `BlendParams_Sizer`.)

New private member function declarations on the interface:

```cpp
   void e_ToggleSection( SectionBar& sender, Control& section, bool start );
   void e_NoticeMouseRelease( Control& sender, const pcl::Point& pos,
                              int button, unsigned buttons, unsigned modifiers );
   void e_LogoPaint( Control& sender, const pcl::Rect& updateRect );
```

(Handler signatures: `SectionBar::section_event_handler` is `void (Control::*)( SectionBar&, Control&, bool )` — `/opt/PixInsight/include/pcl/SectionBar.h:266`; mouse/paint handler types are in `pcl/Control.h` as `Control::mouse_button_event_handler` / `Control::paint_event_handler` — check exact typedef names in that header when implementing and match them.)

- [ ] **Step 2: MmmInterface.cpp — header notice pane**

At the top of `GUIData::GUIData( MmmBlendInterface& w )`:

```cpp
   //
   // Header notice: chevron logo + title / tagline / copyright, in the style
   // of the MosaicByCoordinates script header.
   //
   Logo_Bitmap = Bitmap( MMM_CHEVRON_SVG, sizeof( MMM_CHEVRON_SVG ) - 1, "SVG" );

   Logo_Control.SetScaledFixedSize( 40, 40 );
   Logo_Control.OnPaint( (Control::paint_event_handler)&MmmBlendInterface::e_LogoPaint, w );

   Title_Label.SetText( "Mega Merge Mosaic version " MMM_VERSION_STRING );
   Title_Label.SetStyleSheet( w.ScaledStyleSheet( "QLabel { font-weight: bold; }" ) );

   Tagline_Label.SetText( "Big mosaics, no big deal." );

   Copyright_Label.SetText( "Copyright (c) 2026 Astrometrical | astrometrical.com" );
   Copyright_Label.SetToolTip( "<p>Visit https://astrometrical.com</p>" );
   Copyright_Label.SetCursor( StdCursor::PointingHand );
   Copyright_Label.OnMouseRelease( (Control::mouse_button_event_handler)&MmmBlendInterface::e_NoticeMouseRelease, w );

   NoticeText_Sizer.SetSpacing( 2 );
   NoticeText_Sizer.Add( Title_Label );
   NoticeText_Sizer.Add( Tagline_Label );
   NoticeText_Sizer.Add( Copyright_Label );

   Notice_Sizer.SetMargin( 6 );
   Notice_Sizer.SetSpacing( 8 );
   Notice_Sizer.Add( Logo_Control );
   Notice_Sizer.Add( NoticeText_Sizer, 100 );
   Notice_Control.SetSizer( Notice_Sizer );
```

(`SetCursor( StdCursor::PointingHand )` needs `#include <pcl/Cursor.h>`; if the enum name differs, check `pcl/Cursor.h` — the standard cursor namespace is `StdCursor` and the pointing-hand member is there; drop the cursor line if absent.)

Handlers, alongside the other event handlers:

```cpp
void MmmBlendInterface::e_LogoPaint( Control& sender, const pcl::Rect& )
{
   Graphics g( sender );
   if ( !GUI->Logo_Bitmap.IsNull() )
      g.DrawScaledBitmap( sender.BoundsRect(), GUI->Logo_Bitmap );
}

void MmmBlendInterface::e_NoticeMouseRelease( Control&, const pcl::Point&, int button, unsigned, unsigned )
{
   if ( button != MouseButton::Left )
      return;
   try
   {
#ifdef _WIN32
      ExternalProcess::StartProgram( "cmd.exe", StringList() << "/c" << "start" << "https://astrometrical.com" );
#elif defined( __APPLE__ )
      ExternalProcess::StartProgram( "open", StringList() << "https://astrometrical.com" );
#else
      ExternalProcess::StartProgram( "xdg-open", StringList() << "https://astrometrical.com" );
#endif
   }
   catch ( ... )
   {
      // Non-fatal: the URL is visible in the label text anyway.
   }
}
```

Add includes `<pcl/Graphics.h>`, `<pcl/ExternalProcess.h>`. (`DrawScaledBitmap( const Rect&, const Bitmap& )` — check exact name in `pcl/Graphics.h`; it exists alongside `DrawBitmap` at `Graphics.h:1325`; if only `DrawBitmap(int,int,Bitmap)` fits, pre-scale the bitmap once with `Bitmap::ScaledToWidth`/`Scaled` instead. `MouseButton::Left` is in `pcl/ButtonCodes.h`.)

- [ ] **Step 3: Target Frames section**

```cpp
   //
   // Target Frames section.
   //
   ViewsMode_RadioButton.SetText( "Views" );
   ViewsMode_RadioButton.SetChecked();
   ViewsMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );

   FilesMode_RadioButton.SetText( "Files" );
   FilesMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );

   InputMode_Sizer.SetSpacing( 8 );
   InputMode_Sizer.Add( ViewsMode_RadioButton );
   InputMode_Sizer.Add( FilesMode_RadioButton );
   InputMode_Sizer.AddStretch();

   Views_TreeBox.SetNumberOfColumns( 1 );
   Views_TreeBox.SetHeaderText( 0, "View Id" );
   Views_TreeBox.EnableMultipleSelections();
   Views_TreeBox.SetScaledMinSize( 400, 120 );

   AddViews_PushButton.SetText( "Add Views..." );
   AddViews_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddViewsClick, w );

   RemoveView_PushButton.SetText( "Remove" );
   RemoveView_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveViewClick, w );

   ViewButtons_Sizer.SetSpacing( 6 );
   ViewButtons_Sizer.Add( AddViews_PushButton );
   ViewButtons_Sizer.Add( RemoveView_PushButton );
   ViewButtons_Sizer.AddStretch();

   Files_TreeBox.SetNumberOfColumns( 1 );
   Files_TreeBox.SetHeaderText( 0, "File Path" );
   Files_TreeBox.EnableMultipleSelections();
   Files_TreeBox.SetScaledMinSize( 400, 120 );

   AddFiles_PushButton.SetText( "Add Files..." );
   AddFiles_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddFilesClick, w );

   RemoveFile_PushButton.SetText( "Remove" );
   RemoveFile_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveFileClick, w );

   FileButtons_Sizer.SetSpacing( 6 );
   FileButtons_Sizer.Add( AddFiles_PushButton );
   FileButtons_Sizer.Add( RemoveFile_PushButton );
   FileButtons_Sizer.AddStretch();

   TargetFrames_Sizer.SetSpacing( 4 );
   TargetFrames_Sizer.Add( InputMode_Sizer );
   TargetFrames_Sizer.Add( Views_TreeBox );
   TargetFrames_Sizer.Add( ViewButtons_Sizer );
   TargetFrames_Sizer.Add( Files_TreeBox );
   TargetFrames_Sizer.Add( FileButtons_Sizer );
   TargetFrames_Control.SetSizer( TargetFrames_Sizer );

   TargetFrames_SectionBar.SetTitle( "Target Frames" );
   TargetFrames_SectionBar.SetSection( TargetFrames_Control );
   TargetFrames_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );
```

Note the ASCII `"..."` on both Add buttons (fixes the `â€¦` mojibake).

- [ ] **Step 4: Parameters section**

```cpp
   //
   // Parameters section.
   //
   SessionDir_Label.SetText( "Session directory:" );

   SessionDir_Edit.OnEditCompleted( (Edit::edit_event_handler)&MmmBlendInterface::e_SessionDirEditCompleted, w );

   SessionDir_ToolButton.SetIcon( w.ScaledResource( ":/browser/select-file.png" ) );
   SessionDir_ToolButton.SetScaledFixedSize( 20, 20 );
   SessionDir_ToolButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SessionDirBrowseClick, w );

   SessionDir_Sizer.SetSpacing( 4 );
   SessionDir_Sizer.Add( SessionDir_Label );
   SessionDir_Sizer.Add( SessionDir_Edit, 100 );
   SessionDir_Sizer.Add( SessionDir_ToolButton );
```

(`ToolButton::SetIcon` takes a `Bitmap`: use `SetIcon( Bitmap( w.ScaledResource( ":/browser/select-file.png" ) ) )` if the String overload doesn't exist — check `pcl/ToolButton.h`.)

The remaining rows (`InputSelect`, `BlendMode`, `Feather`, `SurfaceOrder`, `BandRows`, `DefectVeto`, `Flatten`) keep their existing construction (updated ranges from Task 1: SurfaceOrder `SetRange(0,2)` — **update the existing `SetRange(0,8)` here**; FlattenOrder `SetRange(1,2)` — **update the existing `SetRange(0,8)`**), each added to `Parameters_Sizer`:

```cpp
   Parameters_Sizer.SetSpacing( 4 );
   Parameters_Sizer.Add( SessionDir_Sizer );
   Parameters_Sizer.Add( InputSelect_Sizer );
   Parameters_Sizer.Add( BlendMode_Sizer );
   Parameters_Sizer.Add( Feather_NumericControl );
   Parameters_Sizer.Add( SurfaceOrder_Sizer );
   Parameters_Sizer.Add( BandRows_Sizer );
   Parameters_Sizer.Add( DefectVeto_Sizer );
   Parameters_Sizer.Add( Flatten_Sizer );
   Parameters_Control.SetSizer( Parameters_Sizer );

   Parameters_SectionBar.SetTitle( "Parameters" );
   Parameters_SectionBar.SetSection( Parameters_Control );
   Parameters_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );
```

`DefectVeto_Sizer` is new (wraps the checkbox so it indents like other rows in Task 6):

```cpp
   DefectVeto_Sizer.Add( DefectVeto_CheckBox );
   DefectVeto_Sizer.AddStretch();
```

- [ ] **Step 5: Global sizer + section-toggle handler**

```cpp
   Global_Sizer.SetMargin( 8 );
   Global_Sizer.SetSpacing( 6 );
   Global_Sizer.Add( Notice_Control );
   Global_Sizer.Add( TargetFrames_SectionBar );
   Global_Sizer.Add( TargetFrames_Control );
   Global_Sizer.Add( Parameters_SectionBar );
   Global_Sizer.Add( Parameters_Control );

   w.SetSizer( Global_Sizer );
   w.EnsureLayoutUpdated();
   w.AdjustToContents();
```

Handler (standard PCL section-toggle idiom — fix the tree boxes' height while animating, restore afterwards, and let the window shrink when a section closes):

```cpp
void MmmBlendInterface::e_ToggleSection( SectionBar&, Control& section, bool start )
{
   if ( start )
   {
      GUI->Views_TreeBox.SetFixedHeight();
      GUI->Files_TreeBox.SetFixedHeight();
   }
   else
   {
      GUI->Views_TreeBox.SetScaledMinHeight( 120 );
      GUI->Views_TreeBox.SetMaxHeight( int_max );
      GUI->Files_TreeBox.SetScaledMinHeight( 120 );
      GUI->Files_TreeBox.SetMaxHeight( int_max );
      if ( GUI->TargetFrames_Control.IsVisible() )
         SetVariableHeight();
      else
         SetFixedHeight();
      AdjustToContents();
   }
}
```

- [ ] **Step 6: Mode-dependent visibility (item 13)**

Replace `UpdateInputModeControls()`:

```cpp
void MmmBlendInterface::UpdateInputModeControls()
{
   // Show only the active side's list; hiding (not disabling) matches the
   // reference tools and keeps the window compact.
   GUI->Views_TreeBox.SetVisible( m_viewsMode );
   GUI->AddViews_PushButton.SetVisible( m_viewsMode );
   GUI->RemoveView_PushButton.SetVisible( m_viewsMode );

   GUI->Files_TreeBox.SetVisible( !m_viewsMode );
   GUI->AddFiles_PushButton.SetVisible( !m_viewsMode );
   GUI->RemoveFile_PushButton.SetVisible( !m_viewsMode );

   EnsureLayoutUpdated();
   AdjustToContents();
}
```

(`Control::SetVisible( bool )` — `/opt/PixInsight/include/pcl/Control.h:777`.)

- [ ] **Step 7: Build.** Expected: exit 0.

- [ ] **Step 8: Visual smoke check (manual, best-effort)**

If a signed local install flow is set up (see `module/README.md`), install and open the tool window: header notice with chevron + tagline + copyright; two SectionBars that collapse; only one list visible at a time; no mojibake on buttons; clicking the copyright line opens astrometrical.com. Otherwise defer to the user's next PixInsight session and note it in the report.

- [ ] **Step 9: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): branded header, SectionBar layout, mode-dependent lists, ascii ellipses"
```

---

### Task 6: Alignment, sizing, and tooltips

**Files:**
- Modify: `integration/pixinsight/module/MmmInterface.cpp` (GUIData constructor only)

**Interfaces:**
- Consumes: Task 5's control names.
- Produces: visual polish only; no API changes.

- [ ] **Step 1: Label column (right-aligned, fixed width) and edit widths**

At the top of `GUIData::GUIData`, before any control setup:

```cpp
   int labelWidth1 = w.Font().Width( String( "Flatten background, order:" ) + 'M' );
   int editWidth1  = w.Font().Width( String( '0', 8 ) );
```

Apply to every labeled row in the Parameters section:

```cpp
   SessionDir_Label.SetFixedWidth( labelWidth1 );
   SessionDir_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   InputSelect_Label.SetFixedWidth( labelWidth1 );
   InputSelect_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   BlendMode_Label.SetFixedWidth( labelWidth1 );
   BlendMode_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   Feather_NumericControl.label.SetFixedWidth( labelWidth1 );
   Feather_NumericControl.edit.SetFixedWidth( editWidth1 );

   SurfaceOrder_Label.SetFixedWidth( labelWidth1 );
   SurfaceOrder_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );

   BandRows_Label.SetFixedWidth( labelWidth1 );
   BandRows_Label.SetTextAlignment( TextAlign::Right | TextAlign::VertCenter );
```

(NumericControl's `label` is right-aligned by PCL already; `label`/`edit` members verified at `pcl/NumericControl.h:51-52`.)

Indent the two checkbox rows to the label column so their boxes align with the edit column (PI reference style):

```cpp
   DefectVeto_Sizer.AddSpacing( labelWidth1 + 4 );   // insert BEFORE Add( DefectVeto_CheckBox )
   Flatten_Sizer.AddSpacing( labelWidth1 + 4 );      // insert BEFORE Add( FlattenEnabled_CheckBox )
```

(`AddSpacing` calls go first in each sizer's Add order.) Give the spin boxes a consistent width: `SurfaceOrder_SpinBox`, `BandRows_SpinBox`, `FlattenOrder_SpinBox` each get `SetFixedWidth( editWidth1 )`. Give both combo boxes a consistent minimum: `InputSelect_ComboBox.SetMinWidth( editWidth1*2 )`, `BlendMode_ComboBox.SetMinWidth( editWidth1*2 )`.

All row sizers use `SetSpacing( 4 )` and end with `AddStretch()` (Feather's NumericControl manages its own slider stretch — do not wrap it).

- [ ] **Step 2: Tooltips**

Add these exact `SetToolTip` calls (PCL convention is `<p>`-wrapped rich text):

```cpp
   ViewsMode_RadioButton.SetToolTip( "<p>Blend image views that are open in the current PixInsight workspace.</p>" );
   FilesMode_RadioButton.SetToolTip( "<p>Blend image files read directly from disk, without opening them as views.</p>" );
   Views_TreeBox.SetToolTip( "<p>The mosaic panels to merge. All panels must belong to the same mosaic: "
      "registered full-canvas panels (e.g. MosaicByCoordinates output) or plate-solved panels.</p>" );
   AddViews_PushButton.SetToolTip( "<p>Add open views as mosaic panels.</p>" );
   RemoveView_PushButton.SetToolTip( "<p>Remove the selected views from the list.</p>" );
   Files_TreeBox.SetToolTip( "<p>The mosaic panel files to merge. All panels must belong to the same mosaic: "
      "registered full-canvas panels (e.g. MosaicByCoordinates output) or plate-solved panels.</p>" );
   AddFiles_PushButton.SetToolTip( "<p>Add image files as mosaic panels.</p>" );
   RemoveFile_PushButton.SetToolTip( "<p>Remove the selected files from the list.</p>" );

   SessionDir_Edit.SetToolTip( "<p>Directory where analysis is cached (a *.mmm-session folder). "
      "Re-running with the same session directory reuses completed stages.</p>" );
   SessionDir_ToolButton.SetToolTip( "<p>Select the session directory.</p>" );

   InputSelect_ComboBox.SetToolTip( "<p>How the input panels are interpreted:</p>"
      "<p><b>Auto</b> — panels with identical dimensions are treated as registered full-canvas panels; "
      "otherwise they are treated as plate-solved panels and reprojected.</p>"
      "<p><b>Aligned</b> — force registered full-canvas panels (all inputs must share the same dimensions).</p>"
      "<p><b>Solved</b> — force reprojection from each panel's astrometric solution.</p>" );

   BlendMode_ComboBox.SetToolTip( "<p><b>Feather</b> — weighted-average ramp across the overlap.</p>"
      "<p><b>TwoBand</b> — low frequencies feathered, high frequencies seam-cut.</p>"
      "<p><b>Pyramid</b> — full multiband blend; best quality (default).</p>" );

   Feather_NumericControl.SetToolTip( "<p>Feather ramp length in canvas pixels (1-1024). "
      "Larger values give smoother low-frequency transitions across panel overlaps.</p>" );

   SurfaceOrder_SpinBox.SetToolTip( "<p>Polynomial order (0-2) of the per-panel surface fit used to match "
      "panel backgrounds: 0 = constant offset, 1 = plane, 2 = quadratic (default).</p>" );

   BandRows_SpinBox.SetToolTip( "<p>Output rows per streamed band. Advanced: affects streaming granularity "
      "and peak memory only; the default (256) is fine for most images.</p>" );

   DefectVeto_CheckBox.SetToolTip( "<p>Reject single-panel defects (satellite trails, stacking edge artifacts) "
      "in overlap regions by cross-checking panels during the detail blend.</p>" );

   FlattenEnabled_CheckBox.SetToolTip( "<p>Remove a global background gradient from the merged result, "
      "fitting a polynomial of the chosen order (1 or 2). The central background level is preserved.</p>" );
   FlattenOrder_SpinBox.SetToolTip( "<p>Background-flatten polynomial order: 1 = plane, 2 = quadratic.</p>" );
```

- [ ] **Step 3: Build.** Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): PI-style alignment (right labels, fixed columns) + full tooltips"
```

---

### Task 7: Snazzy console progress

**Files:**
- Modify: `integration/pixinsight/module/MmmExecution.cpp` (`ConsoleProgress` only)

**Interfaces:**
- Consumes: stage strings emitted by the host: `"reproject"`, `"analyze"`, `"blend"` (PROTOCOL.md section 6).
- Produces: single-line, in-place progress per stage in the Process Console; Console Abort still cancels.

- [ ] **Step 1: Rewrite ConsoleProgress**

Replace the class body with a line-overwriting, colored progress renderer. The PixInsight console supports `\r` in-line rewrites, `<end>`/`<b>` tags and 24-bit ANSI color escapes (`pcl/Console.h:91,172,318`).

```cpp
class ConsoleProgress : public mmm::ProgressCallback
{
public:
   mmm::Host* host = nullptr;

   void on_progress( const std::string& stage, uint64_t done, uint64_t total ) override
   {
      // Friendly stage names; fall back to the wire string verbatim.
      String name;
      if ( stage == "reproject" )    name = "Reprojecting";
      else if ( stage == "analyze" ) name = "Analyzing";
      else if ( stage == "blend" )   name = "Blending";
      else                           name = String( stage.c_str() );

      if ( name != m_stage )
      {
         // Commit the previous stage's line before starting a new one.
         if ( !m_stage.IsEmpty() )
            m_console.WriteLn();
         m_stage = name;
         m_lastPercent = -1;
      }

      if ( total > 0 )
      {
         int percent = int( 100*done/total );
         if ( percent != m_lastPercent )
         {
            m_lastPercent = percent;
            // 24-char bar in Astrometrical teal; \r rewrites the line in place.
            int fill = 24*percent/100;
            String bar;
            for ( int i = 0; i < 24; ++i )
               bar += ( i < fill ) ? "#" : "-";
            m_console.Write( String().Format(
               "<end>\r<b>%-13s</b> \x1b[38;2;42;149;171m[%s]\x1b[39m %3d%%",
               IsoString( m_stage ).c_str(), IsoString( bar ).c_str(), percent ) );
            if ( percent == 100 )
               m_console.WriteLn();
         }
      }
      else
         m_console.Write( String().Format( "<end>\r<b>%-13s</b> %llu",
                                           IsoString( m_stage ).c_str(), (unsigned long long)done ) );

      // Deliver pending GUI events and honor the Console's own abort button --
      // this is the (only) cancellation path.
      Module->ProcessEvents();
      if ( m_console.AbortRequested() )
      {
         m_console.Abort();
         if ( host != nullptr )
            host->cancel();
      }
   }

private:
   Console m_console;
   String  m_stage;
   int     m_lastPercent = -1;
};
```

Implementation notes for the engineer:
- `String().Format` with `%s` needs 8-bit strings — hence the `IsoString(...).c_str()` conversions. If `%-13s` padding misbehaves with the console's proportional font, pad with `String::LeftJustified( 13 )` instead of printf padding.
- ASCII `#`/`-` bar characters are deliberate (console font coverage is guaranteed); do not switch to Unicode block characters.
- Keep the comment block above the class that explains the synchronous-execution decision; update its first paragraph to say progress is Console-only now (the interface no longer mirrors it).

- [ ] **Step 2: Build.** Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): in-place colored console progress bars per stage"
```

---

### Task 8: Process documentation (integrated doc browser)

**Files:**
- Create: `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`
- Modify: `integration/pixinsight/module/README.md`

**Interfaces:**
- Consumes: process Id `MegaMergeMosaic` (Task 2). PixInsight's integrated documentation system (enabled by default — `MetaProcess::CanBrowseDocumentation()` returns true, and the tool window already shows the Browse Documentation button via `InterfaceFeature::DefaultGlobal`) opens `<PixInsight-install>/doc/tools/<Id>/<Id>.html`.
- Produces: a self-contained HTML doc page, installable by copying the `doc/tools/MegaMergeMosaic` folder into the PixInsight install.

- [ ] **Step 1: Write the doc page**

Create `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`, self-contained (inline CSS, no external assets), structured like PIDoc tool pages: title block (tool name, version, categories), description, then one section per parameter group. Content requirements:

- Title: "MegaMergeMosaic" with version from `MmmVersion.h` (hardcode `1.0.0`; add an HTML comment `<!-- keep in sync with MmmVersion.h -->`).
- Intro paragraph: what the tool does (merge/blend pre-aligned or plate-solved mosaic panels; overlap-band analysis; gain/offset matching; seam-aware multiband blending), tagline, and inputs it expects (MosaicByCoordinates output or plate-solved panels; views or files).
- Parameters section documenting each control with the same semantics as the Task 6 tooltips (Target frames, Session directory, Input, Blend mode, Feather 1–1024, Surface fit order 0–2, Band rows, Cross-panel defect veto, Flatten background 1–2).
- A "Usage" section: minimum two panels, choose a session directory, progress appears in the Process Console, cancel via the Console's Pause/Abort button.
- Footer: `Copyright &copy; 2026 Astrometrical &mdash; <a href="https://astrometrical.com">astrometrical.com</a>`.

Write the full HTML in this step (roughly 120 lines; plain `<h1>/<h2>/<table>` markup, dark-on-light neutral styling).

- [ ] **Step 2: Validate the HTML**

Run: `xmllint --html --noout integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`
Expected: no errors (warnings about HTML5 tags acceptable; prefer XHTML-compatible markup so there are none).

- [ ] **Step 3: README install step**

In `integration/pixinsight/module/README.md`, in the local install section, add after the module-copy step:

```markdown
### Process documentation

The integrated documentation page lives in
[`../doc/tools/MegaMergeMosaic/`](../doc/tools/MegaMergeMosaic/). Install it so
the tool window's Browse Documentation button works:

​```sh
sudo mkdir -p /opt/PixInsight/doc/tools/MegaMergeMosaic
sudo cp ../doc/tools/MegaMergeMosaic/MegaMergeMosaic.html /opt/PixInsight/doc/tools/MegaMergeMosaic/
​```

The distribution package (relocating to the Astrometrical website repo — see
[`../repo/README.md`](../repo/README.md)) should ship this file the same way.
```

(Remove the zero-width characters around the fence when writing — they're only there so this plan's own fencing survives.)

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/doc integration/pixinsight/module/README.md
git commit -m "docs(pixinsight): integrated documentation page for MegaMergeMosaic"
```

---

## Self-Review

- **Spec coverage:** (1) icon → Task 3 (`MmmIcon.h` is the edit-me file); (2) name → Task 2; (3) notice + branding + link → Task 5; (4) collapsible sections → Task 5; (5) margins/alignment → Task 6; (6) tooltips → Task 6, process help → Task 8; (7) ROI removal → Task 1; (8) progress/cancel removal → Task 4, console progress → Task 7 (Console abort path retained); (9) feather int 1–1024 default 256 → Task 1 (user approved min 1); (10) downsample removal → Task 1; (11) order ranges → Task 1 (+ SpinBox ranges in Task 5 Step 4); (12) mojibake → Task 5 (ASCII `...`, ToolButton browse icon); (13) list visibility → Task 5 Step 6.
- **Placeholder scan:** all steps carry concrete code or exact commands; the two "check exact name in header" notes are bounded verification instructions with a stated fallback, not deferred design.
- **Type consistency:** `p_feather` is `int32` everywhere after Task 1 (params, instance, execution snapshot, UI handler). GUIData member names introduced in Task 5 are the ones Task 6 references. `request_cancel` fully removed in Task 4 (declaration + definition + sole caller deleted with the Cancel button).
