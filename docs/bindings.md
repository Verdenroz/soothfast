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

[[bind]]
lang = "cpp"
out = "bindings/cpp"
package = "acme::stats"

[[bind]]
lang = "lua"
out = "bindings/lua"
package = "acme.stats"

[[bind]]
lang = "csharp"
out = "bindings/csharp"
package = "Acme.Stats"
```

`lang` is `python`, `wasm`, `node`, `c`, `go`, `java`, `kotlin`, `r`,
`ruby`, `cpp`, `lua`, or `csharp`, each with the short forms you would
expect (`py`, `js`, `napi`, `cabi`, `golang`, `jni`, `kt`, `extendr`, `rb`,
`c++`/`cxx`, `luajit`, `cs`/`dotnet`). `out` and `package` are required.
For `go`, `package` is the Go module path rather than a distribution name;
the Go package name is its last element. For `java` and `kotlin`,
`package` is the JVM package a caller imports, dotted the normal way; the
two need distinct packages when both bind the same crate, since each
stages its own native library under its own `Natives`. For `r`, `package`
is the R package name: letters, digits and dots, starting with a letter —
no hyphens, since R derives its native init routine from that name by
replacing every other character with `_`. For `ruby`, `package` is the gem
name; the Ruby module classes and the package `Error` are defined under is
derived from it. For `cpp`, `package` is a `::`-delimited C++ namespace
path; the embedded C library's name is its last segment, the same way
Go's package name is its module path's last element. For `lua`, `package`
is the dotted `require` path a caller loads it by, e.g. `acme.stats` for
`require("acme.stats")`; the embedded C crate is named after its last
segment the same way `go`'s and `cpp`'s are. For `csharp`, `package` is
the root namespace, dotted like Java's; the assembly name and the
`.csproj` file are named after it, and the embedded C crate's own name is
derived from it the same way Go's is: the whole namespace, snake_cased.
`module`, `version`, `description`, `repository`, `authors`, `targets`, and
`backend_version` all default to something sensible; `authors` falls back
to the crate's own, and stands in for the crate name where a manifest
format requires one. For `python`, `interpreters = ["python3.14",
"python3.14t"]` names the interpreters `bind build` produces a wheel for,
one wheel each, in place of whichever `python3` maturin finds first.

## Commands

```bash
cargo soothfast bind gen -p PKG            # write the packages
cargo soothfast bind gen -p PKG --check    # fail if they are stale
cargo soothfast bind gate -p PKG           # fail on a consumer-breaking change
cargo soothfast bind build -p PKG          # drive maturin / wasm-pack / napi / go / javac+jar / kotlinc+jar / R CMD INSTALL / rake+gem / c++ / luajit / dotnet build
```

`bind gen` writes a small Rust glue crate per language and the packaging
around it, including a `Cargo.lock` that `--check` fails on when it is
missing or no longer satisfies the manifest, the same as stale file text.
`bind build` hands that crate to the ecosystem's own tool:
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
`bundle`/`gem` installed still hears about the other. C++ rides the same
plain `cargo build` as C and Go, then verifies the generated header with a
syntax-only compile of a small driver (`c++ -std=c++20 -fsyntax-only`,
preferring `$CXX`, then `c++`, `g++`, `clang++`): a header-only wrapper has
no library of its own to build, so this is the whole check a consumer's
build would otherwise catch. A missing compiler skips it, the same
tolerance `go vet`/`go build` gets when `go` is absent. C# rides the same
`cargo build` matrix Go and the JVM backends do, staging each target's
cdylib under `runtimes/<rid>/native/` (the layout `Native.cs`'s own
resolver probes) before running `dotnet build`; a missing `dotnet` skips
only that step, the same posture as a missing `javac`.

## Mapping a foreign type

A type from another crate has no fields rustdoc can read, so a call that
takes or returns one is reported as a gap naming it. `[bind.types]`, a
sub-table of the `[[bind]]` entry above it, says how such a type crosses:

```toml
[[bind]]
lang = "python"
out = "bindings/python"
package = "finance-query"

