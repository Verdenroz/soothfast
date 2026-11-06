#!/usr/bin/env bash
# Sourced. A label soothfast acts on counts only when the last person to apply
# it can write to the repository: the Triage role can label, but must not
# merge or waive a gate through soothfast-bot.
# Needs GH_TOKEN and GITHUB_REPOSITORY.

# perf_accepted_names: the labels that waive regressions, one per line, from
# soothfast.toml on the base branch. A pull request can edit its own copy, so
# its checkout never decides which label waives. Falls back to the default
# name, with a warning, when the CLI cannot read the table.
# Needs CLI and BASE_REF; LABELS_DIR is where to run it (default: here).
perf_accepted_names() {
  (cd "${LABELS_DIR:-.}" && "$CLI" labels --ref "origin/${BASE_REF}" --role perf-accepted 2>/dev/null) && return
  echo "::warning::could not read perf_accepted_labels from origin/${BASE_REF}, so only soothfast:perf-accepted waives regressions" >&2
  echo soothfast:perf-accepted
}

# perf_accepted PR NAME...: succeeds when PR carries one of NAME, applied by
# someone who can write, and sets accepted_label to it. It waives regressions
# only; an error has nothing to accept.
perf_accepted() {
  local pr=$1 name
  shift
  if ! command -v gh >/dev/null; then
    echo "::warning::gh is not installed, so the perf-accepted label was not checked"
    return 1
  fi
  local -a on_pr
  mapfile -t on_pr < <(gh pr view "$pr" --json labels --jq '.labels[].name')
  for accepted_label in "$@"; do
    for name in "${on_pr[@]}"; do
      if [ "$name" = "$accepted_label" ] && labelled_by_writer "$pr" "$accepted_label"; then
        return 0
      fi
    done
  done
  return 1
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
