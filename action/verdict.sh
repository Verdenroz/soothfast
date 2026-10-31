#!/usr/bin/env bash
# Fold every gate job's result into the pull request's one comment and exit
# status. Results are untrusted text; `verdict merge` neutralises the fences.
# Inputs: CLI RESULTS PR_NUMBER GH_TOKEN GITHUB_REPOSITORY, BROKER and
# EXPECT_RUST, EXPECT_IMPORT, EXPECT_DOCS, EXPECT_TEST (space-separated names
# of the results each kind must have) and EXPECT_SHARDS (I/N of each Rust
# gate leg, which expects every package once per leg) (optional).
set -euo pipefail

# shellcheck source=action/labels.sh
source "$(dirname "$0")/labels.sh"

expect=()
read -ra shards <<<"${EXPECT_SHARDS:-}"
for kind in rust import docs test; do
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
if perf_accepted "$PR_NUMBER"; then
  accepted=(--accepted)
fi

body="${RUNNER_TEMP:-/tmp}/soothfast-verdict.md"
rc=0
"$CLI" verdict merge "$RESULTS" --out "$body" "${expect[@]}" "${accepted[@]}" || rc=$?
[ "$rc" -le 1 ] || exit "$rc"

cat "$body"
MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" "$(dirname "$0")/comment.sh" ||
  echo "::warning::could not comment on the pull request (read-only token on a fork?)"
exit "$rc"
