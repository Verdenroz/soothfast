#!/usr/bin/env bash
# The toolchain that builds rustdoc JSON: `--print` names the one pinned in
# action.yml (the only parser of it, also used by the Makefile); otherwise
# install it and export it for later steps, so a job reads and writes the same
# rustdoc store as the build job.
# Inputs: RUSTDOC_TOOLCHAIN (defaults to the pin), GITHUB_ENV when installing.
set -euo pipefail

pinned() {
  sed -n 's/^ *default: \(nightly-[0-9-]*\)$/\1/p' "$(dirname "$0")/../action.yml"
}

if [ "${1:-}" = --print ]; then
  pinned
  exit 0
fi
toolchain=${RUSTDOC_TOOLCHAIN:-$(pinned)}
rustup toolchain install "$toolchain" --profile minimal
echo "SOOTHFAST_RUSTDOC_TOOLCHAIN=$toolchain" >>"$GITHUB_ENV"
