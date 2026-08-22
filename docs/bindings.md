# Native bindings

`cargo soothfast bind` turns an annotated Rust surface into packages other
languages install: a wheel for Python, a `.wasm` plus its JavaScript glue for
the browser and Node. There is no HTTP boundary anywhere in it. The generated
code calls your functions in-process, the way `py-polars` calls polars.

This is the sibling of `cargo soothfast sdk`, not a replacement. An SDK is a
client for something you *serve*; a binding is your library, in another
language.

## Annotate once, configure per language

`#[soothfast::export]` says what crosses over. It never names a language, so
adding one is a config line and no source edit.

<!-- soothfast:export soothfast_demo::Summary -->
```rust ignore
/// A robust summary of one sample set.
#[soothfast::export]
pub struct Summary {
    pub median: f64,
    pub mad: f64,
    pub min: f64,
    pub max: f64,
}

#[soothfast::export]
impl Summary {
    /// Summarize a sample set. Fails on an empty one, which has no median.
    pub fn new(samples: Vec<f64>) -> Result<Self, String> { ... }

    /// Read one statistic by name.
    pub fn get(&self, metric: Metric) -> f64 { ... }
}
```
<!-- /soothfast:export -->

It applies to a function, a struct, an enum, or an inherent `impl` block. An
`impl` registers every `pub fn` in it. Whatever the annotation binds stays
ordinary Rust, callable the way it always was:

```rust
use soothfast_demo::{Metric, Summary, fingerprint};

let summary = Summary::new(vec![3.0, 1.0, 5.0, 4.0]).expect("summarizes");
assert_eq!(summary.median, 3.5);
assert_eq!(summary.get(Metric::Min), 1.0);
assert!(Summary::new(Vec::new()).is_err());

// The frozen FNV-1a soothfast records in baselines and lockfiles.
assert_eq!(fingerprint(b"foobar".to_vec()), 0x8594_4171_f739_67e8);
```

- `#[soothfast::export(skip(wasm))]` narrows to the languages named.
- `#[soothfast::export(skip)]` on one method leaves it out entirely.
- `#[soothfast::export(constructor)]` picks a builder other than `new`.

Each language then gets a `[[bind]]` entry in `soothfast.toml`:

```toml
[[bind]]
lang = "python"
out = "bindings/python"
package = "soothfast-stats"

[[bind]]
lang = "wasm"
out = "bindings/js"
package = "soothfast-stats-js"

[[bind]]
lang = "c"
out = "bindings/c"
package = "soothfast-stats-c"
```

`lang` is `python`, `wasm`, or `c`, each with the short forms you would
expect (`py`, `js`, `cabi`). `out` and `package` are required. `module`, `version`, `description`,
`repository`, `targets`, and `backend_version` all default to something
sensible.

## Commands

```bash
cargo soothfast bind gen -p PKG            # write the packages
cargo soothfast bind gen -p PKG --check    # fail if they are stale
cargo soothfast bind gate -p PKG           # fail on a consumer-breaking change
cargo soothfast bind build -p PKG          # drive maturin / wasm-pack
```

`bind gen` writes a small Rust glue crate per language and the packaging
around it. `bind build` hands that crate to the ecosystem's own tool:
`maturin` for Python, `wasm-pack` for wasm. Neither tool is a dependency of
soothfast; they are host tools, like `cargo bench`.

## What the generated code looks like

Every exported type becomes a wrapper defined in the glue crate. That is not
a style choice: `#[pyclass]` and `#[wasm_bindgen]` expand to trait impls the
orphan rule only permits in the crate that *defines* the type, so a glue
crate cannot annotate yours. The same rule is why a failing call raises
through a local error newtype rather than `impl From<YourError> for PyErr`.

```rust ignore
#[pyclass(name = "Summary")]
pub struct Summary(::soothfast_demo::Summary);

#[pymethods]
impl Summary {
    #[new]
    fn new(samples: Vec<f64>) -> PyResult<Self> {
        Ok(Summary(::soothfast_demo::Summary::new(samples).map_err(BindErrorString)?))
    }
}
```

