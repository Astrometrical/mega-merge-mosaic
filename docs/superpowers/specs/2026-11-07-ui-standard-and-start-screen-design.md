# Mega UI standard and the mmm start screen

Date: 2026-11-07. Status: approved by the user, 2026-11-07.

Supersedes §4 (Branding and look) and §5.1 (Start screen) of
`2026-11-02-standalone-app-design.md`. Everything else in that spec stands.

## 1. Intent

The first pass of the Mega Merge Mosaic (mmm) app works, but nobody has
thought about its UX or UI yet. The goal is an app people find pleasant
without noticing why, until they go back to PixInsight or similar and are
shocked by how slow, bloated, ugly and hard to use it is.

The work is three projects, in this order:

1. **A shared UI standard in `mega-ui`**: tokens, typography, icons and
   reusable components, so every Mega app (mmm now; a standalone Mega
   Image Integrator app and Photometrical-hosted components later) looks
   and behaves the same and echoes the Astrometrical brand.
2. **mmm's branding moments**, starting with the start screen.
3. **The in-session experience** (add, group, plan, merge, export). This
   gets its own design after this one.

This document covers projects 1 and 2. The existing tabs pick up the new
global style for free, but redesigning them is project 3.

## 2. Goals, non-goals, anti-goals

### Goals

- Consistency across apps comes first. A component built for mmm that
  isn't specific to mmm belongs in `mega-ui`, styled entirely from
  tokens.
- The apps echo Astrometrical (`../astrometrical/apps/web`): the same
  colours, text roles, display face, quiet dark surfaces, and the same
  OpenSeadragon-style tiled viewer as the hero device.
- The apps are dark-themed. A light theme is used only where explicitly
  decided.
- The start screen gets the user back into their last session as fast as
  possible, and looks like Astrometrical while doing it.

### Non-goals (this design)

- Redesigning the Panels, Plan and Blend tabs (project 3).
- Logging in with an Astrometrical account. It is parked: the user is
  open to it, but no benefit has been found yet. Keep an eye out for one.
- Pre-merge processing (gradient removal, colour correction, aberration
  correction). It will arrive later as a stage after grouping and before
  (or inside) planning. Nothing here should make that harder.
- A light theme.

### Anti-goals (failure even if it technically works)

- **Startup that feels slow,** or a hero that waits for anything else,
  such as the shelf, animations or a minimum display time.
- **Modal pop-ups for reporting issues.**
- **A Material- or mobile-flavoured UI** (big touch targets, floating
  action buttons, airy cards). These are dense desktop tools, closer to
  Lightroom or DaVinci than to an Android app.
- **Colours or sizes written as literals in app code** instead of using
  tokens.
- **Accessibility traded for looks.** The text greys went through a WCAG
  process on the web; they stay even where they look less appealing.

## 3. Constraints

- egui/eframe stay pinned at 0.36 (shared with workflow-platform). Use no
  third-party egui crate that doesn't support 0.36. Prefer none at all:
  the design system is ours.
- `mega-ui` must not depend on `mmm-core`; the app supplies data through
  small traits or plain structs.
- The glow renderer stays.
- The repo rules apply: tests don't use `test_data/`; every public item
  has a doc comment; fmt, clippy and doc are warning-free.
- The reopen target from the app spec §8 applies: first image on screen in
  under 1 s.

## 4. The style standard (`mega-ui`)

### 4.1 Principles

- **Echo Astrometrical within reason.** The web app's
  `apps/web/src/styles/app.css` and its style guide page
  (`StyleGuidePage.tsx`) are the source of truth for shared tokens. When
  the two disagree, move toward the web unless this document says
  otherwise.
- **Use tokens, never literals.** Apps use named colours, spacing, radii
  and text styles. The tokens live in one module in `mega-ui`.
- **One primary button per view.**
- **Never signal with colour alone.** Every severity colour comes with its
  icon.
- **Quiet and dense, never cramped.** The content (images) carries the
  colour; the chrome stays grey.

### 4.2 Colour tokens

Shared with the web (from `app.css`):

