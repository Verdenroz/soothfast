#!/usr/bin/env bash
# Commit PATHS as APP_SLUG[bot], push them to BRANCH, open or refresh its
# pull request against the default branch, and merge it: auto-merge when the
# default branch carries rules, immediately otherwise. TOKEN is revoked on
# every exit path.
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
git -c "user.name=${APP_SLUG}[bot]" \
  -c "user.email=${bot_id}+${APP_SLUG}[bot]@users.noreply.github.com" \
  commit -q -m "$TITLE"
git push --force "https://x-access-token:${TOKEN}@github.com/${GITHUB_REPOSITORY}" \
  "HEAD:refs/heads/${BRANCH}"

default=$(gh repo view --json defaultBranchRef --jq .defaultBranchRef.name)
pr=$(gh pr list --head "$BRANCH" --base "$default" --state open --json number --jq '.[0].number // empty')
if [ -z "$pr" ]; then
  gh pr create --head "$BRANCH" --base "$default" --title "$TITLE" --body "${BODY:-}" >/dev/null
  pr=$(gh pr view "$BRANCH" --json number --jq .number)
else
  gh pr edit "$pr" --title "$TITLE" --body "${BODY:-}" >/dev/null
fi

rules=$(gh api "repos/${GITHUB_REPOSITORY}/rules/branches/${default}" --jq length)
if [ "$rules" -gt 0 ]; then
  gh pr merge --auto --squash --delete-branch "$pr"
else
  gh pr merge --squash --delete-branch "$pr"
fi