[bind.types]
"chrono::DateTime" = "str"
```

`"str"` is the one mapping so far: the type crosses as a string, rendered
through `Display` on the way out and parsed through `FromStr` on the way
in, so a parameter that fails to parse raises `ValueError` with the parse
error's own message. It converts in Python only for now; every other
backend reports a call or field mentioning a mapped type as unsupported.
The lookup falls back to the bare type name, so `"DateTime" = "str"`
matches `chrono::DateTime` too, and a type reached through a re-export
matches its canonical spelling. Every `[[bind]]` entry's table feeds one
walk of the surface, so the same path may not be mapped two ways.

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

The glue names your items by a public path, through `pub use` re-exports
where the defining module is private, so `Interval` in `mod constants` bound
as `pub use constants::Interval` is spelled `finance_query::Interval`.

Your crate keeps its single runtime dependency. pyo3 and wasm-bindgen appear
only in the generated crate.

## How types cross

| Rust | Python | JavaScript | C | Go | Java | Kotlin | R | Ruby | C++ | Lua | C# |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `String`, `&str` | `str` | `string` | `char *` | `string` | `String` | `String` | character | `String` | `string_view` in, `string` out | `string` | `string` |
| `Vec<u8>`, `&[u8]` | `bytes` | `Uint8Array` | `uint8_t *` + `size_t` | `[]byte` | `byte[]` | `ByteArray` | raw vector | `String` (binary) | `span<const uint8_t>` in, `vector<uint8_t>` out | table or FFI array | `Span<byte>` / `byte[]` |
| `Vec<T>` | array class; a field of handles is a `{T}Seq` | `Array` / typed array | `*_array` struct | `[]T` | `T[]` | `TArray` | numeric vector | `Array` | `span<const T>` in, `vector<T>` out | table or FFI array | `Span<T>` / `T[]` |
| `i64`, `u64` | `int` | `BigInt` | `int64_t` | `int64` | `long` | `Long` | double, checked | `Integer` | `int64_t`, `uint64_t` | `int64_t`/`uint64_t` cdata | `long`, `ulong` |
| `Option<T>` | `T \| None` | `T \| undefined` | nullable pointer, handles and strings | nullable pointer, handles and strings | nullable, handles and strings | `T?`, handles and strings | `T` or `NULL` | `T \| nil` | `optional<T>`, handles and strings | nullable pointer, handles and strings | `null`, handles and strings |
| `HashMap<K, V>` | `dict` | not bound | not bound | not bound | not bound | not bound | not bound | `Hash` | not bound | not bound | not bound |
| `(A, B)` | `tuple` | not bound | not bound | not bound | not bound | not bound | not bound | `Array` | not bound | not bound | not bound |
| `Result<T, E>` | raises an `Error` subclass | throws | `char **error` out-param | `error` | throws (unchecked) | throws (unchecked) | R condition (`stop()`) | raises | throws `Error` | `error()` | throws |
| `async fn` | awaitable | `Promise` | not bound | not bound | not bound | not bound | not bound | not bound | not bound | not bound | not bound |
| exported struct | handle class | handle class | opaque pointer | struct with `Close()` | handle class, `AutoCloseable` | handle class, `AutoCloseable` | external pointer, `$method()` | handle class | handle class, `unique_ptr` member | table with `:close()` | `SafeHandle`, `IDisposable` |
| payload-free enum | `enum` | `enum` | `enum` | typed `int32` + constants | `enum` | `enum class` | validated string | `Symbol` | `enum class` | validated string | `enum` |

An enum carrying data stays an opaque handle, because neither language has a
shape for it; that is reported as a note rather than guessed at.

A method taking `self` by value, or an exported type crossing by value, is
reported instead of bound: both would copy a value the caller is still
holding. Take `&self` and return what the caller needs.

Three idioms a Rust API leans on need no rewriting to bind:

- **A crate-wide alias** such as `type Result<T> = std::result::Result<T,
  Error>` is expanded to what it names, so a method returning `Result<Chart>`
  binds as a `Chart` plus a raise, the same as one spelling both arms.
- **`impl Into<T>` and `impl AsRef<str>` parameters** bind as the type a
  caller would pass (`T`, or a string). The argument the glue hands over
  satisfies the bound on its own, so the call needs nothing extra.
- **An `async fn new`** binds as an awaitable static factory rather than a
  constructor, since no language awaits inside construction:
  `ticker = await Ticker.new("AAPL")`.

A method returning a borrowed value (`&str`, `&[f64]`, `Option<&str>`) is
owned by the Python glue before it crosses; the other backends report it
for now, so return an owned value there. A public field holding an
exported type reads as a handle but has no setter: the handle Python passes
in cannot be moved out of.

A failing call raises from a per-package hierarchy in Python: an `Error`
base, one subclass per error type a bound call returns (`FinanceError`,
named after the Rust type whether or not it is exported), and for an enum
error one subclass per variant (`SymbolNotFound`), so `except
finance_query.SymbolNotFound` works without parsing a message. A variant's
named fields that are primitives, strings, bytes or options of those are
set as attributes on the raised exception (`e.symbol`, `e.retry_after`);
any other field stays in the message only. A `String` error raises the
base. A name already taken by a class or function takes an `Error` suffix.

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
  backstop for a caller that forgets to call it. A call after `Close`
  panics rather than passing the freed pointer on.
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
  whichever comes first; `close()` is idempotent and safe to call twice. A
  call after `close()` throws `IllegalStateException` rather than passing
  the freed pointer to native code.
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
- **A call after `close()` throws `IllegalStateException`** via `check()`,
  the same guarantee Java's handle gives.

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
  `Robj` by hand instead of restricting the shape. An optional string is
  `NULL` coming back, and R's own `NA_character_` is also accepted as
  absent going in.
- **A mutable buffer parameter is a gap.** R vectors are copy-on-write
  values, and nothing about R's calling convention guarantees the vector a
  caller passed in is not aliased elsewhere, so writing through one in
  place is not something a caller can safely observe. Return the sequence
  instead.
- **A call that could alias the receiver checks pointer identity first.**
  A method taking `&mut self` alongside a parameter of its own class, like
  `x$absorb(x)`, compares the two external pointers before the real call
  and raises an R error on a match rather than handing Rust a `&mut` and a
  `&` over the same allocation.
- **A field is both `x$field()` and `field(x) <- value`.** The getter is a
  method call; the setter is R's replacement-function convention, a
  `field<-` generic with a `field<-.ClassName` method dispatching to it, so
  assignment reads like assignment rather than a disguised method call.

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
  glue only has to name the class once. Every receiver and handle
  parameter borrows the wrapped `RefCell` fallibly for the same reason: a
  call that aliases one object twice, like `x.absorb(x)`, raises
  `<Module>::Error` instead of panicking past `rescue`.
- **The package is a gem, not a wheel.** `bind gen` writes the usual
  `<name>.gemspec`/`Gemfile`/`Rakefile` trio around an `ext/<module>/` glue
  crate that `rb_sys`'s `create_rust_makefile` builds. The crate is named
  after the extension and a workspace manifest at the gem root lists it,
  because `RbSys::ExtensionTask` finds it by name in `cargo metadata` run
  from there; `bind build` runs
  `bundle exec rake compile` and then `gem build`, each reported and
  skipped on its own so a machine missing one tool still hears about the
  other.

`async fn` is a gap for Ruby the same way it is for Go, Node and Java: no
runtime to hand a future to.

### C++

C++'s bindings are header-only, the same wrapper-over-C shape Go takes:
`bind gen` writes the `.h`/glue/package trio C already gets, then one
`<name>.hpp` that a consumer `#include`s and links against the same C
library, no separate runtime to learn:

