#!/usr/bin/env bash
# Post a fork pull request's verdict, uploaded by the fork's own run, as the
# pull request's comment. The artifact is attacker-controlled: the number is
# trusted only when it names an open pull request of this repository from the
# fork branch and commit GitHub reports the run was for. The commit alone is
# not enough: a fork can push another pull request's head commit to a branch.
# Inputs: ARTIFACT_DIR HEAD_SHA HEAD_REPO HEAD_BRANCH GH_TOKEN GITHUB_REPOSITORY.
set -euo pipefail

limit=60000
skip() {
  echo "::notice::no comment posted: $1"
  exit 0
}

[ -f "$ARTIFACT_DIR/body.md" ] && [ -f "$ARTIFACT_DIR/pr-number" ] || skip "the run uploaded no verdict"

number=$(head -n 1 "$ARTIFACT_DIR/pr-number")
[[ $number =~ ^[1-9][0-9]{0,9}$ ]] || skip "the pull request number is not an integer"

pull=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${number}" \
  --jq '[.state, .head.sha, .head.repo.full_name // "-", .head.ref] | @tsv') ||
  skip "could not read #$number of ${GITHUB_REPOSITORY}"
IFS=$'\t' read -r state head repo branch <<<"$pull"
[ "$state" = open ] || skip "#$number is not open"
[ "$head" = "$HEAD_SHA" ] || skip "#$number has moved past the commit this run tested"
[ "$repo" = "$HEAD_REPO" ] && [ "$repo" != "$GITHUB_REPOSITORY" ] || skip "#$number is not from the fork this run belongs to"
[ "$branch" = "$HEAD_BRANCH" ] || skip "#$number is not from the branch this run belongs to"

body="${RUNNER_TEMP:-/tmp}/soothfast-fork-verdict.md"
{
  echo "_This verdict comes from a fork's run and was not produced with this repository's token._"
  echo
  # iconv -c exits 1 when it drops bytes, as it does after a cut mid-character.
  head -c "$limit" "$ARTIFACT_DIR/body.md" | iconv -f UTF-8 -t UTF-8 -c || true
  if [ "$(wc -c <"$ARTIFACT_DIR/body.md")" -gt "$limit" ]; then
    printf "\n\n_Truncated: the verdict exceeds the comment size limit. The full text is in the run's job summary._\n"
  fi
} >"$body"

MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" PR_NUMBER="$number" "$(dirname "$0")/comment.sh"
