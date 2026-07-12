# Living docs

Prose rots in two ways. It describes code that has since changed, or it
states a number that is no longer true. The docs engine gates both, and this
page is itself checked by the mechanism it describes.

## Binds: prose tied to a fingerprint

<!-- soothfast:bind soothfast_docs::markdown::scan -->
`<!-- soothfast:bind path::to::item -->` marks the prose that follows as a
claim about that item's *behavior*. `cargo soothfast docs accept` records
the item's current source fingerprint (FNV-1a over its normalized token
stream) in `soothfast.lock`. `cargo soothfast docs check` recomputes it and
fails the build as soon as it no longer matches, which means the code
changed under prose nobody re-read.

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

`soothfast.lock` is plain JSON: `{"version": 1, "binds": {"item::path":
"<16-hex-digit fingerprint>"}}`. Running `docs accept` with explicit PATHS
merges into the existing lock, so binds outside that scope survive. Running
it with no PATHS replaces the whole map, which is how dead binds get
dropped.

## Claims: numbers checked against a real run

A claim marker looks like
`<!-- soothfast:claim item.backend.metric <op> value[unit] -->`. It takes
exactly three dot-separated tokens before the operator (`item` uses `::`
internally, never `.`), one of `< <= > >=`, and a bound with an optional
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
| *(none)* | becomes a real `#[test]`, generated into `tests/soothfast_doc_<file>.rs` |
| `ignore` | excluded entirely: no test, no example, no check |
| `no_run` | compiled but never executed (a plain fn, not `#[test]`) |
| `capture-output` | becomes a runnable example. `docs capture` runs it and splices real stdout into a `text soothfast-output` fence below it |
| `feature=NAME` | gates the generated test/example behind a cargo feature |
| `mock=NAME` / `mock=NAME(ARG)` | activates a `#[soothfast::mock_seam]` backend by name (requires an accompanying `feature=` tag) |
| `covers=path::to::item` | attaches a bind/claim's chip to this block instead of rendering it standalone (comma-separated for several items, no spaces) |

`cargo soothfast docs check -p PKG` runs three checks in order: bind
fingerprints against `soothfast.lock`, claims against `--baseline`, then a
fresh generation of every test and example in memory, diffed against what is
on disk. A stale `docs gen-tests` or `docs capture` run therefore fails CI
just like a stale bind or a violated claim.

## Why this page has almost no `ignore`d code

Every rust block on this site either runs for real, with `capture-output`
producing its output rather than someone typing it, or is at least checked
to compile with `no_run`. An `ignore`d block is invisible to all of the
checks above and can drift silently for years. Reach for it only when a
block genuinely cannot compile on its own, because it needs a real network
or database, and prefer `no_run` even then if the code is otherwise real.
