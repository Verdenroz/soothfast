# Importing

soothfast measures Rust natively. For a project in another language, keep the
harness it already has and import its results: `cargo soothfast import`
turns a result file into a soothfast run, and `gate` compares an imported run
against a baseline or another imported run. Gating, baselines and thresholds
are the same ones Rust benchmarks get, with walltime as the only metric.

```console
$ cargo soothfast import --from jmh results.json --save-baseline base
$ cargo soothfast gate --import jmh new-results.json --baseline base
```

Neither command builds or measures anything, so both work outside a cargo
workspace. The project root is the workspace root if there is one, else the
git toplevel, else the current directory; baselines live in
`.soothfast/baselines/` under it and `soothfast.toml` is read from it.

## Formats

| `--from` | Harness | Produced by |
|----------|---------|-------------|
| `jmh` | JMH | `-rf json` |
| `pyperf` | pyperf | `--output FILE` (also `pyperf timeit`) |
| `pytest-benchmark` | pytest-benchmark | `--benchmark-json FILE` |
| `vitest` | vitest bench | `vitest bench --outputJson FILE` |

## Item ids

Every benchmark becomes an item `<prefix>::<name>`. The prefix defaults to
`jmh`, `pyperf`, `pytest` or `vitest`; `--prefix P` replaces it, which keeps
two suites of one format apart in a single baseline. The name is what the
harness calls the benchmark, so an id is stable until the benchmark is
renamed:

| Format | Name |
|--------|------|
| JMH | `benchmark[mode=M,k=v,...]`: the mode first, then its params sorted by key, so one benchmark run in several modes keeps one id per mode |
| pyperf | the benchmark's `metadata.name`, else the file's (`timeit` for `pyperf timeit`) |
| pytest-benchmark | `fullname` |
| vitest | `<group full name> > <benchmark name>`, the group name being file-relative as vitest prints it |

Two benchmarks that map to one id are an error.

## Metrics and units

Only walltime is imported, stored as `median_ns`, `mad_ns` and `p99_ns` in
nanoseconds per operation. Where the harness keeps its samples, all three are
computed from them; the MAD is unscaled, as in measured runs.

| Format | Samples | Unit |
|--------|---------|------|
| JMH | `rawData` of every fork, else `scorePercentiles` | `scoreUnit`: `ns/op`, `us/op`, `ms/op`, `s/op` |
| pyperf | `values` of every run; calibration runs have none | seconds |
| pytest-benchmark | `stats.data` | seconds |
| vitest | `samples` | milliseconds |

JMH throughput modes (`ops/ns`, `ops/us`, `ops/ms`, `ops/s`) are inverted
sample by sample into time per operation, so a slower run still reads as a
larger number. A throughput score of zero is rejected.

Sample-mode results carry a histogram instead of `rawData`. The median comes
from `scorePercentiles["50.0"]` (its absence is an error naming the
benchmark) and `p99_ns` from `99.0` for time units. Throughput units get no
`p99_ns`, since their 99th percentile is the fastest time.

An unknown unit is an error that names it, not a guess. When a format has no
samples (pytest-benchmark without `data`, vitest without `samples`), the
median is the reported one, the MAD is approximated as `stddev * 0.6745`,
which holds for roughly normal timings only, and there is no p99.

## Baselines

`--save-baseline NAME` merges the run into `.soothfast/baselines/NAME.json`,
replacing the items an earlier import marked with the same prefix and leaving
every other item alone, including Rust items and other prefixes. A full
`measure --save-baseline` keeps imported items too.
`--out FILE` writes the same JSON to a file instead. Exactly one of the two
is required.

## Gating

```console
$ cargo soothfast gate --import FORMAT FILE [--prefix P] --baseline NAME
$ cargo soothfast gate --import FORMAT FILE [--prefix P] --reference-import FORMAT FILE
```

The imported run is the current side. The reference is either a saved
baseline, restricted to items imported under the same prefix, or a second imported
file, for a reference you keep as raw harness output. The comparison is the
one `gate` always runs: walltime medians gate at +10%, an item in the
reference but not in the run is `GONE` and fails (`--allow-gone` accepts
it), and `soothfast-gate.lock` accepts apply. `--ratchet`, `--against-ref`,
`--deps`, `--save-baseline`, `--shard` and `-p` are not available with
`--import`, and `--baseline` cannot be combined with `--reference-import`.
`--filter` gates the items whose name contains it.

## Noise

Walltime from a different machine or a different run is noisy, and an
imported run carries no A/A noise floor to scale the threshold by. Two things
help:

- Gate against a long-lived baseline such as the last release, saved from the
  same kind of machine, rather than against a file from whichever runner
  happened to produce the last result. That is what a ratchet is for.
- Set `[gate] walltime = "advisory"` in `soothfast.toml` where the numbers
  cannot be trusted to a single run. An overrun prints as `SOFT` and the gate
  passes.
