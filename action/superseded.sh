#!/usr/bin/env bash
# Succeeds when someone other than the bot has pushed to the default branch
# since this checkout, so that push's run lands any regeneration and this
# one need not. The rule is land.sh's. Anything it cannot read, including a
# missing tool, token or commit, is "not superseded".
# Inputs: APP_SLUG DEFAULT_BRANCH GH_TOKEN, and the runner's GITHUB_REPOSITORY
# and GITHUB_SHA.
set -uo pipefail

command -v gh >/dev/null || exit 1
[ -n "${APP_SLUG:-}" ] || exit 1
repo="repos/${GITHUB_REPOSITORY}"
tip=$(gh api "$repo/git/ref/heads/${DEFAULT_BRANCH}" --jq .object.sha 2>/dev/null) || exit 1
[ -n "$tip" ] && [ "$tip" != "$GITHUB_SHA" ] || exit 1
only_bot=$(gh api "$repo/compare/${GITHUB_SHA}...${tip}" --jq \
  "(.status == \"ahead\") and (.total_commits == (.commits | length)) and all(.commits[]; .author.login == \"${APP_SLUG}[bot]\")" 2>/dev/null) || exit 1
[ "$only_bot" = false ] || exit 1
echo "regen: ${DEFAULT_BRANCH} has a newer push that is not the bot's, leaving the regeneration to its run"
