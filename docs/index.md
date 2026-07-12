# soothfast

> *soothfast* is Old English *sōþfæst*, "fixed in truth": *sooth* (truth) +
> *fast* (firmly fixed). Docs and numbers that stay true, or CI fails.

Annotations in your code are the source of truth. Both the documentation and
the performance numbers are derived from them, and CI fails when either one
drifts from what the code actually does.

## This site is the dogfood

Every page here is either produced by soothfast from the current code (the
API reference, performance tables, coverage, `llms.txt`) or gated by it.
This page and the guide carry bind and claim markers that fail CI when the
code or the numbers change underneath them. Nothing here is maintained by
hand.

<!-- soothfast:bind soothfast::keep -->
Every measured body needs one thing: route constant inputs and results
through `soothfast::keep`, the `black_box` equivalent. Without it LLVM
const-folds the work away and you measure nothing.
<!-- /soothfast:bind -->

The sentence below is a checked claim, evaluated against this build's own
measurement run:

<!-- soothfast:claim soothfast_docs::bench_markdown_scan.alloc.allocs <= 5000 -->
Scanning an 8,000-line markdown file for bind/claim markers and code fences
costs under five thousand allocations.
<!-- /soothfast:claim -->

## Where to go

- **[Measuring](measuring.md)**: annotate a function and get a gated bench
  suite, with checked complexity, alloc, and tail-latency claims.
- **[Gating](gating.md)**: how `cargo soothfast gate` picks a reference,
  what counts as a regression, and what a ratchet is for.
- **[Living docs](living-docs.md)**: bind and claim marker syntax, the
  `soothfast.lock` fingerprint ledger, and the fence tags that turn a
  markdown code block into a checked test or a captured example.
- **[Spec reconciliation](spec.md)**: `#[route]` reconciled against
  OpenAPI, AsyncAPI, GraphQL and MCP specs, plus the endpoint reference it
  renders.
- **[SDKs](sdk.md)**: Python and TypeScript clients emitted from the same
  handlers, and how to bundle a server inside one.
- **[Reports](reports.md)**: perf tables, SVG trend charts, badges,
  changelog drafts, and `llms.txt`, all produced from a baseline.
- **[API reference](reference/soothfast.md)**: generated from rustdoc JSON
  on every build. Items show their verified claims and measured cost.
- **[Performance](perf/summary.md)**: the latest measurement run as tables
  and trend charts, produced rather than written.
