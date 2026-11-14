//! The narration manifest (`narration.json`), written by `narrate build`.
//!
//! Parsing is strict about the fields the recorder needs and ignores the
//! rest, so `narrate` can add fields without breaking the recorder.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The manifest version this reader supports.
const VERSION: i64 = 1;
/// Default seconds of silence before a clip starts.
const DEFAULT_LEAD: f64 = 0.15;
/// Default seconds of silence after a clip ends.
const DEFAULT_BREATH: f64 = 0.4;
/// The accepted values of a line's `status`.
const STATUSES: [&str; 4] = ["approved", "new", "flagged", "draft"];

/// One displayed caption entry of a line, in seconds from the clip start.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Caption {
    /// The caption text.
    pub text: String,
    /// When the caption appears, in seconds from the clip start.
    pub start: f64,
    /// When the caption disappears, in seconds from the clip start.
    pub end: f64,
}

/// One narrated line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Line {
    /// The line's id, as used by `say`.
    pub id: String,
    /// The displayed text.
    pub text: String,
    /// The line kind (for example `line` or `passage`).
    pub kind: String,
    /// The clip file, or `None` for a draft line.
    #[serde(default)]
    pub clip: Option<String>,
    /// The clip length (or the estimate, for a draft) in seconds.
    pub duration: f64,
    /// One of `approved`, `new`, `flagged` or `draft`.
    pub status: String,
    /// The captions shown while the clip plays.
    #[serde(default)]
    pub captions: Vec<Caption>,
}

/// A parsed manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Narration {
    /// Where the manifest was read from.
    pub path: PathBuf,
    /// Whether this is a draft manifest (estimated durations, no clips).
    pub draft: bool,
    /// Seconds of silence before a clip starts.
    pub lead: f64,
    /// Seconds of silence after a clip ends.
    pub breath: f64,
    /// The lines, in script order.
    pub lines: Vec<Line>,
}

/// Why a manifest could not be read.
#[derive(Debug, thiserror::Error)]
pub enum NarrationError {
    /// The file could not be read.
    #[error("{}: cannot read ({source})", path.display())]
    Io {
        /// The manifest path.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The file is not valid JSON of the expected shape.
    #[error("{}: invalid JSON ({source})", path.display())]
    Json {
        /// The manifest path.
        path: PathBuf,
        /// The underlying error.
        source: serde_json::Error,
    },
    /// The `version` is missing or not supported.
    #[error("{}: unsupported manifest version {found} (supported: {VERSION})", path.display())]
    Version {
        /// The manifest path.
        path: PathBuf,
        /// What the file declared (`missing` when absent).
        found: String,
    },
    /// A field has a bad value.
    #[error("{}: {problem}", path.display())]
    Field {
        /// The manifest path.
        path: PathBuf,
        /// What is wrong.
        problem: String,
    },
}

#[derive(Deserialize)]
struct Raw {
    version: Option<serde_json::Value>,
    #[serde(default)]
    draft: bool,
    lead: Option<f64>,
    breath: Option<f64>,
    #[serde(default)]
    lines: Vec<Line>,
}

impl Narration {
    /// Read and validate the manifest at `path`.
    pub fn load(path: &Path) -> Result<Narration, NarrationError> {
        let json = std::fs::read_to_string(path).map_err(|source| NarrationError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Narration::from_json(path.to_path_buf(), &json)
    }

    /// Parse and validate manifest `json`, recording `path` as its origin.
    pub fn from_json(path: PathBuf, json: &str) -> Result<Narration, NarrationError> {
        let raw: Raw = serde_json::from_str(json).map_err(|source| NarrationError::Json {
            path: path.clone(),
            source,
        })?;
        let field = |problem: String| NarrationError::Field {
            path: path.clone(),
            problem,
        };
        match raw.version.as_ref().and_then(|v| v.as_i64()) {
            Some(VERSION) => {}
            _ => {
                let found = raw
                    .version
                    .as_ref()
                    .map_or_else(|| "missing".to_string(), |v| v.to_string());
                return Err(NarrationError::Version { path, found });
            }
        }
        let lead = raw.lead.unwrap_or(DEFAULT_LEAD);
        let breath = raw.breath.unwrap_or(DEFAULT_BREATH);
        for (name, v) in [("lead", lead), ("breath", breath)] {
            if !v.is_finite() || v < 0.0 {
                return Err(field(format!(
                    "{name} must be a finite number >= 0, got {v}"
                )));
            }
        }
        for line in &raw.lines {
            let id = &line.id;
            if !line.duration.is_finite() || line.duration < 0.0 {
                return Err(field(format!(
                    "line {id:?}: duration must be a finite number >= 0, got {}",
                    line.duration
                )));
            }
            if !STATUSES.contains(&line.status.as_str()) {
                return Err(field(format!(
                    "line {id:?}: unknown status {:?}",
                    line.status
                )));
            }
            for c in &line.captions {
                if !c.start.is_finite() || !c.end.is_finite() || c.start < 0.0 || c.end < c.start {
                    return Err(field(format!(
                        "line {id:?}: bad caption times {}..{}",
                        c.start, c.end
                    )));
                }
            }
        }
        Ok(Narration {
            path,
            draft: raw.draft,
            lead,
            breath,
            lines: raw.lines,
        })
    }

    /// The line with the given id, if any.
    pub fn line(&self, id: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn built() -> String {
        format!(
            r#"{{
 "version": 1,
 "script": "tour.narration.md",
 "voice": "test",
 "draft": false,
 "sample_rate": 24000,
 "lead": 0.15,
 "breath": 0.4,
 "extra_future_field": [1, 2],
 "lines": [
  {{
   "id": "intro",
   "text": "Hello there.",
   "spoken": "Hello there.",
   "kind": "line",
   "clip": "clips/intro.wav",
   "duration": 1.25,
   "at": null,
   "status": "approved",
   "flags": [],
   "captions": [{{"text": "Hello there.", "start": 0.0, "end": 1.25}}],
   "check": {{"heard": "hello there", "cer": 0.0, "pace": null, "take": 1, "key": "k", "sha256": "{SHA}"}}
  }},
  {{
   "id": "passage",
   "text": "One. Two. Three.",
   "spoken": "One. Two. Three.",
   "kind": "passage",
   "clip": "clips/passage.wav",
   "duration": 3.0,
   "at": null,
   "status": "flagged",
   "flags": ["pinned-audio-missing"],
   "captions": [
    {{"text": "One.", "start": 0.0, "end": 1.0}},
    {{"text": "Two.", "start": 1.0, "end": 2.0}},
    {{"text": "Three.", "start": 2.0, "end": 3.0}}
   ],
   "check": {{"heard": "one two three", "cer": 0.0, "pace": null, "take": 2, "key": "k2", "sha256": "{SHA}"}}
  }}
 ]
}}"#
        )
    }

