//! Panel previews through the public API: placement of solved panels in a
//! derived frame, per-panel delivery and progress, cancellation and
//! per-panel failures.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mmm_core::job::{CancelToken, JobCtl};
use mmm_core::mosaic::{FRAME_FILE, Mosaic, MosaicConfig};
use mmm_core::preview::{
    Preview, build_preview, footprint, plan_previews, preview_frame_path, run_previews,
};
use mmm_core::reference::ReferenceFrame;
use mmm_core::synth::{SynthWcs, write_xisf, write_xisf_solved};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mmm-preview-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `n` aligned full-canvas one-channel panels, each covering its own
/// 40×30 block of a 240×180 canvas.
fn aligned_panels(dir: &Path, n: usize) -> Vec<PathBuf> {
    let (w, h) = (240u64, 180u64);
    (0..n)
        .map(|k| {
            let mut p = vec![0f32; (w * h) as usize];
            let (x0, y0) = ((k % 6) as u64 * 40, (k / 6) as u64 * 30);
            for y in y0..y0 + 30 {
                for x in x0..x0 + 40 {
                    p[(y * w + x) as usize] = 0.2 + k as f32 * 0.01;
                }
            }
            let path = dir.join(format!("panel-{k:02}.xisf"));
            write_xisf(&path, w, h, 1, &p).unwrap();
            path
        })
        .collect()
}

fn mosaic(dir: &Path, paths: Vec<PathBuf>) -> Mosaic {
    Mosaic::create(&dir.join("mosaic"), MosaicConfig::new(paths)).unwrap()
}

const PW: u64 = 400;
const PH: u64 = 300;
/// The star's pixel in each raw panel.
const STAR: (f64, f64) = (100.0, 80.0);

/// A raw solved panel: background 0.1 plus a Gaussian star centred on
/// pixel `STAR`, its reference point `refimg` at a fixed sky position.
fn solved_panel(path: &Path, refimg: [f64; 2]) -> SynthWcs {
    let mut p = vec![0f32; (PW * PH) as usize];
    for y in 0..PH {
        for x in 0..PW {
            let r2 = (x as f64 - STAR.0).powi(2) + (y as f64 - STAR.1).powi(2);
            p[(y * PW + x) as usize] = 0.1 + (-r2 / (2.0 * 2.0f64.powi(2))).exp() as f32;
        }
    }
    let wcs = SynthWcs {
        crval: [83.8, -5.4],
        refimg,
        cd: [[-1e-3, 0.0], [0.0, 1e-3]],
    };
    write_xisf_solved(path, PW, PH, 1, &p, &wcs).unwrap();
    wcs
}

fn argmax(p: &Preview) -> (u32, u32) {
    let k = p
        .data
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap()
        .0 as u32;
    (k % p.width, k / p.width)
}

/// Where a raw panel pixel lands in the frame, in canvas pixels.
fn to_canvas(panel: &Path, frame: &ReferenceFrame, px: f64, py: f64) -> (f64, f64) {
    let ReferenceFrame::Solved { frame } = frame else {
        panic!("expected a solved frame, got {frame:?}");
    };
    let m = mmm_core::formats::InputPanel::open(panel)
        .unwrap()
        .wcs_model()
        .unwrap();
    // Pixel centre in PixInsight image coordinates.
    let (ra, dec) = m.pixel_to_sky(px + 0.5, py + 0.5);
    frame.linear_wcs().sky_to_pixel(ra, dec)
}

#[test]
fn footprint_matches_preview_rect() {
    let t = TempDir::new("footprint");
    let a = t.0.join("a.xisf");
    let b = t.0.join("b.xisf");
    solved_panel(&a, [200.0, 150.0]);
    solved_panel(&b, [140.0, 110.0]);
    let m = mosaic(&t.0, vec![a.clone(), b.clone()]);
    let plan = plan_previews(&m);
    let got = Mutex::new(Vec::<Preview>::new());
    let errs = run_previews(&plan, &JobCtl::none(), &|p| got.lock().unwrap().push(p))
        .unwrap()
        .failed;
    assert!(errs.is_empty(), "{errs:?}");
    let frame = ReferenceFrame::load(&preview_frame_path(&m.dir)).unwrap();
    for p in got.into_inner().unwrap() {
        let f = footprint(&p.path, &frame).unwrap().expect("solved frame");
        let block = (p.rect[2] - p.rect[0]) / f64::from(p.width);
        for k in 0..4 {
            assert!(
                (f[k] - p.rect[k]).abs() <= block,
                "{:?}: footprint {f:?} vs preview {:?}",
                p.path,
                p.rect
            );
        }
    }
    // An aligned frame needs a pixel scan: no header-only footprint.
    let aligned = ReferenceFrame::Aligned {
        width: 10,
        height: 10,
        wcs: None,
    };
    assert_eq!(footprint(&a, &aligned).unwrap(), None);
}

