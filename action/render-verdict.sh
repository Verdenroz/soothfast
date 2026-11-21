#!/usr/bin/env bash
# Render the verdict comment body from the result files under a directory.
# Results are untrusted text; `ci verdict` neutralises the fences. Exits 0 for
# a passing verdict, 1 for a failing one and anything else when it could not
# render.
# Inputs: CLI RESULTS OUT PR_NUMBER GH_TOKEN GITHUB_REPOSITORY, BASE_REF or
# LABELS_REF (where labels.sh reads the waiver label names), and optionally
# GH_REPO (to run outside a checkout), EXPECT_RUST, EXPECT_IMPORT,
# EXPECT_DOCS, EXPECT_TEST, EXPECT_CHECK (space-separated names of the results
# each kind must have), EXPECT_SHARDS (I/N of each Rust gate leg, which
# expects every package once per leg) and LANDING (true when the plan ran no
# lane for a bot landing).
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

exec "$CLI" ci --results "$RESULTS" verdict --out "$OUT" "${expect[@]}" "${accepted[@]}" "${landing[@]}"
