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

[[bind]]
lang = "go"
out = "bindings/go"
package = "github.com/acme/soothfast-stats"

[[bind]]
lang = "node"
out = "bindings/node"
package = "soothfast-stats-native"

[[bind]]
lang = "java"
out = "bindings/java"
package = "io.acme.stats"

[[bind]]
lang = "kotlin"
out = "bindings/kotlin"
package = "io.acme.statskt"

[[bind]]
lang = "r"
out = "bindings/r"
package = "acme.stats"

[[bind]]
lang = "ruby"
out = "bindings/ruby"
package = "soothfast-stats"
```

`lang` is `python`, `wasm`, `node`, `c`, `go`, `java`, `kotlin`, `r`, or
`ruby`, each with the short forms you would expect (`py`, `js`, `napi`,
`cabi`, `golang`, `jni`, `kt`, `extendr`, `rb`). `out` and `package` are
required. For `go`, `package` is the Go module path rather than a
distribution name; the Go package name is its last element. For `java` and
`kotlin`, `package` is the JVM package a caller imports, dotted the normal
way; the two need distinct packages when both bind the same crate, since
each stages its own native library under its own `Natives`. For `r`,
`package` is the R package name: letters, digits and dots, starting with a
letter — no hyphens, since R derives its native init routine from that name
by replacing every other character with `_`. For `ruby`, `package` is the
gem name; the Ruby module classes and the package `Error` are defined under
is derived from it. `module`, `version`, `description`, `repository`,
`targets`, and `backend_version` all default to something sensible.

## Commands

```bash
cargo soothfast bind gen -p PKG            # write the packages
cargo soothfast bind gen -p PKG --check    # fail if they are stale
cargo soothfast bind gate -p PKG           # fail on a consumer-breaking change
cargo soothfast bind build -p PKG          # drive maturin / wasm-pack / napi / go / javac+jar / kotlinc+jar / R CMD INSTALL / rake+gem
```

`bind gen` writes a small Rust glue crate per language and the packaging
around it. `bind build` hands that crate to the ecosystem's own tool:
`maturin` for Python, `wasm-pack` for wasm, `napi build` (via `npx`, after an
`npm install` if `node_modules/` is missing) for Node. None of these tools
are a dependency of soothfast; they are host tools, like `cargo bench`. Go
has no such tool: `bind build` runs the C backend's own `cargo build` for the
cdylib, then verifies the wrapper against it with `go vet`/`go build`. Java
and Kotlin both ride that same plain `cargo build`, then `javac` or
`kotlinc` and `jar`, staging each built cdylib under `natives/<os>-<arch>/`
inside the jar so `Natives` can load whichever one matches the JVM it is
running under. A missing `javac`/`kotlinc`/`jar` skips only that packaging
step; the cdylib the matrix already built is still reported. R has no
matrix at all: `bind build` runs `R CMD INSTALL` straight from the
generated source directory into a library under the glue's own
`target/rlib`, never the user's site library. It never builds the usual
`R CMD build` tarball first, because `R CMD INSTALL` extracts one into an
isolated staging directory before compiling, and `src/rust/Cargo.toml`'s
path dependency on the bound crate reaches outside the R package's own
tree — a tarball's staging copy has no sibling to satisfy it. Publishing a
standalone source package needs the bound crate vendored under `src/rust`
first, which `bind build` does not do. Ruby rides `bundle exec rake
compile` (`rb_sys`'s own `cargo build` wrapper) and then `gem build`; each
is reported and skipped on its own, so a machine with only one of
`bundle`/`gem` installed still hears about the other.

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

| Rust | Python | JavaScript | C | Go | Java | Kotlin | R | Ruby |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `String`, `&str` | `str` | `string` | `char *` | `string` | `String` | `String` | character | `String` |
| `Vec<u8>`, `&[u8]` | `bytes` | `Uint8Array` | `uint8_t *` + `size_t` | `[]byte` | `byte[]` | `ByteArray` | raw vector | `String` (binary) |
| `Vec<T>` | array class | `Array` / typed array | `*_array` struct | `[]T` | `T[]` | `TArray` | numeric vector | `Array` |
| `i64`, `u64` | `int` | `BigInt` | `int64_t` | `int64` | `long` | `Long` | double, checked | `Integer` |
| `Option<T>` | `T \| None` | `T \| undefined` | nullable pointer, handles only | nullable pointer, handles only | nullable, handles only | `T?`, handles only | `T` or `NULL` | `T \| nil` |
| `HashMap<K, V>` | `dict` | not bound | not bound | not bound | not bound | not bound | not bound | `Hash` |
| `(A, B)` | `tuple` | not bound | not bound | not bound | not bound | not bound | not bound | `Array` |
| `Result<T, E>` | raises | throws | `char **error` out-param | `error` | throws (unchecked) | throws (unchecked) | R condition (`stop()`) | raises |
| `async fn` | awaitable | `Promise` | not bound | not bound | not bound | not bound | not bound | not bound |
| exported struct | handle class | handle class | opaque pointer | struct with `Close()` | handle class, `AutoCloseable` | handle class, `AutoCloseable` | external pointer, `$method()` | handle class |
| payload-free enum | `enum` | `enum` | `enum` | typed `int32` + constants | `enum` | `enum class` | validated string | `Symbol` |

An enum carrying data stays an opaque handle, because neither language has a
shape for it; that is reported as a note rather than guessed at.

A method taking `self` by value, or an exported type crossing by value, is
reported instead of bound: both would copy a value the caller is still
holding. Take `&self` and return what the caller needs.

### Go

Go's bindings are cgo over the C backend's own header: `bind gen` writes the
`.h`/glue/package trio C already gets, then a `go.mod` and one `<name>.go`
declaring `#cgo CFLAGS`/`LDFLAGS` against it. There is no separate `import "C"`
runtime to learn:

```go
c := NewCounter(0)
defer c.Close()

n, err := c.Bump(5)
```

- **A handle is a struct with an unexported pointer.** `Close` releases it
  through the C `*_free` and is idempotent; `runtime.SetFinalizer` is the
  backstop for a caller that forgets to call it.
- **A slice of one primitive crosses as a pointer and a length**, the same
  buffer C takes, via `unsafe.Pointer` on the slice's backing array. An empty
  slice never takes that address, which cgo would reject.
- **A returned sequence is copied into a Go slice and freed once**, so the
  caller never has to reach for the C `*_array_free` itself.
- **The C error mechanism becomes a Go `error`.** A fallible call reads its
  message off the `char **error` out-parameter and releases it, the same way
  a returned string is released.

Generated Go is gofmt-clean by construction, checked in the golden suite. An
`async fn` is a gap for Go (`no Go runtime story yet`): cgo has no reactor to
hand a future to, unlike wasm-bindgen turning one into a `Promise`.

### Node

napi-rs shares pyo3's and wasm-bindgen's shape, an attribute macro over a
local newtype, but its buffers behave like Python's rather than wasm's: a
napi typed array is a view into V8's own memory for the call, not a copy
into linear memory, so the same zero-copy notes `bind gen` gives Python
apply to Node too. Where it differs from both: a 64-bit integer crosses as a
JavaScript `BigInt`, checked on the way in — a value that would not fit
silently fails the call rather than truncating — and the generated package
ships a `package.json` alongside its `Cargo.toml`, since `napi build` reads
both to name the compiled addon and the JS loader in front of it. `async fn`
is a gap for Node the same way it is for Go: no runtime to hand a future to.

Unlike a wasm-bindgen `.wasm`, which runs unmodified on any platform, a napi
addon is a native binary: one build per target, which is what `bind build
--target` is for.

### Java

Java is the first target with a garbage collector rather than a lock or an
owner: a borrowed buffer reaches it pinned through
`GetPrimitiveArrayCritical` instead of copied or taken as a raw pointer, the
third answer `BufferSupport` has. Pinning blocks collection for the call, so
`bind gen` never calls it offloadable — the buffer notes Python and Node get
for a signature change do not apply here.

There is also no wrapper type. Every other backend defines a local newtype
around your struct (`#[pyclass] pub struct Summary(::acme::Summary)`); JNI
has no such macro, and needs none: a handle boxes your type directly and
crosses as a bare `long`, since Java's own type checker — not an FFI newtype
— is what keeps one class's pointer out of another class's method:

```java
try (Summary s = new Summary(new double[] {3.0, 1.0, 5.0, 4.0})) {
    double median = s.get(Metric.Median);
    double[] devs = s.deviationsAll(new double[] {0.0, 4.0});
}
```

