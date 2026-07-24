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
        Command::Analyze { panels, session } => {
            tracing::info!(?session, n_panels = panels.len(), "analyze requested");
            anyhow::bail!("analyze: not yet implemented");
        }
    }
}