| Token | Hex | Use |
| --- | --- | --- |
| astro-900 | `#050505` | window background |
| astro-800 | `#0e0e0e` | panels |
| astro-700 | `#1a1a1a` | raised surfaces, secondary buttons, chips |
| astro-600 | `#2a2a2a` | borders |
| astro-500 | `#444444` | strong borders |
| astro-400 | `#777777` | fills only (fails AA as text) |
| astro-300…100 | `#aaaaaa` `#d0d0d0` `#eeeeee` | ramp, as on the web |
| content-strong | `#eeeeee` | headings, primary values |
| content-emphasis | `#d0d0d0` | lead text, values in property rows (currently missing from `mega-ui`) |
| content | `#aaaaaa` | body text (default) |
| content-muted | `#999999` | secondary text, labels |
| content-subtle | `#8a8a8a` | captions, placeholders |
| accent-400/500/600 | `#4ebcd4` `#2a95ab` `#1d7588` | OIII: primary actions, selection, focus, progress, info |
| green-300/400 | `#4ed090` `#2ab875` | OIII green: passed, success, merged |
| ha-500 | `#c83050` | Hα: problem fills, borders, icons on tinted backgrounds |

New for the Mega apps (approved by the user):

| Token | Hex | Use |
| --- | --- | --- |
| ha-400 | `#e05a76` | Hα *text*: ha-500 is only about 3.7:1 on our surfaces, under AA. This one is about 5.4:1. |
| sii-300 | `#f5c76a` | light warning: hover, SII chip text |
| sii-400 | `#e8a838` | warning text and icons (about 9:1 on astro-800) |
| sii-600 | `#a8741c` | warning fills and borders |

Where they're used:

- **Check severities:** Problem is ha-400 text with ha-500 accents and a
  circled-cross icon. Note is sii-400 with a triangle icon. Passed is
  green-300 with a tick. (Note going gold is my call, §8.)
- **Session states:** merged is green; plan ready is accent; re-merge
  needed is sii; a problem is ha.
