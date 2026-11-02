//! Build an analysed and blended two-group (Ha + OIII) synthetic session for
//! the app's self-test:
//!
//! ```sh
//! cargo run -p mmm-app --example make_selftest_session -- DIR
//! cargo run -p mmm-app -- --selftest DIR/m.mmm-session
//! ```
//!
//! `DIR` must not hold a session yet. Prints the session folder.

#[path = "../src/synth_mosaic.rs"]
mod synth_mosaic;

use std::path::PathBuf;
use std::process::ExitCode;

use mmm_core::job::JobCtl;
use mmm_core::mosaic::{analyze_all, blend_group};

fn main() -> ExitCode {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: make_selftest_session DIR");
        return ExitCode::from(2);
    };
    if root.join("m.mmm-session").exists() {
        eprintln!("{} already holds a session", root.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!("cannot create {}: {e}", root.display());
        return ExitCode::FAILURE;
    }
    let mut m = synth_mosaic::two_group_mosaic(&root);
    let ctl = JobCtl::none();
    let groups = match analyze_all(&mut m, &ctl) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("analyze failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    for (group, _) in groups.processing_groups() {
        if let Err(e) = blend_group(&mut m, &group, None, &ctl) {
            eprintln!("blend {group} failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("{}", m.dir.display());
    ExitCode::SUCCESS
}
