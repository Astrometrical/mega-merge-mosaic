//! Mega Merge Mosaic: the desktop app.
//!
//! `mega-merge-mosaic [SESSION_DIR]` opens the start screen, or the session
//! directly. `--selftest [SESSION_DIR]` renders a few frames (the start
//! screen, then the session if given), logs the GL renderer and closes
//! itself: exit 0 on success, 1 on failure. It is the only way automated
//! checks launch the GUI.

mod app;
mod session;
mod start;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

const USAGE: &str = "usage: mega-merge-mosaic [--selftest] [SESSION_DIR]";

/// Command-line arguments.
#[derive(Debug, Default, PartialEq)]
struct Args {
    session: Option<PathBuf>,
    selftest: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    for arg in args {
        match arg.as_str() {
            "--selftest" => parsed.selftest = true,
            "-h" | "--help" => return Err(USAGE.into()),
            a if a.starts_with('-') => return Err(format!("unknown option {a}\n{USAGE}")),
            _ if parsed.session.is_some() => {
                return Err(format!("one session per window\n{USAGE}"));
            }
            _ => parsed.session = Some(PathBuf::from(arg)),
        }
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,mega_merge_mosaic=info,mmm_core=info".into()),
        )
        .init();

    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Mega Merge Mosaic")
        .with_app_id("mega-merge-mosaic")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 480.0]);
    match eframe::icon_data::from_png_bytes(start::LOGO_PNG) {
        Ok(icon) => viewport = viewport.with_icon(Arc::new(icon)),
        Err(e) => tracing::warn!("window icon: {e}"),
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        persist_window: !args.selftest,
        ..Default::default()
    };

    let outcome: Option<app::SelfTestOutcome> = args.selftest.then(|| Arc::new(Mutex::new(None)));
    let app_outcome = outcome.clone();
    let session = args.session;
    let run = eframe::run_native(
        "Mega Merge Mosaic",
        options,
        Box::new(move |cc| Ok(Box::new(app::MmmApp::new(cc, session, app_outcome)))),
    );
    if let Err(e) = run {
        tracing::error!("cannot start the window: {e}");
        return ExitCode::FAILURE;
    }
    match outcome.map(|o| o.lock().unwrap_or_else(|e| e.into_inner()).take()) {
        None | Some(Some(Ok(()))) => ExitCode::SUCCESS,
        Some(Some(Err(_))) => ExitCode::FAILURE,
        Some(None) => {
            tracing::error!("self-test: the window closed before the test finished");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn args_parse() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
        let a = parse(&["--selftest", "x.mmm-session"]).unwrap();
        assert!(a.selftest);
        assert_eq!(a.session, Some(PathBuf::from("x.mmm-session")));
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["a", "b"]).is_err());
    }
}