- **Filter groups:** chips and every other group tint use the group's
  assigned colour from the existing group palette (app spec §5.2,
  `GROUP_PALETTE` in `panels_tab.rs`). Well-known filter names get
  physical colours there: Hα/R crimson, G green, OIII cyan, SII orange
  `#e0703a`, B accent-600, L white. The same colour tints images in Plan
  and in the hero. **The SII group keeps its orange.** SII gold is only
  for warnings, so an SII chip never reads as a warning. (The style-sheet
  mockup showed the SII chip in gold; that's superseded.) The palette
  and its filter-name mapping are generic astro knowledge, so they move
  into `mega-ui`. Saved palette indices in existing sessions keep working.

### 4.3 Typography

| Family | Font | Use |
| --- | --- | --- |
| UI | **Inter**, regular and semibold (static fonts, not the variable one) | everything |
| Numeric | **JetBrains Mono** regular | numbers that update live, sizes, coordinates, paths in property rows |

**Angora is not shipped as a font.** It appears only in fixed product
names, so each app's lockup ("MEGA MERGE MOSAIC" in tracked caps) is
pre-rendered from `Branding/Angora.otf` into an image. It is committed
next to the existing logo and wordmark PNGs, at a resolution that stays
crisp at 2× scaling. This keeps the font file out of the repo and avoids
faking letter-spacing for the brand.

**Licence:** Astrometrical has already paid for a commercial licence for
Angora, which its use on the website needed. The download's own note
says only "Freeware, commercial use requires donation" (fontspace), and
the paid licence is what covers use in the apps. If Angora is ever needed
for live text, embedding it is allowed under that licence; the NOTICE
entry would then cite it.

egui has no font-weight property, so each weight is its own named
family. The type scale is a set of named text styles (registered as
`TextStyle::Name`, with Body, Small, Heading, Button and Monospace
remapped onto it):

| Style | Size / weight | Use |
| --- | --- | --- |
| hero | 32 / semibold | session name on the start screen |
| title | 20 / semibold | view titles |
| heading | 15 / semibold | section headings |
| body | 13 / regular | default text, buttons |
| small | 12 / regular | metadata, secondary lines |
| overline | 10.5 / semibold, caps, +0.12em | section labels ("RECENT SESSIONS", "NOTES") |
| numeric | mono 12 | readouts |

egui 0.36 may not support letter-spacing on text. If it doesn't, the
builder fakes the overline tracking, for example by laying out per-glyph,
or drops it if faking it looks worse.

### 4.4 Spacing, radius and density

- Spacing scale: 4 · 8 · 12 · 16 · 24 · 32. Apps use these steps only.
- Radius: 4 for small controls; 6 for buttons and inputs; 8 for cards,
  panels and thumbnails; pill-shaped for chips.
- Controls are 30 px high (buttons, inputs, segmented controls). Large
  buttons are 38 px, used once per screen at most (for example Resume).
- The theme sets the whole `egui::Style`, not just `Visuals`: spacing,
  interaction sizes, text styles and radii. The ad hoc
  `corner_radius`/`item_spacing` values now scattered through `mmm-app`
  go away as those views are reworked.

### 4.5 Motion

- Hover and press colour changes take about 100 ms.
- The hero crossfades over 2 s and drifts in a slow Ken Burns pan, as on
  the web.
- Nothing animates for its own sake, and nothing ever delays content.
- **Reduced motion:** follow the OS setting where it can be read. With it
  on, crossfades become instant cuts and the Ken Burns drift stops.
  Whether there's also an in-app toggle is the builder's call.

### 4.6 Icons

**Lucide**, embedded as an icon font. It's the published `lucide-static`
font; the licence is ISC. *(Built differently, 2026-11-07: it was meant
to go in as a fallback to the UI family so icons could sit inside text,
but Inter has its own glyphs in the private-use range Lucide uses, and
Lucide has digits, so no single fallback chain works. Icons are drawn only
in a Lucide-only family, through `icons::font` and `icons::rich`.)* Its
outline style with 2 px rounded strokes
matches the Heroicons-style line icons on the web. `mega-ui` exposes
named constants for the glyphs we use. If the font proves unworkable in
egui 0.36, the fallback is Lucide SVGs through egui's image loading,
tinted.

Every shipped font gets its licence and NOTICE entry: Inter (OFL),
JetBrains Mono (OFL), Lucide (ISC).

### 4.7 Components

All of these live in `mega-ui`, styled only from tokens, and know nothing
about mmm:

| Component | Notes |
| --- | --- |
| Button | Primary, secondary, quiet, danger, ghost, icon; normal and large sizes. Ghost (translucent, frosted look) is only for use over imagery. Disabled at 40% opacity. |
| Text input | astro-900 fill; accent border plus a 2 px focus ring. |
| Notes field | Multi-line text, saved as you type (no Save button), with the same look as Text input. |
| Segmented control | Choosing a view (Composite / Hα / OIII …). |
| Chip | Neutral, or tinted with an emission-line colour. |
| Status | A coloured dot plus a label. |
| Check list | Problems and notes first; passed checks collapsed into one row ("41 checks passed") that expands, because the passed details can still be worth exploring. Each row has an icon, message and detail line, with a tinted background and a left border by severity. Builds on today's `mega-ui::checks`. |
| Property grid | Muted labels, emphasis values, mono for numbers. |
| Progress | A thin (2 px) accent bar with a label and an "x% · time left" readout. The job/status panels use it. |
| Toast | Non-modal, bottom corner, optional single action ("Show"). It's the only way the app notifies; never a modal dialog. |
| Overline section header | |
| Brand lockup | The Astrometrical logo mark, the app name (a pre-rendered Angora image, §4.3), and a small "BY" plus the Astrometrical wordmark. The app supplies its name image, so MII gets "MEGA IMAGE INTEGRATOR". Comes in two sizes: compact (top left of the start screen) and large (first run). |
| Starfield | A static, star-type-coloured starfield painted behind imagery and in empty states. It's a port of the look of the web's `starfield-core.ts` (spectral weights and colours), without the warp. It's cheap: painted once and cached as a texture or mesh. |
| Hero stage | The tiled viewer in presentation mode: no user pan or zoom, the camera driven by code (fit, Ken Burns drift), and crossfades between tile sources. Built on the existing `mega-ui::viewer`. |
| Session card | Thumbnail (an image clipped to the mosaic outline, or a plain image), title, a metadata line, status, pinned marker, and a selected state. The data comes from the app, so MII sessions can use the same card. |

### 4.8 Gallery and visual tests

- A `mega-ui` example, the **gallery**, shows every token and component.
  It's the egui equivalent of the web's style guide page, and the place
  to look when building anything new.
- Snapshot tests (`egui_kittest`) of the gallery and the start screen
  lock the look in. If image snapshots need egui_kittest's wgpu feature,
  that's acceptable for tests only.

### 4.9 Accessibility

- Text roles meet WCAG 2.2 AA (at least 4.5:1) on astro-900, -800 and
  -700, inherited from the web. New text colours must meet the same bar.
- Severity is never signalled by colour alone.
- Keyboard focus is always visible (the accent ring).
- Reduced motion is respected (§4.5).

## 5. The start screen (mmm)

### 5.1 Purpose

The main use is coming back to the last session. The second is starting a
new one. Finding an older session comes a distant third. Active mosaic
imagers shoot about 6 projects a year (12 if prolific), with around 5 on
the go at once.

### 5.2 Layout: "Hero stage"

The design is option A from the brainstorm mockups. It covers the whole
window:

- **Hero:** the selected session's image fills the window, biased to the
  right, sitting on the starfield. It's darkened by a left-to-right and a
  bottom-to-top gradient and a soft vignette so the text stays readable.
- **Top left:** the brand lockup.
- **Left: the session block**, for the selected session. It defaults to
  the most recently opened one. From top to bottom:
  - an overline: "CONTINUE WHERE YOU LEFT OFF" for the most recent
    session, otherwise "SESSION";
  - the session name (hero style);
  - when it was last opened, and its path;
  - panel and group chips (for example "18 panels", "Hα · 6", "OIII · 6",
    "SII · 6");
  - a status line ("2 panels added since last merge — re-merge needed");
  - a check summary;
  - the editable notes;
  - the buttons: **Resume session** (large primary), **New mosaic**
    (ghost) and **Open…** (quiet).
- **Bottom: the shelf**, headed "RECENT SESSIONS" with "All sessions ›"
  on the right. Pinned sessions come first, then by last opened. It shows
  as many cards as fit, roughly 5 to 10. Clicking a card makes it the
  selected session; double-clicking opens it.

### 5.3 What the hero shows

Chosen by the session's state:

1. **Merged:** the tiled result at full resolution in the hero stage,
   cycling through each group and each saved composition with a 2 s
   crossfade. Each view stays about 8 s, as on the web. Each view uses the
   session's saved Blend display stretch for that view. Stale tiles are
   still shown; staleness is reported in the status line, not on the
   image. If some tiles were never blended (for example only ROIs were
   blended), those areas show whatever the Blend tab shows for them.
2. **Planned but not merged** (preview pyramids exist): the Plan view
   (the panel previews in place), cycling through groups and compositions
   the same way, with the saved Plan stretch. No panel outlines or
   numbers.
3. **Neither:** no image. The starfield shows with one plain line saying
   there's nothing to show yet and what the next step is (for example
   "No panels yet — resume to add them").

