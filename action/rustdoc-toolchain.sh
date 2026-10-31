#!/usr/bin/env bash
# Install the toolchain that builds rustdoc JSON and export it for later steps,
# so a job reads and writes the same rustdoc store as the build job.
# Inputs: RUSTDOC_TOOLCHAIN (defaults to the action's `rustdoc-toolchain`).
set -euo pipefail

toolchain=${RUSTDOC_TOOLCHAIN:-$(sed -n '/^  rustdoc-toolchain:/,/^  token:/s/^    default: //p' "$(dirname "$0")/../action.yml")}
rustup toolchain install "$toolchain" --profile minimal
echo "SOOTHFAST_RUSTDOC_TOOLCHAIN=$toolchain" >>"$GITHUB_ENV"