```cpp
acme::core::Counter c(0);
int64_t n = c.bump(5);
```

- **A handle owns its pointer through a `unique_ptr`** with a stateless
  deleter calling the C `*_free`, which makes the class move-only for
  free: a member with a deleted copy constructor makes the compiler
  delete the class's own, so nothing has to spell `= delete` by hand.
- **A failing call throws.** The generated code reads the C `char
  **error` out-parameter, frees the message after copying it, and throws
  an `Error` (a `std::runtime_error`) built from it — including from a
  throwing constructor, which delegates to a `construct` factory in its
  member-initializer list, since a constructor cannot check an
  out-parameter and bail before its members exist any other way.
- **A plain enum crosses as an `enum class`**, cast to and from the C
  enum it mirrors with `static_cast`, rather than carrying the C names
  into C++.
- **A borrowed buffer crosses as `std::span`**, `const` for an input and
  mutable for an out-parameter — the same pointer-and-length C already
  takes, so this backend answers `buffer_support` the way C does. A
  returned sequence is copied into a `std::vector` and freed once, the
  same shape Go's `float64Slice` takes.
- **A namespace path flattens to one C module name.** `package =
  "acme::core"` becomes the `namespace acme::core { ... }` the header
  declares, but the embedded C crate, its header, and its symbols are
  all named after the path's last segment (`core`), the same way a Go
  module path's last element becomes its package name.