Every image state shows a small label in a hero corner naming the current
view ("OIII · 6 panels", "SHO composite"), with ghost **pause** and
**next** controls, like the web hero. Cycling starts on the view last
shown in the session. With only one view, nothing cycles. The hero is for
looking at: on the start screen the user can't pan or zoom it.

### 5.4 Load order and speed

The hero comes first, and nothing waits for the shelf.

1. At launch, the most recent session is known straight away from the
   recent list in eframe storage, which is already loaded.
2. Its card facts and its coarsest pyramid level are read off the UI
   thread, and the hero appears as soon as that coarsest level arrives.
   Sharper levels fill in, as the viewer already does. **Target: the hero
   image is on screen in under 1 s from launch with a warm disk**, the
   same as the reopen target.
3. The shelf cards, their thumbnails and the other sessions' facts load
   only after the hero's first image is drawn.
4. There's no minimum display time and no loading animation in front of
   the hero. The starfield is simply the background the tiles fade in
   over.

**Reading without the lock.** The start screen never takes a session's
lock. That means another process (another window, or the CLI) could open
that session and blend while the hero is showing it. mmm-core's tile
store currently assumes whoever reads it holds the lock, because a blend
may truncate a store that's memory-mapped elsewhere. So the hero's reads
must (a) never stop another process from opening the session and (b)
survive one starting to write: no crash, at worst stale or missing tiles
until the hero moves on. How is the builder's call; plain file reads
instead of a memory map for the hero are acceptable. Sessions known to be
open in another window are never read live (§5.9).

