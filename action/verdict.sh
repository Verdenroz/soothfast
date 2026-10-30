#!/usr/bin/env bash
# Fold every gate job's result into the pull request's one comment and exit
# status. Results are untrusted text; `verdict merge` neutralises the fences.
# Inputs: CLI RESULTS PR_NUMBER GH_TOKEN GITHUB_REPOSITORY, BROKER and EXPECT
# (space-separated project names that must have a result) (optional).
set -euo pipefail

# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"

expect=()
for name in ${EXPECT:-}; do
  expect+=(--expect "$name")
done
accepted=()
if perf_accepted "$PR_NUMBER"; then
  accepted=(--accepted)
fi

body="${RUNNER_TEMP:-/tmp}/soothfast-verdict.md"
rc=0
"$CLI" verdict merge "$RESULTS" --out "$body" "${expect[@]}" "${accepted[@]}" || rc=$?
[ "$rc" -le 1 ] || exit "$rc"

cat "$body"
MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" "$(dirname "$0")/comment.sh" ||
  echo "::warning::could not comment on the pull request (read-only token on a fork?)"
exit "$rc"