`async fn` is a gap for C++ the same way it is for Go, Node and Java: no
runtime to hand a future to. The header must compile clean under `-Wall
-Wextra -Werror` on both g++ and clang++, checked in the golden suite.

### Lua

LuaJIT's `ffi` reads a C declaration directly, so `bind gen` writes no Rust
glue for it at all: the package is the C backend's own file set plus one
`.lua` module. Its `ffi.cdef` block is rendered from the same declaration
writer the header uses (`cabi::header::declarations`), not by textually
including the `.h`, which has `#include`s and an `extern "C"` block the
FFI's own parser cannot read:

```lua
local acme = require("acme.core")

local counter = acme.Counter.new(0)
local n = counter:bump(5)
counter:close()
```

- **A buffer parameter copies, the same answer wasm and Ruby give for
  their own reasons**, since a plain Lua table boxes each element and has
  no contiguous memory to hand over. The exception: a caller already
  holding a matching FFI array — built with `ffi.new("<ctype>[?]", n)` —
  or a previously returned array passes it straight through, checked with
  `ffi.istype` rather than copied. A VLA array carries no `#`; its length
  comes from `ffi.sizeof(value) / ffi.sizeof("<ctype>")` instead.
- **A returned sequence crosses as its own cdata array, not a copied
  table.** `ffi.metatype` gives it a 1-based `arr[i]` reading straight
  through the C buffer, bounds-checked against `#arr`, and `arr:totable()`
  copies it into a plain table for code that wants one; `ipairs` does not
  iterate cdata without `LUA52COMPAT`, so `for i = 1, #arr do` is the
  idiom. `ffi.gc` frees it as a backstop, and it can be released
  explicitly with `:close()` the same way a handle can.
- **A handle is a table with a `ptr` field**, freed through the C `*_free`
  and registered with `ffi.gc` as a backstop; `:close()` disarms the
  finalizer and frees once, so calling it twice is a no-op the same way
  Go's `Close()` is. A call after `:close()` raises through `error` rather
  than passing the freed pointer to the C library.
- **A payload-free enum crosses as a validated string**, not a mirrored
  ordinal: a lookup table checks it against the type's own variant names
  on the way in and maps an ordinal back to a name on the way out, the
  same shape R's and Ruby's validated strings take.
- **A failing call raises through Lua's own `error`.** The `char **error`
  out-parameter is read the same way C's caller reads it, then the message
  is turned into a Lua string and released before the raise, so nothing
  leaks on the error path either.

`async fn` is a gap for Lua the same way it is for Go, Node, Java, Ruby and
C++: no runtime to hand a future to.

### C#

C#'s bindings are the same wrapper-over-C shape Go's are: `bind gen` writes
the `.h`/glue/package trio C already gets, then a `.csproj` and the C#
sources that call into it. Unlike Go, there is no calling-convention macro
either — `[DllImport]` is metadata the runtime reads, not code generation —
so every extern declaration is written out here the same way the C backend
writes its own header:

```csharp
using (var s = new Summary(new double[] { 3.0, 1.0, 5.0, 4.0 }))
{
    double median = s.Get(Metric.Median);
    double[] devs = s.DeviationsAll(new double[] { 0.0, 4.0 });
}
```

- **A handle is a `SafeHandle` subclass**, its `ReleaseHandle` calling the C
  `*_free`; the class implementing `IDisposable` over it is what a `using`
  block, or an explicit `Dispose()`, actually releases. A call after
  `Dispose()` throws `ObjectDisposedException`: the handle is read through
  a property checking `SafeHandle.IsClosed` rather than
  `DangerousGetHandle()` called bare.
- **A borrowed buffer parameter is pinned with `fixed`**, the P/Invoke
  marshaller's own answer to the same question Java's `GetPrimitiveArrayCritical`
  answers: no copy, but the call cannot hand its work to another thread or
  call back into the runtime while the buffer is pinned.
- **A failing call throws `<Module>Exception`**, one exception type per
  package rather than one per Rust error type, the same choice Ruby makes:
  the generated call reads the `char **error` out-parameter, copies the
  message, frees it, and throws.
- **The native library resolves itself.** A module initializer installs a
  `NativeLibrary.SetDllImportResolver` callback that probes
  `runtimes/<rid>/native/` (where `bind build` stages each target) before
  falling back to the platform's own search, so a package built by
  `bind build` works with nothing on `LD_LIBRARY_PATH`.
