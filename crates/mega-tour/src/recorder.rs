//! The headless recorder: drives an egui app frame by frame through
//! egui_kittest, pipes frames to ffmpeg, and paces `say` calls to the
//! narration schedule.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Id, Modifiers, PointerButton, Pos2, Rect, Vec2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::wgpu::WgpuTestRenderer;

use crate::narration::Narration;
use crate::outputs::{write_timeline, write_vtt};
use crate::overlay::{NoTestCursor, Overlay, ShownCaption, paint_overlay};
use crate::schedule::{Schedule, frames};

/// Recorder settings.
#[derive(Clone, Debug)]
pub struct Config {
    /// Logical stage size (default 1280 × 720).
    pub size: Vec2,
    /// Pixels per point (default 1.5, so 1920 × 1080 output).
    pub pixels_per_point: f32,
    /// Output frames per second (30).
    pub fps: u32,
    /// Directory for `silent.mp4`, `timeline.json`, `captions.vtt` and stills.
    pub out_dir: PathBuf,
    /// The ffmpeg executable: `$FFMPEG`, else `ffmpeg`.
    pub ffmpeg: PathBuf,
    /// Seconds from a caption appearing to its clip starting; `None` uses the
    /// manifest's value.
    pub lead: Option<f64>,
    /// Seconds after a clip before the next caption; `None` uses the
    /// manifest's value.
    pub breath: Option<f64>,
    /// Whether to write a video; false is a probe (no ffmpeg, no timeline).
    pub video: bool,
}

impl Config {
    /// Default settings writing to `out_dir`.
    pub fn new(out_dir: impl Into<PathBuf>) -> Config {
        Config {
            size: vec2(1280.0, 720.0),
            pixels_per_point: 1.5,
            fps: 30,
            out_dir: out_dir.into(),
            ffmpeg: std::env::var_os("FFMPEG")
                .map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from),
            lead: None,
            breath: None,
            video: true,
        }
    }
}

impl Default for Config {
    fn default() -> Config {
        Config::new("tour-out")
    }
}

/// Which of several same-labelled buttons to pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The leftmost.
    Left,
    /// The rightmost.
    Right,
}

/// How [`Recorder::wait`] runs.
#[derive(Clone, Copy, Debug)]
pub struct Wait {
    /// One frame is captured per `speed` steps (a time-lapse when > 1).
    pub speed: u32,
    /// Steps in a row the condition must hold.
    pub stable: u32,
    /// Real-time limit before the wait panics.
    pub timeout: Duration,
}

impl Wait {
    /// Real time, one stable step, 30 minute timeout.
    pub fn new() -> Wait {
        Wait {
            speed: 1,
            stable: 1,
            timeout: Duration::from_secs(30 * 60),
        }
    }

    /// Time-lapse: capture one frame per `speed` steps.
    pub fn lapse(mut self, speed: u32) -> Wait {
        self.speed = speed.max(1);
        self
    }

    /// Require the condition to hold for `steps` steps in a row.
    pub fn stable(mut self, steps: u32) -> Wait {
        self.stable = steps;
        self
    }
}

impl Default for Wait {
    fn default() -> Wait {
        Wait::new()
    }
}

/// The harness state: the app plus what the recorder draws over it.
struct Stage<S> {
    app: S,
    overlay: Overlay,
    caption: Option<ShownCaption>,
}

fn ease(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Canonicalise `p` and strip the Windows verbatim prefix, if any.
fn plain(p: &Path) -> io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    let s = c.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => c,
    })
}

/// A scripted headless recording of an egui app of state `S`.
pub struct Recorder<S: 'static> {
    h: Harness<'static, Stage<S>>,
    ff: Option<Child>,
    cfg: Config,
    narration_path: PathBuf,
    schedule: Schedule,
    /// Frames written to the video.
    frames: u64,
    cursor: Pos2,
    dt: f32,
}

