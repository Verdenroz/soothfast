# Changelog

## 0.1.2 - 2026-08-09

<!-- soothfast:notes -->
`changelog.yml` regenerates the Unreleased section on every PR now, which
used to be an all-or-nothing replacement: any hand-written context a
maintainer added ("why this matters", not just "what changed") got
silently deleted the next time the bot ran, sometimes minutes later. A
`<!-- soothfast:notes -->` block anywhere in the Unreleased section now
rides along untouched through every regeneration, spliced back in ahead of
the mechanical API-surface and performance data, until the section is
renamed to a real version and frozen like the rest of the release history.
This paragraph is that feature testing itself: written by hand, expected
to still be here after the bot's own runs on this PR regenerate everything
around it.
<!-- /soothfast:notes -->

### API surface

No public API changes.

### Performance

| item | instructions | median | p99 | allocs | polls |
|---|---:|---:|---:|---:|---:|
| `soothfast_docs::bench_claim_parse` | 2691 | 318.9ns | 324.1ns | 6 | n/a |
| `soothfast_docs::bench_markdown_scan` | 4717100 | 472.72µs | 498.68µs | 4114 | n/a |
| `soothfast_measure::bench_summarize` | 4163365 | 630.90µs | 1.61ms | 4 | n/a |
| `soothfast_measure::bench_sweep_evaluate` | 236 | 26.5ns | 30.4ns | 0 | n/a |
| `soothfast_registry::bench_fnv1a` | 245782 | 131.22µs | 158.49µs | 0 | n/a |
| `soothfast_report::bench_llms_render` | 3466252 | 422.74µs | 428.25µs | 7191 | n/a |
| `soothfast_report::bench_perf_table` | 8493592 | 1.16ms | 1.47ms | 8204 | n/a |
| `soothfast_sdk::bench_emit_typescript` | 19932078 | 2.94ms | 3.32ms | 42930 | n/a |
| `soothfast_sdk::bench_lower` | 11217251 | 1.73ms | 1.86ms | 23907 | n/a |
| `soothfast_site::bench_highlight` | 26477138 | 4.20ms | 4.62ms | 88069 | n/a |
| `soothfast_site::bench_md_render` | 29464161 | 4.41ms | 4.60ms | 68118 | n/a |
| `soothfast_spec::bench_openapi_diff` | 38441983 | 7.60ms | 8.63ms | 77722 | n/a |
| `soothfast_spec::bench_openapi_document` | 14309449 | 2.43ms | 5.11ms | 30152 | n/a |
| `soothfast_spec::bench_serialize_yaml` | 55380004 | 8.11ms | 10.19ms | 74277 | n/a |


## 0.1.1 - 2026-08-09

### API surface

No public API changes.

### Performance

| item | instructions | median | p99 | allocs | polls |
|---|---:|---:|---:|---:|---:|
| `soothfast_docs::bench_claim_parse` | n/a | 375.4ns | 390.7ns | 6 | n/a |
| `soothfast_docs::bench_markdown_scan` | n/a | 442.71µs | 474.80µs | 4114 | n/a |
| `soothfast_measure::bench_summarize` | n/a | 661.03µs | 676.83µs | 4 | n/a |
| `soothfast_measure::bench_sweep_evaluate` | n/a | 25.1ns | 25.9ns | 0 | n/a |
| `soothfast_registry::bench_fnv1a` | n/a | 81.50µs | 84.32µs | 0 | n/a |
| `soothfast_report::bench_llms_render` | n/a | 301.82µs | 311.20µs | 7191 | n/a |
| `soothfast_report::bench_perf_table` | n/a | 822.16µs | 868.62µs | 8204 | n/a |
| `soothfast_sdk::bench_emit_typescript` | n/a | 2.29ms | 2.40ms | 42930 | n/a |
| `soothfast_sdk::bench_lower` | n/a | 1.49ms | 1.62ms | 23907 | n/a |
| `soothfast_site::bench_highlight` | n/a | 3.14ms | 3.20ms | 88069 | n/a |
| `soothfast_site::bench_md_render` | n/a | 3.18ms | 3.24ms | 68118 | n/a |
| `soothfast_spec::bench_openapi_diff` | n/a | 5.50ms | 6.27ms | 77722 | n/a |
| `soothfast_spec::bench_openapi_document` | n/a | 2.00ms | 2.15ms | 30152 | n/a |
| `soothfast_spec::bench_serialize_yaml` | n/a | 6.50ms | 7.01ms | 74277 | n/a |


## 0.1.0 - 2026-08-09

### API surface

Initial release: 309 public items across 9 crates.

#### `soothfast` (7)

