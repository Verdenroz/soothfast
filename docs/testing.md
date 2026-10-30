# Testing

`cargo soothfast test` is a drop-in for `cargo test` that does only the work
a change needs. It builds and runs the test targets of the packages a change
can affect, and skips any test binary whose code and inputs match a run that
already passed.

```console
$ cargo soothfast test --all-features                  # vs the default branch's merge-base
$ cargo soothfast test -p mylib                         # exactly this package, changed or not
$ cargo soothfast test --all                            # no selection, cache still applies
$ cargo soothfast test --no-cache -- --include-ignored  # re-run everything selected
```

Flags before `--` that soothfast does not own go to `cargo test`: features,
`--release`, `--profile`, `--target`. Everything after `--` goes to each test
binary. `--no-doc` skips doc tests. `--no-run` builds the selected packages'
test binaries and runs nothing: no doc tests, and no pass is read or stored.

`-p`/`--package` replaces the change-based selection: it tests exactly the
named packages, and a name that is not a workspace member is an error.
`--exclude` subtracts from whatever was selected.

## Choosing packages

The changed files are the union of `git diff --name-only BASE...HEAD`, the
uncommitted changes and the untracked files. `BASE` is `--base`, else the
merge-base with the default branch (`origin/HEAD`, falling back to
`origin/master`, `origin/main`, `master`, `main`). When `BASE` shares no
history with HEAD, every package is selected and the summary says so.

Each changed file belongs to the workspace package whose manifest directory
contains it, taking the longest match. That package is selected, and so is
every workspace package that depends on it, transitively, through any
dependency kind, dev-dependencies included. A package at the workspace root
owns every file no nested member owns, so with a root package a change to
`docs/` or `.github/` selects it and everything that depends on it unless
`ignore` covers those paths.

These select every package:

- a change to `Cargo.lock`, `rust-toolchain(.toml)`, `.cargo/config(.toml)`
  or the root `Cargo.toml`;
- a change to any file outside every package directory;
- a change to a file an `inputs` glob matches (below).

The `[test]` section of `soothfast.toml` tunes this:

```toml
[test]
ignore = ["docs/**", "*.md"]        # changes here select nothing
inputs = ["fixtures/shared/**"]     # files tests read from outside a package
env    = ["DATABASE_URL"]           # variables whose values change the result
retries = 2                         # reruns of a failed test binary
```

`inputs` wins over `ignore`. Ignore only files no test reads.

`retries` (default 0) reruns each failed test binary alone, through its own
`cargo test -p PKG --lib` (or `--bin`, `--test`, `--bench NAME`), up to that
many times. A binary that passes a rerun is flaky: the summary lists it on a
`test: FLAKY:` line, the command exits 0 for it, and no pass is stored, so the
next run executes it again. Doc tests are not rerun. `--result FILE` writes
the outcome as JSON, with the verdict `flaky` when any binary was.

### Allow-lists

A package that owns many files no test reads, most often a package at the
workspace root, can list the files that matter instead of ignoring the rest:

```toml
[test.packages.mylib]
files = ["src/**", "tests/**", "benches/**", "examples/**", "build.rs", "docs/**"]
```

The globs are relative to the package's manifest directory. A file the
package owns that matches none of them selects nothing and is left out of
the package's key, so a new file at the workspace root no longer re-runs
the root package's tests. `inputs` still wins over the list, `ignore` still
subtracts from it, and the files that select every package (`Cargo.toml`,
`Cargo.lock`, the toolchain and cargo config) select every package whatever
it lists, and a package's own `Cargo.toml` always counts for it. A nested
member's files belong to the member, not to the root's list. A package without a table keeps every file it owns.

List everything the package's tests read from inside its directory:
fixtures, goldens, data files, and the generated `tests/soothfast_docs/`
and `examples/soothfast_capture/` targets (`tests/**` and `examples/**`
cover them). Files pulled in with `include_str!` or `include_bytes!` are in
the key through the executable even when the list leaves them out. The list
is about what tests read, not what `cargo package` ships: a test may read a
file the manifest's `exclude` drops, and that file still needs listing.

A table naming a package the workspace does not have is an error, and so
is a table with no `files`.

## The pass cache

After building, each test executable gets a key. A stored pass under
`.soothfast/tests/<key>` means the executable is skipped, with a line naming
it. Only passes are stored, never failures, and the store keeps the 2048 most
recently used entries.

The key hashes:

- the executable's loaded sections, so source changes, features, profile and
  target are all covered by the code itself. Files pulled in with
  `include_str!` or `include_bytes!` are compiled in, so they count even when
  they live outside the package;
