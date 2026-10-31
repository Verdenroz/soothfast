#!/usr/bin/env bash
# Build one lane of the stores a later job reads: the bench executables of the
# gated packages and of the merge-base, the routes executables of the spec
# packages, or rustdoc JSON. The merge-base is the pull request's base, or the
# parent of a default-branch push, as the gate compares them.
# Inputs: CLI LANE PACKAGES BASE_REF EVENT ON_DEFAULT, and FEATURES, DOCS and
# SPEC (space-separated packages) (optional).
set -euo pipefail

args=(--only "$LANE")
read -ra packages <<<"${PACKAGES:-}"
for pkg in "${packages[@]}"; do args+=(-p "$pkg"); done
read -ra docs <<<"${DOCS:-}"
for pkg in "${docs[@]}"; do args+=(--docs "$pkg"); done
read -ra specs <<<"${SPEC:-}"
for pkg in "${specs[@]}"; do args+=(--spec "$pkg"); done
[ -n "${FEATURES:-}" ] && args+=(--features "$FEATURES")
if [ "$EVENT" = pull_request ]; then
  args+=(--against-ref "origin/${BASE_REF}")
elif [ "$EVENT" = push ] && [ "$ON_DEFAULT" = true ] && git rev-parse --verify --quiet 'HEAD^' >/dev/null; then
  args+=(--against-ref 'HEAD^')
fi
"$CLI" prebuild "${args[@]}"
