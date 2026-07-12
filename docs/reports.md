# Reports

Every table, chart, badge, and changelog entry on this site is rendered
from a baseline (or a pair of them) — never typed by hand. This page's own
example calls the real renderer directly, on a small baseline of its own.

## Perf tables and badges

```rust capture-output
use serde_json::json;
use soothfast_report::{badges, perf_table};

fn main() {
    let baseline = json!({ "items": { "demo::checksum": {
        "perfcnt": { "instructions": 812 },
        "walltime": { "median_ns": 41.2, "p99_ns": 58.0 },
        "alloc": { "allocs": 0, "bytes": 0 }
    }}});

    print!("{}", perf_table::markdown(&baseline));
    println!("{}", badges::coverage_badge("docs", 92));
    println!("{}", badges::gate_badge(Some(true)));
}
```

```text soothfast-output
| item | instructions | median | p99 | allocs | polls |
|---|---:|---:|---:|---:|---:|
| `demo::checksum` | 812 | 41.2ns | 58.0ns | 0 | — |
{"color":"brightgreen","label":"docs","message":"92%","schemaVersion":1}
{"color":"brightgreen","label":"soothfast gate","message":"passing","schemaVersion":1}
```

`perf_table::rows` is the shared data extraction step — `markdown` and
`html` both call it, then format identically otherwise. `badges::badge` is
the raw shields.io endpoint shape (`{schemaVersion, label, message,
color}`); `coverage_badge` and `gate_badge` are the two callers that pick
`color` for you (green ≥90% / yellow ≥70% / red below; passing/failing/
unknown for a gate verdict).

## `cargo soothfast report render`

```console
$ cargo soothfast report render -p mylib --baseline self
report: wrote docs/perf/summary.md
report: wrote docs/perf/summary.html
report: wrote docs/perf/badges/gate.json
report: wrote docs/perf/llms.txt
```

`summary.md`/`summary.html` are `perf_table::markdown`/`html` over the
baseline; `badges/gate.json` reads `.soothfast/gate-status.json` (written by
the last `gate` run) through `badges::gate_badge`. Trend SVGs
(`trend-instructions.svg`, `trend-walltime_median_ns.svg`,
`trend-allocs.svg`) only appear once `.soothfast/trend.jsonl` holds at least
two points for that metric — `trend_chart::render` returns `None` below
that, and `report render` simply skips the file rather than draw an
empty axis. `llms.txt` is only written when `-p PKG` is given; it's `PKG`'s
public surface (signature + first doc line) joined against the baseline by
id, falling back to `covers=` — the answer to "what does `X` measure right
now" without an agent guessing from a possibly-stale README.

## Trend and changelog

`cargo soothfast trend append -p mylib` appends one line
(`{unix, commit, noise_pct, items}`) to `.soothfast/trend.jsonl` per run —
it's what feeds both the SVG charts above and `trend render`, which prints
each metric's first→last drift straight to stdout (it writes no file of its
own; charting is `report render`'s job).

`cargo soothfast report changelog -p mylib --against-ref v1.0` merges a
fresh "Unreleased" section — the public API diff against `v1.0` plus the
current perf table — into `CHANGELOG.md`, in place, preserving every
`## [released]` section already there. Re-running it is idempotent: it
replaces only the `Unreleased` section, never appends a duplicate.
