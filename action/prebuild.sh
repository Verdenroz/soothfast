#!/usr/bin/env bash
# Build lanes of the stores a later job reads, one `cargo soothfast ci build`
# per lane. The bench lane takes PACKAGES and the rustdoc lane DOCS; a lane
# given none builds what soothfast.toml configures for it. The first failing
# lane's exit code is the result, after the rest have run.
# Inputs: CLI LANES (space-separated). Optional: PACKAGES and DOCS
# (space-separated packages), STAGE, SHARDS.
set -euo pipefail

common=()
[ -n "${STAGE:-}" ] && common+=(--stage "$STAGE")
[ -n "${SHARDS:-}" ] && common+=(--shards "$SHARDS")
rc=0
read -ra chosen <<<"$LANES"
for lane in "${chosen[@]}"; do
  flag=-p
  packages=()
  case "$lane" in
  bench) read -ra packages <<<"${PACKAGES:-}" ;;
  rustdoc)
    flag=--docs
    read -ra packages <<<"${DOCS:-}"
    ;;
  esac
  named=()
  for pkg in "${packages[@]}"; do named+=("$flag" "$pkg"); done
  status=0
  "$CLI" ci build "$lane" "${common[@]}" "${named[@]}" || status=$?
  [ "$rc" -ne 0 ] || rc=$status
done
exit "$rc"
