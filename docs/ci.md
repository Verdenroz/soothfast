# CI

soothfast in another repository is one step in the workflow you already
have. On a pull request it gates performance against the base branch and
comments the result. On a push to the default branch it records a measured
reference for later pull requests, regenerates derived files, and lands them
as a pull request authored by soothfast-bot, which merges itself once your
checks pass.

```yaml ignore
jobs:
  soothfast:
    runs-on: ubuntu-latest
    environment: soothfast-bot
    permissions:
      contents: read
      pull-requests: write
      id-token: write
    concurrency:
      group: soothfast-${{ github.ref }}
      cancel-in-progress: true
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Verdenroz/soothfast@<tag-or-sha>
```

## Setup

Two one-time settings, no secrets:

1. Install the [Soothfast Bot](https://github.com/apps/soothfast-bot) GitHub
   App on the repository. It asks for Contents and Pull requests write and
   Metadata read.
2. Create an environment named `soothfast-bot` (Settings, Environments) with
   no deployment branch policy and no required reviewers. The job above runs
   on pull requests too, and a branch policy would fail every one of them,
   the bot's own included. The broker enforces the default-branch rule
   itself.

Because the job names an environment, every run of it, pull request or push,
appears under Environments and Deployments and on the pull request as
"deployed to soothfast-bot". If you would rather keep pull requests out of the environment, split the
step into two jobs with the same `uses:` line: the gate job on
`pull_request` without an environment and with `changelog: false`, and a
regeneration job on `push` with the environment. Leave `gate` on in the push
job unless you want it to stop recording reference runs. That split
lets you put a default-branch policy on the environment; its cost is that
gate comments are then posted by github-actions rather than soothfast-bot,
since only a job in the environment can obtain a bot token.

Two repository settings decide what happens to the bot's pull request:

- If your default branch has a ruleset or branch protection with required
  checks, turn on "Allow auto-merge" so the bot can queue its PR behind them.
  Without rules the bot merges immediately.
- "Automatically delete head branches" removes `bot/soothfast-update` after
  the merge. Without it the branch lingers until the next regeneration
  force-pushes it.

A ruleset that requires approving reviews applies to the bot too: its PR
waits for a human.

## What the step does

Every run installs `cargo-soothfast` pinned to the `soothfast` version in
your `Cargo.lock`, cached across runs. Pin the action to a release no newer
than that version: the scripts pass flags the CLI at the action's ref
understands. Runs that have nothing to do (a push to another branch, a pull
request with `gate: false`) stop there. Otherwise the checkout is unshallowed
so merge-base and tag lookups work.

**On `pull_request`.** For each package (see `packages` below) it runs `cargo
soothfast gate -p PKG --against-ref origin/<base>` and appends the output to
one comment on the pull request, updated in place on later pushes. The
comment shows the last forty lines per package. On a regression it uploads
`.soothfast/triage/` as the `soothfast-triage` artifact and fails the step.
The comment is posted as soothfast-bot by the broker itself: the job sends
the text over its OIDC identity and never holds a token, so a pull request
branch, which runs code nobody has merged, cannot borrow the bot for
anything else. A pull request from a fork has no OIDC identity; there the
comment falls back to `github.token` (github-actions), or is skipped with a
warning where that token is read-only, and the gate result still decides the
step.

**On a push to the default branch.** With `gate: true` it first runs `cargo
soothfast gate -p PKG --against-ref HEAD^` for each package, which stores
HEAD's run in `.soothfast/runs/` so the next pull request whose merge-base is
this commit, or builds the same bench binary, reuses it instead of measuring
its merge-base in a worktree. A regression or an error here is a warning: the
code is already merged, and the steps after it still run. There is no comment
and no triage upload. Recording needs two things from your workflow: a cache
step for `.soothfast/runs/` that wraps this job on pushes as well as on pull
requests (see [Reusing a measured
reference](gating.md#reusing-a-measured-reference)), and `.soothfast/` in
`.gitignore`, since a run is stored under its commit only from a clean tree.
With cargo-soothfast 0.3.2 or earlier in your `Cargo.lock`, a push whose bench
binary matches its parent's records nothing unless a run for that binary is
already stored. Pushes by soothfast-bot can skip the job: they leave the bench
binary alone, so a pull request based on one finds the previous commit's run
by binary.

Then it measures each package into the
`baseline` baseline, regenerates `CHANGELOG.md` against the latest tag (or
lists the initial surface when there is no tag), regenerates any packages
named in `spec`, and lands whatever changed as one pull request on
`bot/soothfast-update`. Pushes to other branches do nothing beyond the
install.

The bot PR triggers your workflows like any other pull request, this step
included, which is how its required checks get satisfied. Jobs that skip on a
`[bot]` actor count as passed for required checks. If the bot PR is left
open, your ruleset gave auto-merge nothing to wait on (a pull request rule
with zero required approvals and no required checks); merge it by hand or
drop the rule.

## Why the environment

The step needs `id-token: write` to prove its identity to the broker. That
permission is also common on jobs that publish to crates.io, PyPI, or a cloud
provider over OIDC. Requiring the `soothfast-bot` environment means only a
job that opts in can obtain a bot token; a compromised action in one of those
other jobs cannot. The broker hands out one kind of token, scoped to your repository for one
hour and revoked when the step finishes: a landing token (contents and pull
requests write) for `push`, `workflow_dispatch`, or `schedule` runs on your
default branch, or on a tag whose commit is already on it. A `pull_request`
run from the repository itself gets no token at all; it asks the broker to
post the gate comment, and the broker does so with a token it holds and
revokes itself. Every other event, ref, or repository the App is not
installed on is refused.

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `packages` | every package with a bench target named `soothfast` | Space-separated packages to gate and measure. `report changelog` uses the same list unless `changelog-packages` says otherwise. |
| `gate` | `true` | Run the gate on pull requests, and record a reference run on default-branch pushes. |
| `changelog` | `true` | Regenerate `CHANGELOG.md` on default-branch pushes. |
| `spec` | none | Space-separated packages whose `mode = "generate"` specs to regenerate. |
| `features` | none | Cargo features for the gate, the baseline measurement, and spec generation. A bench target with `required-features` needs them here. |
| `save-baseline` | none | Baseline name a passing pull-request gate saves its HEAD measurement under, in `.soothfast/baselines/`. Upload that directory as an artifact and a later job (a `docs check` of claims) reads HEAD's numbers without measuring again. A failing gate saves nothing, and neither does one whose bench binaries are identical on both sides, since that pass collects no gating counters. A job reading the baseline has to handle its absence. |
| `changelog-packages` | `packages` | Packages whose API surface `report changelog` diffs. Set it when a crate without a bench target still ships an API. |
| `changelog-features` | `features` | Features for `report changelog`, which decide what the API surface diff contains. Set it wider than `features` when gating under the full feature set is too heavy. |
| `regen-run` | none | Shell run on the default branch after the built-in regeneration, for derived files the inputs above do not cover. `$SOOTHFAST` is the `cargo-soothfast` this step installed, so `"$SOOTHFAST" sdk gen -p mylib` works whether the CLI came from the install or from `binary`. Requires `regen-paths`. |
| `regen-paths` | none | Space-separated pathspecs `regen-run` may change; they land in the same pull request. Required with `regen-run`; without it the step fails rather than discarding the changes. |
| `baseline` | `base` | Baseline name the regeneration measures into. |
| `rustdoc-toolchain` | the nightly the release was tested with | Toolchain for rustdoc JSON. A floating `nightly` can change the JSON format under the API diff. |
| `version` | from `Cargo.lock` | `cargo-soothfast` version to install. |
| `lockfile` | `Cargo.lock` | Where to read the pinned version. |
| `binary` | none | A prebuilt `cargo-soothfast`; skips the install. |
| `token` | `github.token` | Token for the gate comment and repository lookups. |
| `bot-token` | none | Bring your own soothfast-bot installation token; skips the broker. |
| `bot-slug` | `soothfast-bot` | App slug `bot-token` belongs to, for the commit author. |
| `broker` | built in | Token broker URL. |
| `labels` | `false` | On default-branch pushes and schedules, act on pull request labels as soothfast-bot (see [Labels](#labels)). |
| `shard` | none | `I/N` to gate only the items shard I of N owns, from a job matrix. Each shard comments on its own. |
| `comment` | `true` | Post the pull request gate comment. `false` posts nothing and records a result per package for `cargo soothfast verdict merge`. |
| `cache` | `true` | Restore and save `.soothfast/runs`, `items` and `maps`, keyed so a pull request restores what its merge-base recorded, merged with the latest entry its own earlier pushes saved. |
| `rustdoc-cache` | `false` | Restore and save `.soothfast/rustdoc` for a job that runs `spec` or `docs` commands. A hit skips the nightly rustdoc build; see [Rustdoc cache](#rustdoc-cache). |
| `bin-cache` | `false` | Restore `.soothfast/bins`, the built bench executables, before the gate and save them after it when the gate changed them, so a job whose sources a change leaves alone (a docs-only pull request) runs the binary an earlier job built. See [Bench executable cache](#bench-executable-cache). |
| `cache-suffix` | none | Tells apart the legs of a job matrix that share a job name, so each keeps its own `rustdoc-cache` and `bin-cache`. |
| `prebuild` | none | `bench`, `routes` or `rustdoc` to build that lane of the stores in place of the gate (see [One build](#one-build)). `prebuild-docs` and `prebuild-spec` name its packages. |

Outputs are `version` and `cache-hit`.

## Rustdoc cache

`spec` and `docs` commands build rustdoc JSON, which compiles the package
and its dependencies under nightly. With `rustdoc-cache: "true"` the job
keeps `.soothfast/rustdoc` between runs, and a command that finds the JSON
it needs there skips cargo entirely. The reusable `run.yml` keeps it for its
docs check, and for the changelog regeneration on a default-branch push.

Lookup has two levels. A key hashes the package, features, visibility, the
`rustc -vV` of the rustdoc toolchain, the rustdoc format version, every
`RUSTFLAGS`, `RUSTDOCFLAGS`, `CARGO_ENCODED_*`, `CARGO_BUILD_*`,
`CARGO_PROFILE_*` and `CARGO_TARGET_*` variable (bar the target directory
and job count), `Cargo.lock`, the root manifest and cargo config, and the
content of every file git lists in the package and its path dependencies,
apart from `.github` and `.soothfast`. Uncommitted edits count, and a nested
repository or submodule is hashed through. The key selects a manifest of
the other files the last build read (from rustdoc's and cargo's dep-info and
the build scripts' `rerun-if-changed`) and the variables it depends on
(`env-dep` lines and `rerun-if-env-changed`). The entry is named for the key
and a hash of those, so a changed `include_str!` target outside the package,
or a changed build script input, is a miss. The merge-base checkout hashes
the same way, so JSON a default-branch push saved is the base side of a
pull request's `docs diff`.

A result is stored only if nothing it read changed during the build.

Not covered:

- What a build script reads without declaring it, and variables outside the
  families above that it does not name in `rerun-if-env-changed`.
- Cargo configuration outside the repository, such as
  `~/.cargo/config.toml`, and variables such as `RUSTC_WRAPPER`.
- Absolute paths rustdoc records for sources outside the workspace.
- A package whose sources sit outside the repository, or a tree without
  git, has no key and is always rebuilt.

A package at the repository root hashes every file git lists outside the
other packages, so a doc or README edit there misses.

Caches are kept per job, runner OS and architecture, and `cache-suffix`, and
the rustdoc store is capped at 1 GiB, dropping the least recently used entries
first. The action saves through `actions/cache`, which saves a new entry at
the end of every job that did not hit its exact key, even when nothing was
added. A composite action has no end-of-job step to compare against, so
that cost is accepted. Set `SOOTHFAST_TIMINGS=1` to print how long each
rustdoc build, parse and `cargo bench` took.

## Bench executable cache

A job that gates, or runs `spec check`, builds its bench executable from
cold: `cargo bench --no-run` for the gate's HEAD side, and a dev-profile
build for the route listing behind `spec check`. A change that leaves the
sources alone, a docs-only pull request, yields the same bytes, and the
build dominates the job. `bin-cache` keeps `.soothfast/bins` between runs;
a job that finds its executable there builds nothing, and the gate
runs the stored file directly. It gets the package directory as its working
directory, as `cargo bench` gives it, and the variables `CARGO_MANIFEST_DIR`
and `CARGO_PKG_NAME`. Other `CARGO_*` variables and `LD_LIBRARY_PATH` are not
set. A tree with a configured cargo `runner` (a `runner` key in the
repository's or `CARGO_HOME`'s config, or a `CARGO_TARGET_*_RUNNER` variable)
is never served from the store, since the runner is part of what is measured.

The key and the manifest are built as for the [rustdoc cache](#rustdoc-cache),
over the package, bench target, profile, features, `codegen-units` pin,
toolchain, `rustc -vV` and the same variable families, with two additions:
`RUSTC`, `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `RUSTUP_TOOLCHAIN` and
`CARGO_INCREMENTAL` join the variables, and the manifest comes from the
dep-info cargo writes for the bench executable and every workspace
dependency plus the build scripts' `rerun-if-*` lines. An entry is kept only
if every dependency's dep-info could be read.

Markdown files are left out of the key's file list, so a docs-only change is
a hit even for a package at the repository root, where that list is the
whole repository. A markdown file the build included (`include_str!` on a
README, say) is in the manifest and counts.

**The checkout path is part of the key**, and so are the target directory
and `CARGO_HOME`. A binary can carry them: `env!("CARGO_MANIFEST_DIR")`,
`file!()`, `OUT_DIR` and debug info all write absolute paths into its bytes,
and the run cache, item cache and link maps all key on those bytes. Two
checkouts of one tree at different paths gave identical executables only
while no such path was compiled in. A job that moves its checkout between
runs misses; CI runners use one path per repository.

Each entry also keeps the digest of its binary and its link map, written
once the first run has worked them out. A hit reads the digest instead of
computing it, and puts the map back under `.soothfast/maps` if that was
pruned. A map found nowhere is made by linking the bench target again, which
on a cold target directory means a full build, once: the map is then kept in
the entry for the runs after it.

The store holds one executable per checkout, target directory, package, bench
target, features and profile: a newer one replaces the older, so a saved
cache is the binary the branch runs and not its history. The merge-base
worktree builds elsewhere and so has its own. `cache merge` keeps the newest
of each group by the time it was made, and pruning by size removes whole
entries, oldest payload first.

### Cache budget

A saved entry is as large as the executable, tens of MB, and GitHub keeps
about 10 GB of cache per repository, evicting the least recently used. An
entry that displaced the measured-run caches would cost more than it saves,
so the action is conservative:

- `bin-cache` is off by default. Turn it on for the job that builds the bench
  target through the action (`gate: "true"`, the default).
- The executables are restored before the gate and saved after it, in a step
  of their own, only if a listing of the store (names and sizes, not times)
  differs from the one taken before the gate. A job that hit its executable,
  such as a docs-only pull request, saves nothing.
- A job outside the action, such as a `spec check` step in the adopter's own
  workflow, restores and saves `.soothfast/bins` with `actions/cache` on a
  key of its choosing. Key it per job and runner like `rustdoc-cache`, and
  save it with `actions/cache/save` guarded the same way, or only on the
  default branch.

Not covered, beyond what the rustdoc cache lists: a markdown file a proc
macro or build script reads without saying so, the linker and C compiler a
build script invokes, and anything a bench reads at run time besides its
working directory and `CARGO_MANIFEST_DIR`. The store keeps 512 MiB at most.

## Labels

With `labels: "true"` on a job that runs on default-branch pushes and on a
schedule, soothfast-bot sweeps the open pull requests:

| Label | Effect |
|---|---|
| `soothfast:merge` | Queue auto-merge (squash) behind the required checks. Refused when the default branch requires no status check, since the merge would happen at once. |
| `soothfast:update` | Merge the default branch into a pull request that fell behind. |
| `do-not-merge` | Cancel a queued auto-merge and leave the pull request alone. |

A pull request gate labelled `soothfast:perf-accepted` reports its
regressions in the comment and passes; label the pull request, then re-run
the failed gate. The label waives regressions only, never a gate that
errored. A label counts only when the person who last applied it can write
to the repository, so neither a fork nor the Triage role can merge a pull
request or waive its regression through soothfast-bot. `do-not-merge` counts
from anyone, since it only holds a merge back. The sweep runs on events the
broker mints tokens for, so no `pull_request` run gets a write token. `cargo soothfast init --bot --labels` writes the job and
creates the labels.

## Bringing your own token

If you would rather not depend on the broker, install your own GitHub App
with Contents write, Pull requests write, and Metadata read, mint an
installation token in an earlier step (for example with
`actions/create-github-app-token`), and pass it as `bot-token` with the App's
slug as `bot-slug`. The `id-token: write` permission and the environment are
then unnecessary.

## Self-hosted runners

A runner that only has `git`, `bash`, `curl`, `docker` and the runner agent
can run the reusable `run.yml` workflow (see [importing](importing.md)) if it
provides the following.

Every job:

- `git`, `curl`, `bash`, `sed`, `awk` and `grep`. The install step fails
  naming `git` or `curl` when either is missing.
- Network access to github.com, crates.io, and the services the pinned
  actions download from (the Java, Node and `uv` setup actions fetch their
  toolchains).
- `cargo` is set up for you with `dtolnay/rust-toolchain` (stable) when it is
  not on `PATH`, because `cargo soothfast` is a cargo subcommand. That needs
  the network and a C compiler and linker (`cc`) to build a Rust project.
  `cargo-soothfast` itself is installed from crates.io when the workflow's
  ref is a published release, else built from the ref into
  `$RUNNER_TOOL_CACHE/soothfast-cli/<commit>`, cached by commit and linked
  into `$CARGO_HOME/bin`. The first
  build of a ref takes several minutes.

Jobs that run the composite action (the cargo gate and the labels job). A
plain pull request gate needs `cargo` and one of `jq` or `gh` (to read the
default branch). The rest is needed only by the step that uses it, and each
fails naming the tool:

- `gh`, to comment with the job token, to act on labels and to land
  regenerated files. Without it the gate still runs and only skips the
  `soothfast:perf-accepted` check, with a warning.
- `jq`, to comment or mint a token as soothfast-bot, and to find the bench
  packages when none are named.
- `valgrind`, for the callgrind fallback on machines without hardware
  performance counters. The step installs it with `apt-get` when it runs as
  root or passwordless `sudo` is available, and otherwise warns and carries on.
  A runner with performance counters does not need it.
- `rustup`, for a nightly or pinned rustdoc toolchain. `dtolnay/rust-toolchain`
  installs it when it is missing.

Per-project gate jobs:

- Java for Gradle and Maven projects, Node for Node projects and `uv` for
  Python projects come from `actions/setup-java`, `actions/setup-node` and
  `astral-sh/setup-uv`, so the runner needs nothing preinstalled. A Gradle
  project's own `./gradlew` downloads Gradle, and Python itself comes from
  `uv`.
- Gradle and Maven dependencies, and the Gradle wrapper, are cached by
  `actions/setup-java`, keyed on the project's build files.

`step-security/harden-runner` is the first step of every job; it targets
hosted runners and its network audit may not apply on yours.

## One build

Every job that gates, checks docs or runs `spec` commands used to compile for
itself on a cold runner, and many of them compiled the same crates in
parallel. `run.yml` now compiles once. After `plan`, a `build` job runs one leg
per lane that has something to build, on separate machines:

| Lane | Builds | Into |
|---|---|---|
| `bench` | HEAD's bench executable of each gated package, its link map, and the merge-base's executable and map when the run cache holds no run for the merge-base | `.soothfast/bins`, `.soothfast/maps` |
| `routes` | The dev-profile executable behind `spec check`, for each package in the `spec` input | `.soothfast/bins` |
| `rustdoc` | Rustdoc JSON for the docs package and, for each `spec` package that generates a spec, for it and the workspace crates its types come from | `.soothfast/rustdoc` |

A lane with nothing to build is left out, and each leg reads its store first
and builds only what is missing. The `bench` lane leaves the merge-base alone
for the reason the gate does: if the run cache holds a run for it, the gate
reuses that run and never builds the executable. Each leg prints what it
built against what it found, and `SOOTHFAST_TIMINGS=1` times every build.

```text
prebuild: stored   bench executable core (head)
prebuild: built    bench executable server (head)
prebuild: skipped  bench executable core (merge-base)
prebuild: 1 built, 1 skipped, 1 stored
```

Each leg uploads its stores as `soothfast-store-<lane>` (the executables, the
link-map extents and the rustdoc JSON, kept one day). The Rust gate, every
shard of it, and the docs check `need` the build job, download the artifacts
into `.soothfast/` and find a hit for everything they read. They compile
nothing. The tests, and the gates and tests of the other ecosystems, do not
wait for the build job.

A job that runs when the build job failed finds nothing to download and
compiles for itself, so a compile error still shows up in that job's output
and in the verdict.

A hit needs the reading job to be the same machine setup as the building one.
The key names the checkout path, the target directory, `CARGO_HOME`, `rustc
-vV` and the build variables (`RUSTFLAGS`, `CARGO_INCREMENTAL`, and the
`CARGO_PROFILE_`, `CARGO_BUILD_` and `CARGO_TARGET_` families). The path
cannot be made relative: compiled code carries it, so a binary built at
another path is not the same binary. GitHub-hosted runners use one workspace
path per repository for every job. The reading jobs set up the toolchain and
`rust-cache` the way the build legs do, since those steps export some of the
variables. A runner pool with a path per job misses every time, and the jobs
then compile as before.

### Sharding

`shard: 3` on the `run.yml` call splits the Rust gate into three legs, each
gating the items its `I/N` shard owns. Each leg reads the prebuilt
executables of both sides, so measuring is all it does:

```yaml ignore
jobs:
  soothfast:
    uses: Verdenroz/soothfast/.github/workflows/run.yml@<tag-or-sha>
    with:
      soothfast_ref: <tag-or-sha>
      shard: 3
```

The legs record `soothfast-result-rust-shard-<n>` and the verdict folds them
into one section for the package. A pull request's shards cannot save the
`head` baseline, which holds every item, so the docs check reads the default
branch's measurement for its claims. A default-branch push records every item
in its first leg and saves the baseline as before.

`cargo soothfast prebuild` runs the same lanes by hand: `-p PKG` for bench
packages, `--against-ref REF` for the merge-base, `--docs PKG`, `--spec
PKG[:TARGET]` and `--only bench|routes|rustdoc`. The `spec` input of `run.yml`
names the packages for the `routes` and `rustdoc` lanes.

`run.yml` has no `spec` job yet. A spec job of the caller's can run
`cargo soothfast prebuild --spec PKG:TARGET` itself before `spec check`, or
download `soothfast-store-*` into `.soothfast/` after `needs:`-ing the caller's
job, which finishes only when the whole run does. A `spec` job inside
`run.yml`, needing just the build job, is a follow-up.

## One verdict

The reusable `run.yml` workflow gives a pull request one soothfast comment and
one check, however many projects or shards it gates. Every gate job, the test
job and the docs check records a `result.json` (project, kind, verdict, the last forty
lines of output, reuse counts, seconds) and uploads it as
`soothfast-result-<name>`. The `verdict` job downloads them all, runs
`cargo soothfast verdict merge`, and posts one comment marked
`<!-- soothfast-verdict -->` with a section per project, regressed and
errored first. Shards of one project fold into one section.

Each section keeps the output of its job without terminal colour codes, and
without cargo's `warning:` blocks unless the job errored. A docs check that
skipped its claims for lack of a measurement reads `pass (claims not
evaluated)`. `run.yml` sets `CARGO_TERM_COLOR: never` for every job.

Branch protection needs only the `verdict` check, named `<caller job id> /
verdict`. The caller `cargo soothfast init` writes names its job `soothfast`,
so a repo set up with init requires `soothfast / verdict`. The check fails when any project
regressed or errored, or recorded no result; a `soothfast:perf-accepted` label
applied by a writer waives regressions, never errors or docs drift.

## The gate on its own

The gate, with its comment and triage upload, is also a reusable workflow for
callers that want it as a separate job or a matrix over packages:

```yaml ignore
jobs:
  gate:
    permissions:
      contents: read
      pull-requests: write
    uses: Verdenroz/soothfast/.github/workflows/soothfast-gate.yml@<tag-or-sha>
    with:
      package: mylib
```

`cli-artifact` names an artifact holding a prebuilt CLI at
`bin/cargo-soothfast` from earlier in the same run; without it the workflow
installs the release matching your `Cargo.lock`.
