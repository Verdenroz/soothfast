# Living docs

Prose rots in two ways. It describes code that has since changed, or it
states a number that is no longer true. The docs engine gates both, and this
page is itself checked by the mechanism it describes.

## Binds: prose tied to a fingerprint

<!-- soothfast:bind soothfast_docs::markdown::scan -->
`<!-- soothfast:bind path::to::item -->` marks the prose that follows as a
claim about that item's *behavior*. `cargo soothfast docs accept` records
the item's current source fingerprint (FNV-1a over its source span, with
ordinary comments stripped and whitespace normalized) in `soothfast.lock`.
`cargo soothfast docs check` recomputes it and fails the build as soon as it
no longer matches, which means the code changed under prose nobody re-read.
Rewording a `//` comment inside the item leaves the fingerprint alone; a
`///` on a field does not, because that states part of the contract.

A marker the scanner cannot read is an error rather than a skipped line.
That covers an unterminated marker, or one naming a kind other than `bind`
or `claim`. Trailing whitespace after `-->` is fine. Dropping a malformed
marker silently would report a green check for a claim nothing evaluated.
<!-- /soothfast:bind -->

```markdown ignore
<!-- soothfast:bind mylib::sorted -->
`sorted` returns a new, ascending-order vector; the input is untouched.
<!-- /soothfast:bind -->
```

`soothfast.lock` is plain JSON: `{"version": 2, "binds": {"item::path":
"<16-hex-digit fingerprint>"}}`. Running `docs accept` with explicit PATHS
merges into the existing lock, so binds outside that scope survive. Running
it with no PATHS replaces the whole map, which is how dead binds get
dropped. A lock written under an older `version` reports as needing one
re-accept rather than as prose that drifted.

### Binding code in other languages

A target containing `#` is `<path>#<symbol>`: a symbol in a source file, with
the path relative to the project root (the cargo workspace root, else the git
toplevel, else the current directory). Java, Kotlin, Python, JavaScript,
TypeScript and TSX files are supported, chosen by extension. Anything else is
a Rust item path as above. File binds are parsed with tree-sitter, so
`docs check` and `docs accept` need no cargo project and no `-p`; a repo whose
binds are all file binds never builds rustdoc JSON.

```markdown ignore
<!-- soothfast:bind src/app/util.py#Parser.parse -->
<!-- soothfast:bind web/src/price.ts#formatPrice -->
<!-- soothfast:bind app/src/main/java/acme/Cart.java#Cart.total -->
<!-- soothfast:bind app/src/main/kotlin/acme/Cart.kt#Cart.Companion.empty -->
```

Symbol names follow these rules:

- Containers are dotted: `Outer.Inner.method`. A Kotlin companion object is
  the container `Companion`.
- Overloads share one name, and the fingerprint covers all of them.
- In Python, indentation is part of the code and counts toward the
  fingerprint. Ordinary comments do not, in any language; doc comments
  (`/** */`, docstrings) do, since the prose is bound to them.
- A symbol inside code that does not parse is reported as missing, never
  guessed.

The lock key is the whole target string and the fingerprint is the symbol's
own, so Rust binds and the `soothfast.lock` format are unchanged. Failures
read `unsupported language for PATH`, `file not found`, or `symbol not found
in PATH`; `not locked` and `STALE` behave as for Rust items. Claims are
unchanged and still read the measurement baseline.

## Claims: numbers checked against a real run

A claim marker looks like
`<!-- soothfast:claim item.backend.metric <op> value[unit] -->`. The last
two dot-separated parts before the operator are the backend and metric, and
everything ahead of them is the item, so an imported id with dots or spaces
(`pytest::t.py::test_parse`, `vitest::src/a.bench.ts > sum > loop`) works
as written. Then comes one of `< <= > >=`, and a bound with an optional
unit (`ns` ×1, `us`/`µs` ×1e3, `ms` ×1e6, `s` ×1e9). Unitless numbers are
taken as-is, and underscores are allowed for readability, as in `25_000`.
`docs check` evaluates the expression against
`baseline["items"][item][backend][metric]` and fails if the metric is
missing or the comparison does not hold.

