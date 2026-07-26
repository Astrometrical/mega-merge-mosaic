# Contributing to Mega Merge Mosaic

Thanks for your interest in `mmm`. This is a community-first project: the goal
is simply the best tool for the job. It is brand-neutral collaboration — the
tool is deliberately **not** tied to any platform, product, or service, and
contributions are judged on their technical merit, not their source.

## Getting started

Rust via [rustup](https://rustup.rs) (`source ~/.cargo/env` if PATH lacks
cargo).

```sh
cargo test                  # full suite, no big files needed (~seconds)
cargo build --release       # required for real-data runs (2 GB frames)
```

That is enough to build and test. For repo layout, the testing philosophy, the
synthetic ground-truth harness, and the current open-issues list, see
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md); the architecture and algorithm
choices live in [docs/DESIGN.md](docs/DESIGN.md).

## The bar for a PR

CI enforces all three of these on Linux, Windows, and macOS — check them
locally before pushing:

```sh
cargo fmt --all --check     # formatting
cargo clippy --all-targets  # runs with -D warnings in CI
cargo test                  # all tests green
```

- **New behaviour needs a test.** The synthetic harness (`mmm_core::synth`)
  can generate whatever inputs you need — tests never touch `test_data/`.
- **Do not casually recapture the byte-exact regression guards.** A hash
  change means the pipeline output changed; that is only acceptable when it is
  intended, and it must be justified per the procedure in
  [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#testing-philosophy). Never recapture
  to silence a failure you cannot explain.

## Commits & PRs

- Keep commits focused; use a conventional-style subject line where it fits
  (`fix:`, `feat:`, `docs:`, `chore:`).
- Open a PR against `main`, link any related issue, and fill in the checklist
  in the PR template.
- Small, reviewable PRs merge faster than large ones.

## Licensing of contributions

`mmm` is licensed under **Apache-2.0**. By submitting a contribution you agree
that it is provided under the same license: per section 5 of the Apache License
2.0, any contribution you intentionally submit for inclusion is licensed to the
project under Apache-2.0, inbound = outbound. No separate CLA is required.

## Where to discuss

Open a [GitHub issue](https://github.com/Astrometrical/mega-merge-mosaic/issues)
for bugs and feature requests, or start a
[discussion](https://github.com/Astrometrical/mega-merge-mosaic/discussions) for
questions and ideas.

## Integrations & frontends especially welcome

`mmm-core` is a UI-agnostic library — the CLI is just a thin frontend over it.
Frontends and integrations (PixInsight, Siril, standalone GUIs, pipeline glue)
are exactly the kind of contribution this project wants. Build on the public
`mmm-core` API, documented in
[crates/mmm-core/src/lib.rs](crates/mmm-core/src/lib.rs); the `synth` module is
public so downstream tools can generate ground-truth data without multi-GB
inputs.
