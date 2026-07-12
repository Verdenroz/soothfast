# Gating

Measuring produces numbers; gating decides whether they're acceptable.
`cargo soothfast gate` re-measures your code, compares it against a
reference, and exits non-zero the moment any metric regresses past a
threshold — the mechanism that turns a benchmark into a CI check.

## Picking a reference

Three ways to choose what "acceptable" means, in order of how CI actually
uses them:

```console
$ cargo soothfast gate -p mylib                                    # vs a saved --baseline (default "base")
$ cargo soothfast gate -p mylib --against-ref origin/master         # vs the merge-base, measured live
$ cargo soothfast gate -p mylib --backend buildcost --against-ref origin/master   # binary size / build time
```

`--against-ref` never reads a file: it measures HEAD and the merge-base of
`REF` in a temporary git worktree, twice each, interleaved head/base/head/base
("tango" order to spread out thermal and scheduler drift) — then takes the
per-metric minimum across each side's two rounds before comparing. That's
what `make gate` runs against `origin/master` for every crate in
`BENCH_CRATES`, which is why there's no baseline file to go stale in version
control for merge-base gating.

## Thresholds

| metric | threshold | notes |
|---|---|---|
| `perfcnt.instructions` | +5% | hard |
| `callgrind.ir` | +5% | hard |
| `walltime.median_ns` | +10% | hard, but skipped with a warning when the A/A noise floor is ≥5% — a threshold half the noise floor can't tell signal from jitter |
| `alloc.allocs` / `alloc.bytes` | +5% | hard, integer ceiling |
| `buildcost.size_bytes` | +5% | hard |
| `buildcost.build_ms` | +25% | **soft** — printed as a warning, doesn't fail the gate |

## Ratchets

<!-- soothfast:bind soothfast_measure::sweep::evaluate -->
`--ratchet NAME` runs a second comparison against a long-lived baseline (a
last release, say) after the primary one. It exists because ten separate
+4% regressions each pass the primary gate on their own — a ratchet is an
independent floor that catches the sum even when no single change trips the
per-commit threshold.
<!-- /soothfast:bind -->

## `--deps`: advisory, not a different check

`--deps` doesn't change what gets gated. It only adds one warning: if any
measured item's source fingerprint changed relative to the reference,
`cargo soothfast gate --deps` prints `WARN: --deps given but item
fingerprints changed — this is not a pure dependency bump`. The threshold
comparison runs exactly the same either way — `--deps` just tells you
whether a regression is explained by your own code changing or purely by
whatever moved underneath it.

## When it fails

Every run writes `.soothfast/gate-status.json` (`{passed, failures, unix}`)
— `cargo soothfast report render` reads it to render the pass/fail badge.
On failure, the first three failing items each get a callgrind per-function
report written to `.soothfast/triage/<item-with-_-for-::>.txt` (skipped
entirely if callgrind/valgrind isn't available on the machine).

<!-- soothfast:claim soothfast_measure::bench_sweep_evaluate.alloc.allocs <= 0 -->
The complexity-sweep verdict math that `--ratchet`/`--against-ref` runs on
every comparison is itself zero-alloc — gating a hundred items doesn't cost
a hundred allocations of bookkeeping.
<!-- /soothfast:claim -->
