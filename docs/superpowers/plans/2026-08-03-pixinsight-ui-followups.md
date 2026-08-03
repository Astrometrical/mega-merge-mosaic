# PixInsight UI Follow-ups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the console progress line-rewind bug, add a startup branding banner, wire the Reset button, rename three parameters to user-facing language, and default the session directory to an auto-cleaned temp folder behind a new collapsed "Advanced" section.

**Architecture:** Continues on branch `pixinsight-ui-polish` on top of `6def326`. All changes in `integration/pixinsight/module/` plus the doc HTML page. The console renderer switches from `\r` (which the PixInsight console treats as `<bol>` — cursor move + INSERT, causing the observed interleaved output) to `\b` backspace erasure (`<bsp>` deletes glyphs — the idiom PCL's own StandardStatus uses). Session-dir lifecycle is owned by `run_blend` with an RAII guard.

**Tech Stack:** PCL 2.10.4 (`/opt/PixInsight/include/pcl`), C++17/20, `std::filesystem` for recursive deletion, GNU make (`makefile-x64`).

## Global Constraints

- All user-visible string literals in C++ sources plain ASCII (`(c)`, `...`, `" - "`). ANSI escapes written as `\x1b[...m` escape sequences are fine.
- Teal accent for console styling: `\x1b[38;2;42;149;171m`, reset `\x1b[39m`.
- Renames (exact user-facing text):
  - Label `Input:` → `Panel registration method:`; combo items in enum order: `Auto`, `Pre-aligned (MosaicByCoordinates)`, `Align by astrometric solution`. (Parameter id `inputSelect` and element ids `Auto`/`Aligned`/`Solved` are UNCHANGED — scripting surface stays stable.)
  - Label `Surface fit order:` → `Gradient fit order:` (parameter id `surfaceOrder` unchanged).
  - Checkbox `Flatten background, order:` → `Gradient removal, order:` (parameter ids `flatten`/`flattenEnabled` unchanged).
- `labelWidth1` reference string becomes the new longest label: `"Panel registration method:"`.
- Session directory: empty (the new default) ⇒ auto-generate `<File::SystemTempDirectory()>/mmm-session-<pid>-<counter>` per run and best-effort delete it recursively when the run ends (success, error, or cancel). A user-specified directory is used as-is and NEVER deleted.
- The Console abort path in `ConsoleProgress::on_progress` must survive untouched (ProcessEvents → AbortRequested → Abort → host->cancel).
- Every task ends with a clean build: `cd integration/pixinsight/module && make -f makefile-x64 -j$(nproc)` exit 0, no new warnings. No changes to `crates/` or `integration/pixinsight/host/`.
- Commit after every task with the given message.

## File Structure

- Modify: `integration/pixinsight/module/MmmExecution.cpp` — renderer fix, banner, session-dir lifecycle.
- Modify: `integration/pixinsight/module/MmmInterface.h` / `.cpp` — ResetInstance, renames, Advanced section.
- Modify: `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html` — renamed parameters + session-dir semantics.
- Possibly modify: `integration/pixinsight/module/README.md` — only if it names renamed parameters (grep).

---

### Task 1: Console renderer fix (backspace erasure) + startup banner

**Files:**
- Modify: `integration/pixinsight/module/MmmExecution.cpp`

**Interfaces:**
- Consumes: `MMM_VERSION_STRING` from `MmmVersion.h` (add the include).
- Produces: `ConsoleProgress` with in-place rewriting that actually works in the PixInsight console; free function `WriteBanner()` in the anonymous namespace, called once at the top of `run_blend()`.

- [ ] **Step 1: Rewrite the ConsoleProgress rendering**

Why: the PixInsight console maps `\r` to `<bol>` — a cursor move; subsequent text is INSERTED at line start, not overwritten (observed: interleaved "Blending [..] 13%[..] 12%[..]" lines). `\b` maps to `<bsp>`, which DELETES the previous glyph — this is what PCL's StandardStatus uses for in-place percentages. Rewrite by erasing the previously written visible glyphs with backspaces, then writing the new text. Markup tags (`<b>`) and ANSI SGR escapes are zero-glyph — they must NOT be counted in the erase length.

Replace the class with:

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
         if ( !m_stage.IsEmpty() && m_lineLen > 0 )
            CommitLine();
         m_stage = name;
         m_lastPercent = -1;
         m_lineLen = 0;
      }

      if ( total > 0 )
      {
         int percent = int( 100*done/total );
         if ( percent != m_lastPercent )
         {
            m_lastPercent = percent;
            int fill = 24*percent/100;

            // Visible glyphs, assembled separately from markup so the erase
            // length can be counted exactly (tags/ANSI are zero-width).
            IsoString name8 = IsoString( m_stage );
            if ( name8.Length() < 13 )
               name8.Append( ' ', 13 - name8.Length() );
            IsoString bar;
            bar.Append( '#', fill );
            bar.Append( '-', 24 - fill );
            IsoString pct = IsoString().Format( "%3d%%", percent );

            String line = "<b>" + String( name8 ) + "</b> "
                          "\x1b[38;2;42;149;171m[" + String( bar ) + "]\x1b[39m " + String( pct );
            int visible = int( name8.Length() ) + 1 + 1 + 24 + 1 + 1 + int( pct.Length() );

            Redraw( line, visible );
            if ( percent == 100 )
               CommitLine();
         }
      }
      else
      {
         IsoString name8 = IsoString( m_stage );
         if ( name8.Length() < 13 )
            name8.Append( ' ', 13 - name8.Length() );
         IsoString count = IsoString().Format( "%llu", (unsigned long long)done );
         Redraw( "<b>" + String( name8 ) + "</b> " + String( count ),
                 int( name8.Length() ) + 1 + int( count.Length() ) );
      }

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
   int     m_lineLen = 0;   // visible glyphs currently on the in-progress line

   // Erase the previously drawn glyphs with backspaces (the console's <bsp>
   // deletes a glyph; <bol>+write inserts, so \r cannot be used), then draw.
   void Redraw( const String& line, int visible )
   {
      String s = "<end>";
      if ( m_lineLen > 0 )
         s.Append( String( '\b', size_type( m_lineLen ) ) );
      s += line;
      m_console.Write( s );
      m_lineLen = visible;
   }

   void CommitLine()
   {
      m_console.WriteLn();
      m_lineLen = 0;
   }
};
```

Note: `String( '\b', n )` uses the same `(char, count)` constructor shape already used elsewhere (`String( '0', 8 )` in MmmInterface.cpp); verify the argument order against that existing call and `/opt/PixInsight/include/pcl/String.h`, and adapt if it is `(count, char)`. Keep the existing comment block above the class (synchronous-execution rationale) — update only sentences describing the rewrite mechanism if they mention `\r`.

- [ ] **Step 2: Startup banner**

Add `#include "MmmVersion.h"` and, in the anonymous namespace, a banner writer:

