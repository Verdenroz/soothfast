# Contributing to soothfast

Thank you for your interest in contributing to soothfast. This document covers everything you need to get started.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Reporting Bugs](#reporting-bugs)
- [Reporting Security Vulnerabilities](#reporting-security-vulnerabilities)
- [Suggesting Features](#suggesting-features)
- [Development Setup](#development-setup)
- [Building and Testing](#building-and-testing)
- [The Dogfood Gate](#the-dogfood-gate)
- [Code Style](#code-style)
- [Dependency Policy](#dependency-policy)
- [Submitting a Pull Request](#submitting-a-pull-request)
- [Commit Message Format](#commit-message-format)
- [Release Process](#release-process)

---

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating you agree to abide by its terms. Report violations to **harveytseng2@gmail.com**.

---

## Reporting Bugs

Before filing a bug, search [existing issues](https://github.com/Verdenroz/soothfast/issues) to avoid duplicates.

Open a new issue and include:

- The crate affected (`soothfast`, `soothfast-macros`, `soothfast-registry`, `soothfast-measure`, `soothfast-docs`, `soothfast-spec`, `soothfast-report`, `cargo-soothfast`).
- The version (from `Cargo.toml` or `cargo soothfast --version`).
- Steps to reproduce, including a minimal code example where possible.
- Actual vs. expected behavior.
- Rust version (`rustc --version`) and OS. For measurement issues, note whether
  `perf_event_open` is available (bare metal vs. container vs. CI runner) and
  which backend was selected.

---

## Reporting Security Vulnerabilities

**Do not open a public issue for security vulnerabilities.** See [SECURITY.md](SECURITY.md) for the private disclosure process.

---

## Suggesting Features

Open an issue with the `enhancement` label. Describe:

- The problem you are trying to solve.
- Your proposed solution and any alternatives you considered.
- Whether you are willing to implement it (helps prioritize).

For large changes (new metric backends, new spec providers, new public API surface), open an issue for discussion before writing code — this avoids wasted effort if the direction does not fit the project.

---

## Development Setup

**Prerequisites:**

- Rust **1.85 or later** (the workspace MSRV) — install via [rustup](https://rustup.rs/)
- `valgrind` (optional, for the callgrind measurement backend on machines
  without `perf_event_open` access)
- A nightly toolchain (optional, only for the rustdoc-JSON docs engine:
  `rustup toolchain install nightly`)

**Clone and build:**

```bash
git clone https://github.com/Verdenroz/soothfast.git
cd soothfast
cargo build --workspace
```

**Install git hooks** (optional but recommended):

```bash
pre-commit install   # or: prek install
```

---

## Building and Testing

```bash
make help          # list all targets
make check         # fmt + clippy + tests (same as CI's check job)
make baselines     # measure the self-bench crates into the shared baseline
make ci            # everything CI runs, minus the PR gates
make gate          # all merge-base gates (perf + build cost)
make docs          # regenerate every site page, then serve the docs site locally with live reload
```

Or directly with Cargo:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

All tests must pass before a PR is merged.

---

## The Dogfood Gate

soothfast gates its own performance with itself. Every PR runs the regression
gate against the merge-base of the PR branch and `master`: the base revision is
measured in a temporary git worktree with interleaved rounds, so there are no
committed baselines to go stale.

Run it locally before pushing:

```bash
make gate              # all gates against the origin/master merge-base
make gate BASE=master  # or against a local ref
```

If the gate fails in CI, triage artifacts are uploaded (`.soothfast/triage/`)
and a summary is posted as a PR comment. A genuine regression should be fixed;
an intentional cost change should be justified in the PR description so the
maintainer can re-baseline the affected assertions.

---

## Code Style

- Run `cargo fmt --all` before committing. CI enforces formatting.
- Run `cargo clippy --workspace --all-targets -- -D warnings`. All warnings are errors in CI.
- Public API items must have doc comments (`///`). See existing items for tone and style.
- No `unwrap()` or `expect()` in library code outside of tests.
- Prefer small, focused commits. One logical change per commit.

---

## Dependency Policy

Dependencies are a measured, gated claim in this project.
The full CLI dependency tree is intentionally tiny (~12–15 crates), and the
core `soothfast` library that user crates depend on stays at essentially
`linkme` + proc-macro machinery. Hand-rolled beats imported here: stats,
markdown scanning, SVG charts, and git operations are all written in-repo.

A PR that adds a dependency must justify it in the description and will be
held to the `cargo deny` policy (`deny.toml`): crates.io only, no wildcard
versions, allowed licenses only.

---

## Submitting a Pull Request

1. **Fork** the repository and create a branch from `master`:
   ```bash
   git checkout -b feat/my-feature
   ```
2. Make your changes, including tests for any new behaviour.
3. Run `make check` and `make gate` locally. Fix any issues.
4. Push your branch and open a PR against `master`.
5. Fill in the PR description: what changed, why, and how you tested it.
6. Address any review comments. All CI checks — including the dogfood gate — must pass before merge.

PRs that add features or fix bugs should include tests. PRs that only touch documentation or CI do not require new tests.

---

## Commit Message Format

```
<type>: <short description>

<optional body — explain the why, not the what>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`

Examples:
```
feat: add async backend poll/wake counters
fix: handle empty rustdoc JSON surface in docs diff
perf: avoid re-parsing callgrind output per assertion
```

---

## Release Process

Releases are managed by the project maintainer. Publishing to crates.io is
automated: pushing a `v*` tag runs the full check suite and the gate, then
publishes the workspace crates in dependency order (see
`.github/workflows/release.yml`). If you believe a bug fix warrants a release,
comment on the relevant issue.
