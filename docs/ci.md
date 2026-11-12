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

## Setting up and pinning

`cargo soothfast init` writes the workflow, `soothfast.toml` and the
`.gitignore` entry for any repository. A repository with no Cargo manifest
is set up from the Gradle, Maven, Python and Node projects it tracks, as
`[[project]]` tables; the Rust-only steps are skipped.

A repository pins soothfast in several places that must agree: the
`soothfast*` requirements in `Cargo.toml` and their entries in `Cargo.lock`,
the `uses: <owner>/soothfast...@<sha> # vX.Y.Z` refs in its workflows, a
`soothfast_ref` input, and a literal `cargo install cargo-soothfast
--version X.Y.Z`. `cargo soothfast bump [--to VERSION]` moves all of them to
one release, the running CLI's own unless `--to` says otherwise. It resolves
each tag to its commit with `git ls-remote`, keeps the version comment in
step with the SHA, updates `Cargo.lock` for just the packages it moved, and
prints one line per file it changed. A requirement the release already
satisfies, such as `"0.5"` against 0.5.7, is left as written.

`cargo soothfast bump --check` writes nothing and needs no network. It
prints `file:line  found X, want Y` for each stale pin and exits 1 when there
is one, so it can gate a pull request. A line that names `cargo-soothfast`
with a version it cannot read is printed as unrecognised and does not fail
the check.
## Checking the setup

`cargo soothfast status` prints what state the setup is in, from what is
already on disk and in git. It builds, measures and fetches nothing. Each
line is a state word (`ok`, `stale`, `failed` or `missing`), the subject and,
unless it is `ok`, the command that addresses it:

```text
failed  gate  2 failure(s); triage: a_item.txt  -> cat .soothfast/triage/a_item.txt
stale   cli version  cli 0.5.6, Cargo.lock pins 0.5.5  -> cargo install --locked cargo-soothfast --version 0.5.5
ok      2 checks
```

The checks cover the CLI against the `soothfast` version Cargo.lock pins, the
last `gate` verdict and its triage, `soothfast.lock` once markdown carries
`bind` markers, the files `init` writes against what `init --check` would
report, and the last `test` run. The `init` check reads without the network,
so a release pin that is not already in the workflow reads as the tag. Its
row is `missing` when the workflow is absent and `stale` with the drifted file
names otherwise. The `test` row is `failed`, `stale` when the last pass was on
a different tree, or `missing` when no run is recorded. Gate runs and
baselines that are not recorded locally read as `ok`, since CI measures them;
`--all` shows which. `--all` lists the `ok` lines instead of counting them,
and `--json` prints the rows as an array of `state`, `subject`, `detail` and
`fix`. The exit code is 0 when every row is `ok` and 1 otherwise. A failed
gate points at its triage file; accepting a regression stays a decision for a
reviewer.

## What the step does

Every run installs `cargo-soothfast` pinned to the `soothfast` version in
your `Cargo.lock`, cached across runs. The binary is downloaded from the
GitHub release and checked against its `SHA256SUMS`; if that fails the step
warns and falls back to `cargo-binstall`. Pin the action to a release no newer
than that version: the scripts pass flags the CLI at the action's ref
understands. Runs that have nothing to do (a push to another branch, a pull
request with `gate: false`) stop there. Otherwise the checkout is unshallowed
so merge-base and tag lookups work.

**On `pull_request`.** It runs `cargo soothfast ci gate`, which gates each
package (see `packages` below) against the pull request's base commit, and
posts the output as one comment on the pull request, updated in place on
later pushes. The comment shows the last forty lines per package. On a regression it uploads
`.soothfast/triage/` as the `soothfast-triage` artifact and fails the step.
The comment is posted as soothfast-bot by the broker itself: the job sends
the text over its OIDC identity and never holds a token, so a pull request
branch, which runs code nobody has merged, cannot borrow the bot for
anything else. A pull request from a fork has no OIDC identity; there the
comment falls back to `github.token` (github-actions), or is skipped with a
warning where that token is read-only, and the gate result still decides the
step.

