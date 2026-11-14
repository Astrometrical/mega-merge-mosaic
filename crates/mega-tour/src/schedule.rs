//! The narration-paced caption schedule, in output frames.
//!
//! A `say` shows its caption at the call's frame, starts the clip `lead`
//! seconds later and makes the next `say` wait for the clip plus the breath.
//! Passages show one caption per sentence, switching at frames derived from
//! the clip start, never from wall-clock time.

use crate::narration::{Line, Narration};

/// Convert seconds to output frames, rounding half away from zero and
/// saturating at 0.
pub fn frames(secs: f64, fps: u32) -> u64 {
    (secs * f64::from(fps)).round().max(0.0) as u64
}

/// Convert seconds to output frames, rounding up so the frames cover all of
/// `secs`, and saturating at 0. Floating-point noise under a millionth of
/// a frame is ignored, so 4.4 s at 30 fps is 132 frames, not 133.
pub fn frames_ceil(secs: f64, fps: u32) -> u64 {
    (secs * f64::from(fps) - 1e-6).ceil().max(0.0) as u64
}

/// One scheduled caption; `end` is `None` while it is still open.
struct Span {
    start: u64,
    end: Option<u64>,
    text: String,
}

/// The caption and cue schedule of one recording.
pub struct Schedule {
    narration: Narration,
    fps: u32,
    lead: f64,
    breath: f64,
    speech_until: u64,
    spans: Vec<Span>,
    cues: Vec<(String, u64)>,
}

impl Schedule {
    /// A new, empty schedule over `narration`, with `lead` seconds before each
    /// clip and `breath` seconds after it.
    pub fn new(narration: Narration, fps: u32, lead: f64, breath: f64) -> Schedule {
        Schedule {
            narration,
            fps,
            lead,
            breath,
            speech_until: 0,
            spans: Vec::new(),
            cues: Vec::new(),
        }
    }

    /// Show line `id`'s caption(s) from frame `now` and return the frame its
    /// clip starts on. Callers have already advanced `now` to
    /// [`speech_until`](Self::speech_until).
    ///
    /// # Panics
    /// Panics, naming `id` and the known ids, when `id` is not in the
    /// narration.
    pub fn say(&mut self, id: &str, now: u64) -> u64 {
        let line = self.known_line(id);
        let clip_start = now + frames(self.lead, self.fps);
        let starts: Vec<(u64, String)> = if line.captions.is_empty() {
            vec![(now, line.text.clone())]
        } else {
            line.captions
                .iter()
                .enumerate()
                .map(|(k, c)| {
                    let at = if k == 0 {
                        now
                    } else {
                        clip_start + frames(c.start, self.fps)
                    };
                    (at, c.text.clone())
                })
                .collect()
        };
        let until = clip_start + frames_ceil(line.duration + self.breath, self.fps);

        self.end_open(now);
        let ends: Vec<Option<u64>> = (0..starts.len())
            .map(|k| starts.get(k + 1).map(|s| s.0))
            .collect();
        for ((start, text), end) in starts.into_iter().zip(ends) {
            self.spans.push(Span { start, end, text });
        }
        self.speech_until = until;
        self.cues.push((id.to_string(), clip_start));
        clip_start
    }

    /// Check that line `id` is in the narration, so a caller can fail
    /// before doing anything else.
    ///
    /// # Panics
    /// Panics, naming `id` and the known ids, when it is not.
    pub fn check_id(&self, id: &str) {
        self.known_line(id);
    }

    /// Line `id`, or a panic naming it and the known ids.
    fn known_line(&self, id: &str) -> &Line {
        self.narration.line(id).unwrap_or_else(|| {
            let known: Vec<&str> = self.narration.lines.iter().map(|l| l.id.as_str()).collect();
            panic!("say: unknown narration line {id:?}; known ids: {known:?}");
        })
    }

    /// The frame the current clip and its breath finish on (rounded up, so
    /// the next cue never starts before the clip ends).
    pub fn speech_until(&self) -> u64 {
        self.speech_until
    }

