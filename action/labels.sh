#!/usr/bin/env bash
# Sourced. A label soothfast acts on counts only when the last person to apply
# it can write to the repository: the Triage role can label, but must not
# merge or waive a gate through soothfast-bot.
# Needs GH_TOKEN and GITHUB_REPOSITORY.

# perf_accepted PR: succeeds when PR carries soothfast:perf-accepted, applied
# by someone who can write. It waives regressions only; an error has nothing to accept.
perf_accepted() {
  if ! command -v gh >/dev/null; then
    echo "::warning::gh is not installed, so the soothfast:perf-accepted label was not checked"
    return 1
  fi
  [ "$(gh pr view "$1" --json labels --jq 'any(.labels[]; .name == "soothfast:perf-accepted")')" = true ] &&
    labelled_by_writer "$1" soothfast:perf-accepted
}

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
