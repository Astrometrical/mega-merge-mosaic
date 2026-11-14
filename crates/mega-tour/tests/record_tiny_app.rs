//! Records a tiny egui app end to end. Needs a GPU and ffmpeg:
//! `WGPU_BACKEND=vulkan FFMPEG=<path> cargo test -p mega-tour --test
//! record_tiny_app -- --ignored --nocapture`.

use std::time::Duration;

use mega_tour::narration::Narration;
use mega_tour::{Config, Pick, Recorder, Wait};

#[derive(Default)]
struct Tiny {
    count: u32,
}

const MANIFEST: &str = r#"{
 "version": 1, "draft": true, "lead": 0.15, "breath": 0.4,
 "lines": [
  {"id": "one", "text": "Press the button.", "kind": "line", "clip": null,
   "duration": 1.0, "status": "draft",
   "captions": [{"text": "Press the button.", "start": 0.0, "end": 1.0}]},
  {"id": "two", "text": "Now it counts.", "kind": "line", "clip": null,
   "duration": 1.0, "status": "draft",
   "captions": [{"text": "Now it counts.", "start": 0.0, "end": 1.0}]}
 ]
}"#;

/// Seconds of a WebVTT timestamp, `HH:MM:SS.mmm`.
fn vtt_secs(t: &str) -> f64 {
    let p: Vec<f64> = t.trim().split(':').map(|x| x.parse().unwrap()).collect();
    p[0] * 3600.0 + p[1] * 60.0 + p[2]
}

fn cues(path: &std::path::Path) -> Vec<(String, f64)> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["cues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["start"].as_f64().unwrap(),
            )
        })
        .collect()
}

#[test]
#[ignore = "needs a GPU (WGPU_BACKEND=vulkan on Windows) and ffmpeg ($FFMPEG)"]
fn records_a_tiny_app() {
    let dir = std::env::temp_dir().join(format!("mega-tour-tiny-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = dir.join("narration.json");
    std::fs::write(&manifest, MANIFEST).unwrap();
    let out = dir.join("out");

    let narration = Narration::load(&manifest).unwrap();
    let mut rec = Recorder::new(
        Config::new(&out),
        narration,
        Tiny::default(),
        |ui, app: &mut Tiny| {
            mega_ui::theme::ensure_applied(ui.ctx());
            ui.vertical(|ui| {
                if ui.button("Count").clicked() {
                    app.count += 1;
                }
                ui.label(format!("Count: {}", app.count));
            });
        },
    )
    .unwrap();

    rec.hold(0.5);
    rec.say("one");
    rec.click_on("Count", Pick::Left);
    let how = Wait::new().stable(3).timeout(Duration::from_secs(60));
    rec.wait("count", how, |a| a.count == 1, |_, _| {});
    assert_eq!(rec.state().count, 1);
    // End on a `say`: `finish` lets its clip play out.
    rec.say("two");
    rec.finish().unwrap();

    assert!(std::fs::metadata(out.join("silent.mp4")).unwrap().len() > 0);
    let tl = cues(&out.join("timeline.json"));
    assert_eq!(tl.len(), 2);
    assert_eq!(tl[0].0, "one");
    assert_eq!(tl[1].0, "two");
    let text = std::fs::read_to_string(out.join("timeline.json")).unwrap();
    assert!(text.contains("silent.mp4"));
    // The second clip starts after the first's start + duration (1.0) + breath (0.4).
    assert!(tl[1].1 >= tl[0].1 + 1.0 + 0.4 - 1e-6, "{tl:?}");
    let vtt = std::fs::read_to_string(out.join("captions.vtt")).unwrap();
    assert_eq!(vtt.matches(" --> ").count(), 2, "{vtt}");
    // The last caption closes at the final frame, so its end is the video's
    // length: it covers the last cue, its clip (1.0) and its breath (0.4).
    let last = vtt.lines().rfind(|l| l.contains(" --> ")).unwrap();
    let end = vtt_secs(last.split(" --> ").nth(1).unwrap());
    assert!(end >= tl[1].1 + 1.0 + 0.4 - 1e-6, "{end} vs {tl:?}\n{vtt}");

    std::fs::remove_dir_all(&dir).unwrap();
}