    /// The caption showing at `frame`: its text and the frame it appeared on.
    pub fn caption_at(&self, frame: u64) -> Option<(&str, u64)> {
        self.spans
            .iter()
            .rev()
            .find(|s| s.start <= frame && s.end.is_none_or(|e| frame < e))
            .map(|s| (s.text.as_str(), s.start))
    }

    /// End the open caption at `now`, so nothing shows until the next `say`.
    pub fn clear(&mut self, now: u64) {
        self.end_open(now);
    }

    /// Close any open caption at the final frame `last`.
    pub fn finish(&mut self, last: u64) {
        self.end_open(last);
    }

    /// The `(line id, clip start frame)` cues, one per `say`, in order.
    pub fn cues(&self) -> &[(String, u64)] {
        &self.cues
    }

    /// The closed caption spans `(start, end, text)` in frames, in order,
    /// without zero-length spans.
    pub fn captions(&self) -> Vec<(u64, u64, String)> {
        self.spans
            .iter()
            .filter_map(|s| s.end.map(|e| (s.start, e, s.text.clone())))
            .filter(|(a, b, _)| b > a)
            .collect()
    }

    /// End every open span, and clamp any scheduled past `at`, to `at`.
    fn end_open(&mut self, at: u64) {
        for s in &mut self.spans {
            s.end = Some(s.end.map_or(at, |e| e.min(at)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample() -> Narration {
        Narration::from_json(
            PathBuf::from("n.json"),
            r#"{"version":1,"lines":[
            {"id":"a","text":"Alpha.","kind":"line","clip":"a.wav","duration":2.0,"status":"approved"},
            {"id":"b","text":"Beta.","kind":"line","clip":"b.wav","duration":1.0,"status":"approved"},
            {"id":"p","text":"One. Two. Three.","kind":"passage","clip":"p.wav","duration":4.0,"status":"approved",
             "captions":[{"text":"One.","start":0.0,"end":1.0},
                         {"text":"Two.","start":1.0,"end":2.5},
                         {"text":"Three.","start":2.5,"end":4.0}]},
            {"id":"d","text":"Draft.","kind":"line","clip":null,"duration":3.3,"status":"draft"}
            ]}"#,
        )
        .unwrap()
    }

    fn sched() -> Schedule {
        Schedule::new(sample(), 30, 0.15, 0.4)
    }

    #[test]
    fn frames_round() {
        assert_eq!(frames(0.15, 30), 5);
        assert_eq!(frames(2.4, 30), 72);
        assert_eq!(frames(0.5, 30), 15);
        assert_eq!(frames(1.0 / 60.0, 30), 1);
        assert_eq!(frames(-1.0, 30), 0);
    }

    #[test]
    fn say_schedules_clip_and_breath() {
        let mut s = sched();
        let start = s.say("a", 10);
        assert_eq!(start, 15);
        assert_eq!(s.speech_until(), 15 + 72);
        assert_eq!(s.caption_at(10), Some(("Alpha.", 10)));
    }

    #[test]
    fn say_waits_for_previous_speech() {
        let mut s = sched();
        let first = s.say("a", 0);
        let now = s.speech_until();
        let second = s.say("b", now);
        assert!(second >= now);
        assert!(second > first + frames(2.0, 30));
        assert_eq!(s.cues().len(), 2);
        assert_eq!(s.cues()[0], ("a".to_string(), first));
        assert_eq!(s.cues()[1], ("b".to_string(), second));
    }

    #[test]
    fn same_line_twice_gives_two_cues() {
        let mut s = sched();
        s.say("a", 0);
        let now = s.speech_until();
        s.say("a", now);
        assert_eq!(s.cues().len(), 2);
        assert_eq!(s.cues()[0].0, "a");
        assert_eq!(s.cues()[1].0, "a");
        assert!(s.cues()[1].1 > s.cues()[0].1);
    }

    #[test]
    fn captions_switch_in_output_frames() {
        let mut s = sched();
        let clip = s.say("p", 100);
        assert_eq!(clip, 105);
        assert_eq!(s.caption_at(100), Some(("One.", 100)));
        assert_eq!(s.caption_at(clip + 29), Some(("One.", 100)));
        assert_eq!(s.caption_at(clip + 30), Some(("Two.", clip + 30)));
        assert_eq!(s.caption_at(clip + 74), Some(("Two.", clip + 30)));
        assert_eq!(s.caption_at(clip + 75), Some(("Three.", clip + 75)));
        assert_eq!(s.caption_at(99), None);
    }

    #[test]
    fn draft_lines_pace_by_estimated_duration() {
        let mut s = sched();
        let start = s.say("d", 0);
        assert_eq!(start, 5);
        assert_eq!(s.speech_until(), 5 + frames_ceil(3.3 + 0.4, 30));
        assert_eq!(s.speech_until(), 5 + 111);
        assert_eq!(s.cues(), &[("d".to_string(), 5)]);
    }

    #[test]
    fn frames_ceil_rounds_up_but_ignores_float_noise() {
        assert_eq!(frames_ceil(2.4, 30), 72);
        assert_eq!(frames_ceil(1.41, 30), 43);
        assert_eq!(frames_ceil(1.0 / 60.0, 30), 1);
        // 4.0 + 0.4 is 4.4000000000000004, whose 132.00000000000003 frames
        // must not become 133.
        assert_eq!(frames_ceil(4.0 + 0.4, 30), 132);
        assert_eq!(frames_ceil(0.0, 30), 0);
        assert_eq!(frames_ceil(-1.0, 30), 0);
    }

    #[test]
    fn speech_until_covers_the_whole_clip() {
        let n = Narration::from_json(
            PathBuf::from("n.json"),
            r#"{"version":1,"lines":[
            {"id":"a","text":"A.","kind":"line","clip":"a.wav","duration":1.01,"status":"approved"}]}"#,
        )
        .unwrap();
        let mut s = Schedule::new(n, 30, 0.15, 0.4);
        let start = s.say("a", 0);
        // 1.41 s is 42.3 frames: rounding to 42 would start the next cue
        // before this clip and its breath end.
        assert_eq!(s.speech_until(), start + 43);
        assert!((s.speech_until() - start) as f64 / 30.0 >= 1.01 + 0.4);
    }