- **A handle registers with a shared `Cleaner`** on close or collection,
  whichever comes first; `close()` is idempotent and safe to call twice.
- **A public constructor is real**, not a disguised factory: `bind gen`
  generates one that delegates to the package-private pointer-wrapping
  constructor through a `Raw` marker class, so the two can never collide on
  the same `(long)` signature.
- **A failing call throws** the package's own unchecked exception rather
  than aborting the JVM. The generated crate builds with `panic = "abort"`,
  the same as C's cdylib, so every fallible JNI call catches its `Result`
  and throws instead of ever reaching a bare `.expect()`.
- **The native library loads itself.** Every class touching JNI carries a
  `static { Natives.load(); }`, and `Natives` looks for the library as a
  jar resource under `natives/<os>-<arch>/` first — extracting it to a temp
  file and `System.load`-ing it — before falling back to
  `System.loadLibrary`, so a package built by `bind build` works with
  nothing on `java.library.path`.

`async fn` is a gap for Java the same way it is for Go and Node: no runtime
to hand a future to.

### Kotlin

Kotlin binds the same plan Java does, over the same JNI glue crate: nothing
in `src/lib.rs` changes, byte for byte, whether `[[bind]] lang` says `java`
or `kotlin`. What differs is only the source `bind gen` writes to call into
it. `@JvmStatic external fun` in a `companion object` compiles to the exact
static native method a `private static native` Java declaration would, so
the glue links against either without knowing which one is asking:

```kotlin
Summary(doubleArrayOf(3.0, 1.0, 5.0, 4.0)).use { s ->
    val median = s.get(Metric.Median)
    val devs = s.deviationsAll(doubleArrayOf(0.0, 4.0))
}
```

- **Nullable types replace `null`-or-handle.** `Option<T>` crosses as `T?`
  rather than a value Java callers have to remember might be null; a
  `null` pointer from the native side becomes Kotlin `null` at the
  boundary, nowhere else.
- **A field accessor is a `val` property**, read as `s.median` rather than
  Java's own `s.median()` method call.
- **A payload-free enum is an `enum class`**, crossing as its ordinal the
  same both ways as Java's plain enum.
- **The pointer-wrapping constructor collision is a private nested
  `object` marker** rather than Java's `Raw` class with a static
  `INSTANCE`; Kotlin's own singleton objects need no separate accessor.
- **Free functions are top-level, not static methods on a holder class.**
  `@file:JvmName("<Module>")` names the file's own compiled class after the
  module, so the native symbols still land where the glue expects them.

`kotlinc` and `kotlin` are host tools the same way `javac` and the JVM are:
not a soothfast dependency, just what `bind build` shells out to.

### R

R is interpreted with boxed scalars and slow loops, so it is where a Rust
binding wins by the largest ratio. It is also where the boundary is
cheapest to cross: R's numeric and raw vectors are contiguous and its
collector never moves an object, so extendr reads a borrowed one
zero-copy, the same answer as Python and C.

```r
s <- Summary(c(3.0, 1.0, 5.0, 4.0))
median <- s$get("Median")
devs <- s$deviations_all(c(0.0, 4.0))
```

- **A handle is an external pointer with an S3 class and `$method()`
  dispatch**, not a wrapper `bind gen` writes by hand: extendr's own
  `#[extendr] struct` derive gives the local newtype its pointer shape and
  its class tag. It also registers a finalizer automatically, so unlike
  Java's `Cleaner` backstop there is no `close()` to forget — R's collector
  frees the boxed Rust value on its own.
- **A payload-free enum crosses as a validated string**, not a mirrored
  ordinal: the glue matches it against the type's own variant names on the
  way in and raises an R error naming the bad value if it does not match,
  rather than a mismatched-ordinal panic.
- **A 64-bit or platform-width integer crosses as a checked double.** R has
  no 64-bit integer type at all, so `i64`, `u64`, `isize`, and `usize`
  spell as `f64` at the boundary; a helper rejects the call if the value
  carries a fraction or falls outside the target type's range rather than
  truncating it.
