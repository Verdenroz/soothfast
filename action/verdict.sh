#!/usr/bin/env bash
# Fold every gate job's result into the pull request's one comment and exit
# status. Results are untrusted text; `ci verdict` neutralises the fences.
# Inputs: those of render-verdict.sh, minus OUT, and BROKER (optional).
# A body that could not be posted is staged in $RUNNER_TEMP/soothfast-verdict-comment
# for the workflow to upload, which is how a fork's verdict reaches comment.yml.
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
  cp "$body" "$staged/body.md"
  echo "$PR_NUMBER" >"$staged/pr-number"
fi
exit "$rc"