#[test]
fn solved_preview_lands_in_frame() {
    let t = TempDir::new("solved");
    let a = t.0.join("a.xisf");
    let b = t.0.join("b.xisf");
    solved_panel(&a, [200.0, 150.0]);
    // B's pixel p shows the sky of A's pixel p + (60, 40).
    solved_panel(&b, [140.0, 110.0]);
    let m = mosaic(&t.0, vec![a.clone(), b.clone()]);
    let plan = plan_previews(&m);
    let got = Mutex::new(Vec::<Preview>::new());
    let errs = run_previews(&plan, &JobCtl::none(), &|p| got.lock().unwrap().push(p))
        .unwrap()
        .failed;
    assert!(errs.is_empty(), "{errs:?}");
    // The frame was derived header-only and saved as the previews' frame;
    // the analysed frame is analysis's alone.
    let frame = ReferenceFrame::load(&preview_frame_path(&m.dir)).unwrap();
    assert!(!m.dir.join(FRAME_FILE).exists());
    assert_eq!(frame.kind_name(), "solved");
    let mut got = got.into_inner().unwrap();
    got.sort_by(|x, y| x.path.cmp(&y.path));
    let (pa, pb) = (&got[0], &got[1]);
    assert_eq!((&pa.path, &pb.path), (&a, &b));
    for k in 0..4 {
        let want = if k % 2 == 0 { 60.0 } else { 40.0 };
        let d = pb.rect[k] - pa.rect[k];
        assert!((d - want).abs() <= 1.0, "rect[{k}] offset {d}, want {want}");
    }
    // The rect spans the panel footprint (~400×300 canvas px at one scale).
    assert!(
        (pa.rect[2] - pa.rect[0] - PW as f64).abs() <= 2.0,
        "{:?}",
        pa.rect
    );
    assert!(
        (pa.rect[3] - pa.rect[1] - PH as f64).abs() <= 2.0,
        "{:?}",
        pa.rect
    );
    // Each star lands in the preview pixel holding its canvas position.
    for p in [pa, pb] {
        let (cx, cy) = to_canvas(&p.path, &frame, STAR.0, STAR.1);
        let sx = (p.rect[2] - p.rect[0]) / p.width as f64;
        let sy = (p.rect[3] - p.rect[1]) / p.height as f64;
        let want = (
            ((cx - p.rect[0]) / sx).floor() as u32,
            ((cy - p.rect[1]) / sy).floor() as u32,
        );
        assert_eq!(argmax(p), want, "{:?}", p.path);
        // Linear background preserved; the centre is covered.
        let mid = (p.height / 2 * p.width + p.width / 2) as usize;
        assert!((p.data[mid] - 0.1).abs() < 1e-3, "{}", p.data[mid]);
    }
    // B's star sits 60, 40 canvas px right of / below A's.
    let (ax, ay) = to_canvas(&a, &frame, STAR.0, STAR.1);
    let (bx, by) = to_canvas(&b, &frame, STAR.0, STAR.1);
    assert!((bx - ax - 60.0).abs() < 0.5 && (by - ay - 40.0).abs() < 0.5);

    // Downsampled: the star still lands in (or next to) its preview pixel,
    // and the warp keeps the same rect origin.
    let small = build_preview(&a, &frame, 64, &JobCtl::none()).unwrap();
    assert!(small.width.max(small.height) <= 64);
    assert_eq!(small.rect[0], pa.rect[0]);
    assert_eq!(small.rect[1], pa.rect[1]);
    let (cx, cy) = to_canvas(&a, &frame, STAR.0, STAR.1);
    let s = (small.rect[2] - small.rect[0]) / small.width as f64;
    let want = ((cx - small.rect[0]) / s, (cy - small.rect[1]) / s);
    let (mx, my) = argmax(&small);
    assert!(
        (mx as f64 + 0.5 - want.0).abs() <= 1.0 && (my as f64 + 0.5 - want.1).abs() <= 1.0,
        "argmax {:?} vs {want:?}",
        (mx, my)
    );
    let mid = (small.height / 2 * small.width + small.width / 2) as usize;
    assert!((small.data[mid] - 0.1).abs() < 1e-3, "{}", small.data[mid]);
}