Your crate keeps its single runtime dependency. pyo3 and wasm-bindgen appear
only in the generated crate.

## How types cross

| Rust | Python | JavaScript | C |
| --- | --- | --- | --- |
| `String`, `&str` | `str` | `string` | `char *` |
| `Vec<u8>`, `&[u8]` | `bytes` | `Uint8Array` | `uint8_t *` + `size_t` |
| `Vec<T>` | array class | `Array` / typed array | `*_array` struct |
| `Option<T>` | `T \| None` | `T \| undefined` | nullable pointer, handles only |
| `HashMap<K, V>` | `dict` | not bound | not bound |
| `(A, B)` | `tuple` | not bound | not bound |
| `Result<T, E>` | raises | throws | `char **error` out-param |
| `async fn` | awaitable | `Promise` | not bound |
| exported struct | handle class | handle class | opaque pointer |
| payload-free enum | `enum` | `enum` | `enum` |

An enum carrying data stays an opaque handle, because neither language has a
shape for it; that is reported as a note rather than guessed at.

A method taking `self` by value, or an exported type crossing by value, is
reported instead of bound: both would copy a value the caller is still
holding. Take `&self` and return what the caller needs.

## C is the one without a framework

pyo3 and wasm-bindgen do the marshaling for the other two. C has nothing, so
this backend writes it out: a `cdylib` and a `staticlib` behind a header,
under the flat symbol the wrapper model already assigns every call.

```toml
[[bind]]
lang = "c"
out = "bindings/c"
package = "acme-core"
```

```c
#include "acme_core.h"

char *error = NULL;
acme_core_summary *s = acme_core_summary_new(samples, 4, &error);

acme_core_f64_array dev = acme_core_summary_deviations_all(s, values, 3);
acme_core_f64_array_free(dev);
acme_core_summary_free(s);
```

Four rules cover the whole surface:

- **A handle is an opaque pointer** you release with its `*_free`. The struct
  is declared but never defined, so C cannot read or copy what is inside.
- **A sequence is a pointer and a length.** Going in, that is the caller's own
  memory and nothing is copied. Coming back, it is a two-field struct with a
  matching `*_array_free`.
- **A failing call takes a trailing `char **error`.** On success it writes
  `NULL`; on failure it writes a message and returns a zero you must not
  read. Passing `NULL` discards the message. Messages are released with
  `<module>_string_free`.
- **Nothing is freed for you.** That is the note `bind gen` leads with.

What C has no spelling for is reported rather than guessed: maps, tuples,
sequences of non-primitives, `Option` of anything but an exported type, and
`async fn`, which has nothing to await with.

The generated crate builds with plain `cargo build --release` and ships a
`.pc` file, so a consumer finds the header and the library through
pkg-config rather than hardcoded paths.

`bind build` takes a matrix, which is how a C library is usually shipped:

```bash
cargo soothfast bind build -p PKG \
  --target x86_64-unknown-linux-musl --target aarch64-apple-darwin
```

A target whose toolchain is missing is reported and skipped, not fatal, so a
machine builds what it can:

```
soothfast: skipping aarch64-unknown-linux-gnu: cargo build failed — is the
           target installed? (rustup target add aarch64-unknown-linux-gnu)
bind build: bindings/c [c] — 2 artifact(s)
```

The run fails only when nothing built. The header is reported alongside every
library, since one header describes them all. Which library kinds appear is
the target's business: a `musl` triple is `crt-static` by default, so it
yields the `.a` and no `.so`.

## Speed, and what actually governs it

The Rust body runs at native speed. The boundary does not, and for a small
function the crossing costs more than the work. What decides whether a
binding beats the host language is how much data has to be converted to get
there.

Measured on the dogfood package, 100k `f64`, best of nine runs, against the
same computation written in the host language. Above 1.0 means the binding
wins:

| shape | Python | JavaScript (wasm) |
| --- | --- | --- |
| one call per element | 0.9x | 0.2x |
| batch, plain `list` in | 4.0x | n/a |
| batch, buffer in and out | 56x | 0.8x |
| batch, into the caller's buffer | 56x | 0.8x |

**Cross once, not per element.** `deviations` called in a loop is slower than
never binding at all: the arithmetic is a subtract, an abs and a divide, and
reaching it costs more than doing it.

**Hand over a buffer, not a sequence.** Any parameter that is a contiguous
run of one primitive goes through the buffer protocol in Python, so
`array.array`, `memoryview` and numpy arrive as a pointer with nothing
unboxed. A plain `list` still works and is still copied element by element,
which is the whole distance between 4x and 56x. `bytes` counts: reading
800 KB as `Vec<u8>` costs 1.6 ms through a buffer against 7.9 ms unboxed one
byte at a time.

**Take the sequence back the same way.** A returned `Vec<f64>` comes back as
an array class exporting the buffer protocol, so `numpy.asarray(x)` and
`memoryview(x)` read it without copying, and `x.tolist()` copies only when
asked. Boxing 100k floats into a list cost 18 ns each; the array class costs
1.0 ns including the computation.

**An out-parameter is not a shortcut.** Writing through an `&mut [f64]` the
caller owns reads the same as handing back a fresh sequence, because the
array class already allocates once and copies nothing. It starts to pay near
10M elements, where the allocation stops being free: 1.3x there, nothing at
100k.

**JavaScript is a different problem.** wasm has its own address space, so
every buffer is copied into it whatever the signature says, and there is no
borrowed form to reach for. V8 already compiles a loop over a `Float64Array`
to native code with no boxing at all, so there is far less to win back: the
compute itself runs about 3x faster in wasm, and the copies spend all of it.
An out-parameter does not rescue this either, because wasm-bindgen copies a
mutable slice in as well as back out: it trades one JavaScript allocation for
one more crossing. Nothing about the signature changes the answer, which is
why `bind gen` offers wasm no advice about it.

Python does get that advice, per function, derived from the same model that
generates the code:

```
note: deviations_all: returning `Vec<f64>` allocates a fresh sequence per
      call; an `&mut [f64]` parameter would let the caller reuse one
```

Keep state in Rust where you can. A handle holds the real value, so reading
one back crosses no conversion: a field read off `Summary` is ~67 ns in
Python and ~14 ns in JavaScript. This is the shape polars takes to its
conclusion, with data living in Rust across a whole query rather than one
call.

Numbers here are one machine and are illustrative. Nothing gates them; the
gated claims in this repo are the ones under `soothfast:claim` markers.

## The lock is released where it pays

A Python thread calling into Rust holds the interpreter lock for the whole
call, so two threads calling the same binding take turns. Any call carrying a
buffer releases it for the duration of the Rust body, which is the shape
where the work is worth the ~90 ns the handover costs:

| threads | wall | speedup |
| --- | --- | --- |
| 1 | 21 ms | 1.00x |
| 2 | 21 ms | 1.97x |
| 4 | 28 ms | 3.01x |
| 12 | 37 ms | 6.88x |

A scalar call keeps the lock. Releasing it would more than double a 72 ns
call, and there is no work to overlap.

Three things have to hold before a call gives up the lock, and all three are
read off the same model that generates the code:

- **The receiver must be provably shareable.** A `&self` method needs the
  exported type to be `Sync`, and a `&mut self` one needs it `Send`. These
  come from the auto-trait impls in rustdoc's own output, and an absent impl
  counts as a no: a guess that turned out wrong would not compile, and a
  binding that fails to build is worse than one that keeps the lock.
- **Every argument has to travel.** A handle parameter borrows a Python
  object, which is the thing the lock protects, so a call taking one keeps
  it. Buffers, scalars, strings and payload-free enums all travel.
- **What comes back has to travel too**, including the error type of a
  failing call.