```cpp
// Shameless branding at the start of every run: teal Astrometrical chevron +
// name/version/tagline. ASCII only; ANSI truecolor for the teal.
void WriteBanner()
{
   const String teal  = "\x1b[38;2;42;149;171m";
   const String reset = "\x1b[39m";
   Console c;
   c.WriteLn( "<end><cbr>" );
   c.WriteLn( teal + "     /\\" + reset );
   c.WriteLn( teal + "    /  \\" + reset + "     <b>Mega Merge Mosaic</b> version " MMM_VERSION_STRING );
   c.WriteLn( teal + "   / /\\ \\" + reset + "    Big mosaics, no big deal." );
   c.WriteLn( teal + "  /_/  \\_\\" + reset + "   (c) 2026 Astrometrical | https://astrometrical.com" );
   c.WriteLn();
}
```

Call `WriteBanner();` as the FIRST statement of `run_blend()` (before validation, so even a validation error shows who we are). Mind the C++ escaping: each backslash in the art is `\\` in the literal; the blank chevron interior uses spaces.

- [ ] **Step 3: Build.** `cd integration/pixinsight/module && make -f makefile-x64 -j$(nproc)` — exit 0, no new warnings.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module/MmmExecution.cpp
git commit -m "fix(pixinsight): console progress via backspace erasure (\r inserts, not overwrites) + startup banner"
```

---

### Task 2: Reset button + parameter renames

**Files:**
- Modify: `integration/pixinsight/module/MmmInterface.h`
- Modify: `integration/pixinsight/module/MmmInterface.cpp`

**Interfaces:**
- Consumes: `TheMmmBlendProcess` (already included via MmmProcess.h).
- Produces: `void ResetInstance() override;` — the footer Reset button now restores defaults. Renamed labels per Global Constraints.

- [ ] **Step 1: ResetInstance override**

`MmmInterface.h`: declare `void ResetInstance() override;` next to the other ProcessInterface overrides (`ProcessInterface::ResetInstance()` is virtual — `/opt/PixInsight/include/pcl/ProcessInterface.h:707`).

`MmmInterface.cpp`, after `ImportProcess`:

```cpp
void MmmBlendInterface::ResetInstance()
{
   // The control bar's Reset button. Import a default-constructed instance;
   // ImportProcess() re-derives the Views/Files mode and refreshes controls.
   MmmBlendInstance defaultInstance( TheMmmBlendProcess );
   ImportProcess( defaultInstance );
}
```

- [ ] **Step 2: Renames**

In `GUIData::GUIData`:
- `InputSelect_Label.SetText( "Panel registration method:" );`
- Combo items (order MUST stay Auto/Aligned/Solved):
  ```cpp
  InputSelect_ComboBox.AddItem( "Auto" );
  InputSelect_ComboBox.AddItem( "Pre-aligned (MosaicByCoordinates)" );
  InputSelect_ComboBox.AddItem( "Align by astrometric solution" );
  ```
- `SurfaceOrder_Label.SetText( "Gradient fit order:" );`
- `FlattenEnabled_CheckBox.SetText( "Gradient removal, order:" );`
- `labelWidth1` reference string: `w.Font().Width( String( "Panel registration method:" ) + 'M' )`.

Updated tooltips (replace the existing ones for these four controls; keep `<p>` style, plain ASCII):

```cpp
   InputSelect_ComboBox.SetToolTip( "<p>How panels are placed on the output canvas:</p>"
      "<p><b>Auto</b> - detect the correct mode automatically from the inputs.</p>"
      "<p><b>Pre-aligned (MosaicByCoordinates)</b> - panels are registered full-canvas frames "
      "sharing the same dimensions (e.g. MosaicByCoordinates output); blend them directly.</p>"
      "<p><b>Align by astrometric solution</b> - reproject each panel onto a common frame "
      "using its astrometric solution (every panel must be plate-solved).</p>" );

   SurfaceOrder_SpinBox.SetToolTip( "<p>Polynomial order (0-2) of the per-panel gradient fit used "
      "to match panel backgrounds before blending: 0 = constant offset, 1 = plane, "
      "2 = quadratic (default).</p>" );

   FlattenEnabled_CheckBox.SetToolTip( "<p>Remove the residual global background gradient from the "
      "merged result, fitting a polynomial of the chosen order. The central background level "
      "is preserved.</p>" );
   FlattenOrder_SpinBox.SetToolTip( "<p>Gradient removal polynomial order: 1 = plane, 2 = quadratic.</p>" );
