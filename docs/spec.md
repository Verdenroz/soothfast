# Specs

For servers, the API surface that matters is not Rust's. It is whatever a
client actually calls against: an OpenAPI path, an AsyncAPI channel, a
GraphQL field, an MCP tool. `#[soothfast::route]` declares which spec
operation a handler implements.

What soothfast does next depends on who owns the contract, and the two
directions are opposites:

| | you serve it | someone else serves it |
|---|---|---|
| mode | `generate` | `check` |
| source of truth | the code | the spec file |
| command | `spec gen` | `spec check` |
| what fails CI | a consumer-breaking change (`spec gate`) | drift in either direction |

A contract you serve can be derived from your handlers, so the spec and the
code cannot disagree and there is nothing to remember to update. A contract
someone else serves cannot be generated from your source at all, so
reconciliation is the only option there. `mode` in `soothfast.toml` picks
per file and defaults to `check`, since generation overwrites.

> This page is illustrative rather than dogfooded like the rest of the site.
> Nothing in this workspace is a server, so there is no real spec file to
> reconcile against. The annotation itself is real and runs below. Only the
> spec file it names is fictional.

## Declaring a route

`#[route]` is pure metadata: it doesn't require an HTTP framework, doesn't
touch the function body, and doesn't need the spec file to exist at compile
time (only `spec check` reads it, at run time). `spec` and `operation` are
required; `method` and `path` are optional extra fields to reconcile.

Three further optional attributes describe the wire *shape* rather than the
operation's identity: `request`, `response` and `status`. They are recorded
on the route and read by spec generation. `spec check` ignores them, since
reconciliation matches on identity alone.

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
shape. Use `response` to name it, and `status` to set the success code:

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

An override always wins over inference and resolves the gap it answers, so
a route whose erased return is named this way stops being reported.

### Detached markers