### 5.5 Session card facts and where they come from

Each card and the session block show:
- name;
- last opened;
- panel count;
- groups with their panel counts;
- state: one of *no panels*, *plan ready*, *merged*, *merged · exported*,
  *re-merge needed*;
- problem and note counts;
- pinned or not;
- notes;
- availability: *missing* (the folder is gone or the drive unplugged) or
  *open in another window*.

These come from **small files only**. The start screen never opens a
session: it doesn't take the session lock, run analysis or load panels.
Facts that live only in an open session's memory today (check counts,
staleness, export state) are written by the app into a small summary in
the session folder whenever they change. The start screen reads that
summary. Without one (an older session, or one made with the CLI), the
card shows what the configuration alone provides and leaves out the rest.
The summary's format and location are the builder's call. It belongs in
`mmm-core`'s session definition so the CLI could write it too.

**Thumbnails** come from the coarsest level of the result pyramid, or of
the preview pyramids if there's no result yet, composed with the same
view and stretch the hero would use first. They're clipped to the
mosaic's actual outline (no-data is transparent), drawn over a dark
background. If that proves too slow for many sessions, caching a small
thumbnail image is acceptable (the builder's call).

### 5.6 Notes and pins

- **Notes** are free text belonging to the session, stored in its
  configuration so they travel with the folder. The CLI ignores them.
  They can be edited from the session block and are saved as you type. If
  the session is open in another window, its notes are read-only here.
  The in-session view will also show them (project 3).
- **Pins** are the user's own library preference and are stored in app
  storage alongside the recent list, not in the session.
- No tags or folders. With this many sessions they would be clutter.

### 5.7 All sessions

"All sessions ›" replaces the shelf area with a scrollable grid of every
known session's cards, with a search box over names and notes. Esc or a
back control returns to the shelf. "Known" means the recent list, with
the limit raised from today's 10 to something generous (the builder's
call; 100 is plenty).

### 5.8 New, Open and typed paths

- **New mosaic** opens one save-style dialog: choose the location and
  type the name in one step, which creates `<name>.mmm-session` there. The
  session opens straight away, in its first step (Panels), ready to add
  panels. This replaces today's two-field form (parent folder plus name).
- **Open…** opens a folder picker for an existing session.
- **Typed path:** today's always-works text field stays, because some
  places have no file dialog (for example WSLg without a desktop portal).
  It moves out of the main layout: it appears inline when a dialog can't
  be shown, and it's always reachable from a small menu next to Open….
- Opening a second session while one is open still opens a second window
  (app spec §3). **From a session window**, "Open another…" (and New)
  now launches a new window that starts on this start screen. That
  replaces today's floating picker over the session, which can't host a
  full-window hero. Whether a session window can also close its session
  and return to the start screen in place is for project 3, since it
  needs a session-to-start transition the app doesn't have today.

### 5.9 Missing and busy sessions

- **Missing:** the card stays on the shelf, greyed out, with the status
  "Missing" and two actions: **Locate…** (pick its new folder) and
  **Remove from list**. A missing session is never chosen as the default
  hero; the most recent available one is.
- **Open in another window:** the card says so and shows only its cached
  thumbnail; its tiles are never read live. Such a session is never the
  default hero. Usually it's the one you're already working on, and a new
  window was opened to get to a different one. The default is the most
  recent session that's available and not open elsewhere. Selecting it
  shows the session block over its static thumbnail. **Resume** becomes
  **Switch to window** where the platform allows; otherwise it says where
  the session is open.

### 5.10 First run

With no sessions, the hero area is the starfield with a larger brand
lockup. There are two buttons, **New mosaic** (primary) and **Open…**,
and one line about what mmm does. No shelf.

## 6. Decided by the user

- Build our own UI standard in `mega-ui` instead of adopting a component
  kit (no Material).
