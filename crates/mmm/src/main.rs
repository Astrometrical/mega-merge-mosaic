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
