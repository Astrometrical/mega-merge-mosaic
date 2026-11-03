//! The status bar (spec §5): what is running and its progress on the
//! left; the check counts, the canvas size with megapixels, the group
//! count and this process's memory and CPU on the right. The memory/CPU
//! readout is sampled by [`SysMonitor`] on its own thread, at most once
//! per second.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use mega_ui::jobs::JobView;
use mega_ui::status::{StatusItem, status_bar};
use mega_ui::theme::palette;

use crate::checks::CheckCounts;

/// How often [`SysMonitor`] samples the process.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// `n` with a space between each group of three digits ("18 310").
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A canvas size with megapixels: `"9 255 × 18 310 (169 MP)"`. Below
/// 10 MP one decimal is shown.
pub fn format_canvas(width: u64, height: u64) -> String {
    let px = width as u128 * height as u128;
    let mp = if px == 0 {
        "0".to_string()
    } else if px < 10_000_000 {
        format!("{:.1}", px as f64 / 1e6)
    } else {
        group_digits(((px + 500_000) / 1_000_000) as u64)
    };
    format!(
        "{} × {} ({mp} MP)",
        group_digits(width),
        group_digits(height)
    )
}

/// A byte count in binary units: `"512 B"`, `"300 MB"`, `"1.5 GB"`. Below
/// 10 units one decimal is shown.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

/// One sample of this process's resource use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SysSnapshot {
    /// Resident memory, in bytes.
    pub rss: u64,
    /// CPU use as a share of the whole machine, in percent (0–100).
    pub cpu: f32,
}

/// Samples this process's memory and CPU on a background thread, at most
/// once per second; the UI reads the latest sample without waiting. The
/// thread ends within a second of the monitor being dropped.
pub struct SysMonitor {
    latest: Arc<Mutex<Option<SysSnapshot>>>,
}

impl SysMonitor {
    /// Start sampling.
    pub fn start() -> Self {
        let latest = Arc::new(Mutex::new(None));
        let weak = Arc::downgrade(&latest);
        if let Err(e) = std::thread::Builder::new()
            .name("mmm-sysinfo".into())
            .spawn(move || sample_loop(weak))
        {
            tracing::warn!("no memory/CPU readout: cannot start its thread: {e}");
        }
        SysMonitor { latest }
    }

