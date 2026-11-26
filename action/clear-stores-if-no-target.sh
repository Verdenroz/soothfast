#!/usr/bin/env bash
# Says whether the cargo target directory is empty, and when it is, empties
# the soothfast stores that let a job skip compiling. A job whose stores all
# hit compiles nothing, and rust-cache saves whatever target it finds under a
# key that no later run saves again. The caller runs its build once more with
# nothing to hit, so the entry it saves holds a real target.
# Inputs: none. Output: empty=true|false.
set -euo pipefail

if compgen -G "${CARGO_TARGET_DIR:-target}/*/deps/*" >/dev/null; then
  echo "empty=false" >>"$GITHUB_OUTPUT"
  exit 0
fi
rm -rf .soothfast/bins .soothfast/rustdoc .soothfast/items .soothfast/runs .soothfast/maps
echo "empty=true" >>"$GITHUB_OUTPUT"
