# Gating

Measuring produces numbers. Gating decides whether they are acceptable.
`cargo soothfast gate` re-measures your code, compares it against a
reference, and exits non-zero as soon as any metric regresses past a
threshold. That is what turns a benchmark into a CI check.

## Picking a reference

Three ways to choose what "acceptable" means, in order of how CI actually
uses them:

```console
$ cargo soothfast gate -p mylib                                    # vs a saved --baseline (default "base")
$ cargo soothfast gate -p mylib --against-ref origin/master         # vs the merge-base, measured live
$ cargo soothfast gate -p mylib --backend buildcost --against-ref origin/master   # binary size / build time
```

`--against-ref` never reads a file. It measures HEAD and the merge-base of
`REF` in a temporary git worktree, in rounds interleaved as
head/base/head/base. That "tango" order spreads out thermal and scheduler
drift. Deterministic gating counters (perf instructions, callgrind Ir) do
not drift, so they are collected once per side in the first round; the
second round re-measures only the timing-sensitive backends, and the
per-metric minimum across a side's rounds is its comparison value. When the
two sides' bench binaries have byte-identical `.text` sections, no
measurable change is possible and the gate short-circuits to a single cheap
pass whose assertions still run. This is
what `make gate` runs against `origin/master` for
every crate in `BENCH_CRATES`, and it is why merge-base gating has no
baseline file that can go stale in version control.

Before the reference side is measured, its locked `soothfast*` crates are
pinned to the versions in HEAD's `Cargo.lock`. The harness compiles into
both bench binaries, so without this a measurement-protocol change riding
along in a lockfile bump would be reported as a regression in *your* code.
Other dependencies are deliberately left as the reference locked them —
a slow `serde` bump is exactly what the gate exists to catch.

## Reusing a measured reference

Measuring the reference side is the most expensive thing the gate does, and
it repeats. Every push to a branch has the same merge-base, and on master the
commit gated as HEAD becomes the next commit's reference. A run is therefore
kept under `.soothfast/runs/`, keyed by the commit and by every condition
outside it that moves the numbers: the `rustc` version, the pinned
`codegen-units`, the flags reaching rustc, the CPU model, the locked
`soothfast` versions the reference is pinned to, and the measurement scope
(`-p`, `--features`, `--bench`, `--backend`, `--samples`, `--filter`). A
merge-base with a stored run under the same key is served from there, and
the worktree is never built:

```console
gate: reusing the measured merge-base 1aa6c4de39f9408a4279b9f9d78a6a8ed0de6a39
```

The commit is not the only key. A merge-base that was never gated has no run
under its commit, which on master is most of them, since the gate usually runs
only on benchmarkable changes. Once such a reference is built, its machine code
is checked against the cache as well: a byte-identical `.text` under the same
conditions is the same measurement, whichever commit produced it.

```console
gate: reusing a run measured from the same merge-base binary
```

That second key saves the measurement and not the build, because the binary is
what the lookup needs. It is also what lets the identical-binaries short
circuit keep its result, so a reference measured once carries forward through
commits that leave the bench binary alone.

Counters carry across runs; the clock does not. A reused reference was timed
in a different process, so `walltime` softens to `SOFT` for that comparison
while instructions, Ir and allocation counts gate exactly as they would
against a freshly measured reference. `--no-reuse-base` measures the
reference again regardless.

For this to pay off in CI, `.soothfast/runs/` has to outlive the job. Cache
it keyed on the merge-base commit:

```yaml
- uses: actions/cache@v4
  with:
    path: .soothfast/runs
    key: soothfast-runs-${{ github.ref_name }}-${{ github.sha }}
    restore-keys: |
      soothfast-runs-${{ github.ref_name }}-
```

The key has to be unique per commit, with the prefix in `restore-keys`. An
exact key that never changes is never rewritten once saved, so the cache
would keep the first commit's runs and every later commit would miss.

## Thresholds

