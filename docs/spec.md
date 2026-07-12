# Specs

For servers, the API surface that matters isn't Rust's — it's whatever a
client actually calls against: an OpenAPI path, an AsyncAPI channel, a
GraphQL field, an MCP tool. `#[soothfast::route]` declares which spec
operation a handler implements.

What soothfast then does depends on **who owns the contract**, and the two
directions are opposites:

| | you serve it | someone else serves it |
|---|---|---|
| mode | `generate` | `check` |
| source of truth | the code | the spec file |
| command | `spec gen` | `spec check` |
| what fails CI | a consumer-breaking change (`spec gate`) | drift in either direction |

A contract you serve can be *derived* from your handlers, so the spec and the
code cannot disagree and there is nothing to remember to update. A contract
someone else serves cannot be generated from your source at all — there,
reconciliation is the only option. `mode` in `soothfast.toml` picks per file,
and defaults to `check`, since generation overwrites.

> This page is illustrative rather than dogfooded like the rest of the
> site: nothing in this workspace is a server, so there's no real spec file
> to reconcile against. The annotation itself is real and runs below —
> only the spec file it names is fictional.

## Declaring a route

`#[route]` is pure metadata: it doesn't require an HTTP framework, doesn't
touch the function body, and doesn't need the spec file to exist at compile
time (only `spec check` reads it, at run time). `spec` and `operation` are
required; `method` and `path` are optional extra fields to reconcile.

Three further optional attributes — `request`, `response` and `status` —
describe the wire *shape* rather than the operation's identity. They are
recorded on the route today and read by spec generation; `spec check`
ignores them, since reconciliation matches on identity alone.

```rust capture-output
use soothfast::route;

#[route(
    spec = "specs/openapi.yaml",
    operation = "getItem",
    method = "GET",
    path = "/items/{id}"
)]
pub fn get_item(id: u64) -> String {
    format!("item-{id}")
}

fn main() {
    println!("{}", get_item(42));
}
```

```text soothfast-output
item-42
```

## Overriding an inferred shape

Generation reads a handler's own signature: extractor wrappers like
`Json<T>` and `Query<T>` mark which parameters are part of the wire
contract, and the return type gives the response. Nothing needs restating
while the signature says it.

The exception is a return type that erases itself. `-> impl IntoResponse`
carries only a trait bound, so no static analysis can recover the concrete
shape — `response` names it, and `status` sets the success code:

```rust ignore
#[route(
    spec = "specs/openapi.yaml",
    operation = "createItem",
    method = "POST",
    path = "/items",
    response = "Item",
    status = 201
)]
pub async fn create_item(Json(body): Json<NewItem>) -> impl IntoResponse {
    // request body inferred as NewItem; response named above
}
```

An override always wins over inference, and resolves the gap it answers —
a route whose erased return is named this way stops being reported.

## Generating

`cargo soothfast spec gen -p PKG` writes every spec file a `[[spec]]` entry
marks `mode = "generate"`. Which files those are, and the metadata no code
can supply, live in `soothfast.toml`:

```toml
[[spec]]
path = "specs/openapi.yaml"
mode = "generate"
title = "Items API"          # defaults to the package name
version = "2.1"              # defaults to the package version
servers = ["https://api.example.com"]

[[spec]]
path = "vendor/stripe.yaml"  # they serve it; we only reconcile
mode = "check"
```

Rendering is byte-deterministic and only rewrites a file whose content
actually changed, so a no-op run leaves nothing to commit.

### Dialects

All four dialects `spec check` reads are also generated, so any surface
soothfast can verify it can also derive. The dialect comes from the file
name, or from an explicit `dialect =` key when the name doesn't say:

| dialect | `dialect =` | inferred from | `#[route(method = ...)]` |
|---|---|---|---|
| OpenAPI 3.1 | `openapi` | anything else (the default) | `GET`, `POST`, `PUT`, ... |
| AsyncAPI 3.0 | `asyncapi` | `asyncapi` in the filename | `SEND`, `RECEIVE`, `PUBLISH`, `SUBSCRIBE` |
| GraphQL SDL | `graphql` | `.graphql`, `.graphqls`, `.sdl` | `QUERY`, `MUTATION`, `SUBSCRIPTION` |
| MCP tools | `mcp` | `mcp` or `tools` in the filename | `TOOL` |

```toml
[[spec]]
path = "specs/api.yaml"
mode = "generate"
dialect = "asyncapi"        # the filename would have said OpenAPI
```

A method from the wrong vocabulary is a **conflict**, not a guess: a `GET`
route pointed at a GraphQL file fails the run rather than being filed under
some invented root.

**OpenAPI** emits 3.1, not 3.0, which cannot express the `const` and
`prefixItems` that serde's enum and tuple representations require.

**AsyncAPI** emits 3.0, whose `action` is written from the *application's*
point of view, while 2.x's `publish`/`subscribe` were written from the
client's — so they read as opposites. Both vocabularies work and mean the
same thing, which keeps one annotation valid whether the file it faces is
generated 3.0 or hand-authored 2.x:

| method | action | meaning |
|---|---|---|
| `SEND`, `SUBSCRIBE` | `send` | this application publishes the message |
| `RECEIVE`, `PUBLISH` | `receive` | this application consumes it |

A sender's payload is what the handler returns; a receiver's is what it
accepts.

**MCP** builds each tool's `inputSchema` from the request body's fields plus
any parameters, and its `outputSchema` from the success response — omitting
`outputSchema`, with a note, when that response is not an object, since an
omitted output schema means "unstructured" where a wrapped one would be a
contract nobody serves. Shared types travel inside each tool as `$defs`,
transitively and no wider.

