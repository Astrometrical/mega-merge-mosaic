use clap::{Parser, Subcommand};

/// Mega Merge Mosaic — fast merging/blending of pre-aligned astro mosaic panels.
#[derive(Parser)]
#[command(name = "mmm", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase log verbosity (-v, -vv)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Command {
    /// Print header metadata for panel files (and optionally quick pixel stats)
    Info {
        /// Input panel files (FITS/XISF)
        #[arg(required = true)]
        panels: Vec<std::path::PathBuf>,

        /// Also scan pixel data for min/max/zero-fraction (reads whole file)
        #[arg(long)]
        stats: bool,
    },

    /// Analyze panels: build tiled cache, coverage masks, and the overlap graph
    Analyze {
        /// Input panel files (FITS/XISF), all pre-aligned on a common canvas
        #[arg(required = true)]
        panels: Vec<std::path::PathBuf>,

        /// Session directory for cached analysis (created if missing)
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,
    },

    /// Report analysis results: the overlap-graph edge table
    Report {
        /// Session directory produced by `mmm analyze`
        #[arg(short, long, default_value = "mosaic.mmm-session")]
        session: std::path::PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| level.into()),
        )
        .init();

    match cli.command {
        Command::Info { panels, stats } => {
            for path in &panels {
                info_panel(path, stats)?;
            }
            Ok(())
        }
        Command::Analyze { panels, session } => {
            tracing::info!(?session, n_panels = panels.len(), "analyze requested");
            let t0 = std::time::Instant::now();
            let s = mmm_core::analyze::analyze(&panels, &session)?;
            let (w, h, ch) = s.canvas;
            println!("canvas: {w}x{h} x{ch}ch   session: {}", s.dir.display());
            println!(
                "{:>3}  {:<28} {:>26} {:>8}  per-channel mean",
                "id", "file", "bbox [x0,x1)x[y0,y1)", "nonzero"
            );
            for p in &s.panels {
                let name = p
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.path.display().to_string());
                let bbox = format!(
                    "[{},{})x[{},{})",
                    p.bbox[0], p.bbox[2], p.bbox[1], p.bbox[3]
                );
                let means = p
                    .ch_mean
                    .iter()
                    .map(|m| format!("{m:.6}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "{:>3}  {:<28} {:>26} {:>7.1}%  {}",
                    p.id,
                    name,
                    bbox,
                    100.0 * p.nonzero_frac,
                    means
                );
            }
            println!("analyze: {:.2}s", t0.elapsed().as_secs_f64());
            Ok(())
        }
        Command::Report { session } => report(&session),
    }
}

fn report(session_dir: &std::path::Path) -> anyhow::Result<()> {
    use mmm_core::overlap::OverlapGraph;
    use mmm_core::session::Session;

    let session = Session::open(session_dir)?;
    let graph = OverlapGraph::load(&session.overlap_graph_path())?;
    let (w, h, ch) = session.canvas;
    println!(
        "canvas: {w}x{h} x{ch}ch   panels: {}   session: {}",
        session.panels.len(),
        session.dir.display()
    );

    let name = |id: usize| -> String {
        session
            .panels
            .get(id)
            .and_then(|p| p.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("panel {id}"))
    };
    println!("\noverlap edges ({}):", graph.edges.len());
    println!(
        "{:>3}-{:<3} {:>8} {:>12}  {:<24} files",
        "a", "b", "cells", "~px", "bbox8 [x0,x1)x[y0,y1)"
    );
    for e in &graph.edges {
        let bbox = format!(
            "[{},{})x[{},{})",
            e.bbox8[0], e.bbox8[2], e.bbox8[1], e.bbox8[3]
        );
        println!(
            "{:>3}-{:<3} {:>8} {:>12}  {:<24} {} | {}",
            e.a,
            e.b,
            e.n_cells,
            e.n_cells * 64,
            bbox,
            name(e.a),
            name(e.b)
        );
    }

    let comps = graph.components(session.panels.len());
    println!("\nconnected components: {}", comps.len());
    for (i, comp) in comps.iter().enumerate() {
        let ids = comp.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(" ");
        println!("  #{i}: {ids}");
    }

    match mmm_core::photometry::Photometry::load(&session.photometry_path()) {
        Ok(phot) => report_photometry(&phot, &name),
        Err(_) => println!("\nno photometry results (re-run `mmm analyze`)"),
    }
    Ok(())
}