A marker fn in a `[[bench]]` target (see [Where route markers live](#where-route-markers-live))
has an empty signature, so there is nothing to infer from and its
attributes *are* the contract. `params` names the struct whose fields
flatten into query parameters, and `path_params` types the path's
`{placeholder}`s. Without it every placeholder is an open `string`, which is
honest but weaker than the enum the real handler takes.

```rust ignore
#[route(
    spec = "specs/openapi.yaml",
    operation = "getHolders",
    method = "GET",
    path = "/holders/{symbol}/{type}",
    params = "HoldersQuery",
    path_params = "type: HolderType",
    response = "HoldersResponse"
)]
fn route_get_holders() {}
```

`path_params` takes either comma-separated `name: Type` bindings or a single
struct name whose *field* names pick out the placeholders, the shape an axum
`Path<HolderPath>` extractor would take. Naming a placeholder the path does
not have is an error rather than a silent no-op, since it is always a typo.
Placeholders nothing names keep the open `string`.

A `params` field that happens to name a placeholder types it too, and is
then not also emitted as a query parameter. The URL template is the more
specific declaration, and one name cannot be in two places. Rust spells a
keyword field `r#type` or `type_` where the URL only ever says `{type}`, so
both spellings match. A route that genuinely wants the same name in both
places renames one side, either with `#[serde(rename)]` on the field or by
renaming the placeholder. Where both apply, `path_params` wins.

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
name, or from an explicit `dialect =` key when the name does not say:

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

A method from the wrong vocabulary is a conflict rather than a guess. A
`GET` route pointed at a GraphQL file fails the run instead of being filed
under some invented root.

OpenAPI emits 3.1 rather than 3.0, which cannot express the `const` and
`prefixItems` that serde's enum and tuple representations require.

AsyncAPI emits 3.0, whose `action` is written from the application's point
of view, while 2.x's `publish` and `subscribe` were written from the
client's, so they read as opposites. Both vocabularies work and mean the
same thing, which keeps one annotation valid whether the file it faces is
generated 3.0 or hand-authored 2.x:

| method | action | meaning |
|---|---|---|
| `SEND`, `SUBSCRIBE` | `send` | this application publishes the message |
| `RECEIVE`, `PUBLISH` | `receive` | this application consumes it |

A sender's payload is what the handler returns. A receiver's is what it
accepts.

MCP builds each tool's `inputSchema` from the request body's fields plus any
parameters, and its `outputSchema` from the success response. When that
response is not an object, `outputSchema` is omitted with a note, since an
omitted output schema means "unstructured" where a wrapped one would be a
contract nobody serves. Shared types travel inside each tool as `$defs`,
transitively and no wider.

GraphQL is the one dialect that cannot pass JSON Schema through, because SDL
is a different type system. Inputs and outputs become separate declarations
(`Item` and `ItemInput`), nullability moves from the parent's `required` list
onto the reference (`id: String!`), and a request body becomes one `input:`
argument rather than a splat of fields. Anything with no faithful SDL
spelling becomes a `JSON` or `Int64` custom scalar plus a note, never a
quietly wrong type. That covers untagged unions, flattened structs, maps,
64-bit integers, and field names SDL cannot spell.

### Who names the wire

Field and variant names come from whichever framework actually serialises the
type. For most types that is serde, and `#[serde(rename)]` / `rename_all` are
read straight out of rustdoc's preserved attributes.

A type async-graphql serves is different. The response JSON is built by
async-graphql's resolver, which never consults serde, so `#[graphql(...)]` is
the wire truth and serde's attributes are ignored for naming entirely. The
order is container `rename_fields` or `rename_items`, then a field's or
item's own `name`, with `#[graphql(skip)]` taking a field off the wire. Types
with no `#[graphql]` attribute at all are still renamed, because
async-graphql's defaults are camelCase for fields and
`SCREAMING_SNAKE_CASE` for enum items.

The two frameworks' casing genuinely disagrees, so this is a correctness
matter rather than a tidiness one: async-graphql renames through `Inflector`,
which opens a new word at every digit, and puts `price_change_percentage_24h`
on the wire as `priceChangePercentage24H` where serde says `…24h`. Adding a
serde rename to paper over that is worse still, because those types are
usually also `Deserialize`d from a library's snake_case JSON, where a rename
silently zeroes every field instead of erroring.

Detection reads the trait impls the derive generated, which rustdoc records
even though it drops the derive itself, and falls back to the presence of a
`#[graphql]` attribute. A type that is *both* an async-graphql object and a
serde-serialised REST body is genuinely ambiguous; annotate it with
`#[route(response = "...")]` if the derived answer is the wrong one.

Three things cannot be derived, and each is *reported* rather than guessed:
erased return types, `#[serde(with = "...")]` fields whose wire shape is
defined by code, and foreign types with no mapping. Each emits an open
schema, imprecise but never wrong, and names itself in the run's output.
Foreign types are the configurable one:

```toml
[spec.types]
"uuid::Uuid" = { type = "string", format = "uuid" }
"rust_decimal::Decimal" = { type = "string" }
```

### Transparent wrappers

A literal schema is the wrong tool for a *transparent newtype wrapper*, a
foreign type like `#[serde(transparent)] pub struct Json<T>(pub T)`, which is
exactly its type argument on the wire. The lookup key carries no generic
arguments, so one literal would have to stand for every `T`, and every
`Json<Quote>`, `Json<Money>` and `Json<Value>` alike would render as whatever
that one literal said. Say `transparent` instead:

```toml
[spec.types]
"async_graphql::types::json::Json" = { transparent = true }
```

Now `Json<Quote>` resolves to whatever `Quote` resolves to, whether that is
walked locally, pulled from a workspace crate, or mapped from this same
table. There is no wrapper component, no `$ref` to one, and no gap.
`Json<serde_json::Value>` still emits `{}`, because that is what
`serde_json::Value` maps to: the honest answer for an unconstrained payload,
arrived at through the argument rather than around it.

`transparent = true` forwards the first type argument. A wrapper whose shape
rides on a later one names its position, zero-based:

```toml
"tagged::Tagged" = { transparent = 1 }   # Tagged<Tag, T> is T on the wire
```

Two rules keep the directive honest. `transparent` and a literal schema in
one entry contradict each other, since a wrapper cannot both forward and be
a fixed schema, so the config is rejected naming the offending key rather
than letting one silently win. And a type declared transparent but *used*
with no argument at that position has nothing to forward to, so it reports a
gap and emits an open schema instead of guessing what it wrapped.

The key `transparent` is what distinguishes a directive from a literal
schema. It is not a JSON Schema keyword, so every literal entry keeps its old
meaning, including the bare `{ }` that means "genuinely unconstrained".

Locally-defined types need none of this. A `#[serde(transparent)]` struct in
the crate being documented, and every single-field newtype, already forward
to their inner type by being walked.

### Types from other crates in the workspace

A type defined in a *sibling crate of the same cargo workspace* is not one of
those three: it is absent from the package's own rustdoc index, but its own
crate can be documented too, and then it is as walkable as a local type. So
generation documents every workspace member the package depends on and
resolves across them by default. An enum in the crate next door keeps its
`enum:` constraint instead of collapsing to a bare `type: string`.

The order of preference is `[spec.types]` first (an explicit mapping is the
escape hatch, and outranks anything derived), then the workspace crates, then
a reported gap. Registry dependencies are never documented: they have no
source tree here to walk.

Each extra crate costs one nightly rustdoc build, so both halves are
adjustable per `[[spec]]` entry:

```toml
[[spec]]
path = "openapi.yaml"
mode = "generate"
workspace_types = true               # the default; false reports gaps instead
workspace_crates = ["finance-query"] # default: every workspace dep of the package
```

A crate that fails to document warns and is skipped, leaving its types to
report as gaps rather than failing the run. That covers a missing nightly
toolchain, a compile error, or a member that is not a library.

### Two types, one name

Resolving more types means more names to keep apart, and a name has to be a
property of the *type*, not of the walk that reached it: every dialect merges
the components of all its operations into one document, so a name chosen by
whichever operation arrived first would both churn between runs and collide
across operations.

So the whole assignment is computed before anything is walked, from the
documents alone. A type keeps its bare Rust name while no other type wants
it. Where several do, each takes the shortest trailing run of its module path
that tells it apart:

| Canonical path | Component |
| --- | --- |
| `finance_query::constants::Region` | `Region` |
| `finance_query::constants::indices::Region` | `indices_Region` |

Unambiguous names are settled first, so qualifying one type can never take a
bare name another type was entitled to. The result depends only on which
types exist, not on route order and not on which operation referenced what.
Two runs over the same code therefore emit byte-identical specs, and one type
is never silently described by another's schema.

## Gating

Once a spec is generated it can no longer disagree with the code, so the
useful question moves from "does this match?" to "did this break the people
calling it?".

<!-- soothfast:bind soothfast_spec::openapi::diff::diff -->
`cargo soothfast spec gate -p PKG` rebuilds the spec from the merge-base in a
temporary worktree and compares it against this branch's, so there is no
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

`--from-committed` skips the worktree rebuild and reads the committed spec
files at the merge-base instead. Every parsed file must re-render
byte-identically or the gate fails, so a stale or hand-edited base can never
be silently diffed. It is the right default for CI that already requires
`spec gen --check` on every merge, which pins the committed file to what the
merge-base would generate.

Every dialect gates on that same asymmetry, read off whichever key states
the direction. For **AsyncAPI** it is the operation's own `action`: a
message you `send` is one a consumer reads, so dropping a guaranteed field
breaks them, while on a `receive` the same edit relaxes a demand and it is
the *new required field* that breaks the producer. For **GraphQL** it is the
declaration kind: `T!` becoming `T` withdraws a guarantee on a `type` and
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
each (the dialect is auto-detected, and no `--provider` flag exists), and matches
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
benches. That is fine for a crate with a handful of routes, but it means a
file named after benchmarking ends up holding pure spec and route metadata
with no perf measurement in it at all, which reads as a mix-up to anyone
landing on it for the first time.

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
section per spec file, and one row per declared operation giving the method,
path, handler id, reconciliation status, the handler's own rustdoc summary,
and a measured-cost line when the handler is also a measured item,
attributed by id or `covers=`. It is the FastAPI-style page `#[route]` exists to
produce without hand-maintaining it.