impl<S: 'static> Recorder<S> {
    /// Build the harness around `ui` and `state`, and start ffmpeg when
    /// `config.video` is set. `ui` draws into a root ui covering the whole
    /// viewport; it should apply the app's theme itself.
    pub fn new(
        config: Config,
        narration: Narration,
        state: S,
        mut ui: impl FnMut(&mut egui::Ui, &mut S) + 'static,
    ) -> io::Result<Recorder<S>> {
        std::fs::create_dir_all(&config.out_dir)?;
        let fps = config.fps;
        let dt = 1.0 / fps as f32;
        let schedule = Schedule::new(
            narration.clone(),
            fps,
            config.lead.unwrap_or(narration.lead),
            config.breath.unwrap_or(narration.breath),
        );
        let narration_path = narration.path.clone();
        let stage = Stage {
            app: state,
            overlay: Overlay::default(),
            caption: None,
        };
        let h = Harness::builder()
            .with_size(config.size)
            .with_pixels_per_point(config.pixels_per_point)
            .with_step_dt(dt)
            .renderer(NoTestCursor(WgpuTestRenderer::new()))
            .build_ui_state(
                move |kui, st: &mut Stage<S>| {
                    let ctx = kui.ctx().clone();
                    // A root ui over the whole viewport, as eframe gives the
                    // app (kittest's own has an 8 pt outer margin).
                    let rect = ctx.content_rect();
                    let mut root = egui::Ui::new(
                        ctx.clone(),
                        Id::new("tour-root"),
                        egui::UiBuilder::new().max_rect(rect),
                    );
                    ui(&mut root, &mut st.app);
                    paint_overlay(&ctx, &st.overlay, st.caption.as_ref());
                },
                stage,
            );
        let ff = if config.video {
            Some(spawn_ffmpeg(&config)?)
        } else {
            None
        };
        let cursor = pos2(config.size.x * 0.62, config.size.y * 0.7);
        let mut rec = Recorder {
            h,
            ff,
            cfg: config,
            narration_path,
            schedule,
            frames: 0,
            cursor,
            dt,
        };
        rec.h.state_mut().overlay.cursor = Some(cursor);
        Ok(rec)
    }

    /// The egui context.
    pub fn ctx(&self) -> &egui::Context {
        &self.h.ctx
    }

    /// The app state.
    pub fn state(&self) -> &S {
        &self.h.state().app
    }

    /// The app state, mutably.
    pub fn state_mut(&mut self) -> &mut S {
        &mut self.h.state_mut().app
    }

    fn now(&self) -> f64 {
        self.h.ctx.input(|i| i.time)
    }

    fn overlay(&mut self) -> &mut Overlay {
        &mut self.h.state_mut().overlay
    }

    /// Point the overlay at the caption scheduled for the next frame.
    fn sync_caption(&mut self) {
        let fps = self.cfg.fps as f32;
        let frame = self.frames;
        let shown = self
            .schedule
            .caption_at(frame)
            .map(|(text, since)| ShownCaption {
                text: text.to_string(),
                age: frame.saturating_sub(since) as f32 / fps,
            });
        self.h.state_mut().caption = shown;
    }

    fn capture(&mut self) {
        let img = self.h.render().expect("render");
        if let Some(ff) = &mut self.ff {
            ff.stdin
                .as_mut()
                .expect("ffmpeg stdin")
                .write_all(img.as_raw())
                .expect("write to ffmpeg");
        }
        self.frames += 1;
    }

    /// Step the app once, without capturing.
    fn step(&mut self) {
        self.sync_caption();
        self.h.step();
    }

    /// One step, captured.
    pub fn frame(&mut self) {
        self.step();
        self.capture();
    }

    /// Save a PNG still of the current view into the output directory.
    pub fn still(&mut self, name: &str) {
        self.sync_caption();
        let img = self.h.render().expect("render");
        img.save(self.cfg.out_dir.join(name)).expect("save still");
    }

    /// Hold the current view for `secs` seconds of video.
    pub fn hold(&mut self, secs: f32) {
        for _ in 0..frames(f64::from(secs), self.cfg.fps) {
            self.frame();
        }
    }

    /// Hold until the current clip (and its breath) has played.
    fn finish_speech(&mut self) {
        while self.frames < self.schedule.speech_until() {
            self.frame();
        }
    }

    /// Show narration line `id`'s caption and start its clip; the voice sets
    /// the pace, so this first waits for the previous clip to finish.
    ///
    /// # Panics
    /// Panics when `id` is not in the narration.
    pub fn say(&mut self, id: &str) {
        self.finish_speech();
        self.schedule.say(id, self.frames);
    }

    /// Remove the caption once the current clip has played.
    pub fn clear_caption(&mut self) {
        self.finish_speech();
        self.schedule.clear(self.frames);
    }

    fn point(&mut self, p: Pos2) {
        self.overlay().cursor = Some(p);
        self.h.input_mut().events.push(Event::PointerMoved(p));
    }

    /// Glide the cursor to `to` along a slight arc, eased.
    pub fn move_to(&mut self, to: Pos2) {
        let from = self.cursor;
        let d = from.distance(to);
        let n = frames(f64::from(0.35 + d / 1400.0), self.cfg.fps).max(1);
        let bow = if d > 1.0 {
            (to - from).normalized().rot90() * (d * 0.06)
        } else {
            Vec2::ZERO
        };
        for i in 1..=n {
            let e = ease(i as f32 / n as f32);
            let p = from + (to - from) * e + bow * (std::f32::consts::PI * e).sin();
            self.point(p);
            self.frame();
        }
        self.cursor = to;
    }

    fn button(&mut self, pressed: bool) {
        let pos = self.cursor;
        self.h.input_mut().events.push(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    /// Click at the cursor, with a ripple.
    pub fn click(&mut self) {
        let (p, now) = (self.cursor, self.now());
        let ov = self.overlay();
        ov.pressed = true;
        ov.ripples.push((p, now));
        self.button(true);
        self.frame();
        self.frame();
        self.overlay().pressed = false;
        self.button(false);
        self.frame();
    }

    /// The rect of the button labelled `label`: the rightmost one when `pick`
    /// is [`Pick::Right`], else the leftmost.
    pub fn find(&self, label: &str, pick: Pick) -> Option<Rect> {
        let rects = self
            .h
            .query_all_by_role_and_label(Role::Button, label)
            .map(|n| n.rect());
        let x = |a: &Rect, b: &Rect| a.center().x.total_cmp(&b.center().x);
        match pick {
            Pick::Right => rects.max_by(x),
            Pick::Left => rects.min_by(x),
        }
    }

    /// Move to the button labelled `label` and click it.
    ///
    /// # Panics
    /// Panics when no such button exists.
    pub fn click_on(&mut self, label: &str, pick: Pick) {
        let rect = self
            .find(label, pick)
            .unwrap_or_else(|| panic!("no button labelled {label:?}"));
        self.move_to(rect.center());
        self.hold(0.25);
        self.click();
    }

    /// Run until `done` has held for `how.stable` steps in a row. One frame
    /// is captured per `how.speed` steps (a time-lapse when > 1); each step
    /// is paced to a real frame time, so the app's wall clock and egui's
    /// agree.
    ///
    /// # Panics
    /// Panics when `how.timeout` passes first.
    pub fn wait(
        &mut self,
        what: &str,
        how: Wait,
        done: impl Fn(&S) -> bool,
        mut on_step: impl FnMut(&mut Recorder<S>, u32),
    ) {
        let started = Instant::now();
        let speed = how.speed.max(1);
        self.overlay().fast = (speed > 1).then_some(speed);
        let (mut i, mut held) = (0u32, 0u32);
        while held < how.stable {
            assert!(
                started.elapsed() < how.timeout,
                "timed out waiting for {what}"
            );
            let t0 = Instant::now();
            self.step();
            if i.is_multiple_of(speed) {
                self.capture();
            }
            on_step(self, i);
            i += 1;
            held = if done(self.state()) { held + 1 } else { 0 };
            if let Some(rest) = Duration::from_secs_f32(self.dt).checked_sub(t0.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        self.overlay().fast = None;
        eprintln!(
            "tour: {what} after {:.1} s ({i} steps)",
            started.elapsed().as_secs_f32()
        );
    }

    /// Print every labelled AccessKit node with its position, to stderr.
    pub fn dump_labels(&self) {
        for n in self.h.root().children_recursive() {
            let a = n.accesskit_node();
            if let Some(l) = a.label() {
                let r = n.rect();
                eprintln!(
                    "  {:?} {l:?} @ ({:.0},{:.0}) {:.0}×{:.0}",
                    a.role(),
                    r.min.x,
                    r.min.y,
                    r.width(),
                    r.height()
                );
            }
        }
    }

    /// Close the video, then write `captions.vtt` and (for a video) the
    /// `timeline.json` that `narrate mux` reads.
    pub fn finish(mut self) -> io::Result<()> {
        if let Some(mut ff) = self.ff.take() {
            drop(ff.stdin.take());
            let status = ff.wait()?;
            if !status.success() {
                return Err(io::Error::other(format!("ffmpeg failed: {status}")));
            }
        }
        self.schedule.finish(self.frames);
        let fps = self.cfg.fps;
        let out = &self.cfg.out_dir;
        write_vtt(&out.join("captions.vtt"), &self.schedule.captions(), fps)?;
        if self.cfg.video {
            write_timeline(
                &out.join("timeline.json"),
                "silent.mp4",
                &plain(&self.narration_path)?,
                &plain(out)?,
                fps,
                self.schedule.cues(),
            )?;
        }
        eprintln!(
            "tour: {} frames ({:.1} s of video)",
            self.frames,
            self.frames as f32 / fps as f32
        );
        Ok(())
    }
}

fn spawn_ffmpeg(cfg: &Config) -> io::Result<Child> {
    let w = (cfg.size.x * cfg.pixels_per_point) as u32;
    let h = (cfg.size.y * cfg.pixels_per_point) as u32;
    Command::new(&cfg.ffmpeg)
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ])
        .args([
            "-s",
            &format!("{w}x{h}"),
            "-r",
            &cfg.fps.to_string(),
            "-i",
            "-",
        ])
        .args(["-c:v", "libx264", "-preset", "medium", "-crf", "18"])
        .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
        .arg(cfg.out_dir.join("silent.mp4"))
        .stdin(Stdio::piped())
        .spawn()
}
