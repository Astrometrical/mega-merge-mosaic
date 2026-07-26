## Description

<!-- What does this PR change, and why? -->

Fixes #<!-- issue number, if any -->

## Checklist

- [ ] `cargo fmt --all --check` passes locally
- [ ] `cargo clippy --all-targets` is clean (CI runs with `-D warnings`)
- [ ] `cargo test` passes locally
- [ ] Tests added/updated for new behaviour (synthetic inputs; tests never
      touch `test_data/`)
- [ ] Byte-exact regression-guard hashes **not** recaptured — or, if they were,
      the change is intended and justified per
      [docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md#testing-philosophy)
- [ ] Docs updated if this change is user-facing (README / DEVELOPMENT / DESIGN)
