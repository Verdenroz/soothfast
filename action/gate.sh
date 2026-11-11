#!/usr/bin/env bash
# Run `cargo soothfast ci gate` and do what it leaves to the workflow: decide
# whether a writer's label waives regressions, and post the comment it wrote.
# A pull request is gated against its base and a default-branch push records
# HEAD against its parent. Exits as `ci gate` does: 1 on a regression no label
# waives, 2 on an error.
# COMMENT=false posts nothing; the verdict records under
# $RUNNER_TEMP/soothfast-results are for a caller that folds them into one
# comment.
# Inputs: CLI PACKAGES, and on a pull request GH_TOKEN GITHUB_REPOSITORY
# PR_NUMBER. Optional: BROKER, FEATURES, SAVE_BASELINE, SHARD (I/N, one comment
# per shard), COMMENT, GATE_MARKER (the comment's marker line) and AGAINST_REF
# (gate against this ref whatever the event).
set -euo pipefail

args=()
read -ra pkgs <<<"$PACKAGES"
for pkg in "${pkgs[@]}"; do args+=(-p "$pkg"); done
[ -n "${FEATURES:-}" ] && args+=(--features "$FEATURES")
[ -n "${SAVE_BASELINE:-}" ] && args+=(--save-baseline "$SAVE_BASELINE")
marker='<!-- soothfast-gate -->'
if [ -n "${SHARD:-}" ]; then
  args+=(--shard "$SHARD")
  marker="<!-- soothfast-gate-shard-${SHARD%/*}-of-${SHARD#*/} -->"
fi
[ -n "${AGAINST_REF:-}" ] && args+=(--against "$AGAINST_REF")

pull=false
[ "${GITHUB_EVENT_NAME:-}" = pull_request ] && pull=true
if [ "$pull" = true ]; then
  # shellcheck source=action/labels.sh
  source "$(dirname "$0")/labels.sh"
  BASE_REF="$GITHUB_BASE_REF"
  mapfile -t waivers < <(perf_accepted_names)
  if perf_accepted "$PR_NUMBER" "${waivers[@]}"; then
    args+=(--accepted "$accepted_label")
  fi
fi

rc=0
"$CLI" ci gate "${args[@]}" || rc=$?

body="$RUNNER_TEMP/soothfast-results/gate-comment.md"
if [ "$pull" = true ] && [ "${COMMENT:-true}" != false ] && [ -f "$body" ]; then
  MARKER="${GATE_MARKER:-$marker}" BODY_FILE="$body" "$(dirname "$0")/comment.sh" ||
    echo "::warning::could not comment on the pull request (read-only token on a fork?)"
fi
exit "$rc"
