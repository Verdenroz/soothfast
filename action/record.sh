#!/usr/bin/env bash
# Gate HEAD against its parent on a default-branch push, which stores HEAD's
# run in .soothfast/runs for a later pull request whose merge-base is this
# commit, or builds the same bench binary. Never fails: the code is already
# merged, and the regeneration after this step must still run.
# A HEAD with no parent lists each package's items as `no-parent` in the
# baseline instead, so the docs check names why its claims were not checked.
# Inputs: CLI PACKAGES, FEATURES and SAVE_BASELINE (optional).
set -euo pipefail

read -ra pkgs <<<"$PACKAGES"
features=()
[ -n "${FEATURES:-}" ] && features=(--features "$FEATURES")
if ! git rev-parse --verify --quiet 'HEAD^' >/dev/null; then
  echo "::warning::HEAD has no parent to gate against; no reference run recorded"
  if [ -n "${SAVE_BASELINE:-}" ]; then
    for pkg in "${pkgs[@]}"; do
      "$CLI" baseline mark --into "$SAVE_BASELINE" -p "$pkg" --reason no-parent \
        --detail "HEAD has no parent to gate ${pkg} against" "${features[@]}" ||
        echo "::warning::could not list ${pkg}'s items, so its claims will fail rather than read not evaluated"
    done
  fi
  exit 0
fi
save=()
[ -n "${SAVE_BASELINE:-}" ] && save=(--save-baseline "$SAVE_BASELINE")
for pkg in "${pkgs[@]}"; do
  status=0
  SOOTHFAST_RECORD=1 "$CLI" gate -p "$pkg" --against-ref 'HEAD^' "${features[@]}" "${save[@]}" || status=$?
  if [ "$status" -ne 0 ]; then
    echo "::warning::soothfast gate for ${pkg} against HEAD^ exited ${status}; its items are listed gate-failed"
  fi
done
