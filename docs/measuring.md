# Measuring

Annotate a function and it becomes a gated benchmark. This page is dogfood.
The prose is bound to soothfast's own source, the example below compiles and
runs `soothfast-measure`'s production code in CI with its real output spliced
in, and the numbers are checked claims evaluated against this build's own
measurement run.

## From annotation to measurement

```rust no_run
use soothfast::{bench, fixture, keep};

soothfast::bench_main!();

#[fixture]
fn samples_n(n: usize) -> Vec<f64> {
    let mut x: u64 = 42;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 11) as f64 / (1u64 << 53) as f64
        })
        .collect()
}

// Checked claims: complexity verified by size sweep, allocations exact.
#[bench(group = "stats", setup_sized = samples_n, sizes(1024, 4096, 16384),
        complexity = "n log n", alloc = 8,
        covers = "soothfast_measure::stats::summarize")]
fn bench_summarize(input: &[f64]) {
    let mut v = input.to_vec();
    keep(soothfast_measure::stats::summarize(keep(&mut v)));
}
```

That attribute is the real annotation from `soothfast-measure`'s own bench
suite. Every soothfast crate carries one and measures itself this way.
`cargo soothfast measure` runs it under every available backend, and `cargo
soothfast gate` compares against a baseline or merge-base and exits non-zero
on a regression.

## Benches inside the crate

A bench target is a separate crate, so it reaches only public items. To
measure a private or `pub(crate)` function, put the bench in the crate beside
it, behind a cargo feature named `soothfast`, and make the dependency
optional so the feature switches it on:

```toml ignore
[dependencies]
soothfast = { version = "0.8", optional = true }

[dev-dependencies]
soothfast = { version = "0.8", features = ["runner"] }
```

`cargo soothfast init -p PKG --internals` writes the dev-dependency, the bench
target and its stub, and the optional dependency. That dependency gives the
package a `soothfast` feature with no `[features]` line, and the stub's
`use my_crate as _;` links whatever the crate registers:

```rust ignore
#[cfg(feature = "soothfast")]
#[soothfast::bench(group = "wire", setup = wire_quote,
                   covers = "crate::adapters::yahoo::parse_quotes_batch")]
fn de_quote_batch(json: &String) {
    soothfast::keep(parse_quotes_batch(json));
}
```

`measure`, `gate`, `coverage` and the merge-base build turn the feature on
for a package that declares it, in the head tree and in the base tree alike,
so the comparison builds both sides the same way. A package that does not
declare it builds exactly as before. The bench is registered like any other,
so a change to the private function it runs moves that item and no other.

`covers` takes the crate-relative path. It is not a public item, so it does
not count toward `coverage measure`'s denominator, and it is not warned about
as long as its first module exists in the package's `src/` (a file, a
directory with `mod.rs`, or an inline module in the crate root).

## What callgrind's counts depend on

callgrind counts are exact for a given binary, valgrind and libc, but not for
a given binary alone. glibc picks its `memcpy`, `strlen` and `memchr` variants
at load time (ifunc) from the CPU features it sees, and valgrind passes most
of the host's features through to the guest. An unchanged bench can therefore
count differently on a runner with another CPU model, by up to 6% on
soothfast's own benches, and the difference moves with the items that lean on
string and memory routines.

The harness always hides AVX-512 from the measured process with
`GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX512F,...`, because valgrind cannot
decode it. `cpu` under `[gate]` hides more:

```toml
[gate]
cpu = "portable"   # or "default", or ["AVX2", "ERMS"]
```

- `"default"` hides AVX-512 and nothing else. It is what you get when `cpu`
  is unset, and it keeps every existing baseline and cached run valid.
- `"portable"` also hides every CPU feature above x86-64-v2 that glibc
  dispatches on, and pins the per-model preferred bits those dispatchers
  read, so the same routines run whatever the runner's CPU.
- A list hides AVX-512 and the names it holds. Names are glibc's, spelled as
  `glibc.cpu.hwcaps` takes them: letters, digits and underscores, and never
  an AVX-512 name, which is always hidden.

`--cpu default|portable|NAME,NAME` on `measure` and `gate` sets the same
thing and wins over the file. A package can set its own in
`[package.NAME]` or `[package.NAME.gate]`, resolved the way `features` is:
the flag, then the group table, then the package table, then `[gate]`.

