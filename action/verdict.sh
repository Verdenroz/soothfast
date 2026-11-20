#!/usr/bin/env bash
# Fold every gate job's result into the pull request's one comment and exit
# status. Results are untrusted text; `ci verdict` neutralises the fences.
# Inputs: CLI RESULTS PR_NUMBER BASE_REF GH_TOKEN GITHUB_REPOSITORY, BROKER and
# EXPECT_RUST, EXPECT_IMPORT, EXPECT_DOCS, EXPECT_TEST, EXPECT_CHECK
# (space-separated names of the results each kind must have) and EXPECT_SHARDS
# (I/N of each Rust gate leg, which expects every package once per leg)
# (optional), LANDING (true when the plan ran no lane for a bot landing).
# A body that could not be posted is staged in $RUNNER_TEMP/soothfast-verdict-comment
# for the workflow to upload, which is how a fork's verdict reaches comment.yml.
set -euo pipefail

# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"

"$(dirname "$0")/latest-attempt.sh" "$RESULTS"

expect=()
read -ra shards <<<"${EXPECT_SHARDS:-}"
for kind in rust import docs test check; do
  names=EXPECT_${kind^^}
  for name in ${!names:-}; do
    if [ "$kind" = rust ] && [ "${#shards[@]}" -gt 0 ]; then
      for shard in "${shards[@]}"; do expect+=(--expect "${name}#${shard}:${kind}"); done
    else
      expect+=(--expect "${name}:${kind}")
    fi
  done
done
accepted=()
mapfile -t waivers < <(perf_accepted_names)
if perf_accepted "$PR_NUMBER" "${waivers[@]}"; then
  accepted=(--accepted "$accepted_label")
fi

landing=()
[ "${LANDING:-}" != true ] || landing=(--landing)

body="${RUNNER_TEMP:-/tmp}/soothfast-verdict.md"
rc=0
"$CLI" ci --results "$RESULTS" verdict --out "$body" "${expect[@]}" "${accepted[@]}" "${landing[@]}" || rc=$?
[ "$rc" -le 1 ] || exit "$rc"

cat "$body"
[ -z "${GITHUB_STEP_SUMMARY:-}" ] || cat "$body" >>"$GITHUB_STEP_SUMMARY"
if ! MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" "$(dirname "$0")/comment.sh"; then
  echo "::warning::could not comment on the pull request (read-only token on a fork?)"
  staged="${RUNNER_TEMP:-/tmp}/soothfast-verdict-comment"
  mkdir -p "$staged"
  cp "$body" "$staged/body.md"
  echo "$PR_NUMBER" >"$staged/pr-number"
fi
exit "$rc"
