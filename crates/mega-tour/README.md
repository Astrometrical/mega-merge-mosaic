# mega-tour

Headless, narration-paced recordings of egui apps. A tour is a short Rust
script that drives a real app (clicks, waits, holds) while the recorder renders
it frame by frame with `egui_kittest` (wgpu), draws a caption card and a
cursor over it, and pipes the frames to ffmpeg. It never opens a window.
It pairs with `tools/narrate`: `narrate build` (or `narrate draft`) writes the
`narration.json` manifest that paces the tour, and `narrate mux` lays the
approved clips onto the silent video.

The crate is generic: it has no `mmm-*` dependencies (only `mega-ui`, for the
theme), so any egui app can be recorded. The NGC 7000 tour of Mega Merge
Mosaic lives in `crates/mmm-app/src/tours/ngc7000.rs` and runs through
`tools/tour.sh`.

## API in brief

- `Config::new(out_dir)`: stage size (1280 x 720 at 1.5 ppp, so 1920 x 1080
  output), `fps` (30), the ffmpeg path (`$FFMPEG`, else `ffmpeg`), optional
  `lead` and `breath` overrides, and `video` (false is a probe: no ffmpeg, no
  timeline).
- `narration::Narration::load(path)`: the typed reader for `narration.json`.
- `Recorder::new(config, narration, state, ui)`: `ui` draws the app into the
  root ui; the recorder owns the state `S` (`state()`, `state_mut()`, `ctx()`).
- Steps: `say(id)`, `clear_caption()`, `hold(secs)`, `move_to(pos)`, `click()`,
  `click_on(label, Pick)` (finds a widget by its AccessKit label, glides the
  cursor there, clicks), `find(label, Pick)`, `still(name)`.
- `wait(what, Wait, done, on_step)`: step the app until `done(&state)` holds for
  `Wait::stable(n)` consecutive steps. `Wait::lapse(speed)` records only every
  `speed`-th frame and shows a "sped up" badge, so a long computation becomes a
  few seconds of video. Timeouts panic.
- `dump_labels()`: prints the AccessKit labels, for finding what to click.
- `finish()`: flushes ffmpeg and writes the outputs.

## Timing model

The voice sets the pace. `say(id)` waits for the previous clip and its breath to
finish (holding the last frame), shows the line's caption at the current frame,
and schedules the clip to start `lead` seconds later. Passages show one caption
per sentence, switching at frames derived from the clip start. Everything is in
output frames; nothing depends on wall-clock time, so a slow machine produces
the same video. Time-lapsed waits advance video time by one frame per
`speed` steps. A draft manifest (no audio, estimated durations) paces the tour
the same way, which is how the script is timed before any voice exists.

## Outputs

In the out directory:

- `silent.mp4`: H.264, 1920 x 1080, 30 fps, no audio;
- `timeline.json`: `{version, fps, video, narration, cues: [{id, start}]}` (start in seconds),
  the clip start of each `say`, read by `narrate mux`;
- `captions.vtt`: one WebVTT cue per caption;
- stills (`*.png`) from `still(name)`.

## Writing a tour

A tour is an ignored test (it needs a GPU and ffmpeg) so it can use the app's
own test-only constructors. Sketch, in the style of the NGC 7000 tour:

```rust
let mut rec = Recorder::new(Config::new(out), narration, shell, |ui, s| s.ui(ui))?;
let none = |_: &mut Recorder<Shell>, _: u32| {};

rec.say("open");
rec.wait("session open", Wait::new().stable(1), |s| s.session().is_some(), none);
rec.hold(2.5);

rec.say("analyse");
rec.wait("analysis", Wait::new().lapse(8).stable(30), |s| s.idle(), none);
rec.still("analysed.png");

rec.say("groups");
rec.click_on("H · 2", Pick::Left);
rec.hold(1.5);

rec.clear_caption();
rec.finish()?;
```

Write the narration script (`*.narration.toml`) first, one line per `say` id,
run `narrate draft` for a timing-only manifest, record, review the draft video,
then `narrate build` with the voice, approve in `narrate review`, and mux.
Keep waits predicate-based (`done`), not time-based, and use `lapse` for
anything slow.
