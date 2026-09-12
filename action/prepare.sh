#!/usr/bin/env bash
# Work out what this run will do and fetch what it needs: the default-branch
# check, the package list, full history for merge-base and tag lookups, the
# callgrind fallback, and the rustdoc toolchain when anything regenerates.
# Inputs: GATE REGEN (true|false) PACKAGES RUSTDOC_TOOLCHAIN GH_TOKEN, BIND
# (optional).
# Outputs: on_default_branch, packages.
set -euo pipefail

default=$(gh api "repos/${GITHUB_REPOSITORY}" --jq .default_branch)
on_default=false
[ "${GITHUB_REF:-}" = "refs/heads/${default}" ] && on_default=true
echo "on_default_branch=$on_default" >>"$GITHUB_OUTPUT"

gating=false
[ "${GITHUB_EVENT_NAME:-}" = pull_request ] && [ "$GATE" = true ] && gating=true
regenerating=false
[ "${GITHUB_EVENT_NAME:-}" != pull_request ] && [ "$on_default" = true ] && [ "$REGEN" = true ] && regenerating=true
if [ "$gating" = false ] && [ "$regenerating" = false ]; then
  echo "packages=" >>"$GITHUB_OUTPUT"
  exit 0
fi

if [ -z "$PACKAGES" ]; then
  PACKAGES=$(cargo metadata --format-version 1 --no-deps |
    jq -r '[.packages[] | select(any(.targets[]; .kind == ["bench"] and .name == "soothfast")) | .name] | join(" ")')
fi
if [ -z "$PACKAGES" ]; then
  echo "::error::no package has a bench target named soothfast; pass the packages input"
  exit 1
fi
echo "packages=$PACKAGES" >>"$GITHUB_OUTPUT"

if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
  git fetch --quiet --unshallow --tags
fi
if [ "$gating" = true ] && [ -n "${GITHUB_BASE_REF:-}" ]; then
  git fetch --quiet origin "+refs/heads/${GITHUB_BASE_REF}:refs/remotes/origin/${GITHUB_BASE_REF}"
fi

if [ "$(uname -s)" = Linux ] && ! command -v valgrind >/dev/null && command -v apt-get >/dev/null; then
  sudo apt-get update -qq && sudo apt-get install -y -qq valgrind
fi

# bind gate reads rustdoc JSON on pull requests too, not just on regen.
if [ "$regenerating" = true ] || { [ "$gating" = true ] && [ -n "${BIND:-}" ]; }; then
  rustup toolchain install "$RUSTDOC_TOOLCHAIN" --profile minimal
  echo "SOOTHFAST_RUSTDOC_TOOLCHAIN=$RUSTDOC_TOOLCHAIN" >>"$GITHUB_ENV"
fi
