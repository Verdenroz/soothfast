# Reports

Every table, chart, badge, and changelog entry on this site is rendered from
a baseline, or a pair of them, rather than typed by hand. The example below
calls the real renderer directly on a small baseline of its own.

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
| `demo::checksum` | 812 | 41.2ns | 58.0ns | 0 | n/a |
{"color":"brightgreen","label":"docs","message":"92%","schemaVersion":1}
{"color":"brightgreen","label":"soothfast gate","message":"passing","schemaVersion":1}
```

`perf_table::rows` is the shared data extraction step. `markdown` and `html`
both call it and are otherwise identical. `badges::badge` is the raw
shields.io endpoint shape (`{schemaVersion, label, message, color}`), and
`coverage_badge` and `gate_badge` are the two callers that pick `color` for
you: green at 90% or above, yellow at 70% or above, red below that, and
passing, failing or unknown for a gate verdict.

## `cargo soothfast report render`

```console
$ cargo soothfast report render -p mylib --baseline self
report: wrote docs/perf/summary.md
report: wrote docs/perf/summary.html
report: wrote docs/perf/badges/gate.json
report: wrote docs/perf/llms.txt
```

`summary.md` and `summary.html` are `perf_table::markdown` and `html` over
the baseline. `badges/gate.json` reads `.soothfast/gate-status.json`, written
by the last `gate` run, through `badges::gate_badge`.

The trend SVGs (`trend-instructions.svg`, `trend-walltime_median_ns.svg`,
`trend-allocs.svg`) only appear once `.soothfast/trend.jsonl` holds at least
two points for that metric. Below that `trend_chart::render` returns `None`
and `report render` skips the file rather than draw an empty axis.

`llms.txt` is written only when `-p PKG` is given. It is `PKG`'s public
surface, meaning each signature and its first doc line, joined against the
baseline by id and falling back to `covers=`. An agent reading it can answer
"what does `X` measure right now" without guessing from a README that may be
out of date.

## Trend and changelog

`cargo soothfast trend append -p mylib` appends one line per run
(`{unix, commit, noise_pct, items}`) to `.soothfast/trend.jsonl`. That file
feeds both the SVG charts above and `trend render`, which prints each
metric's drift from first to last point straight to stdout. It writes no
file of its own, since charting is `report render`'s job.

`--from-baseline NAME` builds the point from a saved baseline instead of
measuring again — a pipeline whose gate already ran with `--save-baseline`
appends the very numbers it just verified. `-p PKG` scopes the point the
same way it scopes a measured run: other packages' entries and buildcost
pseudo-items in the shared baseline file stay out.

`cargo soothfast report changelog -p mylib --against-ref v1.0` merges a
fresh "Unreleased" section into `CHANGELOG.md` in place. Every
`## [released]` section already there survives. Re-running it is idempotent:
it replaces the `Unreleased` section rather than appending a duplicate.

The draft leads with what merged since `v1.0`, grouped into Features, Fixes,
Performance, Documentation and Internal by each commit's conventional type,
each entry carrying the pull request its subject named. That comes from
`git log`, not a forge API, since every merge lands as a squash whose
subject already holds the number. Release commits, and the bots that
regenerate derived artifacts, are dropped so a release does not list its own
paperwork.

Below a rule sit the derived sections, evidence rather than narrative: the
public API diff against `v1.0`, and the measured movement past gate
thresholds. A section with nothing to report is omitted rather than shipped
holding a sentence saying so, and a draft with nothing at all to report is
its heading alone.

Hand-written prose survives the same regeneration by living inside a
`<!-- soothfast:notes -->` / `<!-- /soothfast:notes -->` pair anywhere in the
Unreleased section. Add it once; every later run carries it forward, spliced
back in right after the heading, until the section is renamed to a real
version and frozen like the rest of the released history.

A section with no notes yet gets that pair with commented-out prompts for
the two things no measurement supplies: what the release means for someone
using it, and what a consumer has to do. They stay comments until someone
writes into them, so an untouched block renders as nothing.

Pass several `-p PKG` flags to cover more than one crate, and leave
`--against-ref` off entirely for a first release, where there is no earlier
version to diff against and the draft lists the surface it ships instead.
