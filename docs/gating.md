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
single pass whose assertions still run. Under callgrind that pass plans the
one binary per item: stored items are spliced in with their claims, only the
rest are measured, and what is measured is stored along with the run. The
first identical run, such as the first after an upgrade, therefore costs one
callgrind pass over one side's items. A repeat finds every item stored and
measures nothing, and the next push that changes code measures only what it
changed. The pass carries the gating counters, so `--save-baseline` saves it.
With `--no-reuse-base`, `--no-item-cache`, or a backend other than callgrind
the gate takes the cheap walltime pass instead, which stores nothing and is
not saved as a baseline. Under `SOOTHFAST_RECORD=1` that fallback keeps the
counters and stores its run. This is
what `make gate` runs against `origin/master` for
every crate in `BENCH_CRATES`, and it is why merge-base gating has no
baseline file that can go stale in version control.

The merge-base builds into its own target dir, `soothfast-worktree` inside
yours, because a workspace member compiles to the same artifact names from
either checkout. Before that build the gate copies in every dependency HEAD
already compiled:

```console
gate: seeded 293 dependency units (1013.9 MiB) from HEAD's target dir in 0.1s
```

Cargo then finds the merge-base's unchanged dependencies fresh and compiles
only the workspace members and whatever `Cargo.lock` changed. The copy is a
reflink on filesystems that support it and a full copy elsewhere.

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
outside it that moves the numbers:

- the `rustc` version, the pinned `codegen-units`, and the flags reaching
  rustc;
- the locked `soothfast` versions the reference is pinned to;
- the measurement scope: `-p`, `--features`, `--bench`, `--backend`,
  `--samples`, `--filter`;
- the gating backend that actually measured, which under the default `auto`
  is whichever this host can run, so a `perfcnt` run is never served to a
  gate that fell back to `callgrind`, or the reverse;
- the machine the counts came from. For `perfcnt` and `walltime` that is the
  CPU model, since retired instructions and time differ across
  microarchitectures. For `callgrind` it is the CPU valgrind presents to the
  guest, the libc file the guest loads (its contents, so a distro rebuild
  that keeps the version number still counts), and the valgrind version
  string, which a distro-patched valgrind may keep unchanged. valgrind
  synthesizes the same guest CPU on different host models, so callgrind runs
  from differently named CI hosts share a key, while a machine with another
  glibc or valgrind does not;
- for `callgrind`, the method the harness counts with, `one-process` or
  `two-process` (`SOOTHFAST_CALLGRIND_TWO_PROCESS=1` forces the latter). The
  two differ slightly (teardown, the dump requests), so a count taken one way
  is never served to a gate that counts the other. A harness that does not
  name its method counts as `two-process`.

A merge-base with a stored run under the same key is served from there, and
the worktree is never built:

```console
gate: reusing the measured merge-base 1aa6c4de39f9408a4279b9f9d78a6a8ed0de6a39
```

The gate asks HEAD's bench binary which backend it resolves (and, for
callgrind, what its guest sees) before any lookup. A harness too old to
answer, or one that cannot run a named `--backend` here, turns the cache off
for that invocation rather than keying on a guess:

```console
gate: run cache disabled for this invocation: bench harness does not answer --env (soothfast runner: unknown runner arg "--env") (harness soothfast@0.3.2, ..., CLI ...)
```

Each stored run records what it was keyed on under `measured_on` (for
callgrind, the guest keys and `callgrind_method`), so two runs that should
have matched can be compared directly. A change to any keyed
condition, including a CI image that updates glibc or valgrind, starts a new
key; runs stored under the old one are not migrated, and age out. The same
holds for runs stored by cargo-soothfast 0.3.2 and earlier, whose keys named
neither the resolved backend nor the callgrind guest.

The commit is not the only key. A merge-base that was never gated has no run
under its commit, which on master is most of them unless every push records
one (see below). Once such a reference is built, its machine code
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

A stored run is reused by every later gate with its key, so a single bad
reading would fail all of them. callgrind counts are exact, but perfcnt
counts are not: before a perfcnt run is stored, its instruction counts are
read a second time, and a third time where the first two disagree by more
than the gate's counter tolerance (0.5%, or 150 instructions for small
counts). The run is stored with the reading two of them agree on, or not at
all:

```console
gate: not storing the merge-base run: pkg::bench instructions read 3271181 to 3504110 (7.1% apart)
```

This settles what is stored, not what is compared: the gate in progress
still compares its first readings. Two readings that agree on the same wrong
value would still be stored; a bad perfcnt reading has not been seen to
repeat within one run. When a gate fails against a reused perfcnt reference,
it says so before its verdict:

```console
gate: reference reused from 1aa6c4de39f9408a4279b9f9d78a6a8ed0de6a39; rerun with --no-reuse-base to re-measure
```

`cargo soothfast measure` reads the same store. When callgrind gates, it looks
up a run stored for HEAD's bench binary under the same key, and on a hit takes
Ir and allocation counts from it while measuring walltime again in a
timing-only pass:

```console
measure: reusing counters for binary 3f1c9a0e5b7d2c48; timing measured fresh
```

Timing is always measured by the process that saves it, so a
`--save-baseline` never carries walltime from another run or host, and every
bench assertion (`alloc`, `p99`, `complexity`) is evaluated on fresh data, as
in a full run. The saved Ir and allocation counts are the stored run's, which
callgrind makes exact, and each saved item records what it reused under
`reused`. perfcnt
counts take seconds to measure, so under perfcnt `measure` always runs in
full; a run that lacks callgrind counts is treated as a miss; `--no-reuse`
measures in full regardless. A full `measure` stores its run the way a gate
stores HEAD's, so a job that saves the runs cache also feeds it from `measure`.

For this to pay off in CI, the default branch has to measure its own commits,
and `.soothfast/runs/` has to outlive the job. The [action](ci.md) does the
first: on a push to the default branch with `gate: true` it runs `cargo
soothfast gate -p PKG --against-ref HEAD^`, which stores HEAD's run under its
commit and its bench binary. A pull request whose merge-base is that commit
reuses it by commit; one whose merge-base came later but builds the same
binary, such as a bot commit that only touched `CHANGELOG.md`, reuses it by
binary. A run is stored under the commit only when `git status --porcelain`
is empty, so `.soothfast/` must be in `.gitignore`; otherwise only the binary
key is written.

When HEAD builds the same binary as HEAD^ and nothing is stored for it yet,
as on the first push after adopting the action, the recording gate
(`SOOTHFAST_RECORD=1`, set by the action) takes one pass with the gating
counters and stores it. A pull request gate in that state keeps its
timing-only pass and stores nothing, since no counter can move and a run
without them must never serve as a reference. cargo-soothfast 0.3.2 and
earlier skip the counters on that pass and store nothing, so there a
docs-only push records no run until a later push changes the binary. The
action installs the CLI version in your `Cargo.lock`, so bumping the action
alone does not change this.