- **`Option<T>` is `NULL` for anything the plan can otherwise carry**, not
  only an exported type: unlike C, Go, Java, and Kotlin, R has no
  borrowed-or-owned ambiguity for a plain value, so `bind gen` builds the
  `Robj` by hand instead of restricting the shape.
- **A mutable buffer parameter is a gap.** R vectors are copy-on-write
  values, and nothing about R's calling convention guarantees the vector a
  caller passed in is not aliased elsewhere, so writing through one in
  place is not something a caller can safely observe. Return the sequence
  instead.

R has no cross-compilation matrix and no distributable tarball either — see
Commands above for what `bind build` does instead.

### Ruby

magnus wraps each exported type the same way pyo3 and wasm-bindgen do, a
local newtype around your struct, but registers every method explicitly in
an `#[magnus::init]` function instead of through an attribute macro over
the `impl` block. A payload-free enum gets no wrapper at all: it crosses as
a Ruby `Symbol`, checked against the known variant names on the way in,
since nothing about a `Symbol` value proves it names one of them:

```ruby
counter = AcmeCore::Counter.new(0)
counter.bump(5)
counter.at(:low)
```

- **Every buffer is copied, both ways.** A Ruby `Array` boxes each element,
  so a `Vec<f64>` parameter has to be unboxed one value at a time whether
  the signature borrows or owns it, and a `String`'s bytes may move under a
  compacting collector, so there is no pointer to hand over either. `bind
  gen` gives Ruby the same no-advice verdict it gives wasm, for the same
  reason: taking a borrow saves no copy here.
- **A writable buffer still looks mutated to the caller.** `&mut [f64]`
  converts the incoming `Array` to an owned `Vec`, calls with that, then
  writes it back into the same `Array` object element by element with
  `RArray::store`. It costs an extra pass over the buffer that Python's and
  Node's zero-copy views don't pay, but the caller sees the same mutation
  either way; gapping the parameter instead would have been cheaper to
  generate and dishonest about what the signature promises.
- **A failing call raises `<Module>::Error`**, one exception class per
  package rather than one per Rust error type: magnus turns any `Err`
  returned from a bound call into a raised exception on its own, so the
  glue only has to name the class once.
- **The package is a gem, not a wheel.** `bind gen` writes the usual
  `<name>.gemspec`/`Gemfile`/`Rakefile` trio around an `ext/<module>/` glue
  crate that `rb_sys`'s `create_rust_makefile` builds; `bind build` runs
  `bundle exec rake compile` and then `gem build`, each reported and
  skipped on its own so a machine missing one tool still hears about the
  other.

`async fn` is a gap for Ruby the same way it is for Go, Node and Java: no
runtime to hand a future to.

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

<!-- soothfast:claim soothfast_demo::bench_deviations_all.alloc.allocs <= 1 -->
`deviations_all` allocates the `Vec<f64>` it returns.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_demo::bench_deviations_into.alloc.allocs <= 0 -->
`deviations_into` writes into the caller's own buffer instead and allocates
nothing.
<!-- /soothfast:claim -->

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

<!-- soothfast:claim soothfast_demo::bench_summary_new.alloc.allocs <= 6 -->
Building the handle is where that cost is paid: `Summary::new` sorts the
sample set twice.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_demo::bench_summary_get.alloc.allocs <= 0 -->
Reading a statistic back off it allocates nothing at all.
<!-- /soothfast:claim -->

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

Wiring both into CI is one input on the [soothfast
action](ci.md#inputs): `bind` regenerates and gates a package's bindings on
the default branch and on pull requests, the same way `spec` does for
specs. Building the packages themselves is `bind-build`/`bind-target`,
covered in [Building native bindings](ci.md#building-native-bindings).

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

A backend with a lock or a garbage collector may need one more:
`BufferSupport::Pinned` is JNI's own answer, a buffer that stays zero-copy
but can never be offloaded, since pinning it blocks collection for the
call. `plan::offloadable` checks for it before anything else.

A language with no Rust bridge crate of its own follows `cgo/` instead:
render the plan as a wrapper over the C backend's own header and library,
rather than writing a marshaling framework from scratch. That needs a
`types.rs` mapping the C spelling to the host language's, a package file
(`go.mod`, `package.json`, whatever that ecosystem expects), and the glue
renderer that composes them — no Rust of its own, since the C backend already
built the cdylib every other FFI can read.