**GraphQL** is the one dialect that cannot pass JSON Schema through, because
SDL is a different type system. Inputs and outputs become separate
declarations (`Item` and `ItemInput`), nullability moves from the parent's
`required` list onto the reference (`id: String!`), and a request body
becomes one `input:` argument rather than a splat of fields. What has no
faithful SDL spelling — untagged unions, flattened structs, maps, 64-bit
integers, field names SDL cannot spell — becomes a `JSON` or `Int64` custom
scalar and a note, never a quietly wrong type.

Three things cannot be derived, and each is *reported* rather than guessed:
erased return types, `#[serde(with = "...")]` fields whose wire shape is
defined by code, and foreign types with no mapping. Each emits an open
schema — imprecise, never wrong — and names itself in the run's output.
Foreign types are the configurable one:

```toml
[spec.types]
"uuid::Uuid" = { type = "string", format = "uuid" }
"rust_decimal::Decimal" = { type = "string" }
```

## Gating

Once a spec is generated it can no longer disagree with the code, so the
useful question moves from "does this match?" to "did this break the people
calling it?".

<!-- soothfast:bind soothfast_spec::openapi::diff::diff -->
`cargo soothfast spec gate -p PKG` rebuilds the spec from the merge-base in a
temporary worktree and compares it against this branch's — there is no
committed baseline to go stale. Requiredness means opposite things in the two
directions, which is what the classification turns on: on a **request**, the
server growing stricter breaks callers, so a newly required field is
breaking and dropping a requirement is not; on a **response**, the server
providing less breaks callers, so a field that stops being guaranteed is
breaking and a new one is not. Removed operations, properties, status codes
and enum values are breaking either way, as is any changed type. The gate
exits non-zero on a breaking change unless `--allow-breaking` says the break
is deliberate.
<!-- /soothfast:bind -->

Every dialect gates on that same asymmetry, read off whichever key states
the direction. For **AsyncAPI** it is the operation's own `action`: a
message you `send` is one a consumer reads, so dropping a guaranteed field
breaks them, while on a `receive` the same edit relaxes a demand and it is
the *new required field* that breaks the producer. For **GraphQL** it is the
declaration kind — `T!` becoming `T` withdraws a guarantee on a `type` and
relaxes one on an `input`. For **MCP** a tool's arguments flow inward and its
result outward, so the two schemas are compared in opposite directions.
Reversing an AsyncAPI action, moving a channel's address, and withdrawing a
tool's structured output are breaking on their own.

GraphQL is diffed as a type graph rather than as SDL text, so reformatting
is invisible and a real edit is named at the field it happened to.

`spec gen --check` is the companion staleness gate: it fails when
regenerating would change a committed file, so the bot's output can never
silently diverge from the code.

## Reconciliation

<!-- soothfast:bind soothfast_spec::reconcile::reconcile -->
`cargo soothfast spec check -p PKG` runs the bench binary with
`--list-routes`, groups declared `#[route]`s by their `spec` file, parses
each (dialect auto-detected — no `--provider` flag exists), and matches
by `operation` name: a route with no matching spec op is `missing_spec`; a
match whose `method`/`path` differ from what the spec declares is
`mismatched`; a spec op with no implementing route is `missing_handler`.
Anything else is `matched`. An empty `method` or `path` on the route side
skips just that one cross-check.
<!-- /soothfast:bind -->

Four provider dialects, sniffed from the file automatically:

| dialect | source shape |
|---|---|
| OpenAPI | `paths.<path>.<method>`; operation = `operationId` or `"METHOD path"` |
| AsyncAPI | 2.x `channels.<channel>.{subscribe,publish}`, or 3.0 `operations` with `action`; operation = `operationId` or `"channel.verb"` |
| GraphQL SDL | `type Query/Mutation/Subscription { ... }` fields |
| MCP tool schema | a `tools[]` array, matched by `name` |

## Where route markers live

`#[route]` only works inside a binary that links the registry, and
`soothfast` is meant to stay a dev-dependency so it usually can't sit directly on the real handler
in `src/`. The common workaround is a thin marker function, `covers`-style,
in the crate's `[[bench]]` target.

By default `spec check` and `docs routes` read that registry from the same
`benches/soothfast.rs` target that `measure`/`gate` use for performance
benches — fine for a crate with a handful of routes, but it means a file
named after benchmarking ends up holding pure spec/route metadata with no
perf measurement in it at all, which reads as a mix-up to anyone landing on
it for the first time.

Pass `--target NAME` to point a command at a different `[[bench]]` target:

```toml
[[bench]]
name = "soothfast"          # measure / gate read this one
harness = false

[[bench]]
name = "soothfast-routes"   # spec check / docs routes read this one
harness = false
```

```console
$ cargo soothfast spec check -p myserver --target soothfast-routes
$ cargo soothfast docs routes -p myserver --target soothfast-routes
```

Route markers can then live in `benches/soothfast-routes.rs`, separate from
the perf benches in `benches/soothfast.rs`.

## The endpoint reference

`cargo soothfast docs routes -p PKG` renders `docs/routes/<pkg>.md`: one
section per spec file, one row per declared operation — method, path,
handler id, reconciliation status, the handler's own rustdoc summary, and
(when the handler is also a measured item, attributed by id or `covers=`) a
measured-cost line. It's the FastAPI-style page `#[route]` exists to
produce without hand-maintaining it.