The action caches `.soothfast/runs/`, the per-item store, and the link maps
that [per-item reuse](#per-item-reuse) reads. A default-branch push saves
them under its own commit, and a pull request restores exactly the entry its
merge-base saved. When that push has not finished, it restores its own newest
entry, then the default branch's newest. A fallback still serves every item
whose code is unchanged, since items are keyed by the code they ran, not by
commit.

Set `cache: "false"` to manage the cache yourself. Keep the key unique per
commit with a prefix in `restore-keys`: an exact key is never rewritten once
saved, so every later commit would miss.

## Per-item reuse

The run cache reuses a whole run. Under `callgrind`, the gate and `measure`
can also reuse a single item, keyed on the machine code the item executed
rather than on the commit. Callgrind already lists every function a run
executes; the runner reports that set, and the CLI hashes those functions in
each bench binary. A side whose executed code a stored entry covers does not
measure that item: the entry's metrics and assertion verdicts are placed on
that side, and only the remaining items run (`--id` on the runner). An item
whose code is unchanged reuses one entry on both sides, so its delta is zero.
A changed item reuses whatever each side measured before, such as HEAD's run
from an earlier push to the same pull request. Its walltime then comes from
another run, so it is reported but never fails the gate; the counters still
gate it.

```console
gate: items a stored measurement covers: head 14/16, base 16/16, measuring 2 (2 code changed)
```

The reasons in parentheses say why the rest missed: `code changed`, `claims
changed` (the item's fingerprint), `unhashable here` (a function that cannot
be resolved), `never stored` (nothing in this cache scope, often a cache that
was not restored) and `binary unreadable`.

To see why an item's code changed between the two sides of a gate, set
`SOOTHFAST_EXPLAIN=pkg::item` (or `all`) on the gate. For each such item it
lists the functions of its newest stored entry that hash differently in
HEAD's and the merge-base's binaries, and the thread-local image when that
differs.
When both sides are one binary (identical bench binaries), it lists instead
the functions of the newest entry that binary cannot hash.

The whole-run keys above are tried first. Per-item reuse applies on the full
measurement path and when `measure` finds no run stored for its binary. It
is a miss, never a guess, whenever anything cannot be resolved. An entry is
used only if all of these hold:

- every executed function of the bench binary exists there by its exact
  symbol (`.llvm.N` suffixes dropped) and normalizes to the same
  instructions. Addresses are normalized out: branch targets and
  rip-relative displacements become symbols, and a data reference becomes
  the content of the object it points into;
- relocated data is hashed by what it points at, so a vtable or function
  table whose target changed hashes differently even when its bytes in the
  file do not (a linker may leave a relocated slot zero or filled with an
  address);
- every other object the item ran in (libc, the loader, valgrind's
  preload) has the same content digest. `???`, valgrind's own trampolines,
  is covered by the valgrind version in the run cache key;
- the item's fingerprint is unchanged, since the claims a stored assertion
  was judged against (`alloc = N`, `p99 = ...`) are not part of the code;
- the thread-local initial image (`.tdata`, and the size of `.tbss`) is
  unchanged. Code reads it through `%fs` offsets that name no address, so it
  is part of every key.

An item whose claims failed is never stored, so a failure is measured again
rather than replayed. A binary not linked position-independent is never
reused: its code can name data by absolute address, which is not resolved.

An item with `sizes(...)` and a `complexity` claim runs under callgrind once
per size, since a sweep can reach code the largest size never executes. The
claim is fitted on those exact Ir counts, and the item is stored with the
union of the functions every size executed. Only `callgrind` is reused:
`perfcnt` and `walltime` hosts behave as before, and still fit the claim on
instructions and wall time respectively.

### Where data begins and ends

A data reference with a sized symbol hashes that symbol. rustc and LLVM emit
most constants (string literals, panic locations, constant pools, jump
tables) under local labels, which the symbol table does not keep. For those
the CLI reads the linker's map: each input section is a real object, since
rustc gives every constant its own section and code cannot read past the one
it addresses. A reference then hashes the input section it lands in, not the
whole output section. A merged pool (`SHF_MERGE`) is one input section in the
map; see below for how the fixed-size ones are split.

The map comes from a second link of the bench target alone
(`cargo rustc --bench ... -- -C link-arg=-Wl,-Map=...`), written next to the
measured binary as a separate artifact of the same bytes: dependencies are
not rebuilt and the measured binary is not touched. The CLI checks that the
two have the same loaded-section digest before it trusts the map, and keeps
the map and its extents under `.soothfast/maps/`, named by that digest, so a
map is never read for another binary. lld, mold and GNU ld maps are read. A
map that cannot be built or read is a miss, not a guess:

```console
gate: no link map for <binary> (unrecognized map format); unnamed constants hash as whole sections, and code that reads a duplicate-named local static or writable data with no symbol or label is left unhashed
```

A reference outside every input section falls back to the whole output
section the same way. On aarch64, `adrp`+`add` immediates carry layout and
only cause misses.

A pool of fixed-size constants (`.rodata.cst8`, `.rodata.cst16`) is split
into its entries, so a reference hashes only the entry it falls in. That
assumes code never reads across two entries through one reference, which
holds because LLVM gives each pool constant its own label and the linker
merges whole entries. String pools (`.rodata.str*`) stay one object.

A writable object (`.data`, `.bss`) is told from another by its symbol name,
or by the link map's object and section for one with no symbol, as well as
by its bytes: two zeroed statics hold the same bytes, and code that reads
one is not code that reads the other. A symbol whose name no other data
symbol or label of the binary has is its own identity. Two that share a name
(C `static`s of the same name in two files) are told apart by the input
section each is defined in, named by the object's path below the target
directory and its archive member, so `a/util.o` and `b/util.o` differ. Code
symbols do not count towards a duplicate. A new static of the same name
elsewhere makes both duplicates, which costs a miss until the link map names
them and never a wrong hit. Read-only data, and data the loader seals after
relocation (the `PT_GNU_RELRO` range, read from the program headers), is
keyed by its bytes alone; a binary with no such range prints a line saying
so and treats relocated data as writable. Data in a writable section with no
symbol or input around it is named by the labels `nm` lists at its address
without a size (`__dso_handle`, `__TMC_END__`), provided their names are
unique in the binary, and is keyed with the whole section so reads on either
side of the label stay covered. Without a link map, a function that reads a
duplicate-named local static, or writable data with neither a symbol nor
such a label, is left unhashed. The object part of an input section's name
follows the compiler's code generation units, which can move on an unrelated
edit. That costs a miss and never a wrong hit, which is why the symbol name
is used whenever it is unique.

A jump table stores code offsets as 32-bit integers, so its bytes change
whenever code moves. Where the instructions prove how a table is read, it is
hashed as the places in the function it jumps to instead of as bytes. The
read must have this shape, in one basic block:

```text
movslq (%rA,%rI,4),%rB
add    %rA,%rB
jmp    *%rB
```

and every definition of `%rA` that reaches the load, along every path from
the function's entry, must be a `lea` of the same table. The base may be
loaded once near the top and kept across calls in a callee-saved register; a
call clobbers the caller-saved ones. The proof builds the function's
control-flow graph, in which a table jump has an edge to each place its table
names, and it needs the graph to be complete: an indirect jump that is not a
proven table jump, or a branch to an address that is no instruction of the
function (other than a tail call), leaves the function with no proof. Code
no edge reaches is a landing pad the unwinder enters from a call, with the
callee-saved registers restored to their values at that call, so it starts
from the join of every call in the function for those registers and from
nothing known for the rest. Alignment padding is not code anyone enters. The input section holding the table must start at a proven base and
hold only words that land inside the function, measured from the nearest
proven base before them. Anything else hashes as bytes. This reads x86-64
assembly only; on other architectures every table hashes as bytes.

The same load and add with no jump after them read a table of offsets to
data, such as the names an enum's `Debug` impl picks from or a header name
table. Its words move whenever the strings do. Proven the same way, it is
hashed as the data each word names, so an edit elsewhere that shifts the
strings leaves the function's key alone. A word that does not land on a
symbol, a link-map input or a pool entry, only on the section around it,
leaves the table as bytes.

`__stop_SEC` symbols that the linker defines are named, and the section they
end is hashed, but the section that happens to follow is not: a pointer one
past the end is never dereferenced.

What still misses, and why:

- code that changed, which is the point;
- a function that keeps its jump-table base in a callee-saved register it
  also uses for another value across calls, where a landing pad that does
  not end in `_Unwind_Resume` rejoins the code before the table load. Which
  call unwinds to which pad is in the exception tables, which are not read,
  so every call's value reaches the pad. A pad that ends in `_Unwind_Resume`
  rejoins nothing, which is how `std`'s `Once::call` proves and an edit that
  moves code still reuses the items it does not touch;
- code that reads a section whose bytes are layout, such as
  `.eh_frame_hdr`, through a pointer that is not a `__stop_` symbol;
- a constant changed inside a string pool, or any data without a sized
  symbol or a link-map section to bound it;
- an unreadable map, or a binary that `objdump` cannot disassemble.

All of these are sound and cost hits. An edit that keeps the size of code the
same, such as changing a constant, leaves the rest of the executed code
alone, and items that do not read it are reused.

### The commit-key path

When the reference is served by commit key, the gate still measures HEAD. The
item store is consulted for HEAD alone: an item whose executed code and
fingerprint match a stored entry is spliced in, the rest are measured, and
each is compared against the real reference number. Its delta is not assumed
to be zero.

### Reused items and switches

A reused item reports the stored numbers on both sides, so it shows an exact
zero delta where a fresh run of the two checkouts would differ by the noise
callgrind sees from different working directories (about 0.1%). It keeps the
walltime it was stored with, and a baseline saved
from the run marks it under `reused` with `from: "item cache"`. `--no-item-cache`,
`--no-reuse-base` and `SOOTHFAST_RECORD=1` measure every item; a recording run
still stores what it measured, so pull requests find it. A harness that predates `--id`
turns it off for that invocation, the way the run cache is turned off:

```console
gate: per-item reuse disabled for this invocation: bench harness does not take --id (harness soothfast@0.3.4, ..., CLI ...)
```

Entries live under `.soothfast/items/`, keyed by the same conditions as a
run minus the commit and `--filter`, up to eight per item.

## Sharding

`gate --shard I/N` gates only the items shard I of N owns. A hash of the
item id picks the shard, so each item belongs to exactly one, and an item
deleted from the registry is still reported GONE by its shard. Run the
shards as a job matrix to split a measurement too large for one runner:

```yaml
strategy:
  matrix:
    shard: [1, 2, 3, 4]
steps:
  - uses: Verdenroz/soothfast@<tag-or-sha>
    with:
      shard: ${{ matrix.shard }}/4
```

Each shard posts its own comment and fails on its own items. On the default
branch only shard 1 records a reference: it measures every item and saves
under the key pull requests restore, and each shard compares its own items
against that run. On a pull request each shard saves what it measured under
its own key, for a rerun.
`--shard` and `--save-baseline` cannot be combined, since a baseline must
hold every item.

## Thresholds

| metric | threshold | notes |
|---|---|---|
| `perfcnt.instructions` | +5% | hard |
| `callgrind.ir` | +5% | hard |
| `walltime.median_ns` | +10%, or 3x the A/A noise floor when that is higher | hard. A threshold within ~2 sigma of the run's jitter cannot tell signal from noise, so it scales with the measured floor — noisy runners gate looser instead of not at all. A fail is also downgraded to a SOFT warning when the deterministic evidence contradicts it: unchanged fingerprint, instructions/Ir flat (within 0.5%, or within a fixed 150-instruction floor for small benchmarks), and alloc counts/bytes identical. Identical code doing identical work cannot have gotten slower — that is the clock measuring the machine. In `--against-ref` mode a fail must also reproduce in both interleaved round pairings; one lucky round on either side cannot manufacture it. Soft under `[gate] walltime = "advisory"`. |
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

A shared CI runner can be too noisy for any walltime threshold to hold. On
one, make walltime advisory:

```toml
[gate]
walltime = "advisory"
```

Walltime slower than the reference then prints as `SOFT` with the reason
`walltime is advisory in soothfast.toml` and never fails the gate. `gate
accept` records no walltime entry for it. Instructions, Ir, allocations, polls
and wakes, and size gate exactly as before, and so does a bench's own `p99`
claim: that is a number the bench declares, not a comparison with the
reference. `--against-ref` then skips the two timing-only rounds and measures
each side once, head first. A baseline it saves, and a run it stores for later
gates to reuse, holds one walltime sample per item instead of the minimum of
two. The default, `"hard"`, keeps the row above.

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

## When the harness moves under a PR

The same argument applies to soothfast itself. A `--against-ref` run builds
the merge-base's bench binary from the merge-base's lockfile, so a PR that
bumps soothfast has the reference embedding the old measurement harness and
HEAD embedding the new one. The harness is inside the measured loop, and a
protocol change of a few hundred instructions is several percent of a 30K
bench. That delta is the harness, not your code.

So the gate pins it. Before building the reference it rewrites the
worktree's lockfile to HEAD's soothfast versions, and when the merge-base's
own requirement excludes them (`soothfast = "0.2"` cannot take `0.3.1`) it
widens that requirement in the worktree's manifests and retries. The
worktree is a throwaway checkout under `.soothfast/worktrees/`, so nothing
in your tree is touched.

```console
gate: pinning soothfast 0.2.0 -> 0.3.1 in the merge-base worktree (harness must match HEAD)
gate: widened the merge-base's soothfast requirements to retry the pin
```

Pinning can still fail: an offline runner, a yanked version, a requirement
the rewrite does not recognise. The gate then says so, in the banner, in the
verdict line, and in `.soothfast/triage/harness-mismatch.txt`:

```console
gate: HARNESS MISMATCH — the merge-base could not be pinned to HEAD's soothfast (soothfast 0.2.0 -> 0.3.1); deltas below may be the harness, not this change
FAIL  demo::parse instructions 30104.0 -> 31680.0 (+5.2%)
gate: FAILED (1 regression(s)) — measured against a different harness (soothfast 0.2.0 -> 0.3.1); pass --allow-harness-change if the harness bump explains it
```

A regression on such a run fails, and `--allow-harness-change` is what lets
it pass. Reach for it once you have read the deltas and they are the shape
of a harness bump: a small uniform shift across every item, nothing
concentrated in what the PR touched. It waives comparison failures only.
Checked claims hold HEAD against its own declared numbers, so they fail
through it.

Under callgrind the gate also compares the counting method of the two bench
binaries. A merge-base that counts another way, or whose method cannot be
read, reads as a mismatch too:

```console
gate: HARNESS MISMATCH — base measured with callgrind method two-process, head with one-process; comparison is not conclusive
```

Such a merge-base is measured in full and nothing of it is stored, since its
counts would otherwise be filed under HEAD's method.

A run that is otherwise clean still reports the mismatch. Both sides being
measured with different instruments is not evidence in either direction,
and a quiet pass would be as wrong as a red verdict.

## Features

A bench target declaring `required-features`, or a crate whose hot paths sit
behind a feature, needs `--features` on every run. `soothfast.toml` carries it
once, for `gate`, `measure` and `spec gen` alike:

```toml
[gate]
features = "bench-gate"
```

An explicit `--features` on the command line wins; the file only fills in what
the command line left unset. The action's `features` input sets the same thing
for CI, so a repository that configures the file gets one behaviour from a
hand-run `cargo soothfast gate` and from the action.

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
regression. The identical-binaries short circuit saves it whenever its pass
carried the gating counters: the per-item callgrind pass, or the fallback
under `SOOTHFAST_RECORD=1`. The cheap walltime pass collects none, so it
skips the save and the previously saved baseline stays.

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
