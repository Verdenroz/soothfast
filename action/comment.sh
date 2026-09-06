#!/usr/bin/env bash
# Create or update this action's one comment on a pull request, identified
# by a marker line. The broker posts it as soothfast-bot; a job with no OIDC
# identity (a fork pull request) falls back to GH_TOKEN, whose author is
# shared with every other action in the repository, hence the marker rather
# than "edit my last comment".
# Inputs: GH_TOKEN PR_NUMBER MARKER BODY_FILE, BROKER (optional).
set -euo pipefail

# shellcheck source=action/oidc.sh
source "$(dirname "$0")/oidc.sh"

if oidc=$(oidc_token); then
  payload=$(jq -n --argjson pr "$PR_NUMBER" --arg marker "$MARKER" --rawfile body "$BODY_FILE" \
    '{pull_request: $pr, marker: $marker, body: $body}')
  response=$(curl -sS --max-time 30 -w '\n%{http_code}' -X POST "$BROKER/comment" \
    -H "Authorization: Bearer $oidc" -H "content-type: application/json" --data-binary "$payload") || response=$'\n000'
  status=${response##*$'\n'}
  body=${response%$'\n'*}
  if [ "$status" = 200 ]; then
    echo "commented as $(jq -r .app_slug <<<"$body")"
    exit 0
  fi
  reason=$(jq -r '.reason // empty' <<<"$body" 2>/dev/null || true)
  echo "::notice::soothfast-bot did not post the comment (HTTP $status${reason:+: $reason}); posting with the job token instead"
fi

with_marker="${RUNNER_TEMP:-/tmp}/soothfast-comment.md"
{ echo "$MARKER"; cat "$BODY_FILE"; } >"$with_marker"
id=$(gh api "repos/${GITHUB_REPOSITORY}/issues/${PR_NUMBER}/comments" --paginate \
  --jq "[.[] | select(.body | startswith(\"$MARKER\")) | .id][0] // empty")
if [ -n "$id" ]; then
  gh api -X PATCH "repos/${GITHUB_REPOSITORY}/issues/comments/${id}" -F "body=@${with_marker}" >/dev/null
else
  gh api -X POST "repos/${GITHUB_REPOSITORY}/issues/${PR_NUMBER}/comments" -F "body=@${with_marker}" >/dev/null
fi
