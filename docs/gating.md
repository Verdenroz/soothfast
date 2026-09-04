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
two sides' bench binaries have byte-identical loaded sections, code and
data alike, no measurable change is possible and the gate short-circuits to a
single cheap pass whose assertions still run. This is
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
is checked against the cache as well: byte-identical loaded sections under the
same conditions are the same measurement, whichever commit produced it.

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
| `walltime.median_ns` | +10%, or 3x the A/A noise floor when that is higher | hard. A threshold within ~2 sigma of the run's jitter cannot tell signal from noise, so it scales with the measured floor — noisy runners gate looser instead of not at all. A fail is also downgraded to a SOFT warning when the deterministic evidence contradicts it: unchanged fingerprint, instructions/Ir flat (within 0.5%, or within a fixed 150-instruction floor for small benchmarks), and alloc counts/bytes identical. Identical code doing identical work cannot have gotten slower — that is the clock measuring the machine. In `--against-ref` mode a fail must also reproduce in both interleaved round pairings; one lucky round on either side cannot manufacture it. |
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

So the gate pins it. Every measurement build compiles the workspace's own
crates with `codegen-units = 1`, on both sides of a comparison, whatever the
checked-out tree's profile says. One unit cannot be repartitioned. This costs
build time, which is the price of a comparison that means something;
`--codegen-units N` or `inherit` opts out, as does

```toml
[gate]
codegen-units = "inherit"
```

Dependencies are left alone. They are identical on both sides, so they
already partition identically, and pinning them as well would compile the
whole graph a second time for nothing.

Pinning cannot reach everything. `RUSTFLAGS`, `.cargo/config.toml` and the
rustc version all change codegen and none of them is the gate's to set. A
`[profile.bench.package.mycrate]` table does not override it: the gate's
value arrives as config, which outranks the manifest. Every run therefore
records what it was built with, printed as `build=<digest>` in the gate
banner. When the two sides disagree the gate says which field differs and
downgrades the deterministic counters to SOFT for that comparison:

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

## Accepting an intentional shift

A ratchet widens what passes for every future PR, permanently. Sometimes
what you need is the opposite: one bench legitimately costs more now, for
one reviewed reason, and the ongoing threshold should stay exactly as tight
as it was for everyone else.

```console
$ cargo soothfast gate accept -p mylib --against-ref origin/master \
    --justification "risk block now runs on zero-trade paths (bug fix)"
```

`gate accept` re-runs the same comparison `gate` would, and for every
metric that's currently failing, records its new value into a committed
`soothfast-gate.lock` — a top-level file, like `soothfast.lock`, not under
`.soothfast/`, so the acceptance and its justification land in the PR diff
where a reviewer actually sees them. A later `gate` run reports a covered
metric as `ACPT`, naming the lock file and the justification inline, instead
of `FAIL`:

```console
ACPT  mylib::bt_no_trade_risk_metrics instructions 288000.0 -> 412345.0 (+43.2%) per soothfast-gate.lock: "risk block now runs on zero-trade paths (bug fix)"
```

Two things keep this from becoming a second, quieter tolerance:

- **The ceiling is one-sided, anchored at the accepted value, not a band
  around it.** A metric at or under the accepted number always passes, no
  matter how far below the *original* reference it lands — a follow-up fix
  that claws back most of an accepted regression is an improvement, not a
  new review target.
- **It expires on its own.** The next `--save-baseline` (`gate` or
  `measure`) that captures the accepted value as the new normal removes the
  lock entry for that bench automatically. There is nothing to clean up by
  hand, and nothing left over once the accepted state ships.

`gate accept` refuses a bench that isn't currently failing, unless it also
clears `--headroom` (below) — accepting ahead of an actual regression would
otherwise let the lock file fill with speculative headroom instead of a
record of what really shifted. If the reference side moves past the
reviewed commit before the accept is consumed, the entry goes stale and the
next `gate` fails again with a `NOTE` pointing back at `gate accept` — the
review was against a specific old state, and that state moved.

## Accepting a near-threshold population

One change can legitimately move a broad population of benches at once — a
shared-code refactor, say — leaving some sitting just under the threshold
with ordinary run-to-run measurement noise. Since `accept` only ever
records what's failing *right now*, a different subset of that population
flips over on every fresh `gate` run, and the lock file never converges.

```console
$ cargo soothfast gate accept -p mylib --against-ref origin/master \
    --justification "shared parser rewrite" --headroom 1
```

`--headroom PCT` also records a passing metric within `PCT` points of its
threshold, as long as its delta first clears a noise floor (3x the same
deterministic-counter or walltime noise margin `gate` already scales its
own thresholds by) — a bench parked near the line by chance measurement
wobble, with no real delta, never qualifies. `--only`/`-p`/`--filter`
scoping still narrows what a headroom sweep can touch.

Naming a bench via `--only` that isn't in the recordable set (failing, or
headroom-eligible) is a skip, not a hard stop: `gate accept` still records
everything else, still writes `soothfast-gate.lock`, and prints a `WARN`
for the name it skipped. The exit code says which happened: `0` means every
named bench (or, with no `--only`, everything failing or headroom-eligible)
was recorded; `3` means at least one named bench was skipped, including the
case where none of them qualified — never `0`, so a CI step chained as
`gate accept ... || [ $? -eq 3 ]` can tell "some entries missing" from
"nothing happened."

Size `PCT` to the population's real jitter, not one run's wobble: a bench
sitting at +3.8% locally can measure +5.4% on a noisier CI backend, and a
band only 1 point wide can still miss it. Aim for 2–3x the observed
run-to-run swing across the benches you're sweeping, and when in doubt
widen rather than narrow — the noise-floor qualifier already keeps an
unrelated bench from riding along, so extra headroom costs little.

## Walltime alongside an accepted bench

Accepting a bench's instructions or `callgrind_ir` doesn't automatically
cover its walltime. Walltime is noisy enough on its own (15–20% run to run
on a shared CI runner) that freezing one accept-time sample as a ceiling
would make future noise more likely to trip, not less. Instead, once a
bench's deterministic counters are accepted and still match the reference,
walltime is allowed to run hot up to that accepted growth plus the normal
walltime threshold: counters accepted at +17% permit walltime up to roughly
+27% before it fails on its own. A walltime move that just tracks the
already-reviewed cost doesn't need a second review; a wall-clock-only
regression (lock contention, syscalls) past that ceiling still does.

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