    fn parse(json: &str) -> Result<Narration, NarrationError> {
        Narration::from_json(PathBuf::from("narration.json"), json)
    }

    #[test]
    fn parses_a_built_manifest() {
        let n = parse(&built()).unwrap();
        assert!(!n.draft);
        assert_eq!((n.lead, n.breath), (0.15, 0.4));
        assert_eq!(n.lines.len(), 2);
        let p = n.line("passage").unwrap();
        assert_eq!(p.captions.len(), 3);
        assert_eq!(p.captions[1].text, "Two.");
        assert_eq!(p.status, "flagged");
        assert_eq!(p.clip.as_deref(), Some("clips/passage.wav"));
        assert_eq!(n.line("intro").unwrap().duration, 1.25);
        assert!(n.line("nope").is_none());
    }

    #[test]
    fn rejects_unknown_status() {
        let json = built().replace("\"approved\"", "\"maybe\"");
        let err = parse(&json).unwrap_err().to_string();
        assert!(err.contains("maybe") && err.contains("intro"), "{err}");
    }

    #[test]
    fn parses_a_draft_manifest() {
        let json = r#"{"version": 1, "draft": true, "lines": [
            {"id": "a", "text": "Hi.", "kind": "line", "clip": null,
             "duration": 0.8, "status": "draft", "captions": [], "check": null}]}"#;
        let n = parse(json).unwrap();
        assert!(n.draft);
        assert_eq!(n.lines[0].clip, None);
        assert_eq!(n.lines[0].status, "draft");
    }

    #[test]
    fn rejects_newer_and_missing_versions() {
        let newer = built().replace("\"version\": 1", "\"version\": 2");
        assert!(matches!(parse(&newer), Err(NarrationError::Version { .. })));
        let err = parse(r#"{"lines": []}"#).unwrap_err();
        assert!(matches!(err, NarrationError::Version { .. }));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn rejects_bad_numbers() {
        let neg = built().replace("\"duration\": 1.25", "\"duration\": -1.0");
        let err = parse(&neg).unwrap_err().to_string();
        assert!(err.contains("duration") && err.contains("intro"), "{err}");
        let bad = built().replace(
            "\"start\": 1.0, \"end\": 2.0",
            "\"start\": 2.0, \"end\": 1.0",
        );
        let err = parse(&bad).unwrap_err().to_string();
        assert!(err.contains("caption"), "{err}");
    }

    #[test]
    fn ignores_unknown_fields() {
        // `extra_future_field` is already in the fixture; add a line-level one.
        let json = built().replace(
            "\"kind\": \"line\"",
            "\"kind\": \"line\", \"future\": {\"a\": 1}",
        );
        assert!(parse(&json).is_ok());
    }

    #[test]
    fn lead_and_breath_default() {
        let n = parse(r#"{"version": 1, "lines": []}"#).unwrap();
        assert_eq!((n.lead, n.breath), (0.15, 0.4));
        assert!(!n.draft);
    }

    #[test]
    fn load_names_the_file_on_io_error() {
        let err = Narration::load(Path::new("definitely/missing.json")).unwrap_err();
        assert!(err.to_string().contains("missing.json"));
    }
}