    /// The latest sample, if one has been taken.
    pub fn latest(&self) -> Option<SysSnapshot> {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Sample into `latest` every [`SAMPLE_INTERVAL`] until it is dropped.
fn sample_loop(latest: Weak<Mutex<Option<SysSnapshot>>>) {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let Ok(pid) = sysinfo::get_current_pid() else {
        tracing::warn!("no memory/CPU readout: unknown process id");
        return;
    };
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
    let mut sys = System::new();
    loop {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        let snap = sys.process(pid).map(|p| SysSnapshot {
            rss: p.memory(),
            cpu: (p.cpu_usage() / cpus).clamp(0.0, 100.0),
        });
        match latest.upgrade() {
            Some(slot) => *slot.lock().unwrap_or_else(|e| e.into_inner()) = snap,
            None => return,
        }
        std::thread::sleep(SAMPLE_INTERVAL);
        if latest.strong_count() == 0 {
            return;
        }
    }
}

/// The left side: the first job's label and progress, or "Ready".
pub fn job_items(jobs: &[JobView]) -> Vec<StatusItem> {
    let Some(job) = jobs.first() else {
        return vec![StatusItem {
            text: "Ready".into(),
            color: None,
        }];
    };
    let progress = if job.total > 0 {
        let pct = (job.done as f64 / job.total as f64 * 100.0).clamp(0.0, 100.0);
        format!("{} {pct:.0}%", job.stage)
    } else {
        job.stage.clone()
    };
    vec![
        StatusItem {
            text: job.label.clone(),
            color: Some(palette::TEXT_STRONG),
        },
        StatusItem {
            text: progress,
            color: None,
        },
    ]
}

/// `n` and `noun`, plural unless `n` is 1 (`"1 problem"`, `"2 notes"`).
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// The check counts, coloured: "Checks: N problems · N notes · N passed".
pub fn check_items(c: CheckCounts) -> Vec<StatusItem> {
    let item = |text: String, color| StatusItem { text, color };
    vec![
        item("Checks:".into(), None),
        item(count(c.problems, "problem"), Some(palette::HA_500)),
        item("·".into(), None),
        item(count(c.notes, "note"), Some(palette::ACCENT_400)),
        item("·".into(), None),
        item(format!("{} passed", c.passed), Some(palette::GREEN_300)),
    ]
}

/// The right side: check counts (once checked), canvas (when known), group
/// count, memory and CPU (once sampled).
pub fn right_items(
    checks: Option<CheckCounts>,
    canvas: Option<(u64, u64)>,
    groups: usize,
    sys: Option<SysSnapshot>,
) -> Vec<StatusItem> {
    let item = |text: String| StatusItem { text, color: None };
    let mut items = checks.map(check_items).unwrap_or_default();
    if let Some((w, h)) = canvas {
        items.push(item(format!("Canvas {}", format_canvas(w, h))));
    }
    items.push(item(format!(
        "{groups} group{}",
        if groups == 1 { "" } else { "s" }
    )));
    if let Some(s) = sys {
        items.push(item(format!("RAM {}", format_bytes(s.rss))));
        items.push(item(format!("CPU {:.0}%", s.cpu)));
    }
    items
}

/// Draw the status bar.
pub fn show(
    ui: &mut egui::Ui,
    jobs: &[JobView],
    checks: Option<CheckCounts>,
    canvas: Option<(u64, u64)>,
    groups: usize,
    sys: Option<SysSnapshot>,
) {
    status_bar(
        ui,
        &job_items(jobs),
        &right_items(checks, canvas, groups, sys),
    );
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use mega_ui::jobs::JobView;

    use super::*;

    #[test]
    fn megapixel_formatting() {
        assert_eq!(format_canvas(9255, 18310), "9 255 × 18 310 (169 MP)");
        assert_eq!(format_canvas(70_000, 48_000), "70 000 × 48 000 (3 360 MP)");
        assert_eq!(format_canvas(600, 420), "600 × 420 (0.3 MP)");
        assert_eq!(format_canvas(0, 0), "0 × 0 (0 MP)");
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }

    #[test]
    fn job_items_show_progress_or_ready() {
        assert_eq!(job_items(&[])[0].text, "Ready");
        let view = JobView {
            id: 1,
            label: "Blend Ha".into(),
            stage: "blend".into(),
            done: 1,
            total: 4,
            cancellable: true,
        };
        let items = job_items(std::slice::from_ref(&view));
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Blend Ha", "blend 25%"]);
        let unknown = JobView { total: 0, ..view };
        assert_eq!(job_items(&[unknown])[1].text, "blend");
    }

    #[test]
    fn right_items_list_canvas_groups_and_resources() {
        let snap = SysSnapshot {
            rss: 300 * 1024 * 1024,
            cpu: 12.4,
        };
        let counts = CheckCounts {
            problems: 2,
            notes: 1,
            passed: 5,
        };
        let items = right_items(Some(counts), Some((9255, 18310)), 2, Some(snap));
        let colors: Vec<_> = items[..6].iter().map(|i| i.color).collect();
        assert_eq!(
            colors,
            [
                None,
                Some(palette::HA_500),
                None,
                Some(palette::ACCENT_400),
                None,
                Some(palette::GREEN_300)
            ]
        );
        let texts: Vec<String> = items.into_iter().map(|i| i.text).collect();
        assert_eq!(
            texts,
            [
                "Checks:",
                "2 problems",
                "·",
                "1 note",
                "·",
                "5 passed",
                "Canvas 9 255 × 18 310 (169 MP)",
                "2 groups",
                "RAM 300 MB",
                "CPU 12%"
            ]
        );
        let texts: Vec<String> = right_items(None, None, 1, None)
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert_eq!(texts, ["1 group"]);
    }

    #[test]
    fn monitor_samples_this_process() {
        let m = SysMonitor::start();
        let deadline = Instant::now() + Duration::from_secs(10);
        let snap = loop {
            if let Some(s) = m.latest() {
                break s;
            }
            assert!(Instant::now() < deadline, "no sample");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(snap.rss > 0);
        assert!(snap.cpu >= 0.0);
    }
}
