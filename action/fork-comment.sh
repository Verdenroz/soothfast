#!/usr/bin/env bash
# Post a fork pull request's verdict, uploaded by the fork's own run, as the
# pull request's comment. The artifact is attacker-controlled: the number is
# trusted only when it names an open pull request of this repository whose
# head is the commit GitHub reports the run was for, so a stale run cannot
# overwrite a newer verdict and no run can comment on another pull request.
# Inputs: ARTIFACT_DIR HEAD_SHA GH_TOKEN GITHUB_REPOSITORY.
set -euo pipefail

limit=60000
skip() {
  echo "::notice::no comment posted: $1"
  exit 0
}

[ -f "$ARTIFACT_DIR/body.md" ] && [ -f "$ARTIFACT_DIR/pr-number" ] || skip "the run uploaded no verdict"

number=$(head -n 1 "$ARTIFACT_DIR/pr-number")
[[ $number =~ ^[1-9][0-9]{0,9}$ ]] || skip "the pull request number is not an integer"

pull=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${number}" --jq '[.state, .head.sha] | @tsv') ||
  skip "#$number is not a pull request of ${GITHUB_REPOSITORY}"
read -r state head <<<"$pull"
[ "$state" = open ] || skip "#$number is not open"
[ "$head" = "$HEAD_SHA" ] || skip "#$number has moved past the commit this run tested"

body="${RUNNER_TEMP:-/tmp}/soothfast-fork-verdict.md"
{
  echo "_This verdict comes from a fork's run and was not produced with this repository's token._"
  echo
  head -c "$limit" "$ARTIFACT_DIR/body.md" | iconv -f UTF-8 -t UTF-8 -c
  if [ "$(wc -c <"$ARTIFACT_DIR/body.md")" -gt "$limit" ]; then
    printf "\n\n_Truncated: the verdict exceeds the comment size limit. The full text is in the run's job summary._\n"
  fi
} >"$body"

MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" PR_NUMBER="$number" "$(dirname "$0")/comment.sh"