The `&self` borrow is held across the release, so pyo3's borrow flag stops
another thread mutating the same handle mid-call: it raises `RuntimeError:
Already borrowed` rather than racing. Buffer *contents* are the caller's
responsibility, as they are in numpy: the buffer protocol stops the object
being resized while a call is reading it, but nothing stops another thread
overwriting the elements.

## Async needs a runtime

Python's event loop drives the future pyo3 hands it, but it is not a Rust
reactor. A future built on tokio finds nothing to register its timers and I/O
with, so the glue owns a runtime and enters it around each poll:

```python
await summary.refresh()      # tokio::time, tokio::spawn, reqwest all work
```

The runtime is entered per poll rather than held across awaits, so a
suspended future never leaves a thread carrying a context it did not enter.
Concurrency, cancellation and interleaving with Python's own coroutines are
unchanged: twenty gathered 50 ms calls finish in 52 ms.

Glue crates binding an `async fn` take a `tokio` dependency for this, and
cargo unifies it with whatever tokio the bound crate already pulls in. A
surface with nothing async takes neither the dependency nor the runtime.

JavaScript needs none of this. wasm-bindgen turns a future into a Promise and
the JavaScript event loop is the reactor.

## Drift is gated

Adding, removing, or reshaping an export changes the package your consumers
installed, so `soothfast.lock` pins the whole exported set:

```bash
cargo soothfast docs check -p PKG    # fails on an unlocked or changed export
cargo soothfast docs accept -p PKG   # re-locks after review
```

The fingerprint covers the bound contract and nothing else. Rewriting a
function body leaves it alone; changing a parameter type does not.

`bind gate` answers the other question, against the merge base rather than
the lockfile: would this change break code already written against the
package?

```
BREAK changed the bound signature of soothfast_demo::Summary::get
add   added soothfast_demo::Summary::deviations
bind gate: FAILED (1 breaking change(s) vs origin/master)
```

`--allow-breaking` releases one deliberately.

## Two things that will bite you

**The bench target must name the library.** Registrations reach the bench
binary through `linkme`, and the linker keeps a library only when something
references it. A bench that never names your crate discovers nothing:

```rust ignore
use my_crate as _;

soothfast::bench_main!();
```

`bind gen` says so rather than emitting an empty package.

**Nothing registers on wasm32.** `linkme` has no wasm32 support, and nothing
reads a registry there anyway, so every slice is empty on that target. This
is what lets an annotated crate compile to wasm at all. Discovery runs on the
host, where it belongs.

## Adding a language

The wrapper model lives in `soothfast-bind/src/plan.rs` and is decided once
for every language: which types become handle classes, which associated fn
builds one, which fields get accessors, what raises. A backend renders that
plan; it never re-derives it, so two languages cannot disagree about what the
same Rust type is.

A new backend is a `{mod, glue, package}.rs` trio and a `BindKind` variant.
pyo3, wasm-bindgen, and napi-rs all share one shape, an attribute macro over
a local newtype, so each is a small file.

The C backend shares none of it, which is what makes it the honest test of
the model. It needed no new field: ownership and nullability were already on
every type, and `Function::symbol` was already a flat name for a target with
no namespaces of its own. It did add one file, `types.rs`, because C is the
only target that spells the same type twice, once in the header and once in
the glue, and those two must not drift.

Two things a backend declares about itself rather than reading off the plan:

- `BindKind::buffer_support` says whether a borrowed buffer arrives as a
  pointer. Python and C say yes; wasm copies into its own address space
  whatever the signature says. Advice derived from the plan is filtered
  through this, so no backend is told to change a signature that would not
  help it.
- `plan::unsupported` says what the target cannot spell. Every answer becomes
  a reported `Gap` and the item is left out, never bound to a guess.

A backend with a lock or a garbage collector will need one more: JNI reaches
a buffer either pinned, which blocks collection, or copied, which is a third
answer `BufferSupport` does not yet have.
