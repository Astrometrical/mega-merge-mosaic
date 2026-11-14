# Narration pipeline: scripted tours and narration in a cloned voice

Status: design, 2026-11-14. The user skipped the question rounds ("you have
good context, and I'm after general capability at this point; we will refine
and adapt over time"). Everything here not attributed to the user is a call
made in this design, listed under §11.

Background reading:
- `voice/README.md` (untracked, in the main checkout) has the voice-clone findings that shape this design.
- The spike is on branch `spike/tour-video`: `crates/mmm-app/src/tour_spike.rs`, uncommitted.

## 1. Intent

The user wants animated help, tutorials and showcases. These are videos of the real app doing real processing, narrated in the user's own voice. A spike proved both halves:

- **Headless recording:** a scripted walkthrough of the real `Shell` can be rendered headless (egui_kittest plus wgpu), with a drawn cursor and captions, and piped to ffmpeg as an MP4.
- **Cloned voice:** Qwen3-TTS can clone the user's Australian voice well enough that a blind listener rated it 4.5 out of 5, against 4.94 for the real recording.
- **Narration paces the video:** each caption's clip plays out before the next one starts.

Four listening rounds then settled how to make the voice reliable (§3).

This project turns the spike into general capability. In the user's words:

> Once we have this working well, we will likely pull it out into a generic repository so the whole egui headless rendering pipeline and narration can be used across multiple projects. I would also use the narration part to narrate other video content. So keep it decoupled as much as is practical.

So there are two reusable products and one first consumer:

1. **`narrate`:** turns a narration script into checked, approved voice clips and a timing manifest, and mixes clips onto any video. It knows nothing about egui or Mega Merge Mosaic.
2. **`mega-tour`:** records scripted, narration-paced walkthroughs of any egui app, headless. It knows nothing about TTS; it only reads the manifest.
3. **The NGC 7000 tour in `mmm-app`:** the spike's walkthrough rebuilt on the two products. It is the first real use, and the acceptance test.

## 2. Goals, non-goals, anti-goals

### Goals

1. **Script in, narration out.** Lines of text with ids go in. Each line comes out as a voice clip in the chosen voice, with the pronunciation list applied and every take checked automatically.
2. **A fast human loop for what machines can't hear.** Pronunciation and accent can only be judged by ear, so listening to new or flagged lines must be quick. So must approving, rejecting and fixing them.
3. **Stable once approved.** An approved line never changes on rebuild unless its text, voice or reference changes.
4. **Edit one line, regenerate one line.** Everything else comes from cache.
5. **Narration-first video.** A recorded tour waits for each clip, the way the spike does.
6. **Video-first narration.** Narration can be laid over existing video at fixed times, with overruns reported.
7. **Decoupled, and easy to lift out.** The parts meet only through documented files (§8):
   - `narrate` is a self-contained Python package.
   - `mega-tour` is a Rust crate that depends on nothing `mmm-*`.
8. **Tests run without a GPU, model weights or the user's voice** (CLAUDE.md: tests synthesize their inputs).

### Non-goals (for now)

- **Live in-app playback** (help that plays inside the running app). The user set this aside as a separate concern.
- **Mixing narration with a video's own audio**, such as music or ducking. Mux writes the narration as the only audio track; mixing can come later.
- **More TTS models.** The backend boundary (§5.3) makes a second model possible, but only Qwen3-TTS is built.
- **Shipping narration to end users,** for example bundled with the app or on the website. Output stays local until a later decision.
- **Extracting into a separate repository.** Design for it; don't do it yet.

### Anti-goals: failing even if it technically works

- **Narration that gives itself away:** wrong brand words ("maga mosaic"), American "day-ta", "zed" where the user says "zee", or garbled takes reaching a video unheard. Every new line is heard by the user before it is approved.
- **Narration faster than today's.** The user: "though it reads fast … it still sounds natural and not rushed, so I'm not concerned, so long as it doesn't get faster than it is." A pace gate enforces this (§5.4).
- **Voice data leaking.** The user's reference recordings and generated audio must never be committed or uploaded (§9).
- **Coupling that blocks the lift-out:** `narrate` importing Rust or egui knowledge, or `mega-tour` depending on `mmm-core`.

## 3. What the spike and listening rounds established

The full record is `voice/README.md`, with ratings in `voice/round*-ratings.json`. These facts are design inputs:

- **Model:** Qwen3-TTS 12Hz 1.7B Base (Apache 2.0), cloning from a reference clip and its exact transcript ("ICL").
  - **No style control:** tone comes only from the reference.
  - **Batching:** a batch broadcasts one prompt, and each row samples on its own.
- **Reference by job:**
  - **Short lines:** best from a ~10 s explainer reference (rated 4.08, against 3.08 for 28 s and 49 s).
  - **Long passages:** best from a long reference matched to the tone, generated in one go. Story ~53 s for showcases, explainer ~49 s for tutorials; 5/5, against 4/5 from 10 s references.
  - **Stitching sentence by sentence** rated worse than generating the passage in one go.
- **Pronunciation:** with the list (`voice/pronunciations.json`) and the 10 s reference, 30 of 30 phrases came out right.
  - **Fails as written:** some words do, for example "Paull" became "Pohl" and "XISF" became "exis".
  - **Respellings depend on context:** "Astrohmetrical" worked after "made by" but failed after "From". A respelling must be tried in several sentences.
  - **Long references bring errors back:** they also undid fixes that the 10 s reference didn't need ("Pohl", "maga").
- **Whisper large-v3-turbo** as a word check:
  - It catches dropped, wrong and garbled words.
  - It can't hear accent or vowel quality: "maga" passes as "Mega", and "zed" passes as "Z".
  - Its formatting needs normalising (number words and digits, "x" for "by", "α", "GB"/"gigabytes", British and American spelling).
- **Takes vary:** one take in three can be garbled (for example "S-H-O-O-H-O-O-H-R…"), run letters together ("RG&B"), or rush. So generate several takes and pick.
  - On a tie in word error, the take whose punctuation matches the intended text wins; it keeps "R, G and B" apart.
- **Pace:**
  - **The clone:** reads 4.6–5.3 syllables a second, against the user's 3.6–3.9, mostly through shorter gaps. Long references come out slower (4.6–4.9) than 10 s ones (5.0–5.3).
  - **Phrase takes:** reached up to 6.4.
- **Generation on the RTX 2080 Ti (11 GB, Turing: fp16 yes, bf16 no):**
  - **Precision:** fp16 is ~1.4× faster than fp32, with the same output.
  - **Batches pad to the longest row:** rows × the longest text must stay within ~900 characters with a 49 s reference, and ~2400 with a 10 s one. A 6 × 557-character batch on the 49 s reference filled VRAM, and WDDM then spilled to system RAM and crawled for 11 hours. Batch memory grows with reference length plus output length.
  - **A fixed seed** with a batch of one gives identical takes, so seed each batch differently.
  - **No per-take seed:** qwen-tts 0.1.1's `generate_voice_clone` takes no seed or generator. It samples from the global torch random state, once per batch. So a take depends on its batch's make-up and its row; only a batch of one reproduces from its seed, and that runs ~5× slower than batches of 6.
  - **Speed:** short lines ~1.3× real time (10 s reference, batches of 6); three whole passages in one batch ~0.55×.
- **Whisper and Qwen share one environment.** Whisper runs under qwen-tts's pinned transformers (4.57.3); checked 2026-11-14.
- **Narration-first timing** from the spike (`tour_spike.rs`):
  - **Lead:** a caption appears, and its clip starts 0.15 s later.
  - **Breath:** the next caption waits for the clip plus 0.4 s.
  - **Actions** continue during speech.
  - **Time-lapse:** while the app processes, the video captures 1 frame in N. Narration is timed in output frames, so it stays in sync.
  - **Mux:** each clip is delayed to its start (`adelay`), all clips mixed (`amix normalize=0`), then `loudnorm I=-16 TP=-1.5 LRA=11`, AAC 192k, video copied.

## 4. Architecture

```
 narration script (.toml) ──┐
 voice profile (library) ───┼─► narrate build ──► clips/*.wav + narration.json ──┐
 pronunciation lists ───────┘        ▲                                             │
                                     │ approvals (.lock.json)                      ▼
                              narrate review                    mega-tour recorder (Rust, any egui app)
                                                                       │
                                     ┌─────────────────────────────────┘
                                     ▼
                       silent.mp4 + timeline.json + captions.vtt
                                     │
                                     ▼
                     narrate mux ──► narrated.mp4
```

The contracts are files: the script, profile, lexicon, manifest and timeline (§8).
- `narrate draft` writes a manifest with estimated durations and no clips, so a tour can be built and timed before any voice exists, with no GPU.
- Video-first content skips the recorder: `narrate build` then `narrate mux --script`, using the `at` times.

## 5. `narrate`

A Python 3.11 package at `tools/narrate/` (src layout, `pyproject.toml`, uv), with a CLI named `narrate`.

**Dependencies:**
- **Core:** the stdlib plus numpy and soundfile, enough for every unit test.
- **`[qwen]` extra:** the GPU stack, which is qwen-tts and its torch/transformers pins (Whisper uses them too).
- Heavy imports happen only inside the backend and checker modules.

**External tool:** ffmpeg, found through `FFMPEG` or `PATH`.

### 5.1 Inputs

- **Narration script** (§8.1): lines with ids, text, and optional style, kind, fixed time and spoken override.
- **Voice profile** (§8.2): a directory in the voice library. It holds the backend, model id, language, precision, reference clips with exact transcripts for each style and length, the pace limits, and the speaker's own pronunciation list.
  - **Library location:** `NARRATE_VOICES`, default `~/.narrate/voices`. Scripts name the voice; they never hold its path.
- **Pronunciation lists** (§8.3), applied in order:
  1. The script's own lists: project vocabulary such as brand and product names.
  2. The profile's list: speaker and model quirks such as "Dahta" and "Paul".

### 5.2 What `build` does, per line

1. **Spoken form:** a line's `say` override wins. Otherwise apply the pronunciation lists to its `text`: regexes, case-sensitive, word-bounded, in order. Captions and checks use `text`; generation uses the spoken form.
2. **Reference:**
   - **Style:** the line's style, else the script's, else the profile's default.
   - **Kind:** the line's `kind`, else inferred. Three or more sentences, or about 250+ characters, means `passage`; anything shorter is a `line`.
   - **Clip:** a line uses the style's short reference, a passage the long one.
3. **Cache key:** a hash of the backend, model, precision, reference audio bytes, reference transcript and spoken text. The key names a cache entry holding numbered takes.
   - **Seeds:** each batch is seeded from its first take's key and take number, so batches differ. A take is not reproducible on its own (§3), which is why approved audio is stored durably (§5.8).
4. **Takes:** make up to `takes.count` (default 3). Batch by reference, sorted by length, within a padded-size budget scaled to the reference length (§3: ~900 characters at 49 s, ~2400 at 10 s). Gaps across a whole build are filled in as few model loads as possible.
5. **Check each take:**
   - **Transcribe:** Whisper, cached per take.
   - **Word error:** normalise both sides (§3), then compare the transcript with every mix of as-written and respelled terms, keeping the lowest error. The mixes run from `text` itself to the full spoken form.
     - **Why mixes:** Whisper writes a respelling either way. It hears "Dahta" as "Data", as written, but writes "Zabara" for the respelled "Sbarra", as spoken. One line can hold both: "Data courtesy of Adam Zabara."
     - A take fails at or above `checks.max_cer` (default 0.04).
   - **Pace,** measured as in §5.4:
     - **Passages:** a take fails above `pace.max` (default 5.3).
     - **Lines:** never fail on pace. A take over `pace.max` is chosen only when no take at or under it passes the word check. Short lines measure noisily, and the user passed all 30 round-4 phrases, some of which run over 5.3.
6. **Pick:**
   - **Order:** lowest word error (rounded), then punctuation distance to `text`, then pace closest to `pace.target` (default 4.8), then take number.
   - **When nothing passes:** generate more takes up to `takes.max` (default 6). If none pass, keep the best and flag the line.
   - **Rejected takes** (from the lock) never count.
7. **Clip:** trim silence, leaving ~50 ms of padding. Normalise loudness to a common integrated level (default −20 LUFS) and write `<out>/clips/<id>.wav`.
   - **Sentence captions:** a passage also gets one caption per sentence, timed against its clip.
     - Split `text` into sentences.
     - Place each sentence boundary by syllable share.
     - Find the clip's pauses: silences of 0.15 s or more, 30 dB below the peak. Clone audio has short gaps, and real recordings have room noise.
     - Snap each boundary to a pause within 1.5 s of its share position. A small monotonic dynamic programme makes the choice; a boundary with no pause in reach keeps its share position.
     - **Longer pauses are preferred:** a pause's cost is its distance minus its length. A paragraph break's 0.6 s pause beats a comma's 0.2 s one nearby.
     - On round-4 clips the share positions sat within about 0.4 s of real pauses, so most boundaries snap.
     - A line gets a single caption covering the whole clip.
8. **Approval:**
   - **Status:**
     - **`approved`:** the lock pins this key, so use the pinned take.
     - **`flagged`:** no take passed.
     - **`new`:** otherwise.
   - **A pin is tied to its key:** editing a line's text, its pronunciations, the profile's reference or the model changes the key, so the line comes back as `new`.

Then write `narration.json` (§8.4). `build` exits non-zero when a line is flagged, unless `--allow-flagged` is passed.

### 5.3 Backend and checker boundaries

- **Backend:** `synthesize(texts, reference, seeds) -> list[audio]`, plus the sample rate.
  - **`qwen3-tts`:** wraps `Qwen3TTSModel.create_voice_clone_prompt` and `generate_voice_clone`, with one prompt per reference.
  - **`fake`:** for tests; it makes deterministic synthetic audio whose length follows the text.
- **Checker:** `transcribe(audio) -> str`.
  - **`whisper`:** large-v3-turbo through `WhisperForConditionalGeneration` directly. The transformers pipeline fails on Windows when importing torchcodec. Audio over ~28 s is split at its quietest point between 15 s and 28 s.
  - **`fake`:** returns scripted transcripts.
- **Selection:** a profile names its backend; checker and backend are chosen by name, with no hard-wired model in the build logic.

### 5.4 Pace gate

`pace.max` encodes the user's "not faster than it is". Defaults are 5.3 max and 4.8 target, from round 4: the takes the user rated 5 ran at 4.6–4.9.

**Measure,** fixed so the numbers stay on the scale of the rounds:
- **Syllables:** count matches of `[aeiouy]+` in each word of `[a-z]+` in the lowercased spoken form, at least 1 per word, and sum. Use the spoken form because it's what is said: "X I S F" counts 4, where "XISF" would count 1. Passages in the rounds had no respellings, so their figures carry over.
- **Speech span:** from the start of the first non-silent interval to the end of the last, with non-silent meaning within 35 dB of the clip's peak (as `librosa.effects.split(top_db=35)` defines it; reimplement with numpy, since librosa is not a core dependency).
- **Pace:** syllables divided by the speech span.

The round-4 figures in §3 were measured this way:
- **Your readings:** 3.6–3.9.
- **Clone passages:** 4.6–5.3.
- **Phrase takes:** median 4.2, p90 5.6.

**Tests:** pin the measure with synthetic audio.

**Smoke check (manual, plan 1):** the measure must reproduce those figures on the round-4 audio in `voice/pages/round4/audio`.

### 5.5 `review`

`narrate review SCRIPT` builds if needed, then serves a local page (stdlib `http.server`, on 127.0.0.1 only) and opens the browser.

**Contents:**
- **Order:** the script's lines, with `new` and `flagged` first.
- **Each line shows:**
  - the clip;
  - `text`;
  - the spoken form, when it differs;
  - what Whisper heard;
  - pace and the flags;
  - for passages, the sentence captions highlighted in turn as the clip plays.

**Controls per line:**
- **Approve:** pins this take.
- **Reject:** the next build uses the next best take, or makes more.
- **Note:** free text.
- **Approve all unflagged:** one button for every line that isn't flagged.

**Writes and rebuilds:**
- Writes go straight to the lock file.
- After rejections, a "Rebuild" button runs a build on the server and reloads.

**Look:** follows the listening pages from the rounds: loudness-matched clips, and light and dark themes.

### 5.6 `try`: the pronunciation lab

`narrate try WORD --spell A --spell B … --in "From {}." --in "Made by {}." --voice V`

This is the round-3 workflow made into a command:
1. Generate each spelling in each carrier sentence, with two takes each.
2. Serve a page where the user plays the carriers for each spelling (blind) and picks one, or none.
3. Append the pick to a chosen list, the project's or the profile's, with a "why" note.

**Defaults:** with no `--in`, use at least two built-in carriers with different words before the term. Round 4 showed a single carrier misleads.

### 5.7 `draft` and `mux`

- **`narrate draft SCRIPT`:** writes `narration.json` with `clip: null` and estimated durations, using `text` at `pace.target`. It needs no GPU and no voice profile.
- **`narrate mux --timeline T -o OUT`:** reads the timeline (§8.5) and the manifest, then mixes each clip at its start onto the silent video, using the spike's filter graph (§3).
- **`narrate mux --video V --script S -o OUT`:** video-first. It places each line at its `at` time.
- **Overruns:** both forms report any clip that runs past the next cue's start, and fail unless `--allow-overlap`.

### 5.8 Files `narrate` writes

| What | Where | In git? |
|---|---|---|
| Takes, transcripts, prompt cache | `NARRATE_CACHE`, default `~/.narrate/cache`; content-addressed and shared across projects. Safe to delete | no |
| Approved takes | `NARRATE_APPROVED`, default `~/.narrate/approved/<key>/`. A copy of each pinned take and its check results; durable, and never pruned by `narrate` | no (private audio) |
| Clips and `narration.json` | `--out`, default `<script dir>/out/<script stem>/` | no (gitignore it) |
| Approvals | `<script>.lock.json` next to the script | yes: it holds keys, take numbers, durations, transcripts and notes, no audio |

**Pinned takes come from the approved store,** so clearing the cache never changes approved narration.
- **Approved store also missing,** on a new machine for example: the line returns to `new`, with a "pinned audio missing" flag. A take can't be regenerated exactly (§3).
- **Projects that need narration on several machines** copy the approved store. Syncing it is out of scope.

## 6. `mega-tour`

A library crate at `crates/mega-tour`, generic over the app's state.

**Dependencies:** egui, egui_kittest (wgpu), image, serde and serde_json, and `mega-ui` (overlay styling comes from `mega_ui::theme` tokens, per the UI standard). Both `mega-ui` and `mega-tour` lift out together. It never depends on `mmm-*`.

It is the spike's `Rec`, `Overlay`, `NoTestCursor` and ffmpeg pipe, made generic.

**API shape** (the builder may refine names):

```rust
let narration = Narration::load("…/narration.json")?;    // a draft manifest works too
let mut rec = Recorder::new(config, app_state, |ui, st| st.ui(ui), narration)?;
rec.say("open");                            // caption + clip; the next say waits for this one
rec.wait("analysis", Wait::lapse(8).stable(30), |st| st.idle());
rec.click_on("Merge", Pick::Right);         // by AccessKit role + label
rec.hold(1.5);
rec.still("merged.png");
rec.clear_caption();
rec.finish()?;                              // silent.mp4, timeline.json, captions.vtt
```

**Config:**
- logical size, pixels per point and FPS (spike: 1280 × 720 at 1.5 ppp, 30 fps);
- output directory;
- ffmpeg path;
- the lead and breath times (0.15 s and 0.4 s).

**Captions and clips:**
- **Captions show the manifest's `text`,** never the spoken form.
  - **Captions follow the clip:** each line's `captions` entries (§8.4) are shown in turn as its clip plays, so a passage shows one sentence at a time. Captions are also written to the VTT one per entry.
- **Unknown line id:** `say` panics with the id.
- **Draft manifests:** a line without a clip paces by its estimated duration.

**Timing:** in output frames, as in the spike, so time-lapse waits keep the audio in sync.
- **Real-time steps:** each `wait` step is paced to a real frame time, because the app's jobs run on wall-clock threads.

**Outputs:**
- the silent H.264 video (`yuv420p`, `+faststart`);
- `timeline.json`, with each spoken line's clip start in seconds (§8.5);
- `captions.vtt`, with the caption spans.

**Probe mode** (the spike's `TOUR_PROBE`): waits until the app is idle, dumps AccessKit labels and rects, and writes stills, for authoring a tour.

**Tests:**
- **Unit, no GPU:** timing and scheduling (lead, breath, `say` waits, time-lapse frame counts) and timeline and VTT writing.
- **Integration (ignored, GPU):** records a few seconds of a tiny egui app with a draft manifest and checks the outputs.

## 7. The NGC 7000 tour (first consumer)

- **Narration script:** `crates/mmm-app/tours/ngc7000.narration.toml`, holding the spike's eight lines. It is committed with its lock file and the project pronunciation list (`crates/mmm-app/tours/terms.json`).
- **Walkthrough:** the spike's `tour()` ported to `mega-tour`, as an `#[ignore]`d test in `mmm-app`. It needs a GPU, `TOUR_SESSION` (the NGC 7000 session; its data lives outside the repo), the manifest path and ffmpeg.
- **Test-only hooks** (`Shell::jobs()` under `cfg(test)`) are kept as small as the spike's.
- **Runner:** a small script, `tools/tour.sh`, runs the chain: `narrate build`, the recorder test, then `narrate mux`. It falls back to `narrate draft` when no voice is available.
- **Retire the spike:** once the port records the same tour, `tour_spike.rs` and its diffs on `spike/tour-video` are retired.

**Acceptance:**
- **Narrated tour:** the tour renders end to end in the user's voice, with every line approved through `narrate review`.
- **Draft tour:** it also renders with a draft manifest, needing no voice and no narration GPU work.

## 8. File contracts

All JSON outputs carry `"version": 1`. Readers reject a newer major version.

### 8.1 Narration script (TOML, written by people)

```toml
voice = "dan"                       # profile name in the voice library
style = "explainer"                 # default tone (picks the reference)
pronunciations = ["terms.json"]     # project lists, relative to this file

[[line]]
id = "open"
text = "A real mosaic of NGC 7000: two panels, shot through six filters: S, H, O, R, G and B."

[[line]]
id = "showcase"
style = "story"
text = """The North America Nebula is enormous. …"""
# kind = "passage"   # optional; otherwise inferred (§5.2)
# at = 12.5          # optional: fixed start in seconds (video-first)
# say = "…"          # optional: exact spoken text, bypassing the lists
```

Ids are unique, matching `[a-z0-9-]+`.

### 8.2 Voice profile (`<library>/<name>/voice.toml`)

```toml
backend = "qwen3-tts"
model = "Qwen/Qwen3-TTS-12Hz-1.7B-Base"
language = "English"
precision = "fp16"
default_style = "explainer"
pronunciations = ["pronunciations.json"]

[style.explainer]
short = { audio = "refs/explainer-10.wav", text = "…exact transcript…" }
long  = { audio = "refs/explainer-full.wav", text = "…" }

[style.story]
short = { audio = "refs/story-10.wav", text = "…" }
long  = { audio = "refs/story-full.wav", text = "…" }

[pace]
max = 5.3
target = 4.8

[takes]
count = 3
max = 6
```

**Dan's profile** is built from `voice/` in plan 1:
- the reference clips in `voice/refs/`;
- the transcripts in `voice/refs/ref_text.json`;
- the speaker's half of `voice/pronunciations.json`.

### 8.3 Pronunciation list (JSON)

The format of `voice/pronunciations.json`: `{"about", "entries": [{"match", "say", "why"}], "fine_as_written": [...]}`.
- `match` is a Python regex, applied in order.
- `say` may use group references.

### 8.4 Manifest: `narration.json` (written by `narrate`)

```json
{"version": 1, "script": "ngc7000.narration.toml", "voice": "dan", "draft": false,
 "sample_rate": 24000, "lead": 0.15, "breath": 0.4,
 "lines": [{"id": "open", "text": "…", "spoken": "…", "kind": "line", "clip": "clips/open.wav",
            "duration": 5.42, "at": null, "status": "approved", "flags": [],
            "captions": [{"text": "…", "start": 0.0, "end": 5.42}],
            "check": {"heard": "…", "cer": 0.0, "pace": 4.7, "take": 2, "key": "…"}}]}
```

- **Paths** are relative to the manifest.
- **Caption times** are seconds from the clip's start; together the entries cover the clip.
- **Draft manifests** have `"draft": true`, `clip: null` and no `check`. Their captions are split by syllable share over the estimated duration.
- **Pacing defaults:** `lead` and `breath` are the recorder's; a tour config may override them.

### 8.5 Timeline: `timeline.json` (written by the recorder, or derived from `at`)

```json
{"version": 1, "video": "silent.mp4", "narration": "narration.json", "fps": 30,
 "cues": [{"id": "open", "start": 0.15}, {"id": "groups", "start": 9.4}]}
```

## 9. Constraints

- **Privacy:** the user's recordings, reference clips, voice profile and every generated clip stay off git and off any external service.
  - Model weights download from Hugging Face; nothing is uploaded.
  - The voice library lives outside the repo.
  - `/voice/` goes in `.gitignore`.
  - Output dirs are gitignored.
  - Lock files and project pronunciation lists, which hold text only, are committed.
- **Licences:** Qwen3-TTS weights and code are Apache 2.0, and Whisper is MIT. No CC-BY-NC weights (F5-TTS is out).
- **Hardware:** one 11 GB Turing GPU on Windows is the reference machine.
  - **Batching:** the batch budget must keep generation from oversubscribing VRAM (§3).
  - **Model loads:** one per generation round, so at most two per build. The backend is closed before takes are checked, so Qwen and Whisper are never in VRAM together.
  - **Pinning:** the CUDA build (cu126) is pinned through uv.
- **Tests and CI:**
  - **Python job:** a new CI job runs `narrate`'s unit tests on ubuntu: uv, Python 3.11, core dependencies only, with the fake backend and checker. Clean is `ruff` plus `pytest`.
  - **Rust:** the existing CI runs `mega-tour`'s unit tests. GPU tests are `#[ignore]`d, like the UI snapshots.
- **Repo conventions:**
  - **Rust:** the CLAUDE.md conventions apply to `mega-tour`: theme tokens rather than literals, doc comments, and fmt, clippy and doc kept clean.
  - **Docs:** DESIGN.md gains a "Narration and tours" section as built.
- **GUI runs:** the recorder is headless (kittest) and never opens a window. CLAUDE.md's "launch the app only via `--selftest`" rule is untouched.

## 10. Plans (build order)

1. **`narrate` core:**
   - package, script, profile and list parsing; spoken forms; reference choice;
   - backend and checker boundaries, with Qwen and Whisper and the fakes;
   - batching, takes, checks, pace gate and pick; clips; cache and manifest;
   - `build` and `draft`; the CI job;
   - Dan's profile, assembled from `voice/`;
   - the `/voice/` gitignore;
   - the tour's narration script and terms list (§7). They are text only, and the smoke needs them.
   - **Manual smoke:** build the eight tour lines in Dan's voice.
2. **Human loop and video:**
   - the lock file;
   - `review`, with its approve, reject and rebuild loop;
   - `try`;
   - `mux`, in both forms, with the overrun check.
   - **Manual smoke:** approve the eight lines, then mux them onto the spike's silent tour video.
3. **`mega-tour` and the NGC 7000 tour:**
   - the crate, its tests and probe mode;
   - the `mmm-app` tour, terms list and runner;
   - DESIGN.md;
   - retiring the spike.
   - **Acceptance:** §7.

## 11. Calls made in this design (not the user's)

| Call | Why |
|---|---|
| Python for `narrate`, Rust for `mega-tour`, joined only by files | The model stack is PyTorch; the recorder must sit inside egui. Files are the loosest workable coupling, and they suit "narrate other video content" |
| TOML scripts and profiles; JSON manifests, timelines, locks and lists | TOML is pleasant for writing multi-line text, and `tomllib` is in the stdlib. JSON suits machine-written files |
| Voice library outside the repo (`NARRATE_VOICES`); scripts name a voice, not a path | Privacy, and sharing across projects |
| Two pronunciation layers (project and profile) | Brand words belong to a project; "Dahta" and "Paul" belong to the speaker and model |
| Short or long reference chosen per line, inferred from length | The rounds showed opposite winners for lines and passages |
| Take checks compare against both `text` and the spoken form, keeping the lower error | Whisper writes some respellings as the intended word ("Dahta" as "Data") and others as spoken ("Zabara" for "Sbarra") |
| Pace counts syllables in the spoken form | It's what is actually said ("X I S F" is 4 syllables) |
| The backend closes before checking (at most two model loads per build) | Keeps Qwen and Whisper out of VRAM together on an 11 GB card |
| Review through a local stdlib HTTP server writing the lock directly | Faster than the export and import round-trip of the listening pages, with no new dependencies |
| Lock pins by cache key; approved takes copied to a durable private store | Keeps approvals stable without committing audio. Takes can't be regenerated exactly, because qwen-tts samples once per batch with no per-take seed, so the store holds them instead of slowing generation to batches of one |
| Pace is a hard gate for passages only; for lines it only decides the pick | The user's pace remark was about passages, and they passed all 30 phrases, some over the cap |
| Pace measured exactly as the rounds did (§5.4) | Keeps 5.3 and 4.8 on the scale they came from; a different estimator read about 10% lower |
| Sentence captions for passages, timed by matching sentence boundaries to pauses | A 45 s passage as one caption block is unusable on screen. Pause matching needs no word timings from the checker |
| `draft` mode | Lets tours be written and timed without a GPU or a voice; also how `mega-tour` is tested |
| `mega-tour` depends on `mega-ui` | Overlay styling follows the UI standard; both crates lift out together |
| One Python environment | Verified: Whisper runs under qwen-tts's transformers pin |
| `/voice/` and `.claude/worktrees/` added to `.gitignore` | Voice data must not be committed; worktrees must not be swept into commits |

## 12. Left to the builder

- **Internal module layout** of `narrate` and the exact CLI flag names, as long as the commands in §5 exist.
- **The padded-size budget formula,** within the §3 data points.
- **Normalisation rules,** starting from the §3 list, with tests built from the rounds' real failure strings.
- **How pauses are scored in the sentence-caption match.**
- **Review page layout,** within the listening pages' look.
- **`mega-tour` API names, and how `wait` and its predicates are expressed.**
- **The runner script's shell,** which must work from Git Bash on Windows.

## 13. Risks and open questions

- **Long references with the pronunciation list are untested on brand-heavy text.** The review loop is the safeguard; flagged or rejected lines show up quickly.
- **Approved narration lives in one local store** (§5.8). Losing it means re-listening, not re-recording; a backup is up to the user.
- **Sentence captions from pause matching** can be off by a sentence when a passage has pauses inside sentences. Review shows the caption timing, so a bad split is visible.
- **Whisper misses pronunciation errors by design.** The human loop is not optional.
- **qwen-tts 0.1.1 is young,** so pin it exactly and keep the backend boundary thin.
