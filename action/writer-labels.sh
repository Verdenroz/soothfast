#!/usr/bin/env bash
# The pull request's current labels whose last applier can write to the
# repository. Read from the API, since a re-run reuses the event's labels.
# Inputs: PR_NUMBER, GH_TOKEN, GITHUB_REPOSITORY.
# Output: labels (comma-separated).
set -euo pipefail

# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"
mapfile -t labels < <(gh api --paginate "repos/${GITHUB_REPOSITORY}/issues/${PR_NUMBER}/labels" --jq '.[].name')
applied=()
for label in "${labels[@]}"; do
  if labelled_by_writer "$PR_NUMBER" "$label"; then
    applied+=("$label")
  fi
done
echo "labels=$(IFS=,; echo "${applied[*]-}")" >>"$GITHUB_OUTPUT"
