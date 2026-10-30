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
| pytest-benchmark | `fullname`, its file path made relative to the project root so a checkout's directory name never enters an id |
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
imported run carries no A/A noise floor. The gate derives one per item from
the data instead: the larger of the two sides' relative dispersion
(`mad_ns / median_ns`). The walltime limit is then the larger of +10% and
three times that, as in a native run, and an item whose limit was raised
prints a `NOTE` with the floor it used. An item with tight samples keeps the
fixed +10%. Two more things help:

- Gate against a long-lived baseline such as the last release, saved from the
  same kind of machine, rather than against a file from whichever runner
  happened to produce the last result. That is what a ratchet is for.
- Set `[gate] walltime = "advisory"` in `soothfast.toml` where the numbers
  cannot be trusted to a single run. An overrun prints as `SOFT` and the gate
  passes.

## Other languages

`cargo soothfast init` wires up repos that mix ecosystems: a cargo workspace
at the root plus any number of Gradle, Maven, Python and Node projects below
it. The root is Rust when it has a `Cargo.toml`. The other projects come from
tracked files (`git ls-files`), at any depth:

- A directory with `settings.gradle(.kts)` is a Gradle project, and the
  `build.gradle(.kts)` files below it belong to it. Without a settings file,
  the top-most `build.gradle(.kts)` directory is the project.
- `pom.xml`, `package.json`: the top-most directories only.
- `pyproject.toml`, `setup.py`: each directory, except those under another
  Python directory.
- Nothing under `target/`, `build/`, `dist/`, `node_modules/`, `.venv/`,
  `vendor/` or `.soothfast/` counts.

Init reports every project it finds. A detected project is gated only when
its own manifests name the harness its default command runs, and a benchmark
command is known:

- Python: `pyproject.toml` or `setup.py` names `pytest-benchmark` or `pyperf`.
- Node: a `package.json` names `vitest`, and either a script is `bench` (or
  runs `vitest bench`) or a `*.bench.*` file is tracked under the directory.
- Gradle: a Gradle file names `kotlinx.benchmark`, `jmh` or
  `me.champeau.jmh`. Gradle also has no default command.
- Maven: a `pom.xml` names `jmh`.

A root `package.json` that only holds tooling is therefore listed as
skipped, not turned into a gate that cannot run. A skipped project prints the
flags that add it:

```bash
cargo soothfast init --dir client \
  --bench-cmd './gradlew jmh -Pout={out}' --bench-out build/jmh.json
```

`--dir D` selects or creates the project in `D` and always gates it, and
`--ecosystem`,
`--bench-cmd`, `--bench-out` and `--from` apply to it. That is also how a
directory with no build file, such as Python tests with no `pyproject.toml`,
joins the set (`--ecosystem python --dir plugin/hooks --bench-cmd ...
--bench-out ...`). Without `--dir` those flags describe the project at the
root. `--ecosystem rust` is the cargo workspace at the root and takes no
`--dir`.

| Ecosystem | Command | Format | Setup |
|-----------|---------|--------|-------|
| `gradle` | none; pass `--bench-cmd` and `--bench-out` | `jmh` | `setup-java` |
| `maven` | `mvn -B -q -DskipTests package && java -jar target/benchmarks.jar -rf json -rff {out}` | `jmh` | `setup-java` |
| `python` | `uv run pytest --benchmark-only --benchmark-json {out}` | `pytest-benchmark` | `setup-uv` |
| `node` | `npx vitest bench --run --outputJson {out}` | `vitest` | `setup-node`, `npm ci` |

The command runs in the project's directory. `{out}` stands for the result
file (default `soothfast-bench.json`, relative to that directory). `--java`
(default `21`) and `--node` (default `22`) pick the toolchain versions.

### What init writes

The projects are saved as `[[project]]` tables of `soothfast.toml`, which is
the source of truth for what runs. Earlier releases saved a single `[init]`
table or `[[init.project]]` tables; both are read as projects and rewritten
as `[[project]]` the next time init writes.

