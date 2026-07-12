# soothfast

> *soothfast* — Old English *sōþfæst*, "fixed in truth": *sooth* (truth) +
> *fast* (firmly fixed). Docs and numbers that stay true, or CI fails.

Measured and documented Rust: annotations in your code are the single source
of truth, and both documentation and performance measurement are **derived**
from them. A CI gate fails when either drifts.

## This site is the dogfood

Every page here is either *produced* by soothfast from the current code
(API reference, performance tables, coverage, `llms.txt`) or *gated* by it
(this page and the guide carry bind and claim markers that fail CI when the
code or the numbers change underneath them). Nothing on this site is a
hand-maintained artifact.

<!-- soothfast:bind soothfast::keep -->
The one thing every measured body needs: route constant inputs and results
through `soothfast::keep` (the `black_box` equivalent), or LLVM const-folds
the work away and you measure nothing.
<!-- /soothfast:bind -->

The sentence below is a checked claim, evaluated against this build's own
measurement run:

<!-- soothfast:claim soothfast_docs::bench_markdown_scan.alloc.allocs <= 5000 -->
Scanning an 8,000-line markdown file for bind/claim markers and code fences
costs under five thousand allocations.
<!-- /soothfast:claim -->

## Where to go

- **[Measuring](measuring.md)** — annotate a function, get a gated bench
  suite: checked complexity, alloc, and tail-latency claims.
- **[Gating](gating.md)** — how `cargo soothfast gate` picks a reference,
  what regresses the build, and what a ratchet is for.
- **[Living docs](living-docs.md)** — bind/claim marker syntax, the
  `soothfast.lock` fingerprint ledger, and the fence-tag vocabulary that
  turns a markdown code block into a checked test or a captured example.
- **[Spec reconciliation](spec.md)** — `#[route]` reconciled against
  OpenAPI/AsyncAPI/GraphQL/MCP specs, and the FastAPI-style endpoint
  reference it renders.
- **[Reports](reports.md)** — perf tables, SVG trend charts, badges,
  changelog drafts, and `llms.txt`, all produced from a baseline.
- **[API reference](reference/soothfast.md)** — generated from rustdoc JSON on
  every build; items show their *verified* claims and measured cost.
- **[Performance](perf/summary.md)** — the latest measurement run as tables
  and trend charts, produced, never written.
