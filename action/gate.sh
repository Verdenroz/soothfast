#!/usr/bin/env bash
# Gate every package against the pull request's base branch and post the
# tail of each package's output as one PR comment. Never exits non-zero on a
# regression: the caller reads the `failed` output so the comment and triage
# upload still happen first. Output is untrusted (the PR's own binaries wrote
# it) and the comment is authored by a write-access identity, so nothing in
# it may escape the code fence.
# COMMENT=false posts nothing and records one verdict result per package under
# $RUNNER_TEMP/soothfast-results, for a caller that folds them into one comment.
# Inputs: CLI PACKAGES BASE_REF GH_TOKEN GITHUB_REPOSITORY PR_NUMBER, BROKER, FEATURES,
# SAVE_BASELINE, SHARD (I/N, one comment per shard) and COMMENT (optional).
# Output: failed (true|false).
set -euo pipefail

read -ra pkgs <<<"$PACKAGES"
features=()
[ -n "${FEATURES:-}" ] && features=(--features "$FEATURES")
save=()
[ -n "${SAVE_BASELINE:-}" ] && save=(--save-baseline "$SAVE_BASELINE")
shard=()
marker='<!-- soothfast-gate -->'
title='## soothfast gate'
if [ -n "${SHARD:-}" ]; then
  shard=(--shard "$SHARD")
  marker="<!-- soothfast-gate-shard-${SHARD%/*}-of-${SHARD#*/} -->"
  title="## soothfast gate (shard ${SHARD})"
fi
regressed=false
errored=false
out_dir="${RUNNER_TEMP:-/tmp}/soothfast-gate"
mkdir -p "$out_dir"
{
  echo "$title"
  for pkg in "${pkgs[@]}"; do
    out="${out_dir}/${pkg}.txt"
    rc=0
    SECONDS=0
    "$CLI" gate -p "$pkg" --against-ref "origin/${BASE_REF}" "${features[@]}" "${save[@]}" "${shard[@]}" 2>&1 | tee "$out" >&2 || rc=$?
    if [ "${COMMENT:-true}" = false ]; then
      name=$pkg
      [ -n "${SHARD:-}" ] && name="${pkg}#${SHARD}"
      "$CLI" verdict record --name "$name" --kind rust --exit "$rc" --output "$out" \
        --seconds "$SECONDS" --out "${RUNNER_TEMP}/soothfast-results/${pkg}.json"
    fi
    case "$rc" in
    0) ;;
    1) regressed=true ;;
    *) errored=true ;;
    esac
    echo "### ${pkg}"
    echo '```'
    # shellcheck disable=SC2016 # literal backticks, nothing to expand
    tail -n 40 "$out" | sed 's/```/` ` `/g'
    echo '```'
  done
} >"${out_dir}/comment.md"

# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"
if [ "$regressed" = true ] && [ "$errored" = false ] && perf_accepted "$PR_NUMBER"; then
  echo "::notice::regressions accepted by the soothfast:perf-accepted label"
  # shellcheck disable=SC2016 # literal backticks, nothing to expand
  echo 'Regressions accepted by the `soothfast:perf-accepted` label.' >>"${out_dir}/comment.md"
  regressed=false
fi
failed=false
if [ "$regressed" = true ] || [ "$errored" = true ]; then
  failed=true
fi

if [ "${COMMENT:-true}" != false ]; then
  MARKER="$marker" BODY_FILE="${out_dir}/comment.md" "$(dirname "$0")/comment.sh" ||
    echo "::warning::could not comment on the pull request (read-only token on a fork?)"
fi

echo "failed=$failed" >>"$GITHUB_OUTPUT"
