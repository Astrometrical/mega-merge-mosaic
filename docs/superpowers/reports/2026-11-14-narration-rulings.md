# Narration project: rulings and deferred items

The narration project (spec `docs/superpowers/specs/2026-11-14-narration-pipeline-design.md`) ran as three plans, executed subagent by subagent with a review after every task and a whole-branch review per plan.

**Plans:**
1. **`narrate` core:** script in, checked clips and a manifest out. Plan `docs/superpowers/plans/2026-11-14-narrate-core.md`, merged at 5a72b3b.
2. **The human loop and video:** a lock and approved store, plus `review`, `try` and `mux`. Plan `docs/superpowers/plans/2026-11-14-narrate-human-loop.md`, merged at b33c39a.
3. **`mega-tour` and the NGC 7000 tour.** Plan `docs/superpowers/plans/2026-11-14-mega-tour.md`.

The user skipped the design questions and asked for the judgment calls to be made and recorded. This file records every call made during execution: what was decided, why, and what it costs if it turns out wrong. Design-time calls are in the spec's §11.

## Still to do (the user)

- **Approve the eight tour lines by ear:** `cd tools/narrate && uv sync --extra qwen && uv run narrate review ../../crates/mmm-app/tours/ngc7000.narration.toml`. Then commit `crates/mmm-app/tours/ngc7000.narration.lock.json`. Until then, `narrate mux` refuses the lines, and `tools/tour.sh` muxes only with `--allow-unapproved`. Acceptance so far:
  - **Smoke tests:** these approved lines only in throwaway copies.
  - **Plan 3:** rendered a narrated preview with `--allow-unapproved`.
- **The spike on `spike/tour-video`:** it is uncommitted in the main checkout, and `mega-tour` supersedes it. Discard it when convenient (app.rs, main.rs, Cargo.toml, Cargo.lock, tour_spike.rs).
- **The voice data in `voice/`:** gitignored and untouched. It holds the recordings, references, pronunciation list, listening pages and the round results.

## Rulings

### Plan 1: `narrate` core

| Ruling | Why | Cost if wrong |
|---|---|---|
| The word check's answer key is every mix of as-written and respelled terms (`Lexicon.variants`), not just the text and the spoken form | The real line "Data courtesy of Adam Sbarra." is heard as "Data … Zabara". That scored exactly 0.04 against both forms, so it would always flag | A little complexity (2^k variants, capped at 64) |
| "and" joins numbers only after hundred or thousand | Keeps "six and seven" from becoming 13 | A one-character mismatch on unusual number phrasing |
| All-zero audio has no speech intervals | Silence must give a span of (0, 0) and an infinite pace; librosa would call it all speech | None |
| The pace measure mirrors `librosa.effects.split` exactly. Its end tolerance in tests is 0.1 s | Keeps 5.3 and 4.8 syllables/s on the listening rounds' scale. Verified to the sample on real clips; calibration reproduced 3.85 and 3.64 | A test tolerance |
| Commit trailers name the model that made the commit | Honest attribution | A cosmetic inconsistency in early commits |
| Sentence captions find pauses at 30 dB / 0.15 s and snap each sentence break to the nearest pause within 1.5 s, preferring longer pauses (cost = distance − pause length) | On real clips the original all-or-nothing rule never fired. Hand-checked on three clone clips: every paragraph break lands on its long pause | A caption switching about 1 s off at a paragraph break |
| Take keys include the profile language | Qwen generates with it, so a language change must regenerate | A language change regenerates takes |
| Whisper is closed before round-2 generation | Spec §9: Qwen and Whisper are never in VRAM together (11 GB card) | One extra Whisper load (~5 s) when round 2 runs |
| Exit codes: 0 ok, 1 flagged, 2 user error, 3 internal | `tools/tour.sh` must tell set-up errors from flagged lines | A fourth exit code |
| Output folders get their own `.gitignore` (`*`) | Cloned-voice audio must never reach another project's git | One small file per output folder |
| Cached transcripts are bound to their take's audio (sha256) | A regenerated take must never inherit another take's transcript | A hash per lookup |

### Plan 2: the human loop and video

