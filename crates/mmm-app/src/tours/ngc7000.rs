//! The NGC 7000 tour: a real two-panel, six-filter session, opened, analysed,
//! merged and viewed as H-alpha, SHO and RGB, narrated by
//! `crates/mmm-app/tours/ngc7000.narration.toml`.
//!
//! Inputs come from the environment: `TOUR_SESSION` (the session dir),
//! `TOUR_NARRATION` (the `narration.json` that `narrate build` wrote) and
//! `TOUR_OUT`. `TOUR_PROBE=1` records nothing: it waits for idle, dumps the
//! AccessKit labels and saves stills.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::pos2;
use mega_tour::narration::Narration;
use mega_tour::schedule::frames;
use mega_tour::{Config, Pick, Recorder, Wait};

use crate::app::Shell;
use crate::session::Request;
use crate::session_window::CanvasMode;
use crate::start::StartScreen;
use crate::start::tests::{RecordingLauncher, StubPicker};

type Rec = Recorder<Shell>;

/// An environment variable that the tour needs, or a clear panic.
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set (see tools/tour.sh)"))
}

/// Whether the session is open and nothing is running.
fn idle(s: &Shell) -> bool {
    s.session().is_some()
        && !s.is_busy()
        && s.jobs().is_some_and(|j| !j.busy())
        && !s.checks().running()
}

fn merged(s: &Shell) -> bool {
    idle(s) && s.session().is_some_and(|x| x.merged)
}

fn mosaic_settled(s: &Shell) -> bool {
    s.window().mode() == CanvasMode::Mosaic && s.window().mosaic().is_settled()
}

fn plan_settled(s: &Shell) -> bool {
    s.window().plan().is_settled()
}

fn recorder(out: PathBuf, narration: Narration, video: bool) -> Rec {
    let shell = Shell::new(StartScreen::new(Vec::new(), Arc::new(StubPicker(None))))
        .with_launcher(Arc::new(RecordingLauncher::default()));
    let mut config = Config::new(out);
    config.video = video;
    Recorder::new(config, narration, shell, |root, shell: &mut Shell| {
        let ctx = root.ctx().clone();
        if mega_ui::theme::ensure_applied(&ctx) {
            shell.ui(root);
        }
    })
    .expect("start the recorder")
}

#[test]
#[ignore = "tour: needs a GPU, ffmpeg, TOUR_SESSION, TOUR_NARRATION and TOUR_OUT"]
fn ngc7000() {
    let session = PathBuf::from(env("TOUR_SESSION"));
    assert!(
        session.is_dir(),
        "TOUR_SESSION {} is not an existing directory",
        session.display()
    );
    let narration_path = PathBuf::from(env("TOUR_NARRATION"));
    let out = PathBuf::from(env("TOUR_OUT"));
    let probe = std::env::var_os("TOUR_PROBE").is_some_and(|v| v == "1");
    let narration = Narration::load(&narration_path)
        .unwrap_or_else(|e| panic!("load {}: {e}", narration_path.display()));
    let mut rec = recorder(out, narration, !probe);
    let ctx = rec.ctx().clone();
    rec.state_mut()
        .request(Request::Open(session.clone()), &ctx);
    if probe {
        // A still every 5 s of steps (one step is one frame).
        let every = frames(5.0, rec.config().fps) as u32;
        let mut next = 0u32;
        rec.wait("idle", Wait::new().stable(30), idle, |rec, i| {
            fail_if_not_opened(rec, &session);
            if i >= next {
                rec.still(&format!("probe_{i:05}.png"));
                next = i + every;
            }
        });
        rec.still("probe_idle.png");
        rec.dump_labels();
        return;
    }
    tour(&mut rec, &session);
    rec.finish().expect("finish the recording");
}

/// Panic with the start screen's error when opening `session` has failed,
/// rather than wait for it.
fn fail_if_not_opened(rec: &Rec, session: &Path) {
    let shell = rec.state();
    if shell.session().is_none()
        && let Some(why) = shell.start.error()
    {
        panic!("session {} did not open: {why}", session.display());
    }
}

/// The walkthrough. `session` was asked to open just before.
fn tour(rec: &mut Rec, session: &Path) {
    let none = |_: &mut Rec, _: u32| {};
    let once = Wait::new().stable(5);

    // 1. Open: previews, analysis and checks run for real (time-lapsed).
    rec.say("open");
    rec.wait(
        "session open",
        Wait::new().stable(1).timeout(Duration::from_secs(120)),
        |s| s.session().is_some(),
        |rec, _| fail_if_not_opened(rec, session),
    );
    rec.hold(2.5);
    rec.say("analyse");
    rec.wait(
        "previews, analysis and checks",
        Wait::new().lapse(8).stable(30),
        idle,
        none,
    );
    rec.still("t1_analysed.png");
    rec.hold(1.5);

    // 2. Groups.
    rec.say("groups");
    rec.hold(1.0);
    rec.click_on("H · 2", Pick::Left);
    rec.wait("H plan", once, plan_settled, none);
    rec.hold(1.5);
    rec.click_on("O · 2", Pick::Left);
    rec.wait("O plan", once, plan_settled, none);
    rec.still("t2_o_plan.png");
    rec.hold(1.5);

    // 3. Merge everything.
    rec.say("merge");
    rec.hold(0.8);
    rec.click_on("Merge", Pick::Right);
    rec.hold(1.0);
    rec.wait("merge", Wait::new().lapse(8).stable(30), merged, none);
    rec.still("t3_merged.png");
    if rec.state().window().mode() != CanvasMode::Mosaic {
        rec.click_on("Mosaic", Pick::Left);
    }
    rec.say("h-mosaic");
    rec.click_on("H · 2", Pick::Left);
    rec.wait("H mosaic", once, mosaic_settled, none);
    rec.hold(3.0);
    rec.still("t4_h_mosaic.png");

    // 4. Composites.
    rec.say("sho");
    rec.click_on("SHO", Pick::Left);
    rec.wait("SHO mosaic", once, mosaic_settled, none);
    rec.hold(3.5);
    rec.still("t5_sho.png");
    rec.say("rgb");
    rec.click_on("RGB", Pick::Left);
    rec.wait("RGB mosaic", once, mosaic_settled, none);
    rec.hold(3.5);
    rec.still("t6_rgb.png");
    rec.say("rendered");
    let size = rec.config().size;
    rec.move_to(pos2(size.x * 0.62, size.y * 0.62));
    rec.hold(3.0);
    rec.clear_caption();
    rec.hold(0.5);
}
