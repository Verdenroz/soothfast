#!/usr/bin/env bash
# Gate HEAD against its parent on a default-branch push, which stores HEAD's
# run in .soothfast/runs for a later pull request whose merge-base is this
# commit, or builds the same bench binary. Never fails: the code is already
# merged, and the regeneration after this step must still run.
# Inputs: CLI PACKAGES, FEATURES (optional).
set -euo pipefail

if ! git rev-parse --verify --quiet 'HEAD^' >/dev/null; then
  echo "::warning::HEAD has no parent to gate against; no reference run recorded"
  exit 0
fi
read -ra pkgs <<<"$PACKAGES"
features=()
[ -n "${FEATURES:-}" ] && features=(--features "$FEATURES")
for pkg in "${pkgs[@]}"; do
  status=0
  "$CLI" gate -p "$pkg" --against-ref 'HEAD^' "${features[@]}" || status=$?
  if [ "$status" -ne 0 ]; then
    echo "::warning::soothfast gate for ${pkg} against HEAD^ exited ${status}; a regression still records the run, an error may not"
  fi
done
