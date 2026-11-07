//! The hero's views and the cards' thumbnails, built on background
//! threads (spec §5.4): a hero build starts as soon as the target
//! session's facts are in, and one for a session no longer the target is
//! dropped; thumbnails load only once the hero has settled, a batch at a
//! time, for the cards on screen. Only available sessions are read.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use egui::{ColorImage, TextureHandle, TextureOptions};
use mega_ui::hero::HeroView;
use mega_ui::viewer::TileSource;

use super::facts::{Availability, CardFacts};
use super::hero_views::{HeroData, build_hero, thumbnail};
use super::{StartScreen, UNREADABLE};

/// Seconds after the screen starts before card thumbnails load even if the
/// hero hasn't settled: a hero that never loads (its tiles never come, or
/// its session's folder never answers) must not hold them back for ever.
const THUMBS_WAIT_MAX_S: f64 = 2.0;

/// Most thumbnails one background batch builds.
const THUMB_BATCH: usize = 16;

/// A hero being built for `path` on a background thread.
pub(super) struct HeroBuild {
    path: PathBuf,
    rx: Receiver<HeroData>,
}

/// The session the hero stage shows.
pub(super) struct Shown {
    path: PathBuf,
    /// Whether it got views (else the stage shows a line saying why).
    has_views: bool,
    /// The view its card thumbnail comes from: the one shown first.
    thumb_source: Option<Arc<dyn TileSource>>,
}

/// A card's thumbnail.
pub(super) enum Thumb {
    /// Being built.
    Queued,
    /// Built (`None`: the session has nothing to show).
    Ready(Option<TextureHandle>),
}

/// What a thumbnail is made from.
enum ThumbJob {
    /// The hero's own first view, already open.
    Source(Arc<dyn TileSource>),
    /// The session's hero, built for its first view.
    Build(Box<CardFacts>),
}

/// Thumbnails from the background thread: a session and its picture.
pub(super) type ThumbResults = Receiver<(PathBuf, Option<ColorImage>)>;

impl StartScreen {
    /// Let go of the hero's views (and their open stores) and of a build
    /// under way; the hero is built again for the target on a later frame.
    pub(super) fn drop_hero(&mut self) {
        self.shown = None;
        self.hero_build = None;
        self.clear_stage(0.0);
    }

    /// New facts arrived for `path`: a hero shown or being built from its
    /// older facts (e.g. from before it was opened elsewhere) is dropped.
    pub(super) fn hero_outdated(&mut self, path: &Path) {
        let building = self.hero_build.as_ref().is_some_and(|b| b.path == path);
        let shown = self.shown.as_ref().is_some_and(|s| s.path == path);
        if building || shown {
            self.drop_hero();
        }
    }

    /// Stop building thumbnails; those not built yet load again later.
    pub(super) fn stop_thumbnails(&mut self) {
        self.thumb_rx = None;
        self.thumbs.retain(|_, t| matches!(t, Thumb::Ready(_)));
    }

    /// Whether the hero is done loading: it has drawn an image, or has
    /// nothing to draw (for the self-test).
    pub fn hero_loaded(&self) -> bool {
        self.stage.first_image_drawn() || self.shown.as_ref().is_some_and(|s| !s.has_views)
    }

    /// The label of the hero's view on screen, once it has drawn an image
    /// (for the self-test).
    pub fn hero_image(&self) -> Option<&str> {
        self.stage
            .first_image_drawn()
            .then(|| self.stage.current_label())
            .flatten()
    }

    /// Empty the stage: no views, no line.
    fn clear_stage(&mut self, now: f64) {
        self.stage.set_views(Vec::new(), 0, now);
        self.stage.set_message(None);
    }

