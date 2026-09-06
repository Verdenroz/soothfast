#!/usr/bin/env bash
# Regenerate derived files on the default branch: a fresh baseline for every
# package, then CHANGELOG.md against the latest tag and any generate-mode
# specs.
# Inputs: CLI PACKAGES BASELINE CHANGELOG (true|false) SPEC (package list).
# Outputs: paths (pathspecs for land.sh), changed (true when any of them
# differs from HEAD, so a no-op run never mints a token).
set -euo pipefail

read -ra pkgs <<<"$PACKAGES"
paths=()

if [ "$CHANGELOG" = true ]; then
  for pkg in "${pkgs[@]}"; do
    "$CLI" measure -p "$pkg" --save-baseline "$BASELINE"
  done
  args=()
  for pkg in "${pkgs[@]}"; do args+=(-p "$pkg"); done
  prev=$(git describe --tags --abbrev=0 2>/dev/null || true)
  if [ -n "$prev" ]; then args+=(--against-ref "$prev"); fi
  "$CLI" report changelog "${args[@]}" --baseline "$BASELINE"
  paths+=(CHANGELOG.md)
fi

if [ -n "$SPEC" ]; then
  read -ra specs <<<"$SPEC"
  for pkg in "${specs[@]}"; do
    "$CLI" spec gen -p "$pkg"
  done
  paths+=('*.yaml' '*.yml' '*.json')
fi

changed=false
if [ "${#paths[@]}" -gt 0 ] && [ -n "$(git status --porcelain -- "${paths[@]}")" ]; then
  changed=true
fi
{
  echo "paths=${paths[*]:-}"
  echo "changed=$changed"
} >>"$GITHUB_OUTPUT"
