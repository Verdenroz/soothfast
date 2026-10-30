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
| `cache` | `true` | Restore and save `.soothfast/runs`, `items` and `maps`, keyed so a pull request restores what its merge-base recorded. |

Outputs are `version` and `cache-hit`.

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

The scripts use `gh`, `jq`, `curl`, `git`, and `cargo`. On Linux the step
installs `valgrind` with `apt-get` when it is missing, for the callgrind
fallback on machines without performance counters.

## One verdict

The reusable `run.yml` workflow gives a pull request one soothfast comment and
one check, however many projects or shards it gates. Every gate job, the test
job and the docs check records a `result.json` (project, kind, verdict, the last forty
lines of output, reuse counts, seconds) and uploads it as
`soothfast-result-<name>`. The `verdict` job downloads them all, runs
`cargo soothfast verdict merge`, and posts one comment marked
`<!-- soothfast-verdict -->` with a section per project, regressed and
errored first. Shards of one project fold into one section.

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