- **A payload-free enum mirrors onto a C# `enum`**, crossing as an explicit
  cast to and from the same ordinal the C header assigns it.

`async fn` is a gap for C# the same way it is for Go, Node, Java, Kotlin,
Ruby, C++ and Lua: no runtime to hand a future to.

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
sequences of non-primitives, `Option` of anything but an exported type or a
string, and `async fn`, which has nothing to await with.

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

Measured with `cargo soothfast bind bench -p soothfast-demo`, 100k `f64`,
best of nine runs per shape, against the same computation written in the
host language. Above 1.0 means the binding wins:

| shape | Python | Node | Go | Java | R | C++ | Lua |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `build_summary` | 11.3x | 9.3x | 5.8x | 0.89x | 2.4x | 4.77x | 15.6x |
| `batch_buffer` | 123x | 2.1x | 0.46x | 2.7x | 348x | 1.58x | 1.95x |
| `batch_into` | 146x | 5.1x | 2.0x | 3.4x | n/a | 2.00x | 1.98x |
| `per_element` | 1.6x | 0.02x | 0.12x | 0.29x | 0.25x | 0.33x | 0.31x |

<!-- soothfast:claim soothfast_demo::bind::python::build_summary.ratio.ratio >= 5 -->
Python still beats a from-scratch Python sort/MAD by at least 5x building
the handle.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_demo::bind::python::batch_buffer.ratio.ratio >= 60 -->
The buffer path stays at least 60x over a Python loop doing the same
arithmetic.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_demo::bind::node::build_summary.ratio.ratio >= 4 -->
Node beats a from-scratch typed-array sort/MAD building the handle by at
least 4x.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_demo::bind::node::batch_buffer.ratio.ratio >= 1 -->
Node's buffer path still beats the equivalent typed-array loop.
<!-- /soothfast:claim -->

Python and Node are gated above; Go, Java, R, C++ and Lua are measured the
same way but not gated. R has no `batch_into` shape: extendr gaps
`deviations_into` there (see below). Two of these numbers look backwards
and are worth a sentence each: Go's `batch_buffer` under 1.0 is the cgo
call plus the copy-and-free of the returned slice against a Go loop
compiled straight to native code, and Java's `build_summary` under 1.0 is
the JDK's own dual-pivot sort matching Rust's on 100k doubles while the
JNI path still pays a region copy of the input.

C++ is the floor: a compiled host calling the C ABI directly, with no
marshalling layer at all, so its ratios are the cost of the crossing
itself — about 6 ns per call against roughly 2 ns for the host's own
inlined loop, which is why `per_element` lands under 1.0x while both
batch shapes, amortizing that one crossing over 100k elements, come out
ahead.

Lua is the other interpreted host. Returning the array struct as cdata
instead of copying it into a table, freed by `ffi.gc`, flips `batch_into`:
reusing a previously returned array as the write buffer needs no copy on
either side, so its 104 µs is the same pure crossing cost C++ pays for the
identical call, and the shape goes from 0.27x to 1.98x. `batch_buffer`
recovers from 0.37x to 1.95x once the array is released with `:close()`
instead of left for the collector: with an explicit `:close()`, the
returned-array call costs the same 105 µs as reusing an output buffer, and
matches C++'s 127 µs for the identical C function, so none of the earlier
288 µs was the call convention — it was the collector holding each 800 KB
result until a later cycle, which made every call page-fault fresh memory
while the previous result was still held. The plain-table path, reported
separately as `batch_buffer_table`, still pays its input copy on top and
stays under 1.0x at 0.56x. `build_summary` still wins at 15.6x: LuaJIT's
own sort over a table of boxed numbers is slow enough that even a round
trip through Rust's sort comes out ahead.

wasm is not part of this matrix — this machine has no `wasm-pack` to
measure it with. Its numbers below are from an earlier by-hand run and
predate `bind bench`: 0.8x for a batched call, 0.2x for one call per
element.

**Cross once, not per element.** `deviations` called in a loop is slower than
never binding at all: the arithmetic is a subtract, an abs and a divide, and
reaching it costs more than doing it.