#[test]
fn previews_report_each() {
    let t = TempDir::new("each");
    let paths = aligned_panels(&t.0, 9);
    let m = mosaic(&t.0, paths.clone());
    let plan = plan_previews(&m);
    let ready = Mutex::new(Vec::<PathBuf>::new());
    let progress = Mutex::new(Vec::<(String, u64, u64)>::new());
    let obs = |s: &str, d: u64, n: u64| progress.lock().unwrap().push((s.to_string(), d, n));
    let ctl = JobCtl::new(CancelToken::new(), Some(&obs));
    let errs = run_previews(&plan, &ctl, &|p| {
        assert!(p.width > 0 && p.height > 0);
        ready.lock().unwrap().push(p.path);
    })
    .unwrap()
    .failed;
    assert!(errs.is_empty(), "{errs:?}");
    let mut ready = ready.into_inner().unwrap();
    ready.sort();
    assert_eq!(ready, paths);
    let progress = progress.into_inner().unwrap();
    assert!(progress.iter().all(|(s, _, n)| s == "previews" && *n == 9));
    let mut done: Vec<u64> = progress.iter().map(|p| p.1).filter(|d| *d > 0).collect();
    done.sort();
    assert_eq!(done, (1..=9).collect::<Vec<_>>());
    assert_eq!(progress.last().unwrap().1, 9);
}

#[test]
fn preview_cancels() {
    let t = TempDir::new("cancel");
    let paths = aligned_panels(&t.0, 12);
    let m = mosaic(&t.0, paths.clone());
    let plan = plan_previews(&m);

    // Cancelled before it starts: nothing is delivered.
    let tok = CancelToken::new();
    tok.cancel();
    let n = Mutex::new(0usize);
    let e = run_previews(&plan, &JobCtl::new(tok.clone(), None), &|_| {
        *n.lock().unwrap() += 1
    })
    .unwrap_err();
    assert!(e.is_cancelled());
    assert_eq!(*n.lock().unwrap(), 0);
    let frame = ReferenceFrame::Aligned {
        width: 240,
        height: 180,
        wcs: None,
    };
    assert!(
        build_preview(&paths[0], &frame, 64, &JobCtl::new(tok, None))
            .unwrap_err()
            .is_cancelled()
    );

    // Cancelled by the first delivery: the run stops early with Cancelled.
    let tok = CancelToken::new();
    let n = Mutex::new(0usize);
    let e = run_previews(&plan, &JobCtl::new(tok.clone(), None), &|_| {
        *n.lock().unwrap() += 1;
        tok.cancel();
    })
    .unwrap_err();
    assert!(e.is_cancelled());
    let n = *n.lock().unwrap();
    assert!((1..12).contains(&n), "{n} previews delivered after cancel");
}

#[test]
fn bad_panel_does_not_fail_run() {
    let t = TempDir::new("bad");
    let mut paths = aligned_panels(&t.0, 3);
    let bad = t.0.join("broken.xisf");
    std::fs::write(&bad, b"definitely not an image").unwrap();
    paths.insert(1, bad.clone());
    let missing = t.0.join("missing.xisf");
    paths.push(missing.clone());
    let m = mosaic(&t.0, paths.clone());
    let plan = plan_previews(&m);
    let ready = Mutex::new(Vec::<PathBuf>::new());
    let progress = Mutex::new(Vec::<u64>::new());
    let obs = |_: &str, d: u64, _: u64| progress.lock().unwrap().push(d);
    let ctl = JobCtl::new(CancelToken::new(), Some(&obs));
    let errs = run_previews(&plan, &ctl, &|p| ready.lock().unwrap().push(p.path))
        .unwrap()
        .failed;
    assert_eq!(
        errs.iter().map(|e| &e.0).collect::<Vec<_>>(),
        vec![&bad, &missing]
    );
    assert!(errs[0].1.contains("not an XISF or FITS file"), "{errs:?}");
    let mut ready = ready.into_inner().unwrap();
    ready.sort();
    let good: Vec<PathBuf> = paths
        .iter()
        .filter(|p| **p != bad && **p != missing)
        .cloned()
        .collect();
    assert_eq!(ready, good);
    // Every panel, failed or not, counts toward progress.
    assert_eq!(progress.into_inner().unwrap().iter().max(), Some(&5));
    // The frame derived without the unreadable panels is not saved.
    assert!(!preview_frame_path(&m.dir).exists());

    // With a frame in place, a corrupt panel still fails alone.
    ReferenceFrame::Aligned {
        width: 240,
        height: 180,
        wcs: None,
    }
    .save(&preview_frame_path(&m.dir))
    .unwrap();
    let ready = Mutex::new(0usize);
    let errs = run_previews(&plan, &JobCtl::none(), &|_| *ready.lock().unwrap() += 1)
        .unwrap()
        .failed;
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert_eq!(*ready.lock().unwrap(), 3);
}

