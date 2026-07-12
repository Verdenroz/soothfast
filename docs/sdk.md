# SDKs

A spec tells someone what to call. An SDK is the thing they actually import.
`cargo soothfast sdk gen` emits one for Python and TypeScript from the same
annotated handlers the spec comes from, so the client cannot describe a
different API than the server serves.

There are no external generators and no template engine involved. The
emitters render from the same IR the spec emitters use, which is what makes
the output byte-deterministic. A generated SDK is checked into the repo it
belongs to and diffs like ordinary source.

| | what it is | who writes it |
|---|---|---|
| runtime | retries, errors, pagination, decoding | hand-written once, per language |
| models and client | one type per schema, one method per route | emitted per package |
| packaging | `pyproject.toml` or `package.json`, README | emitted per package |

That split is worth the trouble. Everything behavioral lives in a runtime
that ships verbatim, so changing the retry policy is one reviewed edit in
one file instead of a re-render of every consumer's client. The emitted half
stays declarative.

## Lowering

<!-- soothfast:bind soothfast_sdk::lower::lower -->
Every typing question is answered once, before either emitter sees the
model. `lower` turns the JSON Schema IR into a language-neutral `Sdk`:
schemas become `Model`s, operations become `Method`s, and each type node
becomes a `Ty`. A `oneOf` becomes a union the runtime decodes structurally.
A gap the schema engine could not resolve becomes `Ty::Any` plus a reported
note, rather than a guess that would typecheck and then fail on the wire.
<!-- /soothfast:bind -->

The one thing lowering takes an `SdkKind` for is how wire names are spelled:

- Python snake_cases them. Two wire names that collide after snake_casing,
  like `logoUrl` and `logo_url`, are disambiguated rather than silently
  merged, and the rename is reported as a note.
- TypeScript stays wire-faithful. Keys that are not legal bare identifiers
  are quoted instead of renamed, so nothing can collide in the first place.

Either way `Param.wire` keeps the name that goes on the wire, so a renamed
argument never changes the request.

Names the emitted code gives its own arguments are reserved the same way:
`self` and `body` in Python, `options` in TypeScript. A path parameter
called `self` moves aside instead of producing a signature that declares the
same name twice.

## Emitting

```toml
[[sdk]]
package = "acme-items"
language = "python"
```

`cargo soothfast sdk gen -p PKG` writes the tree. Python gets frozen
dataclasses, a sync `Client` and an async `AsyncClient`. TypeScript gets
interfaces, a per-method options interface, and one async `Client`.

TypeScript carries no runtime dependencies. The transport is global `fetch`,
and because the emitted interfaces use the wire property names, a parsed
response is already the typed value. There is no decode step that could
drift from the schema. Python does decode, driven by the dataclass type
hints, which is what lets it snake_case without losing the wire mapping.

## Embedded servers

`embed` turns an SDK from a client for a hosted API into a self-contained
package. A client built with no base URL spawns the bundled server and talks
to that, so consumers deploy nothing.

```toml
[[sdk]]
package = "acme-items"
language = "typescript"
embed = "acme-items-server"
```

The handshake is one line of stdout:

```text
soothfast-ready {"base_url":"http://127.0.0.1:53017"}
```

That single line is the whole contract, and it buys two things. The server
binds port 0 and reports where it landed, instead of both sides guessing a
port and racing. And because launchers scan for the prefix and ignore
everything else, the server stays free to log. It is plain text on a pipe,
so an embedded server need not use soothfast, or even be written in Rust.

Both launchers keep one shared server per binary and launch environment,
reap it on process exit, and honor two escape hatches:

| variable | effect |
|---|---|
| `<PKG>_BASE_URL` | use a running instance, spawn nothing |
| `<PKG>_SERVER_BIN` | use a different binary |

Python resolves eagerly in `__init__`. TypeScript resolves on first request,
which is why its transport accepts a base-URL thunk rather than a string.
Both drain stdout and stderr for the life of the process, since a server
free to log is a server that fills a 64 KiB pipe buffer and blocks on write.
They keep only a tail of stderr, which is all a failure message needs.

### Configuring one

A server takes environment variables. An SDK takes constructor options.
`embed_env_template` bridges the two.

```toml
[[sdk]]
embed = "acme-items-server"
embed_env_template = ".env.template"
embed_env = { HOST = "127.0.0.1" }
```

<!-- soothfast:bind soothfast_sdk::envtemplate::parse -->
The server's own dotenv template is parsed into a typed `ServerEnv`, a
`total=False` TypedDict in Python and an interface in TypeScript, keyed by
the server's real variable names. The grammar is the subset every such file
already uses: `KEY=value` lines, `#` comments above an entry as its
documentation, and `# KEY=value` for a knob deliberately left unset. Names
are unique, so a template that shows an example above the real entry names
one knob rather than two. Anything unparseable is skipped rather than
rejected, since the file belongs to the server and an SDK is not the place
where a stray line becomes fatal.
<!-- /soothfast:bind -->

Because the server's file is the only list, the SDK's configuration surface
cannot drift from what the server actually reads.
<!-- soothfast:bind soothfast_sdk::envtemplate::markdown_table -->
The same knobs are rendered into the package README as a table, identical in
both languages. The variables belong to the server, not to whoever is
calling it.
<!-- /soothfast:bind -->

`embed_env` holds the package's own launch settings, applied to every spawn.
Precedence runs from weakest to strongest:

```text
ambient environment  <  embed_env  <  the caller's server_env
```

Config beats ambient on purpose. A `PORT` the consumer exported for their
own app should not decide where a bundled server binds. An explicit argument
still beats both.

## Shipping a binary

`targets` narrows the platform matrix, and the default is the five
mainstream ones. `cargo soothfast sdk build -p PKG` compiles the server per
target and stages it where each ecosystem expects. A target whose toolchain
is missing is reported and skipped rather than treated as fatal.

The two ecosystems disagree about how a native binary travels, so the
emitters follow each convention rather than inventing a third:

- npm gets one `optionalDependencies` sub-package per target, the esbuild
  pattern. npm installs only the matching one, the launcher resolves it, and
  it falls through to `PATH` when absent. `sdk publish` sends the platform
  packages before the main one, since its optional deps name them by exact
  version.
- Python gets one platform wheel per target, built by a generated hatchling
  hook that force-includes the binary and stamps the tag. Built without
  those variables it produces a pure wheel, which is what an sdist install
  gets.

A `manylinux_*` tag is a claim about the build environment rather than the
table. It holds only if the binary was linked against a glibc that old.
`sdk build` checks `AUDITWHEEL_PLAT` and warns when it cannot tell that it
was, and the release workflow builds the glibc targets inside the manylinux
image so the claim is earned.

`sdk publish` refuses an embedding SDK with nothing staged, or only part of
the matrix, unless you pass `--allow-unbundled`. A package that advertises a
bundled server and quietly falls back to `PATH` is worse than one that
admits it has no binary.

## Gating

`cargo soothfast sdk gate -p PKG` fails when the committed SDK is stale, the
same way `spec gen --check` does for specs. Emission is byte-deterministic,
so "stale" is an exact comparison rather than a heuristic.

The generated SDKs are exercised end to end as well. Each language builds
its golden package and runs it against a live HTTP server, and against the
real subprocess launcher via a hand-rolled fixture that calls `announce` the
way a bundled server would.
