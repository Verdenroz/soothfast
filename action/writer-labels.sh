#!/usr/bin/env bash
# The pull request's labels whose last applier can write to the repository.
# Inputs: PR_NUMBER LABELS (comma-separated labels on the pull request),
# GH_TOKEN, GITHUB_REPOSITORY.
# Output: labels (comma-separated).
set -euo pipefail

# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"
applied=()
IFS=, read -ra labels <<<"${LABELS:-}"
for label in "${labels[@]}"; do
  if [ -n "$label" ] && labelled_by_writer "$PR_NUMBER" "$label"; then
    applied+=("$label")
  fi
done
echo "labels=$(IFS=,; echo "${applied[*]-}")" >>"$GITHUB_OUTPUT"
