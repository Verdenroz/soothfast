#!/usr/bin/env bash
# Commit PATHS as APP_SLUG[bot] onto BRANCH, open or refresh its pull request
# against the default branch, and merge it: auto-merge when the default
# branch has a rule that blocks merging, immediately otherwise. The commit
# is created through the API, which signs it as the App, so it passes a
# rule that requires signed commits.
# Callers that omit BRANCH share bot/soothfast-update. When the branch holds
# only bot commits on the default branch's tip, the commit goes on top of
# them and the ref moves without force, retried from a fresh read when
# another job moved it first; otherwise it lands on the tip with a force
# update. The pull request keeps its opener's title and lists each run's
# TITLE and files in its body.
# When anyone but the bot has pushed to the default branch since this
# checkout, that push's own run lands and this one skips.
# The working tree and HEAD are left as they were. TOKEN is revoked on every
# exit path.
# Inputs: TOKEN APP_SLUG TITLE PATHS (space separated pathspecs), and
# optionally BRANCH (default bot/soothfast-update) and BODY (opens the body
# of a landing this run starts).
set -euo pipefail

# Reads `compare/<tip>...<branch>` on stdin; succeeds when the branch is the
# tip plus bot commits only.
on_tip_of_bot_commits() {
  jq -e --arg tip "$1" --arg bot "$2[bot]" \
    '(.status == "ahead" or .status == "identical") and .merge_base_commit.sha == $tip
      and .total_commits == (.commits | length) and all(.commits[]; .author.login == $bot)' >/dev/null
}

# Body with one "- TITLE: files" line per run: this run's line replaces its
# earlier one, or is appended.
merge_body() {
  local body=$1 title=$2 line=$3 out="" found=0 l
  [ -n "$body" ] || { printf '%s' "$line"; return; }
  while IFS= read -r l; do
    if [[ "$l" == "- $title: "* ]]; then
      [ "$found" = 1 ] || out+="$line"$'\n'
      found=1
    else
      out+="$l"$'\n'
    fi
  done <<<"$body"
  [ "$found" = 1 ] || out+="$line"$'\n'
  printf '%s' "${out%$'\n'}"
}

BRANCH=${BRANCH:-bot/soothfast-update}
: "${TOKEN:?}" "${APP_SLUG:?}" "${TITLE:?}" "${PATHS:?}"
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
files=$(jq -rs 'map(.path) | if length > 10 then (.[:10] | join(", ")) + " and \(length - 10) more" else join(", ") end' "$entries")
line="- ${TITLE}: ${files}"

commit=
for _ in 1 2 3; do
  branch_head=$(gh api "$repo/git/ref/heads/$BRANCH" --jq .object.sha 2>/dev/null || true)
  base=$parent
  base_tree=$(git rev-parse "$parent^{tree}")
  accumulate=false
  if [ -n "$branch_head" ] \
    && gh api "$repo/compare/$parent...$branch_head" | on_tip_of_bot_commits "$parent" "$APP_SLUG"; then
    accumulate=true
    base=$branch_head
    base_tree=$(gh api "$repo/git/commits/$base" --jq .tree.sha)
  fi

  tree=$(jq -s --arg base "$base_tree" '{base_tree: $base, tree: .}' "$entries" \
    | gh api "$repo/git/trees" --input - --jq .sha)
  if [ "$tree" = "$base_tree" ]; then
    echo "land: nothing to commit"
    exit 0
  fi
  # No author or committer: the API signs a commit only when it names neither.
  commit=$(jq -n --arg message "$TITLE" --arg tree "$tree" --arg parent "$base" \
    '{message: $message, tree: $tree, parents: [$parent]}' | gh api "$repo/git/commits" --input - --jq .sha)

  if [ -z "$branch_head" ]; then
    gh api "$repo/git/refs" -f ref="refs/heads/$BRANCH" -f sha="$commit" >/dev/null 2>&1 && break
  else
    gh api -X PATCH "$repo/git/refs/heads/$BRANCH" -f sha="$commit" \
      -F force="$([ "$accumulate" = true ] && echo false || echo true)" >/dev/null 2>&1 && break
  fi
  commit=
done
[ -n "$commit" ] || { echo "::error::land: $BRANCH kept moving, giving up"; exit 1; }

pr=$(gh pr list --head "$BRANCH" --base "$default" --state open --json number --jq '.[0].number // empty')
if [ -n "$pr" ] && [ "$accumulate" = true ]; then
  merge_body "$(gh pr view "$pr" --json body --jq .body)" "$TITLE" "$line" \
    | gh pr edit "$pr" --body-file - >/dev/null
else
  body=$line
  [ -z "${BODY:-}" ] || body="$BODY"$'\n\n'"$line"
  if [ -z "$pr" ]; then
    url=$(gh pr create --head "$BRANCH" --base "$default" --title "$TITLE" --body "$body")
    pr=${url##*/}
  else
    gh pr edit "$pr" --title "$TITLE" --body "$body" >/dev/null
  fi
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
