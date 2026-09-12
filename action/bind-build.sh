#!/usr/bin/env bash
# Build one package's [[bind]] entries. Independent of gate and regen: a
# matrix leg calls the action with only bind-build/bind-target set.
# Inputs: CLI PKG, TARGET (optional).
set -euo pipefail

args=(-p "$PKG")
[ -n "${TARGET:-}" ] && args+=(--target "$TARGET")
"$CLI" bind build "${args[@]}"
