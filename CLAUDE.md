# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

soothfast is a Rust framework where documentation and performance measurement are
*derived from the code* via annotations, and CI gates both doc drift and perf
regressions. Adding `soothfast` to a crate costs one runtime dependency
(`linkme`); all workhorse machinery lives in the separately built
`cargo-soothfast` CLI binary, which is everything CI actually calls.

Core idea: annotate a function with `#[soothfast::measured(...)]` or
`#[soothfast::bench(...)]`, and it becomes a gated benchmark with checked claims
(`alloc = N`, `p99 = "1ms"`, `complexity = "n log n"`). Markdown docs carry
`<!-- soothfast:bind -->` / `<!-- soothfast:claim -->` markers that tie prose to
source code and measured numbers; `cargo soothfast docs check` fails CI the day
either drifts from reality.

## Commands

```bash
make help          # list all Make targets
make check          # fmt --check + clippy -D warnings + cargo test --workspace (mirrors CI's check job)
make baselines      # measure BENCH_CRATES (soothfast-registry, soothfast-measure, soothfast-docs) into the shared "self" baseline
make ci             # docs check/capture against this build's own baseline (mirrors CI's docs job)
make gate           # merge-base gates vs origin/master (or BASE=<ref>): self-benches + build cost
make docs           # regenerate every site page (API reference, perf report, coverage) + serve locally with live reload
make clean          # cargo clean
```

Direct cargo:

```bash
cargo test --workspace                                    # all tests
cargo test -p soothfast-measure stats::                      # single crate / module
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The CLI itself, run via the workspace binary (no separate install needed):

```bash
cargo run --release -p cargo-soothfast -- <command>
```

Key `cargo-soothfast` subcommands (see `cargo-soothfast/src/main.rs` for the full
usage string): `measure`, `gate`, `trend append|render`, `docs
check|accept|gen-tests|capture|diff|reference|routes|build`, `coverage
measure|docs`, `spec gen|gate|check|check-proto`, `sdk gen|gate|publish`,
`report render|changelog`,
`mcp`.

Before pushing, run what CI runs: `make check` and `make gate BASE=master`.
The dogfood gate (`make gate`) measures the merge-base of the current branch
against `origin/master` in a temporary git worktree — there are no committed
baseline files to go stale.

## Architecture

Linear dependency stack, each crate doing one job:

```
soothfast-registry  → soothfast-measure → soothfast-docs → soothfast-site
       ↑                                  ↑
soothfast-macros                      soothfast-spec → soothfast-sdk
       ↑                                  ↑                ↑
    soothfast (user-facing facade)   soothfast-report      │
                                          ↑                │
                                   cargo-soothfast (CLI, everything CI calls)
