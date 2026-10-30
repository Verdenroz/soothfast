#!/usr/bin/env bash
# One checksum over the names and sizes of the files under a directory, so
# two listings differ when a file was added, replaced or removed and not when
# one was only read. Prints "none" for a directory that does not exist.
# Inputs: the directory.
set -euo pipefail

dir=$1
if [ ! -d "$dir" ]; then
  echo none
  exit 0
fi
find "$dir" -type f ! -name '*.tmp' | sort | while read -r file; do
  printf '%s %s\n' "$file" "$(wc -c <"$file")"
done | cksum | cut -d' ' -f1