**Hand over a buffer, not a sequence.** Any parameter that is a contiguous
run of one primitive goes through the buffer protocol in Python, so
`array.array`, `memoryview` and numpy arrive as a pointer with nothing
unboxed. A plain `list` still works but is copied element by element, which
is most of the gap behind `batch_buffer`'s number above. `bytes` counts:
reading 800 KB as `Vec<u8>` costs 1.6 ms through a buffer against 7.9 ms
unboxed one byte at a time.

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

**A field of handles is a seq, not a list.** A public `Vec<T>` field whose
`T` is an exported `Clone` struct reads as a `{T}Seq` handle in Python:
`chart.candles` clones the vector once and builds a `Candle` only when one
is indexed, where a list would build all 10,529 of them on every attribute
access, quadratic in a loop that indexes the field. The seq has `len`,
negative indexing, iteration, `tolist()` for the list when it is wanted,
and a column getter per field of the element type: a primitive column
comes back as the same buffer-protocol array class a `Vec<f64>` return
uses (`chart.candles.close` is one `F64Array`), an optional one as a list
of `T | None`, a string one as a list of `str`. A method returning
`Vec<T>` still returns a list of handles.

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

## Measuring the boundary

The ratios above come from `cargo soothfast bind bench`, not a stopwatch by
hand. A `[[bind]]` entry names a script:

```toml
[[bind]]
lang = "python"
out = "bindings/python"
package = "soothfast-stats"
bench = "bindings/python/bench.py"
```

`bench` is a path relative to the package root, in the host language.
`bind bench` builds that entry the way `bind build` does, then runs the
script and reads one JSON object per line off its stdout:

```
{"shape": "batch_buffer", "binding_ns": 101769.0, "host_ns": 11077107.0, "n": 100000}
```

`binding_ns` and `host_ns` are the best-of-nine medians of one call through
the binding and of the same computation written in the host language — the
script owns that measurement, not the harness. Anything else on stdout is
ignored, so a script is free to log; stderr passes straight through.
`bind bench` computes the ratio itself (`host_ns / binding_ns`; above 1.0
means the binding wins), prints a table, and — with `--save-baseline
NAME` — files each shape into the baseline under
`<crate>::bind::<lang>::<shape>`, metric `ratio`:

```bash
cargo soothfast bind bench -p PKG --only python,node --save-baseline self
```

`--only LANG[,LANG]` (shared with `bind build`) narrows which entries run.
The command fails if a script exits non-zero, prints the same shape twice,
or prints no shapes at all; a language whose toolchain isn't on this
machine is skipped with a named message instead, so one missing tool
doesn't sink the whole run. `bind bench` can launch Python, Node, Go, the
JVM, R, the C ABI directly, C++ (compiled with the same compiler
discovery `bind build` uses) and, as of this measurement, Lua; wasm, Ruby
and C# still have no launcher of their own.

Once saved, a ratio reads like any other measured metric — the Speed
section above gates four of them this way:

```
<!-- soothfast:claim soothfast_demo::bind::python::batch_buffer.ratio.ratio >= 60 -->
```

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

The future never sees pyo3's own waker. That waker takes the GIL when
called, and the polling thread is the one holding it, so a library thread
that wakes from under one of its own locks (h2 wakes a stream from inside
its connection lock) would wait for the GIL while the poller waits for the
lock. The glue polls with a waker that only signals a `Notify`, and one
runtime task per call relays each signal to pyo3 from a context holding
nothing, which also coalesces a burst of wakes from a streaming body into
one trip through the event loop.

Glue crates binding an `async fn` take a `tokio` dependency for this, and
cargo unifies it with whatever tokio the bound crate already pulls in. A
surface with nothing async takes neither the dependency nor the runtime.

### Free-threaded Python

The module declares `gil_used = false`, so on a free-threaded interpreter
(`python3.14t`; the default build still has a GIL) importing it does not
switch the GIL back on for the whole process. The declaration holds because
nothing in the glue shares mutable state outside pyo3's own borrow checking:
a handle's `&mut self` calls and setters go through `PyRefMut`, a returned
array is read-only through the buffer protocol, and the runtime is `Sync`.

On the default build the GIL is rarely in the way of the binding itself: a
sync call releases it for the Rust body (`py.detach`), and an `async fn`
runs on the runtime's workers with the Python thread holding the GIL only
for each brief poll. What a free-threaded build adds is parallelism for
the *Python* code around the calls, and for the Python objects a getter
builds. Each interpreter needs its own wheel; name both under
`interpreters` and `bind build` produces `cp314` and `cp314t`.

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
