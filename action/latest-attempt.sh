#!/usr/bin/env bash
# Of the artifact directories under a directory named NAME-attempt-N, keep
# the highest N of each NAME. A re-run job uploads its artifact again under
# the next attempt and a later job downloads both, so the stale one has to go
# before anything reads them.
# Inputs: the directory.
set -euo pipefail

dir=$1
[ -d "$dir" ] || exit 0
shopt -s nullglob
for path in "$dir"/*-attempt-*; do
  [ -d "$path" ] || continue
  base=${path%-attempt-*}
  attempt=${path##*-attempt-}
  for other in "$base"-attempt-*; do
    if [ "${other##*-attempt-}" -gt "$attempt" ]; then
      rm -rf "$path"
      break
    fi
  done
done
