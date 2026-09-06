#!/usr/bin/env bash
# Exchange this job's GitHub Actions OIDC identity for a one-hour
# soothfast-bot installation token scoped to this repository.
# Inputs: BROKER (URL, optional). Outputs: token, app_slug, expires_at.
set -euo pipefail

# shellcheck source=action/oidc.sh
source "$(dirname "$0")/oidc.sh"

fail() {
  echo "::error::$1"
  exit 1
}

oidc=$(oidc_token) || fail "soothfast-bot needs 'id-token: write' in the job's permissions"

response=$(curl -sS --max-time 30 -w '\n%{http_code}' -X POST "$BROKER/token" -H "Authorization: Bearer $oidc") ||
  fail "could not reach the broker at $BROKER"
status=${response##*$'\n'}
body=${response%$'\n'*}

if [ "$status" != 200 ]; then
  fail "soothfast-bot refused (HTTP $status): $(jq -r '.reason // .' <<<"$body")"
fi

token=$(jq -er .token <<<"$body") || fail "soothfast-bot returned no token"
echo "::add-mask::$token"
{
  echo "token=$token"
  echo "app_slug=$(jq -r .app_slug <<<"$body")"
  echo "expires_at=$(jq -r .expires_at <<<"$body")"
} >>"$GITHUB_OUTPUT"