| metric | threshold | notes |
|---|---|---|
| `perfcnt.instructions` | +5% | hard |
| `callgrind.ir` | +5% | hard |
| `walltime.median_ns` | +10%, or 3x the A/A noise floor when that is higher | hard. A threshold within ~2 sigma of the run's jitter cannot tell signal from noise, so it scales with the measured floor — noisy runners gate looser instead of not at all. A fail is also downgraded to a SOFT warning when the deterministic evidence contradicts it: unchanged fingerprint, instructions/Ir flat (within 0.5%), and alloc counts/bytes identical. Identical code doing identical work cannot have gotten slower — that is the clock measuring the machine. In `--against-ref` mode a fail must also reproduce in both interleaved round pairings; one lucky round on either side cannot manufacture it. |
| `alloc.allocs` / `alloc.bytes` | +5% | hard, integer ceiling |
| `asyncexec.polls` / `asyncexec.wakes` | +5% | hard, integer ceiling. Present only for `async fn` benches. Catches what instruction counts can miss: a future that polls twice as often does the same work per poll. |
| `buildcost.size_bytes` | +5% | hard |
| `buildcost.build_ms` | +25% | soft: printed as a warning, does not fail the gate |

A bench can widen its own limit with `#[bench(..., tolerance = "8%")]`, which
raises the instruction, Ir and walltime thresholds for that item alone. It is
for bodies too small to survive the default: a loop costing ~20 instructions
per element moves 5% when one register spills, and no fixture size changes
that ratio. Attribute text is part of an item's fingerprint, so the commit
adding a tolerance also reports the item as changed.

## Build settings

Instruction counts only compare across builds that made the same codegen
decisions. Rust compilation is not function-local: at the stock
`codegen-units = 16` rustc spreads modules over 16 units, and adding code
anywhere repartitions them, moving inlining and register allocation in
functions nobody touched. An untouched bench can then report several percent.

So the gate pins it. Every measurement build runs with `codegen-units = 1`,
on both sides of a comparison, whatever the checked-out tree's profile says.
One unit cannot be repartitioned. This costs build time, which is the price
of a comparison that means something; `--codegen-units N` or `inherit` opts
out, as does

```toml
[gate]
codegen-units = "inherit"
```

Pinning cannot reach everything. A `[profile.bench.package.mycrate]` table
still wins over the gate's setting, and `RUSTFLAGS`, `.cargo/config.toml` and
the rustc version all change codegen too. Every run therefore records what it
was built with, printed as `build=<digest>` in the gate banner. When the two
sides disagree the gate says which field differs and downgrades the
deterministic counters to SOFT for that comparison:

```console
gate: build settings differ: profiles (7b1f.. -> 22c0..) — deterministic counters softened
SOFT  ind_alma instructions 81076.0 -> 107851.0 (+33.0%)
```

The run still passes. A profile change that cannot be measured is not a
regression, and it has to be able to land through the gate that will measure
it afterwards. Allocation and poll counts stay hard: they are semantic, and
codegen does not move them.

A baseline saved before build stamps existed compares the same way, and says
to re-save it. `--against-ref` never hits that: both sides are measured fresh
in the same run.

## Ratchets

<!-- soothfast:bind soothfast_measure::sweep::evaluate -->
`--ratchet NAME` runs a second comparison against a long-lived baseline,
your last release for instance, after the primary one. It exists because ten
separate +4% regressions each pass the primary gate on their own. A ratchet
is an independent floor that catches the sum even when no single change
trips the per-commit threshold.
<!-- /soothfast:bind -->

## Reusing the gate's measurement

`--save-baseline NAME` persists the head run a passing gate just measured,
exactly as `measure --save-baseline` would, so a deploy pipeline that needs
both a gate verdict and a fresh baseline pays for one measurement instead of
two. A failing run is never saved — the baseline would ratify its own
regression — and the identical-binaries short circuit skips the save too:
its single cheap pass collects no gating counters, and since the code did
not change, the previously saved baseline is still the truth.

## `--deps`: advisory, not a different check

`--deps` does not change what gets gated. It adds one warning: if any
measured item's source fingerprint changed relative to the reference,
`cargo soothfast gate --deps` prints a note that this is not a pure
dependency bump. The threshold comparison runs the same either way. All
`--deps` tells you is whether a regression comes from your own code
changing or purely from whatever moved underneath it.

## When it fails

Every run writes `.soothfast/gate-status.json` (`{passed, failures, unix}`),
which `cargo soothfast report render` reads to draw the pass/fail badge.
On failure, the first three failing items each get a callgrind per-function
report written to `.soothfast/triage/<item-with-_-for-::>.txt`. That step is
skipped entirely when callgrind or valgrind is not available.

<!-- soothfast:claim soothfast_measure::bench_sweep_evaluate.alloc.allocs <= 0 -->
The complexity-sweep verdict math that `--ratchet` and `--against-ref` run
on every comparison is itself zero-alloc, so gating a hundred items does not
cost a hundred allocations of bookkeeping.
<!-- /soothfast:claim -->