- every file the package owns, `.rs` included, by content, plus modified and
  untracked ones (sources, fixtures, goldens, data files). That is the same
  set selection assigns to it: a nested member's files belong to the member,
  and an ignored file belongs to no package;
- the files matched by `[test] inputs`, and every file whose change selects
  every package, so a run those files select cannot be served from a pass
  stored before they changed;
- the plain `bin` and `example` executables of the package, which its
  integration tests may spawn without embedding;
- the arguments after `--`;
- the values of the `[test] env` variables;
- `rustc -vV`.

`RUST_TEST_THREADS` and `RUST_BACKTRACE` are not inputs.

### What the key does not see

A test that reads anything else can pass from the cache after that thing
changed. Declare what you can with `inputs` and `env`; the rest is a known
limit:

- files outside the package that are not declared, including ones a test
  reaches by a relative path such as `../fixtures/x` or
  `concat!(env!("CARGO_MANIFEST_DIR"), "/../x")`;
- files inside the package that its allow-list leaves out;
- external programs a test spawns, such as `cargo`, `objdump`, `valgrind`,
  `node` or `uv`;
- shared libraries such as libc, and the toolchain beyond `rustc -vV`;
- binaries of other packages, including a bundled example another package's
  test starts;
- the network and the clock.

In this workspace no Rust test spawns a binary or example of a package it
does not own. `soothfast-sdk`'s `embed_server` example is started by the
Python and Node suites, which `cargo soothfast test` does not run.

## Running

Every binary runs in one `cargo test`: the build's own invocation, with the
same packages and flags, without `--no-run`, and with `--no-fail-fast`. The
build is already fresh and resolves the same features, so cargo compiles
nothing and only runs. The test environment (`[env]` config, library paths,
the working directory, `CARGO_*` variables) is cargo's own, and output
streams as it is produced.

soothfast sets itself as cargo's target runner for that invocation, so cargo
hands it each test executable. One with a stored pass is skipped with a
`test: skip ... (cached pass)` line and never starts. The rest run, and each
one's exit status is recorded for the summary. A failing binary does not
stop the others.

A pass is stored only if the test leaves its inputs as it found them. A test
that rewrites its own package files (regenerating goldens, say) prints that
it changed its inputs and stores nothing, so the next run executes it again.
A binary cargo never got to counts as a failure. When cargo exits non-zero,
the summary names `doc tests` only if cargo's output reports a failed doc
test. Otherwise, if every binary passed, it reports `cargo test (exit N)`
and prints the end of cargo's output.

Doc tests cannot be cached. They run at the end of the same invocation for
each selected package whose library has doc tests enabled. Pass `--no-doc`
to skip them: cargo then runs a rustdoc wrapper that skips the selected
packages' doc tests, so the build and the run keep the same targets and the
same keys. Any other rustdoc call, such as a test that builds a generated
crate's doc tests, passes through to the real rustdoc. A target flag such as
`--lib` or `--test NAME` skips doc tests too. `--doc` itself is refused,
since doc tests always run.

### A runner you configured

A runner in `CARGO_TARGET_<TRIPLE>_RUNNER` (`valgrind`, `qemu-aarch64`) is
kept: soothfast runs each uncached binary through it. A runner, or a default
`build.target`, set in a cargo config file or a `--config` argument cannot be
chained that way, so each binary instead runs through its own
`cargo test -p PKG --lib` (or `--bin`, `--test`, `--bench NAME`), followed by
`cargo test --doc`. The command prints which setting caused it. Per-package
runs can resolve features apart from the build, so expect them to recompile
some crates.

The command ends with a summary and exits non-zero if anything failed:

```text
test: 3 of 10 packages selected, 9 binaries run, 4 skipped (cached), doc tests in 3 packages
```

## In CI

The selection needs history to find a merge-base, and the pass cache needs to
outlive the run. This is the shape the workspace's own `check` job uses:

```yaml
- uses: actions/checkout@v6
  with:
    fetch-depth: 0
    persist-credentials: false
- uses: actions/cache/restore@v5
  with:
    path: .soothfast/tests
    key: soothfast-tests-${{ github.ref_name }}-${{ github.sha }}
    restore-keys: |
      soothfast-tests-${{ github.ref_name }}-
      soothfast-tests-${{ github.event.repository.default_branch }}-
- run: |
    selection="--all"
    if [ "$EVENT" = pull_request ]; then
      selection="--base origin/$BASE_REF"
    fi
    cargo soothfast test $selection --all-features
  env:
    EVENT: ${{ github.event_name }}
    BASE_REF: ${{ github.base_ref }}
- if: always()
  uses: actions/cache/save@v5
  with:
    path: .soothfast/tests
    key: soothfast-tests-${{ github.ref_name }}-${{ github.sha }}
```

