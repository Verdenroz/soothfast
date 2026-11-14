#!/usr/bin/env bash
# Fetch the trend series from the published site into .soothfast/trend.jsonl,
# then append this commit's point from the `head` baseline. A first run (404)
# keeps any working copy the repository tracks. Any other failure stops the
# job so the site is not published without its history.
# Inputs: TREND_URL (the site's https:// URL) CLI (the cargo-soothfast binary).
set -euo pipefail

: "${TREND_URL:?}" "${CLI:?}"
series=.soothfast/trend.jsonl
body=$(mktemp)
trap 'rm -f "$body"' EXIT

status=$(curl -sSL --retry 3 --retry-all-errors -o "$body" -w '%{http_code}' "${TREND_URL%/}/trend.jsonl")
case "$status" in
  200)
    if [ "$(head -c 1 "$body")" != "{" ]; then
      echo "::error::${TREND_URL%/}/trend.jsonl is not a JSON lines series; check [ci] trend_url"
      exit 1
    fi
    mkdir -p .soothfast
    cp "$body" "$series"
    ;;
  404)
    echo "::notice::no trend series at ${TREND_URL%/}/trend.jsonl; it starts with this commit"
    ;;
  *)
    echo "::error::fetching the trend series returned HTTP $status"
    exit 1
    ;;
esac

if [ -f .soothfast/baselines/head.json ]; then
  "$CLI" trend append --from-baseline head
else
  echo "::notice::no head baseline, so no trend point was appended"
fi
