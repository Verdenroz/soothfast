#!/usr/bin/env bash
# Act on soothfast labels across open pull requests as soothfast-bot (see the
# Labels section of docs/ci.md). do-not-merge counts from anyone; the others
# only from someone who can write.
# Inputs: TOKEN DEFAULT_BRANCH, REVOKE (true when this run minted TOKEN).
set -euo pipefail

: "${TOKEN:?}" "${DEFAULT_BRANCH:?}"
export GH_TOKEN="$TOKEN"
if [ "${REVOKE:-false}" = true ]; then
  trap 'gh api -X DELETE /installation/token >/dev/null || true' EXIT
fi
# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"

default=$DEFAULT_BRANCH
# Without a required check auto-merge merges at once, so a label alone would
# merge code nothing has checked. Rulesets and classic protection both count.
ruled=$(gh api "repos/${GITHUB_REPOSITORY}/rules/branches/${default}" \
  --jq '[.[] | select(.type == "required_status_checks" or .type == "merge_queue")] | length')
protected=$(gh api "repos/${GITHUB_REPOSITORY}/branches/${default}" \
  --jq '(.protection.required_status_checks.contexts // []) + (.protection.required_status_checks.checks // []) | length')
checked=$((ruled + protected))

gh pr list --base "$default" --state open --limit 100 \
  --json number,isDraft,labels,autoMergeRequest,mergeStateStatus \
  --jq '.[] | [.number, .isDraft, ([.labels[].name] | join(",")), (.autoMergeRequest != null), .mergeStateStatus] | @tsv' |
  while IFS=$'\t' read -r pr draft labels queued state; do
    has() { [[ ",${labels}," == *",$1,"* ]]; }
    if has do-not-merge; then
      if [ "$queued" = true ]; then
        gh pr merge --disable-auto "$pr" && echo "sweep: #${pr} auto-merge cancelled (do-not-merge)"
      fi
      continue
    fi
    if has soothfast:update && [ "$state" = BEHIND ] && labelled_by_writer "$pr" soothfast:update; then
      if gh api -X PUT "repos/${GITHUB_REPOSITORY}/pulls/${pr}/update-branch" >/dev/null; then
        echo "sweep: #${pr} updated from ${default}"
      else
        echo "::warning::could not update #${pr} (a fork that disallows maintainer edits?)"
      fi
    fi
    if has soothfast:merge && [ "$draft" = false ] && [ "$queued" = false ] && labelled_by_writer "$pr" soothfast:merge; then
      if [ "$checked" -eq 0 ]; then
        echo "::warning::#${pr} is labelled soothfast:merge, but ${default} requires no status check; not merging"
        continue
      fi
      gh pr merge --auto --squash --delete-branch "$pr" && echo "sweep: #${pr} queued to merge"
    fi
  done
