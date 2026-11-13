#!/usr/bin/env bash
# Commit PATHS as APP_SLUG[bot] onto BRANCH, open or refresh its pull request
# against the default branch, and merge it: auto-merge when the default
# branch has a rule that blocks merging, immediately otherwise. The commit
# is created through the API, which signs it as the App, so it passes a
# rule that requires signed commits. It lands on the default branch's tip:
# when only the bot has committed since this checkout, this run's files
# replace the bot's; when anyone else has, that push's own run lands and
# this one skips. The working tree and HEAD are left as they were.
# TOKEN is revoked on every exit path.
# Inputs: TOKEN APP_SLUG BRANCH TITLE BODY PATHS (space separated pathspecs).
set -euo pipefail

: "${TOKEN:?}" "${APP_SLUG:?}" "${BRANCH:?}" "${TITLE:?}" "${PATHS:?}"
for tool in gh jq; do
  command -v "$tool" >/dev/null || { echo "::error::$tool must be installed on the runner to land files; see docs/ci.md, Self-hosted runners"; exit 1; }
done
export GH_TOKEN="$TOKEN"
entries=$(mktemp)
trap 'rm -f "$entries"; gh api -X DELETE /installation/token >/dev/null || true' EXIT
repo="repos/${GITHUB_REPOSITORY}"

default=$(gh repo view --json defaultBranchRef --jq .defaultBranchRef.name)
head=$(git rev-parse HEAD)
parent=$(gh api "$repo/git/ref/heads/$default" --jq .object.sha)
if [ "$parent" != "$head" ]; then
  only_bot=$(gh api "$repo/compare/$head...$parent" --jq \
    "(.status == \"ahead\") and (.total_commits == (.commits | length)) and all(.commits[]; .author.login == \"${APP_SLUG}[bot]\")")
  if [ "$only_bot" != true ]; then
    echo "land: $default moved past this checkout, leaving the landing to that push's run"
    exit 0
  fi
  # shellcheck disable=SC2016 # git expands $TOKEN when it runs the helper
  git -c credential.helper= \
    -c credential.helper='!f() { echo "username=x-access-token"; echo "password=$TOKEN"; }; f' \
    fetch -q --depth 1 "https://github.com/${GITHUB_REPOSITORY}" "$parent"
fi

read -ra paths <<<"$PATHS"
git add -- "${paths[@]}"
if git diff --cached --quiet "$parent" -- "${paths[@]}"; then
  echo "land: nothing to commit"
  exit 0
fi

while IFS= read -r -d '' meta && IFS= read -r -d '' path; do
  read -r old_mode new_mode _ new_sha status <<<"${meta#:}"
  if [ "$status" = D ]; then
    jq -nc --arg path "$path" --arg mode "$old_mode" '{path: $path, mode: $mode, type: "blob", sha: null}'
  else
    blob=$(git cat-file blob "$new_sha" | base64 | tr -d '\n' \
      | jq -Rs '{encoding: "base64", content: .}' | gh api "$repo/git/blobs" --input - --jq .sha)
    jq -nc --arg path "$path" --arg mode "$new_mode" --arg sha "$blob" '{path: $path, mode: $mode, type: "blob", sha: $sha}'
  fi
done < <(git diff --cached --no-renames --raw -z --abbrev=40 "$parent" -- "${paths[@]}") >"$entries"

tree=$(jq -s --arg base "$(git rev-parse "$parent^{tree}")" '{base_tree: $base, tree: .}' "$entries" \
  | gh api "$repo/git/trees" --input - --jq .sha)
# No author or committer: the API signs a commit only when it names neither.
commit=$(jq -n --arg message "$TITLE" --arg tree "$tree" --arg parent "$parent" \
  '{message: $message, tree: $tree, parents: [$parent]}' | gh api "$repo/git/commits" --input - --jq .sha)
if ! gh api -X PATCH "$repo/git/refs/heads/$BRANCH" -f sha="$commit" -F force=true >/dev/null 2>&1; then
  gh api "$repo/git/refs" -f ref="refs/heads/$BRANCH" -f sha="$commit" >/dev/null
fi

pr=$(gh pr list --head "$BRANCH" --base "$default" --state open --json number --jq '.[0].number // empty')
if [ -z "$pr" ]; then
  url=$(gh pr create --head "$BRANCH" --base "$default" --title "$TITLE" --body "${BODY:-}")
  pr=${url##*/}
else
  gh pr edit "$pr" --title "$TITLE" --body "${BODY:-}" >/dev/null
fi

# History-only rules (deletion, linear history) leave the PR clean, and
# --auto refuses a PR with nothing to wait for.
blocking=$(gh api "$repo/rules/branches/${default}" \
  --jq '[.[] | select(.type == "required_status_checks" or .type == "pull_request" or .type == "merge_queue")] | length')
if [ "$blocking" -gt 0 ]; then
  gh pr merge --auto --squash --delete-branch "$pr"
else
  gh pr merge --squash --delete-branch "$pr"
fi