- `soothfast` (module): Soothfast: measured and documented Rust.
- `soothfast::embed` (module): The embedded-server handshake.
- `soothfast::embed::READY_PREFIX` (constant): The prefix identifying a readiness line. Everything after it is a JSON
- `soothfast::embed::announce` (function): Tell a waiting SDK launcher which base URL this server is serving.
- `soothfast::keep` (function): Optimizer barrier for measured code, the `black_box` equivalent.
- `soothfast::mock` (module): Mock-backend seams for `capture-output`/test examples: activate a
- `soothfast::mock::activate` (function): Resolve and start the named `#[soothfast::mock_seam]`, passing `arg`

#### `soothfast-docs` (36)

- `soothfast_docs` (module): Soothfast docs engine.
- `soothfast_docs::claims` (module): Quantitative-claims ledger: `item.backend.metric <op> value[unit]`
- `soothfast_docs::claims::Claim` (struct): One parsed `soothfast:claim` expression: a measured metric of an item,
- `soothfast_docs::claims::Op` (enum): Comparison operator of a claim expression (`<`, `<=`, `>`, `>=`).
- `soothfast_docs::claims::evaluate` (function): Evaluate against a baseline document; returns (holds, actual value).
- `soothfast_docs::claims::parse` (function): Parse `demo::lcg_checksum.perfcnt.instructions < 25000` (units on the
- `soothfast_docs::diff` (module): Public-surface diff between two builds: what a reviewer needs to see
- `soothfast_docs::diff::SurfaceDiff` (struct): Public-API delta between two [`Surface`] snapshots, by item path.
- `soothfast_docs::diff::compare` (function): Diff two surfaces: added/removed items by path, changed items by span
- `soothfast_docs::diff::render` (function): One `ADDED`/`REMOVED`/`CHANGED` line per item, ready for terminals and
- `soothfast_docs::gentests` (module): Generated artifacts from markdown code blocks: a test file per markdown
- `soothfast_docs::gentests::GENERATED_HEADER` (constant): First line of every generated file; `docs check` uses it to tell
- `soothfast_docs::gentests::capture_examples` (function): Example-file contents for the capture blocks of one markdown doc, in
- `soothfast_docs::gentests::sanitized_stem` (function): `docs/measuring.md` → `soothfast_doc_docs_measuring`.
- `soothfast_docs::gentests::test_file` (function): Test-file content for one markdown doc, or None when it has no testable
- `soothfast_docs::lockfile` (module): `soothfast.lock`: committed fingerprints for every bound item. A bind is
- `soothfast_docs::lockfile::Binds` (type_alias): item path → fingerprint (hex).
- `soothfast_docs::lockfile::LOCKFILE` (constant): Lockfile name, resolved relative to the workspace root.
- `soothfast_docs::lockfile::merge` (function): Merge freshly accepted binds over `existing`. A full-scope accept replaces
- `soothfast_docs::lockfile::read` (function): Read accepted binds from `root`'s lockfile; a missing file is an empty
- `soothfast_docs::lockfile::write` (function): Persist `binds` to `root`'s lockfile as stable, pretty-printed JSON.
- `soothfast_docs::markdown` (module): Line-based markdown scanning: fenced code blocks with info tags, and
- `soothfast_docs::markdown::Bind` (struct): `<!-- soothfast:bind path::to::item -->`
- `soothfast_docs::markdown::ClaimMarker` (struct): `<!-- soothfast:claim item.backend.metric < value -->`
- `soothfast_docs::markdown::CodeBlock` (struct): One fenced code block.
- `soothfast_docs::markdown::Doc` (struct): Everything [`scan`] extracts from one markdown file: fenced code blocks
- `soothfast_docs::markdown::scan` (function): Scan one markdown text. Closing markers (`<!-- /soothfast:... -->`) are
- `soothfast_docs::markdown::splice_output` (function): Replace (or insert) the `text soothfast-output` fence that follows the
- `soothfast_docs::reference` (module): API reference fragments: per-crate markdown generated from the surface —
- `soothfast_docs::reference::render` (function): Render one crate's surface as a markdown fragment (module-grouped).
- `soothfast_docs::reference::resolve_reference_links` (function): Rustdoc comments often disambiguate intra-doc links with a reference-style
- `soothfast_docs::surface` (module): Public API surface from rustdoc JSON: item paths, span-derived
- `soothfast_docs::surface::ItemGroup` (struct): One distinct item, grouped by (kind, fingerprint) to collapse the extra
- `soothfast_docs::surface::ItemInfo` (struct): What the surface records about one public item — enough to detect any
- `soothfast_docs::surface::Surface` (struct): Public items keyed by full path (`crate::module::item`).
- `soothfast_docs::surface::from_rustdoc` (function): Build a surface from a rustdoc JSON document. `source_root` resolves the

#### `soothfast-macros` (6)

- `soothfast_macros` (module): Proc-macros for soothfast.
- `soothfast_macros::bench` (proc_macro): Register a bench harness function, optionally fed by a setup fn.
- `soothfast_macros::fixture` (proc_macro): Mark a deterministic input-builder. Registers metadata only; the function
- `soothfast_macros::measured` (proc_macro): Register a zero-argument function as a directly measured item.
- `soothfast_macros::mock_seam` (proc_macro): Mark a mock-backend setup fn, resolved at runtime by name via
- `soothfast_macros::route` (proc_macro): Declare the spec operation a handler implements — the code-side half of

#### `soothfast-measure` (34)

- `soothfast_measure` (module): Soothfast measurement engine.
- `soothfast_measure::CountingAllocator` (struct)
- `soothfast_measure::alloc` (module): Allocation-counting backend: a `GlobalAlloc` wrapper over `System` that
- `soothfast_measure::alloc::AllocMeasurement` (struct): Per-iteration allocation counts for one item.
- `soothfast_measure::alloc::CountingAllocator` (struct): Counting global allocator; installed by `soothfast::bench_main!`.
- `soothfast_measure::alloc::measure` (function): Measure one workload iteration's allocations: warm once (lazy statics,
- `soothfast_measure::asyncexec` (module): Async-behavior backend: polls and wakes per iteration, measured on the
- `soothfast_measure::asyncexec::AsyncMeasurement` (struct): Per-iteration async behavior for one item.
- `soothfast_measure::asyncexec::measure` (function): Warm once, then min-of-3 single iterations (same shape as the alloc
- `soothfast_measure::callgrind` (module): Callgrind backend: fully deterministic Ir counts for PMU-less
- `soothfast_measure::callgrind::annotate` (function): Human triage report: top self-cost functions for one item's workload.
- `soothfast_measure::callgrind::measure` (function): Per-iteration Ir for one item.
- `soothfast_measure::callgrind::probe` (function): Can valgrind actually run THIS binary? `valgrind --version` is not
- `soothfast_measure::main` (function)
- `soothfast_measure::perfcnt` (module): Deterministic backend: user-space CPU counters via a hand-rolled
- `soothfast_measure::perfcnt::PerfMeasurement` (struct): Per-iteration counter values for one item.
- `soothfast_measure::perfcnt::measure` (function): Measure per-iteration instructions/cycles/cache-refs (min of 3 rounds).
- `soothfast_measure::perfcnt::measure_instructions` (function): Instructions-only measurement, for complexity sweeps.
- `soothfast_measure::perfcnt::probe` (function): Can this environment open an instructions counter on itself?
- `soothfast_measure::runner` (module): In-bench-binary runner: parses argv, measures every registered item with
- `soothfast_measure::runner::main` (function): Entry point installed by `soothfast::bench_main!`.
- `soothfast_measure::stats` (module): Hand-rolled summary statistics (dependency policy: no stats crates).
- `soothfast_measure::stats::Summary` (struct): Robust summary of one sample set.
- `soothfast_measure::stats::summarize` (function): Summarize a non-empty sample set. Sorts in place.
- `soothfast_measure::sweep` (module): Complexity-claim evaluation over a size sweep.
- `soothfast_measure::sweep::DRIFT_LIMIT` (constant): Growth beyond the claim must stay under this factor across the sweep.
- `soothfast_measure::sweep::SweepOutcome` (struct): Verdict of one complexity sweep: how far measured growth strayed from
- `soothfast_measure::sweep::class_value` (function): Cost model for a claimed class at size n.
- `soothfast_measure::sweep::evaluate` (function): Evaluate measured values (one per size, same order) against a claim.
- `soothfast_measure::walltime` (module): Wall-clock backend: adaptive iteration count, warmup, median+MAD summary,
- `soothfast_measure::walltime::DEFAULT_SAMPLES` (constant): Default sample count; odd so the median is a real observation.
- `soothfast_measure::walltime::WallMeasurement` (struct): Wall-clock summary for one item, in ns per iteration.
- `soothfast_measure::walltime::calibrate` (function): A/A noise calibration: measure an identical reference workload against
- `soothfast_measure::walltime::measure` (function): Measure one item: pilot run to pick `iters`, then `samples` timed loops.

#### `soothfast-registry` (25)

- `soothfast_registry` (module): Distributed registry of measured/documented items.
- `soothfast_registry::Assertions` (struct): Checked performance claims attached to a measured item.
- `soothfast_registry::Bencher` (struct): Single-use measurement context handed to registered runner glue.
- `soothfast_registry::Bencher::iter` (function): Hand the workload to the active backend. Call exactly once per glue call.
- `soothfast_registry::Bencher::iter_async` (function): Async workloads: each iteration drives the future to completion on the
- `soothfast_registry::FIXTURES` (static): All fixtures registered with `#[soothfast::fixture]`.
- `soothfast_registry::FixtureItem` (struct): A deterministic input-builder registered via `#[soothfast::fixture]`.
- `soothfast_registry::MEASURED` (static): All items registered with `#[soothfast::measured]` / `#[soothfast::bench]`.
- `soothfast_registry::MOCKS` (static): All mock seams registered with `#[soothfast::mock_seam]`.
- `soothfast_registry::MeasuredItem` (struct): A benchmarked item registered via `#[soothfast::measured]` or `#[soothfast::bench]`.
- `soothfast_registry::MeasuredItem::full_id` (function): Package-qualified stable ID; survives file moves. In a lib target
- `soothfast_registry::MockSeam` (trait): A mocked backend a capture-output/test example can stand up by name.
- `soothfast_registry::MockSeamItem` (struct): A mock-backend setup fn registered via `#[soothfast::mock_seam]`.
- `soothfast_registry::ROUTES` (static): All routes registered with `#[soothfast::route]`.
- `soothfast_registry::RouteItem` (struct): A declared route/operation registered via `#[soothfast::route]` — the
- `soothfast_registry::RouteItem::new` (function): Const constructor for macro expansions.
- `soothfast_registry::RouteItem::with_shape` (function): Attach the shape overrides. Separate from [`RouteItem::new`] so that
- `soothfast_registry::async_counters` (function): (polls, wakes) since process start — backends diff snapshots around a body.
- `soothfast_registry::block_on_counting` (function): Minimal std-only executor with poll/wake counting: enough to measure how
- `soothfast_registry::fixture_items` (function): Read-only view of every registered fixture.
- `soothfast_registry::fnv1a` (function): FNV-1a 64-bit hash.
- `soothfast_registry::measured_items` (function): Read-only view of every registered measured item.
- `soothfast_registry::mock_seam_items` (function): Read-only view of every registered mock seam.
- `soothfast_registry::resolve_mock_seam` (function): Resolve a mock seam by name: an exact `id` match wins; otherwise exactly
- `soothfast_registry::route_items` (function): Read-only view of every registered route.

#### `soothfast-report` (21)

- `soothfast_report` (module): Soothfast report engine: renders measurement baselines
- `soothfast_report::badges` (module): Shields.io endpoint-badge JSON (`https://img.shields.io/endpoint?url=...`)
- `soothfast_report::badges::badge` (function): Raw shields.io endpoint JSON: `{schemaVersion, label, message, color}`.
- `soothfast_report::badges::coverage_badge` (function): Green ≥ 90, yellow ≥ 70, red below.
- `soothfast_report::badges::gate_badge` (function): `None` means no recorded gate verdict — say so rather than guess green.
- `soothfast_report::badges::svg_from` (function): Render endpoint JSON (as produced by the builders above) as a flat SVG
- `soothfast_report::changelog` (module): Changelog drafting: API-surface diff + perf deltas rendered as a draft
- `soothfast_report::changelog::ApiSection` (enum): What the API section has to report.
- `soothfast_report::changelog::DraftInputs` (struct): Inputs already computed by the CLI (API section, two baselines).
- `soothfast_report::changelog::draft` (function): Render the "Unreleased" draft section: API surface + perf table.
- `soothfast_report::llms` (module): `llms.txt`: a compact machine-readable digest of the public surface and
- `soothfast_report::llms::SurfaceEntry` (struct): One public item as fed by the CLI (from the docs-engine surface).
- `soothfast_report::llms::render` (function): Render `llms.txt`: every public item grouped by crate, with its full
- `soothfast_report::perf_table` (module): Perf tables from a baseline document: one row per item, the deterministic
- `soothfast_report::perf_table::Row` (struct): One measured item's metrics, each `None` when that backend didn't run.
- `soothfast_report::perf_table::html` (function): `rows` rendered as a plain `<table>` for embedding in the docs site.
- `soothfast_report::perf_table::markdown` (function): `rows` rendered as a GitHub-flavored markdown table.
- `soothfast_report::perf_table::rows` (function): One `Row` per measured item in the baseline, in baseline (map) order.
- `soothfast_report::trend_chart` (module): Hand-emitted SVG trend charts from the `.soothfast/trend.jsonl` series:
- `soothfast_report::trend_chart::METRICS` (constant): (metric key path in baseline items, display name)
- `soothfast_report::trend_chart::render` (function): Render one metric's chart; None when fewer than 2 points exist.

#### `soothfast-sdk` (20)

- `soothfast_sdk` (module): Soothfast SDK engine: native client emitters over the spec IR.
- `soothfast_sdk::SdkFileSet` (struct): Rendered SDK files, keyed by path relative to the output directory.
- `soothfast_sdk::SdkKind` (enum): A language the SDK engine can emit.
- `soothfast_sdk::SdkOptions` (struct): Everything an emitter needs that the operations don't carry.
- `soothfast_sdk::envtemplate` (module): Reading a server's dotenv template as its configuration surface.
- `soothfast_sdk::envtemplate::EnvVar` (struct): One environment variable the embedded server reads.
- `soothfast_sdk::envtemplate::markdown_table` (function): Render the knobs as a README table. Same text in both languages: the
- `soothfast_sdk::envtemplate::parse` (function): Parse a dotenv-style template into the knobs it documents.
- `soothfast_sdk::lower` (module): JSON Schema IR → the language-neutral SDK model.
- `soothfast_sdk::lower::lower` (function): Lower operations into the SDK model.
- `soothfast_sdk::model` (module): The language-neutral SDK model every emitter renders from.
- `soothfast_sdk::model::Field` (struct): One field of a [`Model`].
- `soothfast_sdk::model::Method` (struct): One client method, lowered from an operation.
- `soothfast_sdk::model::Model` (struct): A named object schema, emitted as a class/interface.
- `soothfast_sdk::model::Param` (struct): One parameter of a [`Method`].
- `soothfast_sdk::model::ParamLoc` (enum): Where a request parameter travels.
- `soothfast_sdk::model::Sdk` (struct): The whole lowered SDK.
- `soothfast_sdk::model::Ty` (enum): A wire type, lowered from JSON Schema.
- `soothfast_sdk::target` (module): Rust target triples → the platform metadata each packaging format needs.
- `soothfast_sdk::target::Target` (struct): One cross-compilation target and how each ecosystem names it.

#### `soothfast-site` (62)

- `soothfast_site` (module): soothfast-site — the native docs-site engine: markdown in, a themed,
- `soothfast_site::BuildInput` (struct)
- `soothfast_site::BuildReport` (struct)
- `soothfast_site::SiteConfig` (struct)
- `soothfast_site::SitePlugin` (trait)
- `soothfast_site::build` (module): The build pipeline: discover pages → plugin markdown pass → render →
- `soothfast_site::build::BuildInput` (struct): Everything a build needs; the CLI assembles this.
- `soothfast_site::build::BuildReport` (struct): What happened, for the CLI to report.
- `soothfast_site::build::build` (function): Build the whole site. Any page failing to render fails the build —
- `soothfast_site::color` (module): Hand-rolled color math for deriving a full Material-Design-3-shaped role
- `soothfast_site::color::Role` (struct): A role's light and dark tones together, as derived from one seed color.
- `soothfast_site::color::RoleTones` (struct): One Material-style color role, resolved to concrete hex values for a
- `soothfast_site::color::contrast` (function): WCAG contrast ratio between two colors, >= 1.0.
- `soothfast_site::color::generate_theme_css` (function): Generate a `theme-vars.css` override for whichever `[site.theme]` seeds
- `soothfast_site::color::parse_hex` (function): Parse a `#rgb`, `#rrggbb`, or bare `rrggbb`/`rgb` hex string into `(r, g, b)` bytes.
- `soothfast_site::color::relative_luminance` (function): WCAG relative luminance, 0.0 (black) .. 1.0 (white).
- `soothfast_site::color::role_from_seed` (function): Derive a full light+dark Material role from one seed hex color, given the
- `soothfast_site::color::to_hex` (function): Render a channel triple as `#RRGGBB`, the form CSS custom properties
- `soothfast_site::config` (module): `soothfast.toml` — site configuration. Hand-rolled TOML subset (tables,
- `soothfast_site::config::NavGroup` (struct): One nav group in the sidebar: a titled list of markdown pages.
- `soothfast_site::config::SiteConfig` (struct): The `[site]` section of `soothfast.toml`, with defaults for every field.
- `soothfast_site::config::ThemeConfig` (struct): The `[site.theme]` section: seed hex colors for the brand roles. Any
- `soothfast_site::config::parse` (function): Parse `soothfast.toml` text into a config. Unknown keys under `[site]` are
- `soothfast_site::evidence` (module): Built-in plugin: measured evidence. Rewrites `soothfast:claim` /
- `soothfast_site::evidence::Evidence` (struct): Evidence renderer over one baseline + the bind lockfile.
- `soothfast_site::highlight` (module): Build-time syntax highlighting: small lexers emitting `<span class=
- `soothfast_site::highlight::highlight` (function): Highlight `code` for `lang`. Always returns escaped HTML.
- `soothfast_site::md` (module): Strict markdown → HTML for the site: the subset soothfast docs actually
- `soothfast_site::md::CodeHook` (type_alias): Custom renderer for one fenced block: (lang, tags, code) → HTML, or
- `soothfast_site::md::Heading` (struct): One collected heading, for the on-page table of contents.
- `soothfast_site::md::Options` (struct): Render hooks: link rewriting and custom code-block rendering, so the
- `soothfast_site::md::Rendered` (struct): A rendered page body.
- `soothfast_site::md::code_html` (function): Default fenced-block rendering: instrument-panel `<figure>` with a
- `soothfast_site::md::render` (function): Render one markdown document.
- `soothfast_site::nav` (module): Sidebar navigation: explicit groups from `[[site.nav]]`, or
- `soothfast_site::nav::Missing` (enum): What to do about a nav entry naming a page the build did not find.
- `soothfast_site::nav::PageMeta` (struct): Minimal facts about a discovered page, gathered before rendering.
- `soothfast_site::nav::build` (function): Resolve nav groups to template data, plus any warnings raised.
- `soothfast_site::nav::href` (function): `base` + `route`, with the empty result mapped to `./` (a valid link to
- `soothfast_site::nav::route_for` (function): Route for a docs-relative markdown path: pretty directory URLs.
- `soothfast_site::nav::with_current` (function): Copy of the nav with `current: true` stamped on the active page and a
- `soothfast_site::plugin` (module): The extension seam. Every feature that is not core rendering — evidence
- `soothfast_site::plugin::PageRecord` (struct): A fully rendered page, as seen by end-of-build events.
- `soothfast_site::plugin::PageRef` (struct): Identity of the page an event concerns.
- `soothfast_site::plugin::SitePlugin` (trait): Pipeline events, in firing order. Default implementations are no-ops so
- `soothfast_site::plugin::page_html` (function): Run one HTML-rewrite pass through all plugins, in order.
- `soothfast_site::plugin::page_markdown` (function): Run one markdown-rewrite pass through all plugins, in order.
- `soothfast_site::search` (module): Built-in plugin: client-side search. Emits `search_index.json` — one
- `soothfast_site::search::Search` (struct): Search index builder.
- `soothfast_site::serve` (module): `cargo soothfast docs serve` — the dev server: serve the built site over
- `soothfast_site::serve::Server` (struct): A bound dev server, not yet running (split from `run` so callers and
- `soothfast_site::template` (module): Minimal template engine for theme files. Supports `{{ path }}` (HTML-
- `soothfast_site::template::Includes` (trait): Resolves `{% include %}` names to template text (backed by the theme).
- `soothfast_site::template::escape` (function): Escape text for HTML element and attribute contexts.
- `soothfast_site::template::render` (function): Render `source` against `ctx`. Values are looked up by dot path
- `soothfast_site::theme` (module): Theme = a named set of files (templates, partials, assets, icons). The
- `soothfast_site::theme::Theme` (struct): A resolved theme: defaults plus user overrides.
- `soothfast_site::toml` (module): The workspace's shared TOML subset: tables, array-of-tables, strings,
- `soothfast_site::toml::TomlValue` (enum): A parsed TOML scalar, string array, or inline table.
- `soothfast_site::toml::logical_lines` (function): Comment-stripped, non-empty logical lines with their 1-based numbers.
- `soothfast_site::toml::parse_string` (function): Parse a leading double-quoted string; returns (content, bytes consumed).
- `soothfast_site::toml::parse_value` (function): Parse a scalar value: string, bool, or string array.

#### `soothfast-spec` (98)

- `soothfast_spec` (module): Soothfast spec engine, working in both directions.
- `soothfast_spec::DeclaredOp` (struct): One operation a spec declares.
- `soothfast_spec::RouteDecl` (struct): One `#[soothfast::route]` annotation from the code.
- `soothfast_spec::SpecKind` (enum): Spec dialects soothfast reads and writes.
- `soothfast_spec::asyncapi` (module): AsyncAPI 3.0 documents assembled from inferred route shapes.
- `soothfast_spec::asyncapi::diff` (module): Compatibility diffing between two generated AsyncAPI documents.
- `soothfast_spec::asyncapi::diff::diff` (function): Compare two generated documents, oldest first.
- `soothfast_spec::asyncapi::document` (function): Assemble an AsyncAPI 3.0 document.
- `soothfast_spec::compat` (module): Consumer compatibility: what changed, and whether it breaks anyone.
- `soothfast_spec::compat::Change` (struct): One difference between two versions of a spec.
- `soothfast_spec::compat::Direction` (enum): Which way the data flows, which decides what "required" costs.
- `soothfast_spec::compat::SchemaDiff` (struct): Compares schemas belonging to two revisions of a document.
- `soothfast_spec::compat::Severity` (enum): Whether a change can break an existing consumer.
- `soothfast_spec::compat::compare_keys` (function): Compare two maps of named entries, reporting bare presence changes and
- `soothfast_spec::compat::deref` (function): Follow a local `$ref` to the schema it names, once.
- `soothfast_spec::compat::is_compatible` (function): True when nothing in the diff would break an existing consumer.
- `soothfast_spec::compat::render` (function): A JSON value as it should read inside a diff message.
- `soothfast_spec::compat::required_set` (function): The `required` names of an object schema.
- `soothfast_spec::compat::sort` (function): Order a diff for reporting: breaking first, then by location, so output is
- `soothfast_spec::dialect` (module): What every generated dialect has in common, and how one is chosen.
- `soothfast_spec::dialect::Document` (struct): A rendered document plus anything that had to be reconciled to build it.
- `soothfast_spec::dialect::Info` (struct): Document metadata that no amount of code reading can derive.
- `soothfast_spec::dialect::Operation` (struct): One operation to emit, pairing spec identity with the inferred shape.
- `soothfast_spec::graphql` (module): GraphQL SDL generated from inferred route shapes.
- `soothfast_spec::graphql::diff` (module): Compatibility diffing between two generated GraphQL type graphs.
- `soothfast_spec::graphql::diff::diff` (function): Compare two generated type graphs, oldest first.
- `soothfast_spec::graphql::document` (function): Assemble a GraphQL type graph.
- `soothfast_spec::graphql::sdl::to_sdl` (function): Render a generated type graph as GraphQL SDL.
- `soothfast_spec::graphql::to_sdl` (function)
- `soothfast_spec::mcp` (module): MCP tool manifests generated from `#[route(method = "TOOL")]` handlers.
- `soothfast_spec::mcp::diff` (function): Compare two generated tool manifests, oldest first.
- `soothfast_spec::mcp::document` (function): Assemble an MCP tool manifest.
- `soothfast_spec::openapi` (module): OpenAPI 3.1 documents assembled from inferred route shapes.
- `soothfast_spec::openapi::diff` (module): Compatibility diffing between two generated OpenAPI documents.
- `soothfast_spec::openapi::diff::diff` (function): Compare two generated documents, oldest first.
- `soothfast_spec::openapi::document` (function): Assemble an OpenAPI 3.1 document.
- `soothfast_spec::proto` (module): `.proto` message field reconciliation, parallel to [`crate::reconcile`]:
- `soothfast_spec::proto::ProtoField` (struct): One field, from either a `.proto` message or a struct's `#[prost(..)]`
- `soothfast_spec::proto::ProtoReconciliation` (struct): Outcome of matching struct fields to a `.proto` message by tag number.
- `soothfast_spec::proto::parse_proto_message` (function): Parse the scalar fields of one `message NAME { ... }` block from `.proto`
- `soothfast_spec::proto::parse_struct_prost_fields` (function): Extract `#[prost(..)]` field declarations from one struct in Rust source
- `soothfast_spec::proto::reconcile_proto` (function): Match struct fields to declared `.proto` fields by tag number, then
- `soothfast_spec::providers` (module): Declared-surface providers: parse each spec dialect into [`DeclaredOp`]s.
- `soothfast_spec::providers::parse` (function): Parse one spec document into the operations it declares, dispatching on
- `soothfast_spec::reconcile` (module): One reconciler for every provider: spec operations ↔ code routes,
- `soothfast_spec::reconcile::Reconciliation` (struct): Outcome of matching `#[route]` declarations against a spec's operations:
- `soothfast_spec::reconcile::reconcile` (function): Match routes to declared operations by operation id, then check that
- `soothfast_spec::schema` (module): Rustdoc JSON → JSON Schema.
- `soothfast_spec::schema::Docs` (struct)
- `soothfast_spec::schema::Extraction` (struct): A schema plus everything it referenced and everything it could not see.
- `soothfast_spec::schema::Extractors` (struct)
- `soothfast_spec::schema::Gap` (enum): A place the extractor could not derive a shape, and why.
- `soothfast_spec::schema::Overrides` (struct)
- `soothfast_spec::schema::Resolver` (struct)
- `soothfast_spec::schema::RouteShape` (struct)
- `soothfast_spec::schema::TypeMapping` (enum)
- `soothfast_spec::schema::TypeTable` (struct)
- `soothfast_spec::schema::docs` (module): The rustdoc documents one resolution runs against.
- `soothfast_spec::schema::docs::Docs` (struct): The rustdoc documents a resolution may walk: the package's own, plus zero
- `soothfast_spec::schema::extract_named` (function): Extract the schema for a named type from a rustdoc JSON document.
- `soothfast_spec::schema::foreign` (module): Canonical-path → JSON Schema mappings for types outside this crate.
- `soothfast_spec::schema::foreign::TypeMapping` (enum): What a table entry says about a type it cannot walk.
- `soothfast_spec::schema::foreign::TypeTable` (struct): Built-in and user-supplied mappings from canonical type path to schema.
- `soothfast_spec::schema::graphql_attrs` (module): async-graphql helper attributes, recovered from rustdoc JSON `attrs`.
- `soothfast_spec::schema::graphql_attrs::ContainerAttrs` (struct): Container-level `#[graphql(...)]` options that change wire names.
- `soothfast_spec::schema::graphql_attrs::FieldAttrs` (struct): Field-level `#[graphql(...)]` options.
- `soothfast_spec::schema::graphql_attrs::Rename` (enum): How a container renames every field or enum item beneath it.
- `soothfast_spec::schema::graphql_attrs::container` (function): Parse container-level `#[graphql(...)]` attributes.
- `soothfast_spec::schema::graphql_attrs::field` (function): Parse field-level `#[graphql(...)]` attributes.
- `soothfast_spec::schema::graphql_attrs::variant` (function): Parse variant-level `#[graphql(...)]` attributes. Enum items take `name`
- `soothfast_spec::schema::graphql_attrs::wire_name_field` (function): The wire name of a field: `name` if given, else the container's rule.
- `soothfast_spec::schema::graphql_attrs::wire_name_variant` (function): The wire name of an enum item: `name` if given, else the container's rule.
- `soothfast_spec::schema::route_sig` (module): Handler signatures → the wire contract they implement.
- `soothfast_spec::schema::route_sig::Extractors` (struct): Maps extractor wrapper types to the role they play.
- `soothfast_spec::schema::route_sig::Overrides` (struct): Attribute overrides for what inference cannot see.
- `soothfast_spec::schema::route_sig::Parameter` (struct): One query, path or header parameter.
- `soothfast_spec::schema::route_sig::RequestBody` (struct): A request body and the content type it arrives as.
- `soothfast_spec::schema::route_sig::Response` (struct): One response, keyed by status code in [`RouteShape`].
- `soothfast_spec::schema::route_sig::Role` (enum): What a handler parameter contributes to the wire contract.
- `soothfast_spec::schema::route_sig::RouteShape` (struct): Everything a handler signature says about its wire contract.
- `soothfast_spec::schema::route_sig::infer` (function): Infer the wire contract of one handler.
- `soothfast_spec::schema::serde_attrs` (module): serde helper attributes, recovered from rustdoc JSON `attrs` strings.
- `soothfast_spec::schema::serde_attrs::ContainerAttrs` (struct): Container-level serde options that change the emitted schema.
- `soothfast_spec::schema::serde_attrs::FieldAttrs` (struct): Field-level serde options that change the emitted schema.
- `soothfast_spec::schema::serde_attrs::Rename` (enum): How a container renames every field or variant beneath it.
- `soothfast_spec::schema::serde_attrs::container` (function): Parse container-level `#[serde(...)]` attributes.
- `soothfast_spec::schema::serde_attrs::field` (function): Parse field-level `#[serde(...)]` attributes.
- `soothfast_spec::schema::serde_attrs::serde_args` (function): Pull the `serde(...)` argument text out of rustdoc's attribute entries.
- `soothfast_spec::schema::serde_attrs::wire_name_field` (function): The wire name of a field, applying `rename` then the container rule.
- `soothfast_spec::schema::serde_attrs::wire_name_variant` (function): The wire name of a variant, applying `rename` then the container rule.
- `soothfast_spec::schema::types` (module): Rustdoc type nodes → JSON Schema.
- `soothfast_spec::schema::types::Resolver` (struct): Walks rustdoc JSON, emitting JSON Schema and collecting [`Gap`]s.
- `soothfast_spec::schema::types::Resolver::resolve` (function): Resolve one rustdoc type node into a JSON Schema.
- `soothfast_spec::schema::types::Subst` (type_alias): Generic parameter bindings captured at a concrete use site: parameter name
- `soothfast_spec::serialize` (module): YAML and JSON rendering for generated documents.
- `soothfast_spec::serialize::to_json` (function): Render a document as pretty JSON, for `.json` spec files.
- `soothfast_spec::serialize::to_yaml` (function): Render a document as YAML, with well-known keys in conventional order.
- `soothfast_spec::sniff_kind` (function): Sniff the dialect from filename + content.

### Performance

| item | instructions | median | p99 | allocs | polls |
|---|---:|---:|---:|---:|---:|
| `soothfast_docs::bench_claim_parse` | n/a | 241.3ns | 261.8ns | 6 | n/a |
| `soothfast_docs::bench_markdown_scan` | n/a | 442.46µs | 486.53µs | 4114 | n/a |
| `soothfast_measure::bench_summarize` | n/a | 661.27µs | 675.30µs | 4 | n/a |
| `soothfast_measure::bench_sweep_evaluate` | n/a | 25.1ns | 28.6ns | 0 | n/a |
| `soothfast_registry::bench_fnv1a` | n/a | 81.49µs | 82.08µs | 0 | n/a |
| `soothfast_report::bench_llms_render` | n/a | 303.41µs | 309.59µs | 7191 | n/a |
| `soothfast_report::bench_perf_table` | n/a | 851.96µs | 864.72µs | 8204 | n/a |
| `soothfast_sdk::bench_emit_typescript` | n/a | 2.29ms | 2.55ms | 42930 | n/a |
| `soothfast_sdk::bench_lower` | n/a | 1.48ms | 1.60ms | 23907 | n/a |
| `soothfast_site::bench_highlight` | n/a | 3.16ms | 3.19ms | 88069 | n/a |
| `soothfast_site::bench_md_render` | n/a | 3.15ms | 3.21ms | 68118 | n/a |
| `soothfast_spec::bench_openapi_diff` | n/a | 5.62ms | 7.30ms | 77722 | n/a |
| `soothfast_spec::bench_openapi_document` | n/a | 2.03ms | 2.39ms | 30152 | n/a |
| `soothfast_spec::bench_serialize_yaml` | n/a | 6.57ms | 8.49ms | 74277 | n/a |
