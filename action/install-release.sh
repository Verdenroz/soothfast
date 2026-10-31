#!/usr/bin/env bash
# Download the released cargo-soothfast for this runner straight from the
# GitHub release and check it against the release's SHA256SUMS. No API call is
# involved. On any failure it warns and reports direct=false so the caller
# falls back to cargo-binstall; nothing unverified is ever extracted.
# Inputs: VERSION BIN (the path the binary must end up at).
# Output: direct (true|false).
set -euo pipefail

repo=https://github.com/Verdenroz/soothfast

fall_back() {
  echo "::warning::could not install the prebuilt cargo-soothfast v${VERSION} directly (${1}); falling back to cargo-binstall, which may compile it from source"
  echo "direct=false" >>"$GITHUB_OUTPUT"
  exit 0
}

case "${RUNNER_OS}-${RUNNER_ARCH}" in
  Linux-X64) target=x86_64-unknown-linux-gnu ;;
  Linux-ARM64) target=aarch64-unknown-linux-gnu ;;
  macOS-X64) target=x86_64-apple-darwin ;;
  macOS-ARM64) target=aarch64-apple-darwin ;;
  Windows-X64) target=x86_64-pc-windows-msvc ;;
  *) fall_back "no prebuilt binary for ${RUNNER_OS} ${RUNNER_ARCH}" ;;
esac

asset="cargo-soothfast-v${VERSION}-${target}.tgz"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
  curl --fail --silent --show-error --location --retry 3 --retry-all-errors \
    --connect-timeout 10 --output "$work/$1" "${repo}/releases/download/v${VERSION}/$1"
}

fetch SHA256SUMS || fall_back "SHA256SUMS is not available for this release"
fetch "$asset" || fall_back "downloading ${asset} failed"

if command -v sha256sum >/dev/null; then
  sha=(sha256sum)
else
  sha=(shasum -a 256)
fi
grep -F "  ${asset}" "$work/SHA256SUMS" >"$work/line" || fall_back "SHA256SUMS has no entry for ${asset}"
(cd "$work" && "${sha[@]}" -c line) || fall_back "checksum mismatch for ${asset}"

dir=$(dirname "$BIN")
mkdir -p "$dir"
tar -C "$dir" -xzf "$work/$asset"
echo "direct=true" >>"$GITHUB_OUTPUT"
