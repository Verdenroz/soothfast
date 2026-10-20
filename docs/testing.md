# Testing

`cargo soothfast test` is a drop-in for `cargo test` that does only the work
a change needs. It builds and runs the test targets of the packages a change
can affect, and skips any test binary whose code and inputs match a run that
already passed.

```console
$ cargo soothfast test --all-features                  # vs the default branch's merge-base
$ cargo soothfast test --base origin/master -p mylib    # narrow to one package
$ cargo soothfast test --all                            # no selection, cache still applies
$ cargo soothfast test --no-cache -- --include-ignored  # re-run everything selected
```

Flags before `--` that soothfast does not own go to `cargo test`: features,
`--release`, `--profile`, `--target`. Everything after `--` goes to each test
binary. `--no-doc` skips doc tests.

## Choosing packages

The changed files are the union of `git diff --name-only BASE...HEAD`, the
uncommitted changes and the untracked files. `BASE` is `--base`, else the
merge-base with the default branch (`origin/HEAD`, falling back to
`origin/master`, `origin/main`, `master`, `main`).

Each changed file belongs to the workspace package whose manifest directory
contains it, taking the longest match. That package is selected, and so is
every workspace package that depends on it, transitively, through any
dependency kind, dev-dependencies included.

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
- every git-tracked file in the package, `.rs` included, by content, plus
  modified and untracked ones (sources, fixtures, goldens, data files);
- the files matched by `[test] inputs`;
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

Each remaining binary runs directly, one after another, with its package
directory as the working directory and the environment cargo sets for tests
(`CARGO_MANIFEST_DIR`, `CARGO_PKG_*`, `CARGO_CRATE_NAME`,
`CARGO_TARGET_TMPDIR`). Output streams as it is produced and a failing
binary does not stop the others.

Doc tests cannot be cached, so `cargo test --doc` runs for each selected
package with a library target. Pass `--no-doc` to skip them; passing a
target flag such as `--lib` or `--test NAME` skips them too.

The command ends with a summary and exits non-zero if anything failed:

```text
test: 3 of 10 packages selected, 9 binaries run, 4 skipped (cached), doc tests in 3 packages
```