```

- **`soothfast-registry`** — the only runtime dependency user crates take on.
  Defines `linkme::distributed_slice` statics (`MEASURED`, `ROUTES`,
  `FIXTURES`) that proc-macro expansions push const items into at link time,
  plus the frozen FNV-1a fingerprint function (`fnv1a`) used to detect when an
  annotated item's token stream changes. Fingerprints are compared across
  builds and persisted in lockfiles/baselines — **never change the FNV-1a
  constants or algorithm.**
- **`soothfast-macros`** — proc-macros (`measured`, `bench`, `fixture`, `route`)
  that parse attribute args (`group`, `setup`/`setup_sized`, `covers`,
  `alloc`, `p99`, `complexity`, `sizes(...)`) and expand to registry
  registrations. They don't alter the annotated function's body.
- **`soothfast`** — the thin public facade re-exporting the macros and registry,
  plus `keep()` (the `black_box` optimizer barrier — routes constant
  inputs/outputs through it or LLVM will const-fold the measured body away)
  and `bench_main!()` (installs the counting allocator + measurement runner
  main, gated behind the `runner` feature so it's a dev-dependency only).
- **`soothfast-measure`** — the measurement engine: backends for perf counters
  (`perfcnt.rs`), callgrind (`callgrind.rs`, valgrind-based fallback for
  PMU-less machines/containers), walltime (`walltime.rs`, median + MAD via
  `stats.rs` — never mean/stddev, so one scheduler blip can't skew it),
  allocation counting (`alloc.rs`, via a global `CountingAllocator`), and
  async poll/wake counters (`asyncexec.rs`). `runner.rs` drives measurement
  and evaluates checked assertions; `sweep.rs` runs complexity sweeps across
  `sizes(...)`.
- **`soothfast-docs`** — the docs engine: ingests rustdoc JSON (nightly-only) to
  build the public API surface (`surface.rs`), scans markdown for `bind`/
  `claim` markers (`markdown.rs`, `claims.rs`), generates and runs doc tests
  from fenced code blocks (`gentests.rs`), diffs public API across refs
  (`diff.rs`), renders API reference fragments (`reference.rs`), and persists
  `soothfast.lock` (bind/claim fingerprints so accepted prose doesn't silently
  drift, `lockfile.rs`).
- **`soothfast-spec`** — the spec engine, working in both directions over the
  same four dialects (`SpecKind`: OpenAPI, AsyncAPI, GraphQL, MCP tools).
  *Generation* (for surfaces you serve): `schema/` turns rustdoc JSON into
  JSON Schema — `types.rs` resolves type nodes and monomorphises generics,
  `adt.rs` renders structs/enums under all four serde tagging
  representations, `serde_attrs.rs` recovers renames from rustdoc's
  preserved attributes, `foreign.rs` maps types it cannot walk, and
  `route_sig.rs` infers a handler's wire contract from its own signature.
  `dialect.rs` then dispatches on `SpecKind` — the CLI never matches over
  dialects — to one of four emitters: `openapi/` (3.1), `asyncapi/` (3.0,
  channels + `action: send|receive`), `graphql/` (a type graph plus an SDL
  renderer in `graphql/sdl.rs`), and `mcp.rs` (tool manifests with `$defs`).
  Each emitter ships its own consumer-compatibility diff; the JSON Schema
  half of that comparison, and the request/response asymmetry it turns on,
  live once in `compat.rs`, and `serialize.rs` renders YAML/JSON with a
  fixed key order. *Reconciliation* (for specs someone else serves):
  provider adapters in `providers.rs` (AsyncAPI in both its 2.x and 3.0
  layouts) and comparison logic in `reconcile.rs`, plus `proto.rs` for wire
  formats with no handler. Three things are underivable — erased returns,
  `#[serde(with)]` fields, and unmapped foreign types — and each becomes a
  reported `Gap` with an open schema rather than a guess. What a *dialect*
  cannot express (a GraphQL union, an MCP non-object result) is a `note` on
  the emitted `Document`, reported the same way and equally non-fatal.