fn report_photometry(phot: &mmm_core::photometry::Photometry, name: &dyn Fn(usize) -> String) {
    let channels = phot.gains.len();

    // Per-channel median rms for the suspect-edge flag.
    let median_rms: Vec<f64> = (0..channels)
        .map(|c| {
            let mut r: Vec<f64> = phot
                .edge_fits
                .iter()
                .filter(|f| f.channel as usize == c)
                .map(|f| f.rms)
                .collect();
            r.sort_by(f64::total_cmp);
            if r.is_empty() {
                0.0
            } else if r.len() % 2 == 1 {
                r[r.len() / 2]
            } else {
                0.5 * (r[r.len() / 2 - 1] + r[r.len() / 2])
            }
        })
        .collect();

    println!("\nphotometric edge fits (I_b ≈ gain·I_a + offset; ⚠ = rms > 3× channel median):");
    println!(
        "{:>3}-{:<3} {:>2} {:>9} {:>10} {:>10} {:>7}",
        "a", "b", "ch", "gain", "offset", "rms", "n"
    );
    for f in &phot.edge_fits {
        let med = median_rms.get(f.channel as usize).copied().unwrap_or(0.0);
        let flag = if med > 0.0 && f.rms > 3.0 * med { "  ⚠ suspect" } else { "" };
        println!(
            "{:>3}-{:<3} {:>2} {:>9.4} {:>10.6} {:>10.3e} {:>7}{}",
            f.a, f.b, f.channel, f.gain, f.offset, f.rms, f.n, flag
        );
    }

    let n_panels = phot.gains.first().map(|g| g.len()).unwrap_or(0);
    println!("\nper-panel corrections (I' = g·I + o), per channel:");
    let mut header = format!("{:>3} ", "id");
    for c in 0..channels {
        header.push_str(&format!(" {:>8}{c} {:>10}{c}", "g", "o"));
    }
    println!("{header}  file");
    for p in 0..n_panels {
        let mut row = format!("{p:>3} ");
        for c in 0..channels {
            row.push_str(&format!(" {:>9.4} {:>+11.6}", phot.gains[c][p], phot.offsets[c][p]));
        }
        println!("{row}  {}", name(p));
    }
}

fn info_panel(path: &std::path::Path, stats: bool) -> anyhow::Result<()> {
    use mmm_core::formats::xisf::XisfPanel;

    let panel = XisfPanel::open(path)?;
    let h = panel.header();
    println!("{}", path.display());
    println!(
        "  geometry: {}x{} x{}ch  {:?}  data @ {} ({} bytes)",
        h.width, h.height, h.channels, h.sample_format, h.data_offset, h.data_size
    );
    for kw in &h.fits_keywords {
        if matches!(kw.name.as_str(), "OBJECT" | "RA" | "DEC" | "INSTRUME" | "BAYERPAT" | "EXPTIME") {
            println!("  {:8} = {}", kw.name, kw.value);
        }
    }

    if stats {
        panel.advise_sequential();
        let t0 = std::time::Instant::now();
        for c in 0..panel.channels() {
            let plane = panel.channel(c);
            let (mut min, mut max, mut zeros, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0u64, 0f64);
            for &v in plane {
                if v == 0.0 {
                    zeros += 1;
                } else {
                    min = min.min(v);
                    max = max.max(v);
                    sum += v as f64;
                }
            }
            let n = plane.len() as u64;
            let nonzero = n - zeros;
            println!(
                "  ch{c}: nonzero {:.1}%  min {:.6}  max {:.6}  mean {:.6}",
                100.0 * nonzero as f64 / n as f64,
                min,
                max,
                if nonzero > 0 { sum / nonzero as f64 } else { 0.0 }
            );
        }
        println!("  stats scan: {:.2}s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}
