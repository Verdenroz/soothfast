#!/usr/bin/env bash
# Whether GitHub created and signed every commit on the pull request for
# AUTHOR, the way land.sh commits through the API. A push cannot produce
# that, whatever author it names. An API error, a full page of commits or a
# head other than HEAD_SHA is false.
# Inputs: PR_NUMBER, AUTHOR, HEAD_SHA, GH_TOKEN, GITHUB_REPOSITORY.
# Output: verified (true or false).
set -euo pipefail

page=100
verified=false
if commits=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${PR_NUMBER}/commits?per_page=${page}"); then
  verified=$(jq --arg author "$AUTHOR" --arg head "$HEAD_SHA" --argjson page "$page" '
    length > 0 and length < $page and .[-1].sha == $head
    and all(.[]; .author.login == $author and .committer.login == "web-flow"
      and .commit.verification.verified == true)' <<<"$commits") || verified=false
fi
echo "verified=$verified" >>"$GITHUB_OUTPUT"