    /// Take a finished hero build; start one for `target` once its facts
    /// are in, when the stage shows something else. A build for a session
    /// no longer the target is dropped.
    pub(super) fn update_hero(&mut self, ctx: &egui::Context, target: Option<&Path>, now: f64) {
        if let Some(build) = &self.hero_build {
            let got = match build.rx.try_recv() {
                Ok(data) => Some(data),
                Err(TryRecvError::Disconnected) => Some(HeroData {
                    views: Vec::new(),
                    start: 0,
                    message: Some(UNREADABLE.into()),
                }),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(data) = got {
                let path = build.path.clone();
                self.hero_build = None;
                if target == Some(path.as_path()) {
                    self.apply_hero(path, data, now);
                }
            }
        }
        if self.shown.as_ref().map(|s| s.path.as_path()) == target {
            return;
        }
        if self.shown.take().is_some() {
            self.clear_stage(now);
        }
        let Some(target) = target else { return };
        if self.hero_build.as_ref().is_some_and(|b| b.path == target) {
            return;
        }
        let Some(facts) = self.facts.get(target).cloned() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(build_hero(&facts));
            ctx.request_repaint();
        });
        self.hero_build = Some(HeroBuild {
            path: target.to_path_buf(),
            rx,
        });
    }

    fn apply_hero(&mut self, path: PathBuf, data: HeroData, now: f64) {
        let thumb_source = data.views.get(data.start).map(|v| v.source.clone());
        let has_views = !data.views.is_empty();
        self.stage.set_views(data.views, data.start, now);
        self.stage.set_message(data.message);
        self.shown = Some(Shown {
            path,
            has_views,
            thumb_source,
        });
    }

    /// Seconds until thumbnails may load: zero once the hero for `target`
    /// has drawn its first image or has nothing to draw, or once
    /// [`THUMBS_WAIT_MAX_S`] have passed since the screen started.
    fn thumbs_wait(&self, target: Option<&Path>, now: f64) -> f64 {
        let settled = match target {
            None => true,
            Some(target) => self.shown.as_ref().is_some_and(|s| {
                s.path == target && (!s.has_views || self.stage.first_image_drawn())
            }),
        };
        if settled {
            return 0.0;
        }
        let since_start = self.started_at.map_or(0.0, |t| now - t);
        (THUMBS_WAIT_MAX_S - since_start).max(0.0)
    }

    /// Start building thumbnails for the cards on screen that lack one,
    /// once the hero has settled (or waited long enough: a frame is
    /// scheduled for then); one batch at a time.
    pub(super) fn request_thumbnails(
        &mut self,
        ctx: &egui::Context,
        wanted: &[PathBuf],
        target: Option<&Path>,
        now: f64,
    ) {
        if self.thumb_rx.is_some() {
            return;
        }
        let needed = wanted.iter().any(|p| {
            !self.thumbs.contains_key(p)
                && self
                    .facts
                    .get(p)
                    .is_some_and(|f| f.availability == Availability::Available)
        });
        let wait = self.thumbs_wait(target, now);
        if !needed || wait > 0.0 {
            if needed {
                ctx.request_repaint_after(Duration::from_secs_f64(wait));
            }
            return;
        }
        let mut jobs = Vec::new();
        for path in wanted {
            if jobs.len() >= THUMB_BATCH || self.thumbs.contains_key(path) {
                continue;
            }
            // Only available sessions are read; busy ones keep what they had.
            let Some(f) = self.facts.get(path) else {
                continue;
            };
            if f.availability != Availability::Available {
                continue;
            }
            let job = match &self.shown {
                Some(s) if s.path == *path => match &s.thumb_source {
                    Some(source) => ThumbJob::Source(source.clone()),
                    None => {
                        self.thumbs.insert(path.clone(), Thumb::Ready(None));
                        continue;
                    }
                },
                _ => ThumbJob::Build(Box::new(f.clone())),
            };
            jobs.push((path.clone(), job));
        }
        if jobs.is_empty() {
            return;
        }
        for (path, _) in &jobs {
            self.thumbs.insert(path.clone(), Thumb::Queued);
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (path, job) in jobs {
                let image = match job {
                    ThumbJob::Source(source) => thumbnail(&HeroView {
                        label: String::new(),
                        source,
                    }),
                    ThumbJob::Build(facts) => {
                        let hero = build_hero(&facts);
                        hero.views.get(hero.start).and_then(thumbnail)
                    }
                };
                if tx.send((path, image)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.thumb_rx = Some(rx);
    }

    pub(super) fn poll_thumbnails(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.thumb_rx else { return };
        let mut got = Vec::new();
        let done = loop {
            match rx.try_recv() {
                Ok(t) => got.push(t),
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };
        if done {
            self.thumb_rx = None;
        }
        for (path, image) in got {
            if !self.library.entries().iter().any(|e| e.path == path) {
                continue;
            }
            let texture = image.map(|img| {
                let name = format!("mmm-start-thumb:{}", path.display());
                ctx.load_texture(name, img, TextureOptions::LINEAR)
            });
            self.thumbs.insert(path, Thumb::Ready(texture));
        }
    }
}
