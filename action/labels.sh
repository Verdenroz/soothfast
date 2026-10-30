#!/usr/bin/env bash
# Sourced. A label soothfast acts on counts only when the last person to apply
# it can write to the repository: the Triage role can label, but must not
# merge or waive a gate through soothfast-bot.
# Needs GH_TOKEN and GITHUB_REPOSITORY.

# labelled_by_writer PR LABEL: succeeds when whoever last applied LABEL to PR
# has write, maintain or admin permission. The caller checks LABEL is on PR.
labelled_by_writer() {
  local pr=$1 label=$2 actor permission
  actor=$(gh api --paginate "repos/${GITHUB_REPOSITORY}/issues/${pr}/events" \
    --jq ".[] | select(.event == \"labeled\" and .label.name == \"${label}\") | .actor.login" | tail -n 1)
  [ -n "$actor" ] || return 1
  permission=$(gh api "repos/${GITHUB_REPOSITORY}/collaborators/${actor}/permission" --jq .permission) || return 1
  case "$permission" in
  admin | maintain | write) return 0 ;;
  *)
    echo "::warning::#${pr}: ignoring ${label}, applied by ${actor} (${permission}), who cannot write here"
    return 1
    ;;
  esac
}
