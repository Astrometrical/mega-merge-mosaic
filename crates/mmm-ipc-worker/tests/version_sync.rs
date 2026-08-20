//! Tripwire keeping the three release-version sources in exact sync. The
//! module ⇄ worker version handshake compares these strings for equality
//! (PROTOCOL.md §10), so a bump that misses one of them would make every
//! packaged run refuse to start:
//!
//! * the workspace `Cargo.toml` version (this binary's `CARGO_PKG_VERSION`),
//! * the PixInsight module's `MMM_VERSION_STRING`
//!   (`integration/pixinsight/module/MmmVersion.h`),
//! * the C++ host library's `kExpectedWorkerVersion`
//!   (`integration/pixinsight/host/mmm_protocol.h`), which the host injects
//!   into every Init and probe request.
//!
//! The module's user documentation states the release version too; it is not
//! part of the handshake, but a stale number shipped to users is still a bug,
//! so it is checked here as well.

use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The value of the first `#define NAME "..."` / `constexpr ... NAME = "...";`
/// style quoted string on the line defining `name` in `text`.
fn quoted_value_on_line(text: &str, name: &str, from: &str) -> String {
    let line = text
        .lines()
        .find(|l| l.contains(name) && l.contains('"'))
        .unwrap_or_else(|| panic!("no quoted {name} definition found in {from}"));
    let mut parts = line.split('"');
    parts.next();
    parts
        .next()
        .unwrap_or_else(|| panic!("malformed {name} line in {from}: {line}"))
        .to_string()
}

#[test]
fn module_version_matches_worker_version() {
    let header = repo_file("integration/pixinsight/module/MmmVersion.h");
    let module = quoted_value_on_line(&header, "MMM_VERSION_STRING", "MmmVersion.h");
    assert_eq!(
        module,
        env!("CARGO_PKG_VERSION"),
        "MmmVersion.h MMM_VERSION_STRING and the workspace Cargo.toml version \
         must be bumped together (the version handshake compares them)"
    );
}

#[test]
fn module_documentation_version_matches_worker_version() {
    let html = repo_file("integration/pixinsight/doc/tools/MegaMergeMosaic/MegaMergeMosaic.html");
    let needle = format!("Version {} &mdash;", env!("CARGO_PKG_VERSION"));
    assert!(
        html.contains(&needle),
        "MegaMergeMosaic.html must state the current release version \
         (expected the subtitle line to contain {needle:?}); bump the doc's \
         version line together with Cargo.toml"
    );
}

#[test]
fn host_expected_worker_version_matches_worker_version() {
    let header = repo_file("integration/pixinsight/host/mmm_protocol.h");
    let expected = quoted_value_on_line(&header, "kExpectedWorkerVersion", "mmm_protocol.h");
    assert_eq!(
        expected,
        env!("CARGO_PKG_VERSION"),
        "mmm_protocol.h kExpectedWorkerVersion and the workspace Cargo.toml \
         version must be bumped together (the host injects it into every \
         Init/probe request and the worker requires equality)"
    );
}
