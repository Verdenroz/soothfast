#!/usr/bin/env bash
# Create or update this action's one comment on a pull request. A marker
# line identifies it: github.token's author is shared with every other
# action in the repository, so "edit the last comment by me" would hit theirs.
# Inputs: GH_TOKEN PR_NUMBER MARKER BODY_FILE.
set -euo pipefail

body="${RUNNER_TEMP:-/tmp}/soothfast-comment.md"
{ echo "$MARKER"; cat "$BODY_FILE"; } >"$body"
id=$(gh api "repos/${GITHUB_REPOSITORY}/issues/${PR_NUMBER}/comments" --paginate \
  --jq "[.[] | select(.body | startswith(\"$MARKER\")) | .id][0] // empty")
if [ -n "$id" ]; then
  gh api -X PATCH "repos/${GITHUB_REPOSITORY}/issues/comments/${id}" -F "body=@${body}" >/dev/null
else
  gh api -X POST "repos/${GITHUB_REPOSITORY}/issues/${PR_NUMBER}/comments" -F "body=@${body}" >/dev/null
fi