The portable preset hides these names. glibc 2.41 and 2.42 accept every one
of them in `glibc.cpu.hwcaps`; they come from `sysdeps/x86/cpu-tunables.c`
in the glibc source (the `CHECK_GLIBC_IFUNC_*` entries), and `ld.so
--list-tunables` shows the `glibc.cpu.hwcaps` tunable that takes them. Back
to 2.35 only `Avoid_STOSB` and `Avoid_Non_Temporal_Memset` are missing. The
CPU features are `AVX`, `AVX2`, `BMI1`, `BMI2`, `ERMS`, `FMA`, `FMA4`,
`LZCNT`, `MOVBE`, `RTM` and `XSAVEC` (the dynamic linker's lazy-binding
trampoline selects on the last). The preferred bits are
`AVX_Fast_Unaligned_Load`, `Avoid_Non_Temporal_Memset`, `Avoid_STOSB`,
`Fast_Copy_Backward`, `Fast_Rep_String`, `Fast_Unaligned_Copy`,
`Fast_Unaligned_Load`, `MathVec_Prefer_No_AVX512`, `Prefer_ERMS`,
`Prefer_FSRM`, `Prefer_No_AVX512`, `Prefer_No_VZEROUPPER`,
`Prefer_PMINUB_for_stringop`, `Slow_BSF` and `Slow_SSE4_2`. What stays is the
x86-64-v2 level: `CMOV`, `CX8`, `POPCNT`, `SSE2`, `SSSE3`, `SSE4_1` and
`SSE4_2`. A name a glibc does not know is ignored by it.

The trade-off is that `"portable"` hides fast paths. Its absolute counts
differ from a default run, and from what your users' machines execute, since
those use AVX2 and `rep movsb`. The gate compares two measurements under the
same setting, so its relative verdict is unaffected, and the counts stop
moving when the runner image changes CPU model.

The mask is part of what a measurement is. Under the default nothing about a
run's identity changes. Under any other mask it joins the run cache and item
cache keys, the stored release measurement of `report`, and the build stamp
saved with every baseline, so runs under different masks never reuse each
other. The cache line names it:

```console
gate: run cache keyed on callgrind (guest 80eb05cd5d172b67 glibc 2.41 88c81164b46fbdd3 valgrind-3.24.0 method one-process hiding AVX,AVX2,...)
```

`cpu` has no effect off x86-64 and glibc, where there is no `hwcaps` tunable
to set (musl, aarch64). The setting is accepted, one line says it was
ignored, and measurements key as under the default. The bench harness has to
be this release or newer: an older one exits with `unknown runner arg
"--cpu-hide"` rather than measure under a mask it ignores. That includes the
last release when `report changelog` measures it: until a release ships with
a harness that knows the setting, the changelog's perf delta against that
release is left out with a warning (see [gating](gating.md)).

## What tests and benches execute

`coverage measure` counts an item as measured when a bench names it in
`covers`. Nothing there checks that the bench runs it. `coverage executed`
asks the question directly: it builds the package's tests, and separately its
bench target, with `-C instrument-coverage`, runs them, and reports which
public items any test ran (`tested`) and which any bench ran (`measured`).

```console
$ cargo soothfast coverage executed -p soothfast-registry
tested measured soothfast_registry::Bencher::iter (function)
       measured soothfast_registry::block_on_counting (function)
                soothfast_registry::Assertions::new (function)
tested          soothfast_registry::resolve_mock_seam (function)
coverage executed: 7/34 public item(s) tested
coverage executed: 9/34 public item(s) measured
```

`--tests` or `--benches` runs one column, and `--json` prints
`{"package", "items": [{"path", "kind", "tested", "measured"}]}` with a
column left out when it did not run. `--bench NAME` picks the bench target
(default `soothfast`) and `--features` the feature set.

The denominator is the public functions, structs, enums, unions and traits of
the package, plus inherent methods. Trait-impl methods are not items of their
own, and `#[doc(hidden)]` items are not in the rustdoc surface.

### How an item counts as executed