```

- [ ] **Step 3: Build.** Exit 0, no new warnings.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): working Reset button; rename registration/gradient parameters"
```

---

### Task 3: Auto temp session directory + cleanup + Advanced section

**Files:**
- Modify: `integration/pixinsight/module/MmmExecution.cpp`
- Modify: `integration/pixinsight/module/MmmInterface.h` / `.cpp`

**Interfaces:**
- Consumes: `File::SystemTempDirectory()` (`/opt/PixInsight/include/pcl/File.h:1901`), existing `s_runCounter` atomic in MmmExecution.cpp.
- Produces: empty `p_sessionDir` (now the default UX) auto-resolves to a per-run temp dir removed after the run; GUIData members `Advanced_SectionBar`, `Advanced_Control`, `Advanced_Sizer` containing the SessionDir row and BandRows row; section starts collapsed.

- [ ] **Step 1: MmmExecution.cpp — session lifecycle**

Remove the empty-sessionDir validation error in `run_blend()` (`"select a session directory"`). Add `#include <filesystem>`. In the anonymous namespace add:

```cpp
// Best-effort recursive removal of an auto-created temp session directory.
// Only ever invoked on paths this module generated under the system temp dir;
// user-specified session directories are never touched.
void RemoveSessionDir( const std::string& dirUtf8 )
{
   try
   {
      std::error_code ec;
      std::filesystem::remove_all(
         std::filesystem::path( reinterpret_cast<const char8_t*>( dirUtf8.c_str() ) ), ec );
      // ec deliberately ignored: cleanup is best-effort (spec: item 5).
   }
   catch ( ... )
   {
      // Never let cleanup failure mask the run's real outcome.
   }
}

// RAII: delete the auto temp session dir on every exit path of run_blend.
struct AutoSessionDirGuard
{
   std::string dir;
   bool        active = false;
   ~AutoSessionDirGuard()
   {
      if ( active )
         RemoveSessionDir( dir );
   }
};
```

In `run_blend()`, after the Params snapshot:

```cpp
   // Empty session dir (the default): run out of a fresh directory under the
   // system temp dir and remove it afterwards, success or failure. A
   // user-specified directory is used as-is and preserved (its cache makes
   // re-runs with the same inputs resume completed analysis stages).
   AutoSessionDirGuard sessionGuard;
   if ( p.sessionDir.empty() )
   {
      String t = File::SystemTempDirectory();
      if ( !t.EndsWith( '/' ) )
         t += '/';
      t += "mmm-session-" +
           String( (unsigned long)
#ifdef _WIN32
                   ::GetCurrentProcessId()
#else
                   ::getpid()
#endif
                 ) + "-" + String( (unsigned long long) s_runCounter.fetch_add( 1 ) );
      p.sessionDir      = std::string( t.ToUTF8().c_str() );
      sessionGuard.dir    = p.sessionDir;
      sessionGuard.active = true;
   }
```