```toml
[gate]
walltime = "hard"

[[project]]
dir = "client"
ecosystem = "gradle"
bench_cmd = "./gradlew jmh -Pout={out}"
bench_out = "build/jmh.json"
from = "jmh"

[[project]]
dir = "plugin/plugins/x/hooks"
ecosystem = "python"
bench_cmd = "uv run pytest --benchmark-json {out}"
bench_out = "bench.json"
from = "pytest-benchmark"
prefix = "hooks"
walltime = "advisory"

[runner]
labels = ["self-hosted", "linux"]

[test]
ci = false

[docs]
package = ["my-core"]
```

A project may also set `prefix` (the item-name prefix of the imported
results). Its `walltime` is `"hard"` unless set to `"advisory"`; init writes
`walltime = "hard"` into each new project, and `[gate] walltime` governs only
the cargo workspace. `[runner] labels` is a list of runner labels (no table
means `ubuntu-latest`), and `ci = false` under `[test]` drops the cargo
`test` job (the other `[test]` keys are unchanged). Init writes them only for
`--runner-labels A,B` and `--no-test`. `[docs] package` (a name or a list)
names the packages `docs check` reads; without it, init picks the workspace
packages the markdown binds (`crate::path`) name, else the root package, and
writes what it chose.

A repo with only a cargo workspace keeps the workflow init has always
written. Once it has other projects, `[runner]` labels or `ci = false` under
`[test]`, the workflow is a caller: triggers, permissions and one job that uses
`.github/workflows/run.yml` from this repository at the pinned release. The
pin is written twice, as the `uses:` ref and as the `soothfast_ref` input,
because a called workflow cannot read its own ref; the workflow stops when
the input is empty, and `init --check` reports a caller where they disagree.
It passes no secrets: the bot authenticates by OIDC and comments use the job's
own token. `--pin REF` writes the `uses:` at another commit, tag or branch,
so an unreleased commit can be tried; a bare commit pin is kept by later
runs. The reusable workflow installs the `cargo-soothfast` that matches the
ref it was called at: the crates.io release when that ref is the commit of a
published release tag, otherwise a build of the ref itself, cached by commit. It runs `cargo soothfast plan --github`
to read `soothfast.toml`, then runs a test job, a gate for the cargo
workspace, one gate job per other project and a docs check when tracked
markdown carries `<!-- soothfast:` markers. The plan job runs on the labels
the caller passes as `runs_on` (written when `[runner]` sets them); every
other job runs on the labels `[runner]` names. `init --check` compares the whole set,
so a flagless check after a flagged init is clean.

Init warns about any other workflow in `.github/workflows` that calls
soothfast, since its jobs run twice next to the generated ones. A plain init
that would only add a second Rust workflow next to one of those writes
nothing unless `--force` is given.

The docs job waits for the cargo gate, which saves the head measurement as the
`soothfast-baseline` artifact, and checks claims against it. A pull request
that leaves the bench binaries unchanged saves none, so the job falls back to
the default branch's measurement, which each default-branch push caches as
`soothfast-baseline-<sha>` (the base commit's, else the nearest). Only when
neither exists does the check report "claims not evaluated: no measurement" as
a notice, still checking binds and examples.

Each project's gate checks out the full history, runs the harness on the
merge base in a `git worktree` and on HEAD, then runs `gate --import` with
the merge base's file as `--reference-import`. When the merge base cannot run
the harness or writes no result file, the gate notes that and passes. Nothing
is stored. An imported gate measures walltime only, so each project gates
hard; set its `walltime = "advisory"` if the runner is too noisy to trust.
Each gate uploads its output as the `soothfast-result-<project>`
artifact.

`docs check` and `docs accept` need no cargo project for `path#Symbol` binds
(see [living docs](living-docs.md)).