An item is executed when any region with a nonzero count lies on a line of its
span, in the same file. The span comes from rustdoc. A function or inherent
method has its own span. A struct, enum, union or trait has its own span plus
the span of every method in every impl block for it, trait impls included, so
a type counts as executed when any of its methods ran. That is a deliberate
choice: a type with no methods is never executed, and neither is one that
code only constructs or reads fields of. A generic is credited at the generic
item, whatever the instantiation.

Mapping is by source span rather than by symbol, so an item reached only
through an inlined call is still credited: the counters sit in the compiler's
MIR, ahead of inlining.

### What it does not see

- Doc tests are not instrumented, so an item only a doc test runs is untested.
- Code that runs only at compile time has no runtime region. A `const fn`
  body evaluated in a static, such as `MeasuredItem::new` in the macro
  statics, reads as not executed.
- Benches run under the dev profile, with the runner's `alloc` backend, which
  runs each item once. Code behind `cfg(debug_assertions)` can credit an item
  the bench profile never reaches. A bench run that exits nonzero, such as a
  claim that holds only under the bench profile, still counts.
- A function a macro generates has its regions on the lines of the macro
  definition, while its span is the invocation, so it reads as not executed.
- The build takes its flags from `CARGO_ENCODED_RUSTFLAGS` or `RUSTFLAGS`
  plus `-Cinstrument-coverage`. Flags set only in `.cargo/config.toml` are not
  applied.

### Cost

The instrumented build has its own target directory,
`target/soothfast-coverage`, and the flags reach only the `--target <host>`
artifacts of that cargo, so build scripts and proc macros are not
instrumented and the artifacts `gate` and `measure` use are not touched.
Profiles go to a temporary directory and are merged by `llvm-profdata`, then
`llvm-cov export` writes the regions. Both tools come from the
`llvm-tools` rustup component; the command names `rustup component add
llvm-tools` when they are missing.

The executed regions are kept under `.soothfast/coverage/`, keyed on the
bytes of the instrumented executables and on the inputs the `test` pass cache
keys on: the package's files, `[test] inputs`, the `[test] env` values and the
compiler version. An unchanged tree reruns nothing: the rerun builds, finds
the key unchanged, and reads the stored regions.

## The statistics underneath

<!-- soothfast:bind soothfast_measure::stats::summarize -->
Every walltime measurement is reduced with `summarize`, which reports median
and median absolute deviation rather than mean and standard deviation. That
way a single scheduler blip cannot drag the summary. It sorts in place and
never sees NaN, since the samples are durations.
<!-- /soothfast:bind -->

This example runs the production function in CI. The output block below is
its captured stdout, spliced in by `cargo soothfast docs capture`, so it
cannot rot: it is produced rather than written. The `covers=` tag on the
fence attaches the bind above to this exact block, so its lock state shows
up here instead of as a separate marker:

```rust capture-output covers=soothfast_measure::stats::summarize
fn main() {
    let mut samples = vec![9.0, 1.0, 5.0, 3.0, 7.0];
    let s = soothfast_measure::stats::summarize(&mut samples);
    println!("median={} mad={}", s.median, s.mad);
    println!("min={} max={}", s.min, s.max);
}
```

```text soothfast-output
median=5 mad=2
min=1 max=9
```

## Numbers in prose are gated facts

<!-- soothfast:claim soothfast_measure::bench_summarize.alloc.allocs <= 8 -->
Summarizing a sample set costs at most eight allocations.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_registry::bench_fnv1a.alloc.allocs <= 0 -->
Fingerprint hashing allocates nothing at all.
<!-- /soothfast:claim -->

A claim can also bound the bytes allocated, which `alloc = N` does not
cover: a rewrite that keeps three allocations but swaps an 8-byte buffer for
a 16-byte one passes the count and changes the size.

```markdown ignore
<!-- soothfast:claim soothfast_registry::bench_fnv1a.alloc.bytes <= 0 -->
Fingerprint hashing allocates no bytes.
<!-- /soothfast:claim -->
```

The gate already compares `alloc_bytes` against the merge-base on its own
(+5%), separately from the allocation count, so an unclaimed size change
still fails it.

If either sentence stops being true, or the output above stops matching the
real one, `cargo soothfast docs check` fails CI. It is the same mechanism
that keeps every page on this site honest.