(`String( (unsigned long) x )` — verify a suitable numeric String constructor/`String().Format` exists; if not, use `String().Format( "mmm-session-%lu-%llu", ... )` appended to `t`. The pid/getpid includes already exist in this file for `MakeShmName`.)

- [ ] **Step 2: MmmInterface — Advanced section**

`MmmInterface.h` GUIData: add after the Parameters section members:

```cpp
      // --- Advanced section ---------------------------------------------------
      SectionBar      Advanced_SectionBar;
      Control         Advanced_Control;
      VerticalSizer   Advanced_Sizer;
```

(The SessionDir_* and BandRows_* members already exist — they just move containers.)

`MmmInterface.cpp` `GUIData::GUIData`:
- Remove `SessionDir_Sizer` and `BandRows_Sizer` from `Parameters_Sizer`.
- After the Parameters section block:

```cpp
   //
   // Advanced section (collapsed by default).
   //
   Advanced_Sizer.SetSpacing( 4 );
   Advanced_Sizer.Add( SessionDir_Sizer );
   Advanced_Sizer.Add( BandRows_Sizer );
   Advanced_Control.SetSizer( Advanced_Sizer );

   Advanced_SectionBar.SetTitle( "Advanced" );
   Advanced_SectionBar.SetSection( Advanced_Control );
   Advanced_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );
```

- Global sizer: add `Advanced_SectionBar` + `Advanced_Control` after the Parameters pair.
- Start collapsed: immediately BEFORE `w.EnsureLayoutUpdated(); w.AdjustToContents();` add `Advanced_Control.Hide();` — check `/opt/PixInsight/include/pcl/SectionBar.h` first: if a collapse API exists on SectionBar (`SetSectionVisible(false)`/`HideSection()` around lines 219-234), prefer that so the bar's arrow state stays in sync; otherwise `Advanced_Control.Hide()` is the PI-module fallback idiom.
- Updated SessionDir tooltip:

```cpp
   SessionDir_Edit.SetToolTip( "<p>Optional working directory for the analysis cache "
      "(a *.mmm-session folder).</p>"
      "<p>Leave empty (default) to use a temporary directory that is removed automatically "
      "when the run finishes. Set a directory to keep the cache: re-running with the same "
      "directory and inputs resumes completed analysis stages.</p>" );
```

- [ ] **Step 3: Build.** Exit 0, no new warnings.

- [ ] **Step 4: Commit**

```bash
git add integration/pixinsight/module
git commit -m "feat(pixinsight): auto temp session dir with cleanup; Advanced section (session dir, band rows)"
```

---

### Task 4: Documentation updates for the renames + session semantics

**Files:**
- Modify: `integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html`
- Modify (only if grep hits): `integration/pixinsight/module/README.md`

- [ ] **Step 1: Update the doc page**

- Rename parameter headings/rows: "Input" → "Panel registration method" with the three new value names (Auto auto-detects; Pre-aligned (MosaicByCoordinates) = registered full-canvas frames; Align by astrometric solution = reprojection from plate solutions); "Surface fit order" → "Gradient fit order"; "Flatten background" → "Gradient removal" — text consistent with the Task 2 tooltips.
- Session directory row: now optional; empty = automatic temporary directory removed after the run; a set directory keeps the cache and re-runs with the same directory and inputs resume completed analysis stages. It now lives in the "Advanced" section together with Band rows — mention the section name.
- Usage section: drop "choose a session directory" as a required step (now optional/advanced).
- Keep the file XHTML-well-formed: validate with `python3 -c "import xml.dom.minidom,sys; xml.dom.minidom.parse('integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html')"` (xmllint is not installed on this machine).

- [ ] **Step 2: README check**

`grep -n "Surface fit\|Flatten background\|Input:" integration/pixinsight/module/README.md` — update any hits to the new names; if no hits, no change.

- [ ] **Step 3: Commit**

```bash
git add integration/pixinsight/doc integration/pixinsight/module/README.md
git commit -m "docs(pixinsight): renamed parameters + auto temp session semantics"
```

---

## Self-Review

- Item 1 (console rewind) → Task 1 (backspace erasure, exact-visible-glyph accounting, abort path untouched).
- Item 2 (branding) → Task 1 banner at run start.
- Item 3 (Reset) → Task 2 ResetInstance override.
- Item 4 (renames) → Task 2 (labels, combo items, tooltips, labelWidth reference) + Task 4 docs.
- Item 5 (session dir) → Task 3 (SystemTempDirectory default + RAII cleanup + Advanced section with Band rows) + Task 4 docs.
- Types: `p.sessionDir` stays `std::string` UTF-8; guard holds the same string; combo order Auto/Aligned/Solved preserved so `pcl_enum` values are unchanged.