| Ruling | Why | Cost if wrong |
|---|---|---|
| Approved store: pins carry the sha256 of the approved audio. The store is content-addressed (`<sha[:2]>/<sha>.wav`), verifies on write and never overwrites | Spec §5.8: unheard audio is never presented as approved. A per-(key, take) store let identical lines overwrite each other's approved audio | A store layout change (nothing real had been approved yet) |
| Lock content: a pin on a rejected take is ignored | Rejected takes never count; a merged lock can hold both | None |
| Rejection numbering: after a cache clear, take numbers may repeat within a key, so a rejection can skip a new take with the same number | Approvals are hash-bound, so no audio is lost; parked | An occasional extra take |
| Atomic writes retry `os.replace` (20 × 25 ms), and racing reads retry briefly | Windows: a rebuild writes `narration.json` while the page polls it. 2,198 of 2,996 saves failed in a measured race before the fix | Up to 0.5 s of latency under contention |
| Review server security: only `Host: 127.0.0.1:<port>` or `localhost:<port>` (else 421), and POSTs only as JSON (else 415) | Voice clips are private. DNS rebinding could read them, and text/plain POSTs need no preflight | Non-browser clients with odd headers break |
| "Approve all unflagged" skips rejected takes and any line carrying a flag | A rejection isn't visible until a rebuild, and `pinned-audio-missing` means "listen again" | A click |
| Approvals are bound to the heard take: the page sends the sha it played, the server re-reads the manifest from disk (409 on mismatch), and all edits get 409 while a build runs | During a rebuild, approve could pin a take other than the one the reviewer heard | A reload click |
| The review page during a rebuild: header-only updates, with clips served with HTTP Range | Real rebuilds take a minute or more; clips weren't seekable in Edge or Chrome | None |
| `narrate try` uses 2 takes and keeps its own carriers ("Next up is…", "…heard of…", "…back to…") | Spec §5.6 pins two takes. The words before the term differ, which suits the round-4 lesson better than the plan's set | Re-picking carriers |
| Blind labels in `try` are blind in the page only | They guard against listening bias, not secrecy | None |
| Duplicate cue ids are allowed in timelines | A tour may say a line twice | None |
| `ApprovedStore.put` replaces a corrupt copy (bytes not matching their name) | That's a repair, not an overwrite of good audio | None |

### Plan 3: `mega-tour` and the NGC 7000 tour

| Ruling | Why | Cost if wrong |
|---|---|---|
| `tools/tour.sh` falls back to a draft only on narrate's "needs the qwen extra" or "voice profile … not found" errors | Any other exit 2 is a real user error that a silent draft would hide | A string match on narrate's messages |
| A stray 786 KB draft video committed by an implementer was removed by rewriting the unpushed commit. Default output is anchored at the repo root, and `**/tours/out/` is ignored | A binary in history would ride into main forever | None (reflog) |
| `tour.sh` makes paths absolute before changing directory | The same bug class as the stray video | None |
| The overlay falls back to egui's own text styles when mega-ui's theme isn't installed | Keeps `mega-tour` usable in any egui app, so it can lift out | None |
| New mega-ui tokens: `CAPTION_FADE_S` 0.3, `RIPPLE_S` 0.45, `CAPTION_WRAP_FRACTION` 0.7, `CAPTION_CARD_ALPHA` 0.94 (today's look rather than `scrim()`'s 0.65). The ripple and press geometry stay literal, alongside the cursor | CLAUDE.md tokens rule | A lighter card, if `scrim()` was wanted |
| The session-open wait times out after 120 s and reports the start screen's error | No silent 30-minute hangs | None |

### Outside the plans

- **CI on main broke when stable Rust 1.100 added clippy's `needless_range_loop`.** Four index loops in mmm-core tests were rewritten (5056e50). Rust 1.100.0 is installed alongside the default toolchain, so `cargo +1.100.0 clippy` reproduces CI.

## Deferred minor items

Each plan's final review triaged its deferred minors as "can wait". The notable ones:

- **`narrate` text:** number-word runs are summed ("one two three" becomes 6, against Whisper's "1, 2, 3"). This fails safe (it flags), but should be fixed before a script counts aloud. The sentence splitter breaks on abbreviations ("Dr.").
- **Script and lock reading:** `at = nan` is accepted by the script reader (mux rejects it), and lock `pin()`/`note()` don't validate ids.
- **Review page:** a failed first state load can freeze note saving. On Windows, a second narrate server can bind a port already held (`SO_REUSEADDR`).
- **`try`:** a pick reformats the pronunciation list it writes.
- **`mega-tour`:** a shared golden manifest fixture for both sides is not built yet. Stills taken right after a time-lapse show the stale "sped up" badge. In the first still, the "Analyse finished" toast sits behind the caption card.

The full lists are in the plan ledgers. Those are gitignored session scratch, so the items above are the ones worth tracking.
