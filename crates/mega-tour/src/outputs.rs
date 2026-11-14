//! The recorder's text outputs: `captions.vtt` and `timeline.json`.

use std::io;
use std::path::Path;

/// Format output frame `frame` as a WebVTT timestamp, `HH:MM:SS.mmm`.
fn timestamp(frame: u64, fps: u32) -> String {
    let ms = (frame as f64 / f64::from(fps) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// `text` as a WebVTT cue payload: a blank line would end the cue, so each
/// run of blank (or whitespace-only) lines becomes a single space, and
/// leading and trailing ones go. Single line breaks stay.
fn cue_text(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            gap = true;
            continue;
        }
        if !out.is_empty() {
            out.push(if gap { ' ' } else { '\n' });
        }
        out.push_str(line);
        gap = false;
    }
    out
}

/// Write `spans` (`(start, end, text)` in frames) as a WebVTT file, one cue
/// per span. Blank lines inside a span's text collapse into single spaces,
/// so no cue is split.
pub fn write_vtt(path: &Path, spans: &[(u64, u64, String)], fps: u32) -> io::Result<()> {
    let mut vtt = String::from("WEBVTT\n\n");
    for (a, b, text) in spans {
        vtt.push_str(&format!(
            "{} --> {}\n{}\n\n",
            timestamp(*a, fps),
            timestamp(*b, fps),
            cue_text(text)
        ));
    }
    std::fs::write(path, vtt)
}

/// Write the `timeline.json` that `narrate mux` reads.
///
/// `narration` is stored relative to `out_dir` when it lies inside it, else
/// as given, always with forward slashes. Cue starts are seconds, rounded to
/// milliseconds.
pub fn write_timeline(
    path: &Path,
    video: &str,
    narration: &Path,
    out_dir: &Path,
    fps: u32,
    cues: &[(String, u64)],
) -> io::Result<()> {
    let shown = narration.strip_prefix(out_dir).unwrap_or(narration);
    let narration = shown.to_string_lossy().replace('\\', "/");
    let cues: Vec<serde_json::Value> = cues
        .iter()
        .map(|(id, frame)| {
            let start = (*frame as f64 / f64::from(fps) * 1000.0).round() / 1000.0;
            serde_json::json!({ "id": id, "start": start })
        })
        .collect();
    let doc = serde_json::json!({
        "version": 1,
        "video": video,
        "narration": narration,
        "fps": fps,
        "cues": cues,
    });
    let text = serde_json::to_string_pretty(&doc).map_err(io::Error::other)?;
    std::fs::write(path, text + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::narration::Narration;
    use crate::schedule::Schedule;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mega-tour-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn vtt_closes_open_caption_at_finish() {
        let n = Narration::from_json(
            PathBuf::from("n.json"),
            r#"{"version":1,"lines":[
            {"id":"a","text":"Alpha.","kind":"line","clip":"a.wav","duration":2.0,"status":"approved"},
            {"id":"b","text":"Beta.","kind":"line","clip":"b.wav","duration":1.0,"status":"approved"}]}"#,
        )
        .unwrap();
        let mut s = Schedule::new(n, 30, 0.15, 0.4);
        s.say("a", 0);
        let now = s.speech_until(); // 77
        s.say("b", now);
        s.finish(now + 45);
        let dir = scratch("vtt");
        let p = dir.join("captions.vtt");
        write_vtt(&p, &s.captions(), 30).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "WEBVTT\n\n00:00:00.000 --> 00:00:02.567\nAlpha.\n\n\
             00:00:02.567 --> 00:00:04.067\nBeta.\n\n"
        );
    }

    #[test]
    fn vtt_cue_text_has_no_blank_lines() {
        let dir = scratch("vtt-blank");
        let p = dir.join("captions.vtt");
        let spans = vec![
            (0, 30, "One.\n\nTwo.\n \t\nThree.\nFour.".to_string()),
            (30, 60, "\n\nLead.\r\n\r\nTrail.\n\n".to_string()),
        ];
        write_vtt(&p, &spans, 30).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nOne. Two. Three.\nFour.\n\n\
             00:00:01.000 --> 00:00:02.000\nLead. Trail.\n\n"
        );
    }

    #[test]
    fn timeline_is_spec_shaped() {
        let dir = scratch("timeline");
        let p = dir.join("timeline.json");
        let cues = vec![("a".to_string(), 15), ("b".to_string(), 100)];
        write_timeline(
            &p,
            "silent.mp4",
            &dir.join("narr/narration.json"),
            &dir,
            30,
            &cues,
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["video"], "silent.mp4");
        assert_eq!(v["narration"], "narr/narration.json");
        assert_eq!(v["fps"], 30);
        assert_eq!(v["cues"][0]["id"], "a");
        assert_eq!(v["cues"][0]["start"], 0.5);
        assert_eq!(v["cues"][1]["id"], "b");
        assert_eq!(v["cues"][1]["start"], 3.333);

        let outside = std::env::temp_dir()
            .join("elsewhere")
            .join("narration.json");
        write_timeline(&p, "silent.mp4", &outside, &dir, 30, &cues).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let n = v["narration"].as_str().unwrap();
        assert!(!n.contains('\\') && n.ends_with("elsewhere/narration.json"));
        assert!(Path::new(n).is_absolute());
    }
}
