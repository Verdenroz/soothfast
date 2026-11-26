#!/usr/bin/env bash
# Sets SOOTHFAST_NO_TARGET for the rest of the job when the cargo target
# directory holds no build output. A job whose stores all hit compiles
# nothing, and rust-cache saves whatever target it finds under a key that no
# later run saves again, so its save-if reads this to skip that entry.
# Inputs: none.
set -euo pipefail

if ! compgen -G "${CARGO_TARGET_DIR:-target}/*/deps/*" >/dev/null; then
  echo "SOOTHFAST_NO_TARGET=1" >>"$GITHUB_ENV"
fi
