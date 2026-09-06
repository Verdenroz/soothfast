#!/usr/bin/env bash
# Regenerate derived files on the default branch: a fresh baseline for every
# package, then CHANGELOG.md against the latest tag, any generate-mode specs,
# and whatever REGEN_RUN produces under REGEN_PATHS.
# Inputs: CLI PACKAGES BASELINE CHANGELOG (true|false) SPEC (package list),
# FEATURES, CHANGELOG_FEATURES, CHANGELOG_PACKAGES, REGEN_RUN, REGEN_PATHS
# (all optional).
# Outputs: paths (pathspecs for land.sh), changed (true when any of them
# differs from HEAD, so a no-op run never mints a token).
set -euo pipefail

read -ra pkgs <<<"$PACKAGES"
paths=()
features=()
[ -n "${FEATURES:-}" ] && features=(--features "$FEATURES")
changelog_features=("${features[@]}")
[ -n "${CHANGELOG_FEATURES:-}" ] && changelog_features=(--features "$CHANGELOG_FEATURES")

if [ "$CHANGELOG" = true ]; then
  for pkg in "${pkgs[@]}"; do
    "$CLI" measure -p "$pkg" --save-baseline "$BASELINE" "${features[@]}"
  done
  read -ra changelog_pkgs <<<"${CHANGELOG_PACKAGES:-$PACKAGES}"
  args=()
  for pkg in "${changelog_pkgs[@]}"; do args+=(-p "$pkg"); done
  prev=$(git describe --tags --abbrev=0 2>/dev/null || true)
  if [ -n "$prev" ]; then args+=(--against-ref "$prev"); fi
  "$CLI" report changelog "${args[@]}" --baseline "$BASELINE" "${changelog_features[@]}"
  paths+=(CHANGELOG.md)
fi

if [ -n "$SPEC" ]; then
  read -ra specs <<<"$SPEC"
  for pkg in "${specs[@]}"; do
    "$CLI" spec gen -p "$pkg" "${features[@]}"
  done
  paths+=('*.yaml' '*.yml' '*.json')
fi

if [ -n "${REGEN_RUN:-}" ]; then
  if [ -z "${REGEN_PATHS:-}" ]; then
    echo "::error::regen-run needs regen-paths, or nothing it changes can land"
    exit 1
  fi
  SOOTHFAST="$CLI" bash -euo pipefail -c "$REGEN_RUN"
  read -ra extra <<<"$REGEN_PATHS"
  paths+=("${extra[@]}")
fi

changed=false
if [ "${#paths[@]}" -gt 0 ] && [ -n "$(git status --porcelain -- "${paths[@]}")" ]; then
  changed=true
fi
{
  echo "paths=${paths[*]:-}"
  echo "changed=$changed"
} >>"$GITHUB_OUTPUT"