**On a push to the default branch.** With `gate: true` it first runs `cargo
soothfast ci gate`, which gates each package against `HEAD^` and stores
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
| `packages` | every package with a bench target named `soothfast` | Space-separated packages to gate and measure. `report changelog` uses the same list unless `[changelog] packages` in `soothfast.toml` says otherwise. |
| `gate` | `true` | Run the gate on pull requests, and record a reference run on default-branch pushes. |
| `changelog` | `true` | Regenerate `CHANGELOG.md` on default-branch pushes, along with `mode = "generate"` specs and `[[sdk]]` packages. |
| `regen` | `false` | Run the regeneration on default-branch pushes even with `changelog: "false"`, for the specs and SDKs alone. |
| `save-baseline` | none | Baseline name a passing pull-request gate saves its HEAD measurement under, in `.soothfast/baselines/`. Upload that directory as an artifact and a later job (a `docs check` of claims) reads HEAD's numbers without measuring again. Counters measured from code proven identical to HEAD's (the item cache, a run stored for the same binary, a run-cache hit on HEAD) are saved with their `reused` marker; walltime from those sources is listed as `unmeasured` (`reused-walltime`) instead. A gate whose claims failed, or that errored, saves no numbers and lists the package's items as `gate-failed`; one that failed only on regressions saves HEAD's numbers, and its verdict or the `soothfast:perf-accepted` label decides. With `shard`, each leg's file holds its shard's items; merge them with `cargo soothfast baseline merge`. |
| `regen-run` | none | Shell run on the default branch after the built-in regeneration, for derived files it does not cover. `$SOOTHFAST` is the `cargo-soothfast` this step installed, so `"$SOOTHFAST" report render` works whether the CLI came from the install or from `binary`. Requires `regen-paths`. |
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
| `labels` | `true` | On default-branch pushes and schedules, ask the broker to reconcile pull request labels, annotating the run instead of failing when it cannot; `"false"` turns it off (see [Labels](#labels)). |
| `shard` | none | `I/N` to gate only the items shard I of N owns, from a job matrix. Each shard comments on its own. |
| `comment` | `true` | Post the pull request gate comment. `false` posts nothing; `cargo soothfast ci verdict` merges the result `ci gate` records for each package. |
| `cache` | `true` | Restore and save `.soothfast/runs`, `items` and `maps`, keyed so a pull request restores what its merge-base recorded, merged with the latest entry its own earlier pushes saved. |
| `rustdoc-cache` | `false` | Restore and save `.soothfast/rustdoc` for a job that runs `spec` or `docs` commands. A hit skips the nightly rustdoc build; see [Rustdoc cache](#rustdoc-cache). |
| `bin-cache` | `false` | Restore `.soothfast/bins`, the built bench executables, before the gate and save them after it when the gate changed them, so a job whose sources a change leaves alone (a docs-only pull request) runs the binary an earlier job built. See [Bench executable cache](#bench-executable-cache). |
| `cache-suffix` | none | Tells apart the legs of a job matrix that share a job name, so each keeps its own `rustdoc-cache` and `bin-cache`. |
| `prebuild` | none | Lanes (`bench`, `routes`, `rustdoc`, space-separated) to build into the stores in place of the gate (see [One build](#one-build)). `routes` builds for every package with a `[[spec]]` entry. |
| `prebuild-docs` | none | Packages whose rustdoc JSON `docs check` reads, in place of the ones `soothfast.toml` configures. Alone, it builds the `rustdoc` lane. They are built only when the docs bind a Rust item, as `docs check` builds them. |
| `prebuild-shards` | none | The `shard` count of the legs that read the stores, so a merge-base whose run every leg has stored is not built. |
| `prebuild-stage` | none | A directory to copy just the entries the lanes built or found into, for a job to upload. |

### Retired inputs

The inputs `features`, `spec`, `changelog-features`, `changelog-packages`,
`prebuild-docs-paths`, `prebuild-docs-features` and `prebuild-spec` are
retired, and the step fails when one is set. The `ci` commands the action runs
read the same settings from `soothfast.toml`:

| Retired input | In `soothfast.toml` |
|---|---|
| `features` | `features` under `[gate]`, or under `[package.NAME]` for one package |
| `spec` | nothing to set: `ci regen` finds each package's `mode = "generate"` `[[spec]]` entries |
| `changelog-features`, `changelog-packages` | `features` and `packages` under `[changelog]` |
| `prebuild-docs-paths` | `paths` under `[docs]` |
| `prebuild-docs-features` | `features` under `[package.NAME.docs]` |
| `prebuild-spec` | `routes` in `prebuild`, with `target` and `features` under `[package.NAME.spec]` |

Outputs are `version` and `cache-hit`.

## Rustdoc cache

`spec` and `docs` commands build rustdoc JSON, which compiles the package
and its dependencies under nightly. With `rustdoc-cache: "true"` the job
keeps `.soothfast/rustdoc` between runs, and a command that finds the JSON
it needs there skips cargo entirely. The reusable `run.yml` keeps it for its
docs check, and for the changelog regeneration on a default-branch push.
Set `[docs] package` in `soothfast.toml` and a `docs` command run without `-p`
reads that package.

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

soothfast-bot acts on pull request labels the moment they change. GitHub
delivers the App's webhooks to the broker, which reads `[bot]` from
`soothfast.toml` on the default branch (never the pull request's copy) and
acts as soothfast-bot:

| Label | Effect |
|---|---|
| `soothfast:merge` | Enable auto-merge behind the required checks, when a writer applies the label. Refused when the default branch requires no status check, since the merge would happen at once. |
| `soothfast:update` | Merge the default branch into a pull request that fell behind. |
| `do-not-merge` | Cancel a queued auto-merge and leave the pull request alone. |

A label counts only when the person who last applied it can write to the
repository, so neither a fork nor the Triage role can merge a pull request
through soothfast-bot. `do-not-merge` counts from anyone, since it only holds
a merge back. Drafts and pull requests that already have auto-merge enabled
are skipped, and the broker logs why. A pull request GitHub would already
merge (every required check passed or was skipped, which path-filtered checks
make common) cannot queue auto-merge, so the bot merges it at once, bound to
the labelled commit and the configured `merge_method`. GitHub turns
auto-merge off when someone without write access pushes to the branch, so
later pushes and sweeps never turn it back on: to queue the merge again,
remove `soothfast:merge` and add it back. The merge is tied to the commit
the label was applied to: if the branch moves or the label comes off before
the bot acts, it neither queues nor merges anything. The bot never
watches statuses; GitHub's auto-merge, rulesets and merge queue decide when
the merge happens.

A pull request gate labelled `soothfast:perf-accepted` reports its
regressions in the comment and passes; label the pull request, then re-run
the failed gate. The label waives regressions only, never a gate that
errored, and it needs a writer too. `perf_accepted_labels` renames it, and
the gate reads that list from `soothfast.toml` on the pull request's base
branch, never the pull request's copy: a pull request that could rename the
waiver to a label a maintainer applied for another reason would waive its
own regressions. When the base branch's table cannot be read (an older
`cargo soothfast`, no file there, a malformed table), the gate warns and
accepts only `soothfast:perf-accepted`.

### Configuration

The broker acts only on a `[bot]` table with `labels = true` in
`soothfast.toml`; repositories without it are left alone. `cargo soothfast
init` turns the bot on: it writes the table, writes the reconcile job and
creates the labels with `gh`; if `gh` cannot, init warns and `cargo soothfast
init --labels` creates them later. `--no-labels` skips those three, and
`--no-bot` skips the bot altogether (no schedule, no labels job, no
changelog regeneration).

Two one-time steps remain: install the [soothfast-bot
App](https://github.com/apps/soothfast-bot) on the repository, and let the
first run create the `soothfast-bot` environment. Until the App is
installed the broker refuses to mint a token; the action warns and lands
nothing as the bot instead of failing the job.

```toml
[bot]
labels = true                       # default false
merge_labels = ["soothfast:merge"]
update_labels = ["soothfast:update"]
update_all = false                  # true: keep every open, non-draft PR up to date
ignore_labels = ["do-not-merge"]
perf_accepted_labels = ["soothfast:perf-accepted"]  # read by the gate, not the broker
merge_method = "squash"             # squash | merge | rebase

[[bot.label]]                       # optional, one per label; read by init only
name = "soothfast:merge"
color = "0e8a16"                    # 6 hex digits, no '#'
description = "Merge once the required checks pass"
```

`cargo soothfast init --labels` (implied by `--bot`) creates each label with
the colour and description of the `[[bot.label]]` entry that names it, else
a default for its role. A bad colour fails `--labels` and only warns when
`--bot` implied it; an entry naming a label no role uses warns. The broker
ignores `[[bot.label]]`. `cargo soothfast labels [--ref REF] [--role ROLE]`
prints the configured names.

`update_all = true` brings every open, non-draft pull request against the
default branch up to date on each push to it, as if each carried an update
label. An ignore label (`do-not-merge` by default) still opts one out. It
needs no writer to apply anything, because the setting is read from the
default branch. Each push evaluates as many pull requests as the Workers
subrequest budget allows, least recently updated first, so successive
pushes reach every open pull request.

A malformed `[bot]` table (an unknown key, a wrong type, a duplicate) makes
the bot act on nothing and log why. Deleting the head branch stays the
repository's "Automatically delete head branches" setting.

### Operator steps

Once per broker deployment (see `bot/`):

1. In the App's settings, set the webhook URL to
   `https://<worker>/webhook`, choose a secret, and subscribe to the Pull
   request and Push events.
2. Store the same secret in the Worker: `wrangler secret put
   GITHUB_WEBHOOK_SECRET`. Without it `/webhook` answers 500.
3. The App needs Contents write, Pull requests write and Metadata read.

### Reconciling missed deliveries

GitHub does not redeliver a webhook that failed. On default-branch pushes and
schedules the action asks the broker to sweep the open pull requests: it
cancels queued merges held by `do-not-merge` and updates labelled branches,
but never enables a merge. `labels` defaults to `"true"`; set `labels:
"false"` to turn the sweep off, as the workflows `init` writes do for every
job but the labels job.

The sweep is best effort, since webhooks are the primary path, and it never
fails the job: a missing `jq`, a job without `id-token: write` and
`environment: soothfast-bot`, or a `bot-token` leaves a notice, and an
unreachable or refusing broker leaves a warning with the broker's reason.

### Limits

- Only pull requests into the default branch are evaluated. For one
  retargeted onto it, the next sweep handles its update label; apply
  `soothfast:merge` again to queue it.
- A push or sweep stops starting pull requests when its remaining Workers
  subrequest budget cannot cover another, taking the least recently updated
  first among those the bot could act on. The broker logs when it truncates.
- The broker caches each repository's `soothfast.toml` for up to 60 seconds
  per Worker instance, so a changed `[bot]` table can take that long to apply.
  A push to the default branch that changes the file, or whose commit list
  cannot show whether it did, drops the cache first.
- A pull request that is already mergeable on a merge-queue branch is not
  enqueued: GitHub refuses the direct merge, and the broker logs it. Apply
  `soothfast:merge` before its checks finish.
- A pull request whose only failing checks are not required is merged at
  once, the same as native auto-merge, which waits for required checks only.
- Branch protection that the App cannot read counts as no required checks, so
  `soothfast:merge` is refused rather than risk an immediate merge.
- On a merge-queue branch the bot relies on GitHub's auto-merge to enqueue
  the pull request; this has not been exercised against a live queue.

With `bot-token` there is no broker, so the label sweep leaves a notice and
does nothing; only a broker, hosted or self-hosted, acts on labels.

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

- `gh`, to comment with the job token and to land regenerated files. Without it the gate still runs and only skips the
  perf-accepted label check, with a warning.
- `jq`, to comment, sweep labels or mint a token as soothfast-bot, and to find
  the bench packages when none are named.
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
parallel. `run.yml` now compiles once. After `plan`, two build jobs run on
separate machines: `Build (bench)` and `Build (rustdoc)`. The gate waits for
the first only, so it never waits for a nightly rustdoc build. The docs check
waits for the second and for the gate, since it reads the gate's baseline.
When the docs bind no Rust item, `docs check` builds no rustdoc, so neither
`Build (rustdoc)` nor a nightly toolchain runs at all.

| Lane | Builds | Into |
|---|---|---|
| `bench` | HEAD's bench executable of each gated package, its link map, and the merge-base's executable and map when the run cache holds no run for the merge-base | `.soothfast/bins` |
| `routes` | The dev-profile executable behind `spec check`, for each package with a `[[spec]]` entry | `.soothfast/bins` |
| `rustdoc` | Rustdoc JSON for the docs package and, for each package that generates a spec, for it and the workspace crates its types come from | `.soothfast/rustdoc` |

`run.yml` builds the `bench` and `rustdoc` lanes. Its `spec` check job builds
what it needs itself, so `run.yml` builds no `routes` lane. A job is skipped when its lane has nothing to build,
and each lane reads its store first and builds only what is missing. The
`bench` lane leaves the merge-base alone for the reason the gate does: if the
run cache holds a run for it, the gate reuses that run and never builds the
executable. That run is keyed by the CPU model, so a leg on a different one finds none
and builds the merge-base itself: a miss, not a wrong answer. When a HEAD bench executable fails to build the merge-base side is
not built, and a rustdoc or routes failure does not stop it. A workspace crate
whose rustdoc fails to build for a generated spec is reported and dropped, as
`spec gen` does. Each lane prints what it built against what it found, and
`SOOTHFAST_TIMINGS=1` times every build.

```text
prebuild: stored   bench executable core (head)
prebuild: built    bench executable server (head)
prebuild: skipped  bench executable core (merge-base)
prebuild: 1 built, 1 skipped, 1 stored
```

Each job uploads `soothfast-store-bench` or `soothfast-store-rustdoc` as a tar
(an artifact drops file modes, a tar keeps them), kept one day. They hold only the entries the lane built or found (`prebuild-stage`): the
executables with their digests and link-map extents, and the rustdoc JSON,
not the rest of a restored cache. The Rust gate, every shard of it, downloads
the bench artifact into `.soothfast/` and the docs check the rustdoc artifact,
and each finds a hit for everything it reads and compiles nothing. The tests,
and the gates and tests of the other ecosystems, do not wait for either build.

A job that runs when its build job failed finds nothing to download and
compiles for itself, so a compile error still shows up in that job's output
and in the verdict. The docs job also keeps the [Rustdoc cache](#rustdoc-cache)
as a fallback, so a store that misses costs no more than before.

A hit needs the reading job to be the same machine setup as the building one.
The key names the checkout path, the target directory, `CARGO_HOME`, `rustc
-vV` and the build variables (`RUSTFLAGS`, `CARGO_INCREMENTAL`, and the
`CARGO_PROFILE_`, `CARGO_BUILD_` and `CARGO_TARGET_` families). The path
cannot be made relative: compiled code carries it, so a binary built at
another path is not the same binary. GitHub-hosted runners use one workspace
path per repository for every job. The reading jobs set up the toolchain and
`rust-cache` the way the build jobs do, since those steps export some of the
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

The legs record `soothfast-result-rust-shard-<i>-of-<n>` and the verdict folds them
into one section for the package. Each leg of a pull request saves the items
its shard owns into its own `head` baseline, stamped with the shard and
listing every item the bench binary holds, and uploads it as
`soothfast-baseline-rust-shard-<i>-of-<n>`. The docs job builds one baseline
from exactly the files of this run with `cargo soothfast baseline merge
--into head --expect N --packages "A B" FILE...`. A package whose gate failed
or errored, sharded or not, saves no numbers and lists the items its shard
owns as `gate-failed`; a leg that wrote no file at all leaves its shard's
items `shard-missing`, named from the other legs' listings. A corrupt file,
or one from a gate split another way, fails the docs job after binds and
examples are checked. A default-branch push records every item in its first
leg and saves the baseline as before; the other legs are skipped on a push,
since they would only set up. A push whose commit has no parent lists its
items as `no-parent`.

The docs check runs with `--against-ref` on the merge-base: a claim reads
"not evaluated" only when the merge-base made it unchanged and its metric is
listed `unmeasured`. A new or edited claim, or one on an item nothing lists,
fails without a measurement. Counters taken from the item cache, from a run
stored for the same binary or from a run-cache hit on HEAD count as this
pull request's, since the code they measured is proven identical; walltime
from those sources does not. A pull request whose bench binaries match the
merge-base, with no stored run of them, reports its counter claims not
evaluated, since its gate takes no counters. The cargo gate measures only a
pull request and a default-branch push. On any other event (a push to
another branch, a tag, `workflow_dispatch`, `merge_group`) the merge writes
one `not-gated-on-event` entry: every claim whose item does not start with a
configured import prefix (each `[[project]]`'s `prefix`, else its format's)
is not evaluated. Imported claims measured on that event are checked as
usual, and a missing or misspelled imported id fails. A prefix equal to a
package name in the cargo workspace (`-` read as `_`), the only name a Rust
id starts with, fails `import`, `gate --import`, and a `baseline merge` of
an import file or an ungated event; with no workspace to read, a note says
the prefixes went unchecked. A merge of Rust shards alone never reads
`soothfast.toml`. Where no measurement was
possible (`not-gated-on-event`, `no-parent`), a claim reads "not evaluated"
even when it is new or edited.

Which items belong to which package comes from each package's bench binary
listing, recorded as `owners` in the baseline, never from an id's name: every
bench file's ids start with the bench target's name. An entry covers only the
ids it names. A failed package's old numbers are exactly the ids it owned and
no other package owns, and its entry names those, or the ones its listing
shows; with neither it names nothing, and a claim on an item no package owns
fails, naming that gate failure as possibly related. A leg that uploaded
nothing at all, from a setup failure or a cancel, gets a `no-measurement`
entry naming the items the Build (bench) job listed for its package from
the stores it built, with `cargo soothfast baseline plan` (the
`soothfast-plan` artifact, passed to the merge as `--plan`): an
unchanged claim on them is not evaluated and a new or edited one fails. An
item no listing names, an import's among them, still fails. An id two packages both
list, as a lib item linked into both their bench binaries is, belongs to
both, and a claim on it reads the number the last save wrote. A full save
leaves alone any id another package owns. `baseline merge` refuses an id two
merged files both hold.

`--save-baseline`, on `gate` or `measure`, needs `-p` in a workspace with more
than one bench package, and fails before measuring without it; with one
bench package the save is that package's. A filtered save updates the
package's numbers in place, and a failed filtered gate drops and names only
the filtered ids. A full save into a baseline
saved before `owners` existed replaces every bench item no other package
owns, and says to re-save the other packages. Baselines saved before
`owners` existed are read with their old unmeasured entries left out, and
the check says to re-save them.

`cargo soothfast prebuild` runs the same lanes by hand: `-p PKG` for bench
packages, `--against-ref REF` for the merge-base, `--docs PKG`, `--spec
PKG[:TARGET]`, `--only bench|routes|rustdoc`, `--stage DIR` and `--shards N`.

A spec job of the caller's builds its own lane with the action:
`prebuild: routes rustdoc` runs those lanes in place of the gate, for the
packages with `[[spec]]` entries, and `prebuild-docs: PKG` alone runs
`rustdoc`. A later job downloads the staged directory into `.soothfast/` before
`spec check`.

## Checks from the file

`cargo soothfast plan` also reads what else `soothfast.toml` configures and
gives `run.yml` one job for each: `ci spec` for every package with a
`[[spec]]` entry, `ci proto` for every package with a `[[proto]]` entry,
`ci coverage` for every docs package with a `coverage` minimum, `ci capture`
for every docs package with docs of its own, and `ci site` when the file has a `[site]` table. A
job is named for its record, `spec-PKG` or `site`, runs
`cargo soothfast ci KIND -p PKG`, and installs the pinned rustdoc toolchain
when its command reads rustdoc JSON (`spec`, `coverage` and `site`). The
verdict expects a result from each. A kind named in `[ci] skip` gets no job.

Only `run.yml` runs these jobs. `cargo soothfast init` writes a workflow that
calls it whenever the file configures one of them or `[ci] pages`. A cargo
workspace with none keeps its test, gate and docs jobs in the workflow itself.

### Deploying the site

`[ci] pages = true` in the root `soothfast.toml` publishes the site to GitHub
Pages on every push to the default branch. The docs job builds it after it
has merged the gate's baselines, so the pages carry that push's measured
numbers, and uploads it as the Pages artifact. A `Deploy site` job then
publishes the artifact. That job checks nothing out, runs no repository code
and holds only `pages: write` and `id-token: write`, so no token that can
write to the repository is involved.

It needs three things: a `[site]` table, a docs package (the docs job is what
builds the site), and the repository's Pages source set to "GitHub Actions".
`cargo soothfast init` adds `pages: write` to the caller's permissions when
the setting is on, and `init --check` reports the caller as drifted until it
has been rerun. A failing docs check deploys nothing.

## One verdict

The reusable `run.yml` workflow gives a pull request one soothfast comment and
one check, however many projects or shards it gates. Every gate job, the test
job, the docs check and each check job records a result (project, kind, verdict, the last forty
lines of output, reuse counts, seconds) and uploads it as
`soothfast-result-<name>`. The `verdict` job downloads them all, runs
`cargo soothfast ci verdict`, and posts one comment marked
`<!-- soothfast-verdict -->` with a section per project, regressed and
errored first. Shards of one project fold into one section.

Each section keeps the output of its job without terminal colour codes, and
without cargo's `warning:` blocks unless the job errored. A docs check that
skipped its claims for lack of a measurement reads `pass (claims not
evaluated)`. `run.yml` sets `CARGO_TERM_COLOR: never` for every job.

Branch protection needs only the `verdict` check, named `<caller job id> /
verdict`. The caller `cargo soothfast init` writes names its job `soothfast`,
so a repo set up with init requires `soothfast / verdict`. The check fails when any project
regressed or errored, or recorded no result; a perf-accepted label
(`soothfast:perf-accepted` unless `perf_accepted_labels` renames it) applied by
a writer waives regressions, never errors or docs drift.

## One command per job

`cargo soothfast ci <kind>` runs one CI job: the flags, the choice of base
and the verdict record that a workflow step would otherwise assemble in
shell. It runs the same on a runner and on your machine. `run.yml` calls
`ci test`, `ci build`, `ci gate`, `ci gate-import`, `ci docs` and `ci verdict`,
and the action's regeneration is `ci regen`.

```sh ignore
cargo soothfast ci test
cargo soothfast ci test --project web --toolchain node-20
cargo soothfast ci
```

The run is read from the runner's environment (`GITHUB_EVENT_NAME`, the
event payload, `GITHUB_REF`, `RUNNER_TEMP`). Where there is none, or to
override it, three flags apply to every kind: `--event pull_request|push|other`,
`--base REF` and `--results DIR`. Flags win. A job records its verdict as
`<name>.json` in the results directory, `$RUNNER_TEMP/soothfast-results`
on a runner, next to its output as `<name>.log`; without a results directory
nothing is recorded, which is what a local run does. The exit code is the
job's own.

`ci test` is the `Test` job of `run.yml`. On a pull request it runs
`cargo soothfast test --base` the pull request's base commit, and on a push
the previous head, both read from the event. A base that is unknown or is not
in the checkout runs every package. `--all` runs every package and ignores
stored passes. Features come from the `test` defaults of each package in
`soothfast.toml`, and anything else is passed on to `cargo soothfast test`.
With `--project DIR` it runs one project outside the workspace, always in
full, taking `--suite`, `--toolchain` and `--name` (default `test-` and the
directory). The record is named `test-rust` for the workspace, so its section
of the pull request comment is the one the workflow writes.

`ci gate` is the gate job. With no `-p` it gates every package that has a
`soothfast` bench target, each on its own, so its features and bench target
come from that package's `gate` defaults in `soothfast.toml`. On a pull
request it runs `cargo soothfast gate --against-ref` the base commit. On a push
to the default branch it gates HEAD against its parent with `SOOTHFAST_RECORD=1`,
which stores the run for later pull requests, and never fails; a HEAD with no
parent lists each package's items as `no-parent` instead when `--save-baseline`
names a baseline. A local run gates against `--base`, or leaves the reference
to `gate`. `--against REF` gates against REF on any event. `--shard I/N`,
`--save-baseline NAME` and `--features F` are passed on, and a shard records
as `PKG#I/N`.

Each package records a `rust` verdict named for it, and the last 40 lines of
every package's output are written to `gate-comment.md` in the results
directory, with any run of three or more backticks spaced out so the output
cannot close the comment's code fence. The exit code is 1 when a package
regressed and 2 when one errored. `--accepted LABEL` turns a regression into
a pass and adds a line to the comment naming the label, but never waives an
error. Who may apply the label is for the workflow to decide before it passes
the flag.
`ci docs` is the `Docs check` job. It checks the `[docs] package`s, or the
`-p` ones given, with the defaults each package resolves in `soothfast.toml`.
On a pull request `docs check` compares against `origin/` the base branch, and
elsewhere in a run against `HEAD^`; a local run compares against nothing.
`--baselines DIR` is where the gate legs' baselines were downloaded, one
directory per leg: they are merged into the `head` baseline, which the check
then reads. `--expect-shards N` is the shard count the gate ran with, and
applies to pull requests only, since a push records in one leg. `--plan FILE`
names the item listing the gate wrote. A run the gate does not measure on
merges as not gated. When no gate measures the repository, claims are skipped
and the output says so. When the merge fails, claims are skipped, binds and
examples are still checked, and the job exits with the merge's code. Without
`--baselines` nothing is merged and the check reads whatever baseline exists
locally. The record is named `docs`.
`ci gate-import` is the perf gate of a project outside the cargo workspace,
the `[[project]]` tables with a benchmark command. It benches the head with
the project's own command, benches the merge base of a pull request in a
worktree of its own, and gates the imported results with `gate --import`.
A push, or a run with no base, only benches the head. Each project is gated
on its own, `--project DIR` picks one and `--name` renames its record (the
ecosystem, plus the directory when two share one). Without `--project` every
project runs and the exit code is the worst.

```sh ignore
cargo soothfast ci gate-import
cargo soothfast ci gate-import --project web --accepted soothfast:perf-accepted
```

A head that fails or leaves no results errors the job, after the base is
benched so the items it names are known. A base that fails or leaves no
results has nothing to compare and passes. A regression exits 1 and an
error anything else; the head's items are saved for the docs check unless
the gate errored, and a pass that cannot save them errors. `--accepted
LABEL` waives a regression, which then exits 0, and never an error. The
worktree is removed whether or not the bench fails.
`ci spec` is the spec checks of every package with a `[[spec]]` entry, or
the `-p` ones given, with the `spec` defaults each package resolves in
`soothfast.toml`. A package that generates a spec runs `spec gen --check`,
which fails on a stale committed spec. On a pull request, and on any run given
`--base`, `spec gate` compares with the base; a push alone has none. A package
with a spec that is not generated runs `spec check`, and one with an `[[sdk]]`
entry runs `sdk gen --check`. Every step runs even after one fails, and each
package records a `check` verdict named `spec-PKG` with the worst exit code.
With nothing configured it says so and records nothing.
`ci proto` runs `spec check-proto` for every package with a `[[proto]]`
entry, or the `-p` ones given, which reconciles each message's fields against
its Rust struct. Each package records a `check` verdict named `proto-PKG`.
With nothing configured it says so and records nothing.
`ci coverage` runs `coverage docs` for every docs package that sets a
`coverage` minimum, in its `[package.NAME.docs]` table or in `[docs]`, or the
`-p` ones given, and fails when a package documents less than its minimum.
Each package records a `check` verdict named `coverage-PKG`. With nothing
configured it says so and records nothing.
`ci capture` runs `docs capture --check` for each docs package, or the `-p`
ones given, which fails when the output recorded beside a capture block no
longer matches what the block prints. A package with no capture block passes.
A lone docs package is checked against every doc. Among several, the blocks of
all the docs would compile into each package's crate, so only a package whose
`[package.NAME.docs]` table names its own `paths` is checked. Each package
records a `check` verdict named `capture-PKG`. With no such package it says so
and records nothing.
`ci site` builds the docs site with `docs build --out DIR`, where DIR is
`target/soothfast-site` unless `--out` says otherwise. It builds against the
`head` baseline when `.soothfast/baselines/head.json` exists. It runs only when
the root `soothfast.toml` has a `[site]` table; without one it says so and
records nothing. The record is a `check` verdict named `site`.

A site whose pages are partly generated (an API reference, a perf report)
names the command that generates them as `site_run` under `[ci]`:

```toml ignore
[ci]
site_run = "make docs-pages"
```

`ci site` runs it with `bash -euo pipefail` from the workspace root before the
build, under the same record, with `$SOOTHFAST` naming the running
`cargo-soothfast` and `$SOOTHFAST_BASELINE` set to `head` when that baseline
exists and empty otherwise. A command that fails stops the build.
`ci build LANE` is the build-once job: it prebuilds into the stores what later
jobs read, with `cargo soothfast prebuild --only LANE` for the `bench`,
`routes` or `rustdoc` lane. With no package named, `bench` builds the gated
packages, `routes` every package with a `[[spec]]` entry, and `rustdoc` the
docs packages and every package that generates a spec. `-p PKG` names the
packages of `bench`, `--spec PKG[:TARGET]` those of `routes`, and `--docs PKG`
and `--spec PKG` those of `rustdoc`; naming any leaves the rest of the lane
unbuilt. `prebuild` reads a spec package's `target` and `features` from its
`[package.NAME.spec]` table and a docs package's `features` from
`[package.NAME.docs]`, unless `:TARGET`, `--features` or `--docs-features`
says otherwise.

```sh ignore
cargo soothfast ci build bench --shards 3 --stage "$RUNNER_TEMP/store" \
  --plan-out "$RUNNER_TEMP/soothfast-plan/plan.json" --listing-out listing.txt
cargo soothfast ci build rustdoc --stage "$RUNNER_TEMP/store"
```

Without `-p` it builds the packages the lane applies to, found the way `plan`
finds them: the benched packages for `bench`, the `[docs] package`s for
`rustdoc`. `routes` needs its packages named, as `PKG[:TARGET]`. Packages
that resolve the same `soothfast.toml` defaults share one `prebuild` call, and
each set that resolves differently gets its own, in the order they first
appear. On a pull request the call compares with the base commit,
and `--shards` is the leg count the gate will read the stores with; on a push
it builds one unsharded lane, comparing with the previous commit on the
default branch. `--stage` is where the stores are copied for upload.

`--plan-out` adds each package's planned items to a plan file once the build
is done, as `baseline plan` does, and a package that cannot be listed is
skipped with a warning. `--listing-out` writes two lines,
`before DIGEST` and `after DIGEST`, the checksum of the names and sizes of the
files under `.soothfast/bins` when the job started and when it ended, so a
job that saves the executables cache saves it when the two differ. The digest
is the FNV-1a hash of the sorted `name size` lines in 16 hex digits, `none`
when the directory does not exist, and ignores files ending in `.tmp`.

A build that fails is an error exit, not a regression. The remaining
packages still build and the plan and listing are still written, and the
exit code is that of the first failure. The job records no verdict.
Bare `ci` does not run it, since it needs a lane.
`ci verdict` folds the records in the results directory into the pull request
comment and the run's verdict. `--expect NAME:KIND` names a job that must have
recorded a result (a missing one errors the verdict), with `KIND` one of
`rust`, `import`, `docs`, `test` or `check`. `--accepted LABEL` waives
regressions, the way the perf-accepted label does, and never errors. The body
is written to `--out`, by default `verdict-comment.md` in the results
directory, and its job output stays inside the code fence. Posting it is left
to the caller. A `check` record is a job whose every non-zero exit is an
error, so nothing waives it.

`ci regen` is the regeneration a default-branch push lands. It measures every
package with a `soothfast` bench target and regenerates `CHANGELOG.md` over
the `[changelog]` packages, or those same packages when it names none
(`--no-changelog` skips both), runs `spec gen`
for each package with a `mode = "generate"` spec and `sdk gen` for each with
`[[sdk]]`, then runs the `REGEN_RUN` command with `$SOOTHFAST` set when that
variable is set (it needs `REGEN_PATHS`). It prints `paths=` and `changed=`
lines, also written to `--paths-out`, and never commits or pushes.
`--baseline NAME` names the baseline it measures into (default `base`).

`cargo soothfast ci` with no kind runs every kind that applies to the event,
in order, and stops at the first failure. `verdict` applies to pull requests
and `regen` to pushes to the default branch.

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
      soothfast_ref: <tag-or-sha>
      package: mylib
```

`soothfast_ref` is the same tag or commit as in the `uses:` line, which the
workflow cannot read for itself. Left empty, the scripts come from the commit
GitHub reports for the workflow, and from the default branch with a warning
when it reports none. `cli-artifact` names an artifact holding a prebuilt CLI at
`bin/cargo-soothfast` from earlier in the same run; without it the workflow
installs the release matching your `Cargo.lock`.
