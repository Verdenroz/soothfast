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
binary. `--no-doc` skips doc tests.

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
```

`inputs` wins over `ignore`. Ignore only files no test reads.

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

- files outside the package that are not declared;
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
A binary cargo never got to, or a nonzero exit cargo reports after every
binary passed, counts as a failure and stores nothing.

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