#[test]
fn unsolved_panel_is_reported_not_fatal() {
    let t = TempDir::new("unsolved");
    let a = t.0.join("a.xisf");
    let b = t.0.join("b.xisf");
    solved_panel(&a, [200.0, 150.0]);
    solved_panel(&b, [140.0, 110.0]);
    // A valid image without an astrometric solution, of another geometry.
    let u = t.0.join("unsolved.xisf");
    write_xisf(&u, 120, 90, 1, &vec![0.4; 120 * 90]).unwrap();
    let m = mosaic(&t.0, vec![a.clone(), u.clone(), b.clone()]);
    let ready = Mutex::new(Vec::<PathBuf>::new());
    let errs = run_previews(&plan_previews(&m), &JobCtl::none(), &|p| {
        ready.lock().unwrap().push(p.path)
    })
    .unwrap()
    .failed;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].0, u);
    assert!(errs[0].1.contains("astrometric solution"), "{errs:?}");
    let mut ready = ready.into_inner().unwrap();
    ready.sort();
    assert_eq!(ready, vec![a, b]);
    // A frame derived from a subset is not saved.
    assert!(!preview_frame_path(&m.dir).exists());
}

#[test]
fn frame_grows_with_inputs() {
    let t = TempDir::new("grow");
    let a = t.0.join("a.xisf");
    let b = t.0.join("b.xisf");
    solved_panel(&a, [200.0, 150.0]);
    solved_panel(&b, [140.0, 110.0]);
    let mut m = mosaic(&t.0, vec![a.clone(), b.clone()]);
    let run = |m: &Mosaic| {
        let got = Mutex::new(Vec::<Preview>::new());
        let errs = run_previews(&plan_previews(m), &JobCtl::none(), &|p| {
            got.lock().unwrap().push(p)
        })
        .unwrap()
        .failed;
        assert!(errs.is_empty(), "{errs:?}");
        got.into_inner().unwrap()
    };
    run(&m);
    let old = ReferenceFrame::load(&preview_frame_path(&m.dir)).unwrap();
    // The frame of the last analysis (as analysis saves it): previews
    // never rewrite it, whatever their own frame becomes.
    old.save(&m.dir.join(FRAME_FILE)).unwrap();
    let analysed = std::fs::read(m.dir.join(FRAME_FILE)).unwrap();
    // C lies 700 px right of A: outside the old canvas.
    let c = t.0.join("c.xisf");
    solved_panel(&c, [200.0 - 700.0, 150.0]);
    let panels = MosaicConfig::new(vec![a, b, c.clone()]).panels;
    let grouping = m.config.grouping.clone();
    mmm_core::mosaic::set_inputs(&mut m, panels, grouping).unwrap();
    let got = run(&m);
    assert_eq!(
        std::fs::read(m.dir.join(FRAME_FILE)).unwrap(),
        analysed,
        "a preview run rewrote the analysed frame"
    );
    let new = ReferenceFrame::load(&preview_frame_path(&m.dir)).unwrap();
    assert_ne!(new, old);
    let (ow, _) = old.canvas();
    let (nw, nh) = new.canvas();
    assert!(nw >= ow + 600, "frame {ow} -> {nw} px wide");
    let pc = got.iter().find(|p| p.path == c).expect("C previewed");
    assert!(
        pc.rect[0] >= 0.0
            && pc.rect[1] >= 0.0
            && pc.rect[2] <= nw as f64 + 1.0
            && pc.rect[3] <= nh as f64 + 1.0,
        "{:?} in {nw}x{nh}",
        pc.rect
    );
    assert_eq!(got.len(), 3);
}
