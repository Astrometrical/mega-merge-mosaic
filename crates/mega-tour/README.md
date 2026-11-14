# mega-tour

Headless, narration-paced recordings of egui apps. A tour is a short Rust
script that drives a real app (clicks, waits, holds) while the recorder renders
it frame by frame with `egui_kittest` (wgpu), draws a caption card and a
cursor over it, and pipes the frames to ffmpeg. It never opens a window.
It pairs with `tools/narrate`: `narrate build` (or `narrate draft`) writes the
`narration.json` manifest that paces the tour, and `narrate mux` lays the
approved clips onto the silent video.

The crate is generic: it has no `mmm-*` dependencies, so any egui app can be
recorded. It uses `mega-ui` for the overlay's colours and tokens, but the app
needn't use mega-ui's theme: the theme is optional. With it installed, the
caption and badge use its type scale (the Astrometrical look); without it,
they fall back to egui's `Heading` and `Body` text styles. The NGC 7000 tour
of Mega Merge Mosaic lives in `crates/mmm-app/src/tours/ngc7000.rs` and runs
through `tools/tour.sh`.

## API in brief

- `Config::new(out_dir)`: stage size (1280 x 720 at 1.5 ppp, so 1920 x 1080
  output), `fps` (30), the ffmpeg path (`$FFMPEG`, else `ffmpeg`), optional
  `lead` and `breath` overrides, and `video` (false is a probe: no ffmpeg, no
  timeline). There is no `Default`: the output directory is always given.
- `narration::Narration::load(path)`: the typed reader for `narration.json`.
- `Recorder::new(config, narration, state, ui)`: `ui` draws the app into the
  root ui; the recorder owns the state `S` (`state()`, `state_mut()`, `ctx()`,
  `config()`). A failed ffmpeg start is an error naming the ffmpeg path.
- Steps: `say(id)`, `clear_caption()`, `hold(secs)`, `move_to(pos)`, `click()`,
  `click_on(label, Pick)` (finds a button, AccessKit `Role::Button`, by its
  label, glides the cursor there, clicks), `find(label, Pick)` (that lookup
  alone), `still(name)`.
- `wait(what, Wait, done, on_step)`: step the app until `done(&state)` holds for
  `Wait::stable(n)` consecutive steps (at least 1). `Wait::lapse(speed)`
  records only every `speed`-th frame and shows a "sped up" badge, so a long
  computation becomes a few seconds of video. `Wait::timeout(limit)` sets the
  real-time limit (30 minutes by default); a timeout panics. `on_step` runs
  after every step, for example to panic early on an error the app shows.
- `dump_labels()`: prints the AccessKit labels, for finding what to click.
- `finish()`: lets the last clip play out, flushes ffmpeg and writes the
  outputs.

## Timing model

The voice sets the pace. `say(id)` checks the id (an unknown one panics before
anything is recorded), keeps recording until the previous clip and its breath
have finished, shows the line's caption at the current frame, and schedules
the clip to start `lead` seconds later. Passages show one caption per
sentence, switching at frames derived from the clip start. Captions and cues
are scheduled in output frames, and a clip plus its breath is rounded up to
whole frames, so the next cue never starts before the clip ends. A draft
manifest (no audio, estimated durations) paces the tour the same way, which
is how the script is timed before any voice exists.

The app never stands still. During a `hold`, or while `say` waits for the
previous clip, the app keeps stepping (animations and background work go on);
the video is not a frozen last frame.

Waits are paced to real time: each step of a `wait` takes at least one real
frame time, because the app's jobs run on wall-clock threads. So the video
length depends on how long the app takes in each wait, divided by the wait's
lapse: a 40 s analysis at `lapse(8)` is about 5 s of video, and a slower
machine gives a longer video. Use `lapse` for anything slow.

## Outputs

In the out directory:

- `silent.mp4`: H.264, 1920 x 1080, 30 fps, no audio;
- `timeline.json`: `{version, fps, video, narration, cues: [{id, start}]}` (start in seconds),
  the clip start of each `say`, read by `narrate mux`;
- `captions.vtt`: one WebVTT cue per caption (blank lines in a caption's text
  become spaces, so a cue never splits);
- stills (`*.png`) from `still(name)`.

## Writing a tour

A tour is an ignored test (it needs a GPU and ffmpeg) so it can use the app's
own test-only constructors. Sketch, in the style of the NGC 7000 tour:

```rust
let mut rec = Recorder::new(Config::new(out), narration, shell, |ui, s| {
    // Optional: mega-ui's theme gives the Astrometrical look. Its fonts bind
    // a frame after it is applied, so skip drawing until then. A plain egui
    // app just draws.
    if mega_ui::theme::ensure_applied(ui.ctx()) {
        s.ui(ui);
    }
})?;
let none = |_: &mut Recorder<Shell>, _: u32| {};

rec.say("open");
rec.wait(
    "session open",
    Wait::new().timeout(Duration::from_secs(120)),
    |s| s.session().is_some(),
    |rec, _| {
        if let Some(e) = rec.state().start.error() {
            panic!("the session did not open: {e}");
        }
    },
);
rec.hold(2.5);

rec.say("analyse");
rec.wait("analysis", Wait::new().lapse(8).stable(30), |s| s.idle(), none);
rec.still("analysed.png");

rec.say("groups");
rec.click_on("H · 2", Pick::Left);
rec.hold(1.5);

rec.say("done");
rec.finish()?; // lets "done" play out
```

Write the narration script (`*.narration.toml`) first, one line per `say` id,
run `narrate draft` for a timing-only manifest, record, review the draft video,
then `narrate build` with the voice, approve in `narrate review`, and mux.
Keep waits predicate-based (`done`), not time-based, and use `lapse` for
anything slow. Give a wait that can fail (opening a file, say) a short
`timeout` and an `on_step` that reports the app's error.