- **`soothfast-sdk`** — the SDK engine: native client emitters over the same
  pre-dialect IR the spec emitters render (`dialect::Operation` +
  `RouteShape`), no external generators or template engines. `lower.rs`
  lowers the JSON Schema IR into a language-neutral model (`model.rs`:
  `Ty`/`Model`/`Method`), deciding every typing question once — snake_case
  attribute derivation with collision fallback, `oneOf` → structurally
  decoded unions, gaps → `Any` plus a note. `python/` renders frozen
  dataclasses, sync + async clients, pagination iterators, and package
  scaffolding (`pyproject.toml`, README) around one hand-written runtime
  (`assets/python/_runtime.py`, `include_str!`'d and emitted verbatim) that
  owns retries/backoff honoring `Retry-After`, error mapping, type-hint-
  driven decoding, and cursor pagers. Emission is byte-deterministic;
  golden tests (`tests/goldens/`, regenerate with `UPDATE_GOLDENS=1`) pin
  the full file tree, and `tests/python/` exercises the generated golden
  SDK end-to-end (run via `uv run --with httpx --with pytest pytest`).
  The TypeScript emitter is a planned second `SdkKind`.
- **`soothfast-report`** — renderers consuming measurement output: perf tables
  (`perf_table.rs`), SVG trend charts (`trend_chart.rs`), badges
  (`badges.rs`), living `CHANGELOG.md` draft generation (`changelog.rs`,
  idempotent — regenerates the Unreleased section in place, preserves
  Released sections), and `llms.txt` (agent-facing docs derived from rustdoc
  + baseline measurements, `llms.rs`).
- **`soothfast-site`** — the native docs-site engine behind `cargo soothfast docs
  build` (no mkdocs, zero new dependencies). Hand-rolled markdown→HTML
  renderer for a strict subset (`md.rs`), build-time syntax highlighting
  (`highlight.rs`), minimal template engine (`template.rs`), embedded
  "soothfast" theme with file-level override via `theme_dir` (`theme.rs` +
  `theme/`), `soothfast.toml` `[site]` config (`config.rs`), and a `SitePlugin`
  event pipeline (`plugin.rs`) through which the built-ins run: evidence
  chips rendering claim/bind markers with live baseline numbers plus
  capture-output run panels (`evidence.rs`), and the client-side search
  index (`search.rs`).
- **`cargo-soothfast`** — the CLI. `invoke.rs` shells out to `cargo bench`/
  `cargo build` and collects records; `gate.rs` implements baseline / merge-base
  worktree / ratchet comparison with regression thresholds and triage-artifact
  output (`.soothfast/triage/`); `buildcost.rs` measures build cost across a
  features matrix; `docs.rs`/`docs_support.rs`, `spec.rs`, `report.rs`,
  `trend.rs`, `coverage.rs` wrap the corresponding engine crates; `mcp.rs`
  serves agent-facing living docs on stdio (`cargo soothfast mcp`).

### Baselines and gating

`cargo soothfast measure --save-baseline NAME` persists measurement runs under
`.soothfast/baselines/`. A run with failing assertions is never saved (would
ratify the regression). `cargo soothfast gate` compares a fresh run against a
saved baseline, a named ratchet, or — for CI — the merge-base of the PR branch
measured live in a temporary worktree, so there's no baseline file to become
stale in version control.

### Dogfood loop

This repo runs soothfast on itself: `soothfast-registry`, `soothfast-measure`, and
`soothfast-docs` each carry a `benches/soothfast.rs` bench target (see
`BENCH_CRATES` in the `Makefile`) measuring their own hot paths, and
`README.md` / `docs/measuring.md` contain `soothfast:bind` / `soothfast:claim`
markers checked against those live measurements in CI (`make ci`). When
editing measured functions in those three crates, expect `make gate` or the CI
`docs`/`soothfast-gate` jobs to fail if behavior or cost changes — that's the
claim being enforced, not a bug in the check.

### CI workflows (`.github/workflows/`)

- `ci.yml` — `check` (fmt/clippy/test), `msrv` (pins to Rust 1.85), `docs`
  (dogfood: `make baselines` then `docs check`/`docs capture` against the
  fresh baseline, plus the `check-llms` drift gate), `deploy-docs`
  (master-only: rebuilds and publishes the native site build to `gh-pages`).
- `security.yml` — `cargo-deny` (advisories/bans/licenses/sources per
  `deny.toml`) and `zizmor` (static analysis of the workflows themselves);
  runs on push/PR and weekly so newly disclosed advisories surface without
  a push.
- `scorecard.yml` — OSSF Scorecard supply-chain analysis, published to the
  public Scorecard API and uploaded to code scanning as SARIF.
- `soothfast-gate.yml` — reusable workflow; runs `cargo soothfast gate` for a
  given package against the PR's merge-base, uploads `.soothfast/triage/` on
  failure, and posts/updates a PR comment with the gate output.
- `changelog.yml` — auto-regenerates the living `CHANGELOG.md` and commits it
  via a bot token (does not retrigger CI).
- `spec.yml` — on push, regenerates `mode = "generate"` spec files and
  commits them with the same bot pattern as `changelog.yml`, so nobody has
  to remember to; on PRs, gates instead — `spec gen --check` fails on a
  stale committed spec and `spec gate` fails on a consumer-breaking change
  vs the merge-base (`--allow-breaking` releases one deliberately).
- `release.yml` — on `v*` tag push, runs checks + gate, then publishes all 10
  workspace crates to crates.io in dependency order.

All third-party actions are pinned to a full commit SHA (never a mutable
tag), every job declares explicit least-privilege `permissions:`, every
step starts with `step-security/harden-runner`, and checkouts that don't
need to push set `persist-credentials: false` — `zizmor` (in `security.yml`)
gates all of this; see `.github/dependabot.yml` for the automated dependency
(cargo + github-actions) update policy.

## Conventions from CONTRIBUTING.md

- Public API items require `///` doc comments.
- No `unwrap()`/`expect()` in library code outside tests.
- Dependencies are treated as a measured, gated claim — the CLI's full
  dependency tree is intentionally tiny (~12–15 crates); stats, markdown
  scanning, SVG charts, and git operations are hand-rolled in-repo rather than
  imported. Justify any new dependency in the PR description; it must also
  satisfy `deny.toml` (crates.io only, no wildcard versions, allowed licenses).
- Commit format: `<type>: <short description>` with types `feat`, `fix`,
  `refactor`, `docs`, `test`, `chore`, `perf`, `ci`.

## Mechanism Design Principle

When shaping any soothfast mechanism (macros, registries, CLI surface,
markdown tag syntax), maintainability and developer experience outrank all
else — never lock in an arbitrary constraint (fixed arity, zero-arg-only,
single-shape signatures) when a slightly more general design costs little
extra and removes the constraint for consumers. Prefer the version a
consumer would need fewer workarounds for, even if it's a few more lines
here.

## Design Context

Scope: `soothfast-site` (the native docs-site engine — `theme.rs`, `theme/`,
`md.rs`, `evidence.rs`), the sole engine behind the docs site (no mkdocs).
The design system already lives in code, mostly in
`soothfast-site/theme/assets/tokens.css` and `site.css` — this section
records the intent behind it so future generation work extends it instead of
drifting from it.

### Users

Rust developers evaluating or maintaining libraries where performance and
doc accuracy are contractual, not aspirational — the kind of reader who
wants to see the actual measured number a claim is gated on, not just prose
asserting it. They're scanning API reference pages and perf reports mid-task,
often cross-referencing a claim against the benchmark that produced it.

### Brand Personality

**Precise, rigorous, unadorned.** The site's own tagline is "the mason's
straightedge" — every visual device should read as an *instrument reading*,
not a decoration:
- Ruler-tick graduations in the topbar, dashed-underline `h2` markers.
- Monospace for anything that is a label, measurement, or gauge (nav,
  chips, tables, code); serif for prose the author wrote.
- Evidence chips (pass/fail/warn) are the one place color carries meaning —
  never used purely decoratively elsewhere.
- Dark code panels in both light and dark themes ("instruments, not paper").

### Aesthetic Direction

- Avoid the generic mkdocs-material default look as an anti-reference — pages
  generated by `soothfast-site` should feel distinct from an unstyled
  Material theme, not like any other framework's docs.
- Avoid marketing/SaaS gloss: no gradient heroes, no illustrated mascots, no
  trust badges beyond the real evidence chips already in place.
- Avoid playful/illustrated dev-tool styling — this is a precision-engineering
  tool, not a "fun startup" product.
- Light and dark themes both fully supported (`prefers-color-scheme` plus an
  explicit `data-theme` override that wins in both directions) — any new
  component must be themed both ways, not light-only with a dark patch.
- Accent color is "gauge indigo" (`--accent`); reserve it for interactive/
  active states and evidence-adjacent emphasis (bind chips, current nav/TOC
  item), not general decoration.

### Design Principles

1. **Every visual element earns its place as information.** If a color,
   border, or icon isn't standing in for a measured fact or a navigational
   state, question whether it belongs.
2. **Monospace = machine-produced or measured; serif = human-authored prose.**
   Keep that split consistent when adding new page types.
3. **Color is evidence, not decoration.** Pass/fail/warn semantics own the
   only saturated colors on the page; the accent is the sole exception, and
   only for actionable/active states.
4. **Both themes, always.** New components ship with explicit light and dark
   values in `tokens.css`, following the existing override pattern.
5. **Hold WCAG 2.1 AA**: sufficient contrast for text and evidence chips in
   both themes, visible focus states (already present via
   `:focus-visible`), and respect `prefers-reduced-motion` (already global).
6. **No decorative side-stripe borders.** Never use `border-left`/
   `border-right` wider than 1px as a colored accent on cards, callouts, or
   list items (the classic generic-admin-dashboard tell) — state and
   severity are carried by background tint, weight, and the evidence-chip
   vocabulary instead (see `.adm`, `.nav-group li a.active`, `.toc-list
   a.current` in `site.css`).
