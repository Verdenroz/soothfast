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
per-metric minimum across a side's rounds is its comparison value. This is
what `make gate` runs against `origin/master` for
every crate in `BENCH_CRATES`, and it is why merge-base gating has no
baseline file that can go stale in version control.

Before the reference side is measured, its locked `soothfast*` crates are
pinned to the versions in HEAD's `Cargo.lock`. The harness compiles into
both bench binaries, so without this a measurement-protocol change riding
along in a lockfile bump would be reported as a regression in *your* code.
Other dependencies are deliberately left as the reference locked them —
a slow `serde` bump is exactly what the gate exists to catch.

## Thresholds

| metric | threshold | notes |
|---|---|---|
| `perfcnt.instructions` | +5% | hard |
| `callgrind.ir` | +5% | hard |
| `walltime.median_ns` | +10%, or 3x the A/A noise floor when that is higher | hard. A threshold within ~2 sigma of the run's jitter cannot tell signal from noise, so it scales with the measured floor — noisy runners gate looser instead of not at all. |
| `alloc.allocs` / `alloc.bytes` | +5% | hard, integer ceiling |
| `asyncexec.polls` / `asyncexec.wakes` | +5% | hard, integer ceiling. Present only for `async fn` benches. Catches what instruction counts can miss: a future that polls twice as often does the same work per poll. |
| `buildcost.size_bytes` | +5% | hard |
| `buildcost.build_ms` | +25% | soft: printed as a warning, does not fail the gate |

## Ratchets

<!-- soothfast:bind soothfast_measure::sweep::evaluate -->
`--ratchet NAME` runs a second comparison against a long-lived baseline,
your last release for instance, after the primary one. It exists because ten
separate +4% regressions each pass the primary gate on their own. A ratchet
is an independent floor that catches the sum even when no single change
trips the per-commit threshold.
<!-- /soothfast:bind -->

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
