# Changelog

## Unreleased (draft vs HEAD)

### API surface

No public API changes.

### Performance

| item | instructions | median | p99 | allocs | polls |
|---|---:|---:|---:|---:|---:|
| `soothfast_docs::bench_claim_parse` | 2694 | 128.2ns | 130.5ns | 6 | — |
| `soothfast_docs::bench_markdown_scan` | 4594132 | 182.44µs | 187.83µs | 4114 | — |
| `soothfast_measure::bench_summarize` | 4139113 | 235.03µs | 257.64µs | 4 | — |
| `soothfast_measure::bench_sweep_evaluate` | 236 | 10.6ns | 10.8ns | 0 | — |
| `soothfast_registry::bench_fnv1a` | 245784 | 79.87µs | 108.07µs | 0 | — |


## 0.1.0

Initial release.
