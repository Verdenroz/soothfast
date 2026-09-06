#!/usr/bin/env bash
# Commit PATHS as APP_SLUG[bot], push them to BRANCH, open or refresh its
# pull request against the default branch, and merge it: auto-merge when the
# default branch has a rule that blocks merging, immediately otherwise. The
# working tree keeps the regenerated files but HEAD is left where it was.
# TOKEN is revoked on every exit path.
# Inputs: TOKEN APP_SLUG BRANCH TITLE BODY PATHS (space separated pathspecs).
set -euo pipefail

: "${TOKEN:?}" "${APP_SLUG:?}" "${BRANCH:?}" "${TITLE:?}" "${PATHS:?}"
export GH_TOKEN="$TOKEN"
trap 'gh api -X DELETE /installation/token >/dev/null || true' EXIT

read -ra paths <<<"$PATHS"
git add -- "${paths[@]}"
if git diff --cached --quiet; then
  echo "land: nothing to commit"
  exit 0
fi

bot_id=$(gh api "/users/${APP_SLUG}[bot]" --jq .id)
head=$(git rev-parse HEAD)
git -c "user.name=${APP_SLUG}[bot]" \
  -c "user.email=${bot_id}+${APP_SLUG}[bot]@users.noreply.github.com" \
  commit -q -m "$TITLE"
# shellcheck disable=SC2016 # git expands $TOKEN when it runs the helper
git -c credential.helper= \
  -c credential.helper='!f() { echo "username=x-access-token"; echo "password=$TOKEN"; }; f' \
  push --force "https://github.com/${GITHUB_REPOSITORY}" "HEAD:refs/heads/${BRANCH}"
git reset -q --soft "$head"

default=$(gh repo view --json defaultBranchRef --jq .defaultBranchRef.name)
pr=$(gh pr list --head "$BRANCH" --base "$default" --state open --json number --jq '.[0].number // empty')
if [ -z "$pr" ]; then
  url=$(gh pr create --head "$BRANCH" --base "$default" --title "$TITLE" --body "${BODY:-}")
  pr=${url##*/}
else
  gh pr edit "$pr" --title "$TITLE" --body "${BODY:-}" >/dev/null
fi

# History-only rules (deletion, linear history) leave the PR clean, and
# --auto refuses a PR with nothing to wait for.
blocking=$(gh api "repos/${GITHUB_REPOSITORY}/rules/branches/${default}" \
  --jq '[.[] | select(.type == "required_status_checks" or .type == "pull_request" or .type == "merge_queue")] | length')
if [ "$blocking" -gt 0 ]; then
  gh pr merge --auto --squash --delete-branch "$pr"
else
  gh pr merge --squash --delete-branch "$pr"
fi