    #[test]
    #[should_panic(expected = r#"unknown narration line "nope"; known ids: ["a", "b", "p", "d"]"#)]
    fn unknown_line_id_is_named() {
        sched().say("nope", 0);
    }

    #[test]
    #[should_panic(expected = r#"unknown narration line "nope"; known ids: ["a", "b", "p", "d"]"#)]
    fn check_id_names_an_unknown_id() {
        sched().check_id("nope");
    }

    #[test]
    fn check_id_accepts_a_known_id() {
        let s = sched();
        s.check_id("p");
        assert!(s.cues().is_empty());
    }

    #[test]
    fn clear_hides_caption() {
        let mut s = sched();
        s.say("a", 0);
        s.clear(50);
        assert_eq!(s.caption_at(49), Some(("Alpha.", 0)));
        assert_eq!(s.caption_at(50), None);
        assert_eq!(s.captions(), vec![(0, 50, "Alpha.".to_string())]);
    }

    #[test]
    fn empty_captions_show_the_line_text() {
        let mut s = sched();
        s.say("b", 0);
        s.finish(40);
        assert_eq!(s.captions(), vec![(0, 40, "Beta.".to_string())]);
    }

    #[test]
    fn zero_length_spans_are_dropped() {
        let mut s = sched();
        s.say("a", 10);
        s.clear(10);
        assert!(s.captions().is_empty());
    }

    #[test]
    fn passage_spans_cover_each_sentence() {
        let mut s = sched();
        let clip = s.say("p", 0);
        s.finish(clip + 200);
        assert_eq!(
            s.captions(),
            vec![
                (0, clip + 30, "One.".to_string()),
                (clip + 30, clip + 75, "Two.".to_string()),
                (clip + 75, clip + 200, "Three.".to_string()),
            ]
        );
    }
}
