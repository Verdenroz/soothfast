# Interchange formats

Everything soothfast consumes is plain JSON. A tool in any language can feed
`gate`, `docs check` and the reports by emitting these three formats; none of
them needs Rust. Each has a JSON Schema (draft 2020-12), and a test in
`cargo-soothfast` validates real writer output against it, so the schemas
cannot drift from the code.

| Format | Schema | Written by | Read by |
|---|---|---|---|
| Runner records | [`runner-record.schema.json`](https://github.com/Verdenroz/soothfast/blob/master/schemas/runner-record.schema.json) | a bench binary's `--json` output | `measure`, `gate`, `spec` |
| Baseline | [`baseline.schema.json`](https://github.com/Verdenroz/soothfast/blob/master/schemas/baseline.schema.json) | `measure --save-baseline` | `gate`, `docs check`, `report`, `trend` |
| Lockfile | [`lock.schema.json`](https://github.com/Verdenroz/soothfast/blob/master/schemas/lock.schema.json) | `docs accept` | `docs check` |

The schemas describe what readers accept. Readers ignore unknown keys and
default a missing one, so a schema marks a key `required` only where a reader
skips or rejects the record without it. Writers emit more than that.

## Runner records

The bench binary (the `[[bench]]` target named `soothfast`, `harness = false`)
prints one JSON object per line on stdout when run with `--json`. `gate` and
`measure` build the target, run it, and fold the records into one run.

- A line not starting with `{` is ignored. A line starting with `{` that is
  not valid JSON fails the run.
- A record with an unknown `type` is ignored.
- Records are discriminated by `type`: `env`, `item`, `calibration`,
  `result`, `executed`, `assertion`, `sweep`, `route`.

A minimal run is an `env` record naming the gating backend, then one `result`
per item and backend, then any `assertion` records:

```json
{"type":"env","gating_backend":"perfcnt"}
{"type":"result","id":"app::decode","fingerprint":"00000000000000ff","covers":"app::decode","backend":"perfcnt","metrics":{"instructions":100,"cycles":50,"cache_refs":5}}
{"type":"assertion","id":"app::decode","kind":"alloc","ok":true,"detail":"allocs 0 <= 0"}
```

What each record does:

- `result` carries one backend's metrics for one item id. The backend picks
  which `metrics` keys are read: `walltime` (`median_ns`, `mad_ns`, `p99_ns`),
  `perfcnt` (`instructions`, `cycles`, `cache_refs`), `callgrind` (`ir`),
  `alloc` (`allocs`, `bytes`), `asyncexec` (`polls`, `wakes`). Counts must be
  JSON integers.
- `env.gating_backend` names the backend `gate` compares. Emit a `result` for
  it on every item.
- `assertion` records with `"ok":false` fail the run; this is how checked
  claims (`alloc`, `p99`, `complexity`) report.
- `fingerprint` is compared across builds to detect a changed benchmark. Any
  stable string per item works; the Rust runner writes 16 hex digits.
- `item` and `route` records answer `--list --json` and `--list-routes
  --json`. `item` needs `id` and `fingerprint`.
- `executed` and `sweep` serve callgrind item reuse and diagnostics; a tool
  that does not use callgrind omits them.

## Baseline

`.soothfast/baselines/NAME.json` is a saved run. `gate` compares against it
(`--baseline NAME`), and `docs check` evaluates claims against it. A tool can
write one directly instead of providing a bench binary:

```json
{
  "version": 1,
  "items": {
    "app::decode": {
      "fingerprint": "00000000000000ff",
      "covers": "app::decode",
      "perfcnt": { "instructions": 100, "cycles": 50, "cache_refs": 5 },
      "alloc": { "allocs": 0, "bytes": 0 }
    }
  }
}
```

- `items` maps an item id to its fingerprint, `covers` path and one object per
  backend: `walltime`, `perfcnt`, `callgrind`, `alloc`, `asyncexec`,
  `buildcost`. Writers always emit every key the schema lists for a backend;
  do the same. `imported` names the import prefix an item came from and is
  absent on measured items.
- `assertions` holds the verdicts of checked claims; `build` holds the
  compiler stamp that makes instruction counts comparable. Without `build`,
  `gate` reports the reference as predating build stamps.
- A claim `<!-- soothfast:claim item.backend.metric <op> value -->` is
  evaluated against `items[item][backend][metric]`.

## Lockfile

`soothfast.lock` at the workspace root pins the fingerprint of every bound
item:

```json
{
  "version": 2,
  "binds": { "app::decode": "00ff00ff00ff00ff" }
}
```

`docs check` reports a bind stale when its current fingerprint differs from
the locked one, and `docs accept` rewrites the file. `version` is the
fingerprint format: a lock with a lower version reads as needing one
re-accept, and a higher one is refused. Hand-writing the fingerprints of
Rust items is not supported; a tool should let `docs accept` produce them.
