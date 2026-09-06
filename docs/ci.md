# CI

soothfast in another repository is one step in the workflow you already
have. On a pull request it gates performance against the base branch and
comments the result. On a push to the default branch it regenerates derived
files and lands them as a pull request authored by soothfast-bot, which
merges itself once your checks pass.

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
regeneration job on `push` with the environment and `gate: false`. That split
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

**On a push to the default branch.** It measures each package into the
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
| `packages` | every package with a bench target named `soothfast` | Space-separated packages to gate and measure. The same list feeds `report changelog`; pass it explicitly when the crates whose API ships differ from the ones with benches. |
| `gate` | `true` | Run the gate on pull requests. |
| `changelog` | `true` | Regenerate `CHANGELOG.md` on default-branch pushes. |
| `spec` | none | Space-separated packages whose `mode = "generate"` specs to regenerate. |
| `baseline` | `base` | Baseline name the regeneration measures into. |
| `rustdoc-toolchain` | the nightly the release was tested with | Toolchain for rustdoc JSON. A floating `nightly` can change the JSON format under the API diff. |
| `version` | from `Cargo.lock` | `cargo-soothfast` version to install. |
| `lockfile` | `Cargo.lock` | Where to read the pinned version. |
| `binary` | none | A prebuilt `cargo-soothfast`; skips the install. |
| `token` | `github.token` | Token for the gate comment and repository lookups. |
| `bot-token` | none | Bring your own soothfast-bot installation token; skips the broker. |
| `bot-slug` | `soothfast-bot` | App slug `bot-token` belongs to, for the commit author. |
| `broker` | built in | Token broker URL. |

Outputs are `version` and `cache-hit`.

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
