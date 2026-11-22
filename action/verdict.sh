#!/usr/bin/env bash
# Fold every gate job's result into the pull request's one comment and exit
# status. Results are untrusted text; `ci verdict` neutralises the fences.
# Inputs: those of render-verdict.sh, minus OUT, and BROKER (optional).
# When the comment could not be posted, the pull request number and the
# expectations are staged in $RUNNER_TEMP/soothfast-verdict-comment for the
# workflow to upload. comment.yml renders the verdict from the run's results.
set -euo pipefail

body="${RUNNER_TEMP:-/tmp}/soothfast-verdict.md"
rc=0
OUT="$body" "$(dirname "$0")/render-verdict.sh" || rc=$?
[ "$rc" -le 1 ] || exit "$rc"

cat "$body"
[ -z "${GITHUB_STEP_SUMMARY:-}" ] || cat "$body" >>"$GITHUB_STEP_SUMMARY"
if ! MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" "$(dirname "$0")/comment.sh"; then
  echo "::warning::could not comment on the pull request (read-only token on a fork?)"
  staged="${RUNNER_TEMP:-/tmp}/soothfast-verdict-comment"
  mkdir -p "$staged"
  echo "$PR_NUMBER" >"$staged/pr-number"
  for var in EXPECT_RUST EXPECT_SHARDS EXPECT_IMPORT EXPECT_DOCS EXPECT_TEST EXPECT_CHECK; do
    echo "${!var:-}" >"$staged/${var,,}"
  done
fi
exit "$rc"