<!-- soothfast:claim soothfast_docs::bench_claim_parse.walltime.p99_ns < 50000 -->
Parsing one claim expression, the exact work `docs check` does per marker on
this page, costs under 50µs at the tail.
<!-- /soothfast:claim -->

## Fence tags on rust code blocks

| tag | effect |
|---|---|
| *(none)* | becomes a real `#[test]`, generated into the page's module of `tests/soothfast_docs/` |
| `ignore` | excluded entirely: no test, no example, no check |
| `no_run` | compiled but never executed (a plain fn, not `#[test]`) |
| `capture-output` | becomes a block of the `soothfast_capture` example. `docs capture` runs it and splices real stdout into a `text soothfast-output` fence below it |
| `feature=NAME` | gates the generated test/example behind a cargo feature (comma-separated for a block that needs several to compile, no spaces — it is then gated behind `all` of them) |
| `mock=NAME` / `mock=NAME(ARG)` | activates a `#[soothfast::mock_seam]` backend by name (requires an accompanying `feature=` tag) |
| `covers=path::to::item` | attaches a bind/claim's chip to this block instead of rendering it standalone (comma-separated for several items, no spaces) |

`cargo soothfast docs check -p PKG` runs three checks in order: bind
fingerprints against `soothfast.lock`, claims against `--baseline`, then a
fresh generation of every test and example in memory, diffed against what is
on disk. A stale `docs gen-tests` or `docs capture` run therefore fails CI
just like a stale bind or a violated claim.

## Generated files

`cargo soothfast docs gen-tests -p PKG` compiles a package's blocks into two
targets, so a page adds a module rather than another binary to link:

- `tests/soothfast_docs/`: one integration test, with a `doc_<page>.rs`
  module per page that has a testable block.
- `examples/soothfast_capture/`: one example, with a `block_<page>_<n>.rs`
  module per `capture-output` block. It runs the block its first argument
  names: `cargo run -p PKG --example soothfast_capture -- readme_1`.

A capture block becomes a module, so it cannot use crate-level attributes
(`#![...]`, `#[macro_use] extern crate`), and `crate::` in it names the
example's root, not the block. A block that defines its own top-level
`fn main` keeps it, whatever it returns. When a block's `feature=` tag is
off, the example reports it as disabled, and `docs capture` skips it and
keeps the output already recorded.

Every block of a package compiles into the same example, so one block that
needs a feature it is not tagged with breaks `docs capture` for the whole
package whenever that feature is off. Capture then builds the example first,
reports one failure naming each block in the compiler's errors by its page
and line, and runs nothing. Tag such a block with `feature=`.

The pages of a package share one test crate, and its capture blocks share
one example crate. Anything crate-global in one block reaches the others: a
`#[macro_export]` macro is visible to every block, a `linkme` registration
lands in every block's registry, and two blocks installing a global
allocator (directly or through `soothfast::bench_main!()`) cannot link.
gen-tests rejects that last case, naming both blocks.

The generated `main.rs` files keep their `mod` lines and dispatch inside a
`verbatim!` macro call. rustfmt leaves such a body alone and does not visit
the modules it declares, so `cargo fmt` cannot change a generated file and
fail `docs check`.

Every generated file starts with an `@generated` header and records the page
it came from. gen-tests rewrites only files whose content changed and removes
generated files nothing produces any more, including the per-page
`tests/soothfast_doc_*.rs` and per-block `examples/soothfast_capture_*.rs`
files of earlier releases. `docs check` fails while any of those remain. A
file without the header is never touched.

The page paths given to a command assign pages to the package. A run with
paths also keeps every page the package's generated files already record,
so `docs gen-tests -p PKG docs/one.md` regenerates that page without
dropping the others. A recorded page that was deleted, or has no blocks
left, is dropped. To move a page to another package, delete its generated
files and run gen-tests for both. A run without paths covers exactly the
default scope.

## Why this page has almost no `ignore`d code

Every rust block on this site either runs for real, with `capture-output`
producing its output rather than someone typing it, or is at least checked
to compile with `no_run`. An `ignore`d block is invisible to all of the
checks above and can drift silently for years. Reach for it only when a
block genuinely cannot compile on its own, because it needs a real network
or database, and prefer `no_run` even then if the code is otherwise real.