- Echo Astrometrical; dark by default.
- The accessible text greys stay, even where they look less appealing.
- Angora may be embedded (the licence allows redistribution in software).
- Inter is the UI font.
- The SII gold warning colour (sii-300/400/600) as proposed.
- Start screen option A, "Hero stage".
- The hero shows the full-resolution tiled result when merged and cycles
  groups and compositions with a crossfade. It shows the plan when not
  merged, and text when there's nothing to show.
- Getting the last session's hero on screen fast comes before the shelf.
- The shelf shows 5 to 10 sessions, with an overflow into "see all".
- Sessions hold editable notes, visible on the start screen; sessions can
  be pinned.
- The style sheet as drafted: type scale, components, spacing, radius,
  motion and Lucide icons.
- Account login is parked.

## 7. Context for project 3 (not designed here)

These came up and must survive into the session-experience design:

- The flow is add panels → group (optional) → (future: process panels) →
  plan → merge → export, but it is **not a wizard**. You can go back and
  add data after merging. The session knows what's stale and reprocesses
  automatically where appropriate.
- **Restore as much app state as possible** when a session is reopened,
  including the mosaic viewer's viewport.
- Issues show in lists that stay visible, never in pop-ups. Good data
  sails through; incomplete or poor data is flagged and can be inspected
  and fixed by hand.
- Exporting all groups and composites should be easy.

## 8. Calls made by Claude, for review

- Check severity *Note* shows in SII gold with a triangle icon (it was
  muted grey before). The severity's name doesn't change.
- Added ha-400 `#e05a76` for Hα text, because ha-500 fails AA as small
  text.
- The type scale numbers, the 30 px control height, the radii and the
  spacing scale.
- JetBrains Mono for numbers that update live.
- Lucide for icons.
- A static starfield without the warp, and no minimum display time.
- The hero can't be interacted with; cycling is about 8 s per view with
  pause and next controls.
- New mosaic uses one save-style dialog. The typed-path field moves to a
  fallback.
- The start screen reads a small summary file written by the app and
  never opens the session itself.
- No tags or folders, only pins.
- The recent-list limit goes up so "All sessions" has something to show.
- The SII *group* keeps mmm's existing orange (`#e0703a`, the "SII
  orange" the user remembered); SII gold is only for warnings. Group chips
  use the group palette colour, not an emission-line override.
- Angora ships only as pre-rendered lockup images, not as a font file.
- "Open another…" in a session window launches a new window on the start
  screen; returning a window to the start screen waits for project 3.
- Sessions open in another window are never the default hero and are
  never read live. The hero's reads must not block other processes or
  crash if one starts writing.

## 9. Left to the builder

- How tokens are expressed in Rust (constants, a theme struct) and how
  the theme is installed.
- Font loading: subsetting, fallbacks, how the icon glyphs are named.
- How the letter-spacing gets faked, if egui can't do it.
- The format and location of the session summary, and when it's written.
- Thumbnail generation and caching.
- Reading the OS reduced-motion setting, and whether to add an in-app
  toggle.
- Exact gradient, vignette and Ken Burns parameters (start from the web's
  `HomePage.tsx` and `DziHeroLayer.tsx`).
- How the gallery is structured and what the snapshot tests cover.
- Updating `docs/DESIGN.md` and the app spec where this design supersedes
  them.

## 10. Later

- **Account login:** look for a real benefit as project 3 goes on.
- **Pre-merge processing stage:** project 3 should leave a slot for it
  between grouping and planning.
- **Feed the new tokens back to the web:** sii-300/400/600 and ha-400
  would make the web's ad hoc amber warnings and its Hα text consistent
  with the apps. That's the user's call, in the Astrometrical repo.

## 11. References

- Brainstorm mockups (local, gitignored):
  `.superpowers/brainstorm/2566-1794055103/content/`:
  `start-directions.html` (option A is the chosen one),
  `warning-colour.html`, `style-sheet.html`.
- Astrometrical web: `apps/web/src/styles/app.css` (tokens),
  `pages/StyleGuidePage.tsx`, `pages/HomePage.tsx` (hero),
  `components/StarfieldLoader.tsx` and `lib/starfield-core.ts`,
  `components/DziHeroLayer.tsx` (Ken Burns over a region of interest),
  `docs/ui-reviews/` (the WCAG audit and the emission-line palette idea).