A pull request compares against its base branch. A push to the default
branch selects every package: a push cannot know which earlier push left a
package red, and a failed binary has no stored pass, so it re-runs until it
is fixed while unchanged passing binaries are skipped. A base with no
merge-base selects every package, so the step never falls back to testing
nothing. The cache key follows the one used for measured runs in
[Gating](gating.md): a branch restores its own earlier passes, then the
default branch's. The cache saves even when a test failed, since only passes
are stored: the next run re-runs the failures and skips what already passed.

## Other languages

A Gradle, Maven, Python or Node project that [`init`](importing.md) wires up
as a `[[project]]` can carry its own test command. The Rust workspace keeps
its per-package selection above; these keys select and cache a whole
project.

```toml
[[project]]
dir = "client"
ecosystem = "gradle"
bench_cmd = "./gradlew jmh -Pout={out}"
test_cmd = "./gradlew check"
watch = ["gradle/libs.versions.toml"]
retries = 2

[[project]]
dir = "api"
ecosystem = "python"
test_cmd = "uv run pytest"
retry_cmd = "uv run pytest --lf"
retries = 1

[[test.suite]]
label = "soothfast:e2e"
project = "api"
cmd = "uv run pytest -m e2e"
```

| Key | Meaning |
| --- | --- |
| `test_cmd` | Run in the project's directory through `sh -c`. Without it the project has no tests to run. With no `bench_cmd`, `bench_out` or `from`, the project is test-only and no gate runs for it. |
| `watch` | Globs relative to the repository root. A change to a matching file runs the project's tests as well as a change under `dir`. |
| `retries` | Reruns after a failure. Default 0. |
| `retry_cmd` | The command for a rerun. Defaults to `test_cmd`. |

A `[[test.suite]]` runs `cmd` in the directory of `project`, which must be a
`[[project]]`, only on a pull request whose `label` a writer applied. It
inherits the project's `retries` and reruns with its own `cmd`. An unknown
key in either table is an error.

```console
$ cargo soothfast test --project client            # vs the default branch's merge-base
$ cargo soothfast test --project client --all      # skip the selection, keep the cache
$ cargo soothfast test --project api --suite soothfast:e2e
```

`--project DIR` runs only when the diff against the merge-base (`--base`
works as above) touches a file under `DIR` or one `watch` matches; it prints
that the project is untouched otherwise. A base with no merge-base runs it.
`--no-cache` runs it even with a stored pass. `--result FILE` writes the
outcome as JSON: `name` (`--name`, else the directory), `project`, `kind`
(`test`), `verdict`, `output` (the last 40 lines) and `wall_s`.

The verdict is `pass`, `reused`, `flaky`, `failed` or `skipped`. A pass is
stored under `.soothfast/tests`, keyed by the content of every tracked file
under `DIR` and every file `watch` matches, the command, and the toolchain
(`java -version`, `python --version` or `node --version`; `--toolchain TEXT`
names it instead). The same key later is `reused` and runs nothing. A
failure reruns up to `retries` times; a pass on a rerun is `flaky`, exits 0
and stores nothing, so the next run executes the tests again.

In the reusable workflow, the plan job restores `.soothfast/tests`, works
out which tests the pull request reaches, and looks each one's key up. Only
a test with no stored pass becomes an entry of the `test-projects` matrix, so
a change that touches no tests, or only tests a pass covers, starts no test
job and installs no toolchain. The plan job writes a result file for each
test it settled that way (`skipped` or `reused`) and uploads them together as
the `soothfast-result-test-settled` artifact. The matrix jobs need only the
plan job and run beside the gate jobs, which do not wait for them. Each
uploads `soothfast-result-test-<name>` and, on a pass, its stored pass, and a
`test-cache` job merges those passes into the cache for the next run.

The plan job has no toolchain installed, so the key names it by the
workflow's `java` and `node` inputs and the runner (Python is keyed as `uv`)
instead of a probe of the installed binary. A patch release that a runner
picks up under the same input does not invalidate a pass.

The `soothfast:test-all` label, applied by a writer, selects every project
and the Rust workspace and ignores stored passes for that run. Labels are
read when the workflow starts, so applying one takes a new push or a
re-run.
