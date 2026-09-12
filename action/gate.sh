#!/usr/bin/env bash
# Gate every package against the pull request's base branch and post the
# tail of each package's output as one PR comment. Never exits non-zero on a
# regression: the caller reads the `failed` output so the comment and triage
# upload still happen first. Output is untrusted (the PR's own binaries wrote
# it) and the comment is authored by a write-access identity, so nothing in
# it may escape the code fence.
# Inputs: CLI PACKAGES BASE_REF GH_TOKEN PR_NUMBER, BROKER, FEATURES and BIND
# (optional).
# Output: failed (true|false).
set -euo pipefail

read -ra pkgs <<<"$PACKAGES"
read -ra binds <<<"${BIND:-}"
features=()
[ -n "${FEATURES:-}" ] && features=(--features "$FEATURES")
failed=false
out_dir="${RUNNER_TEMP:-/tmp}/soothfast-gate"
mkdir -p "$out_dir"
{
  echo '## soothfast gate'
  for pkg in "${pkgs[@]}"; do
    out="${out_dir}/${pkg}.txt"
    "$CLI" gate -p "$pkg" --against-ref "origin/${BASE_REF}" "${features[@]}" 2>&1 | tee "$out" >&2 || failed=true
    echo "### ${pkg}"
    echo '```'
    # shellcheck disable=SC2016 # literal backticks, nothing to expand
    tail -n 40 "$out" | sed 's/```/` ` `/g'
    echo '```'
  done
  for pkg in "${binds[@]}"; do
    out="${out_dir}/bind-${pkg}.txt"
    "$CLI" bind gate -p "$pkg" --base "origin/${BASE_REF}" 2>&1 | tee "$out" >&2 || failed=true
    echo "### bind: ${pkg}"
    echo '```'
    # shellcheck disable=SC2016 # literal backticks, nothing to expand
    tail -n 40 "$out" | sed 's/```/` ` `/g'
    echo '```'
  done
} >"${out_dir}/comment.md"

MARKER='<!-- soothfast-gate -->' BODY_FILE="${out_dir}/comment.md" "$(dirname "$0")/comment.sh" ||
  echo "::warning::could not comment on the pull request (read-only token on a fork?)"

echo "failed=$failed" >>"$GITHUB_OUTPUT"
