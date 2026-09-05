#!/usr/bin/env bash
# Exchange this job's GitHub Actions OIDC identity for a one-hour
# soothfast-bot installation token scoped to this repository.
# Inputs: BROKER (URL). Outputs: token, app_slug, expires_at.
set -euo pipefail

: "${BROKER:?BROKER is required}"

if [ -z "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ]; then
  echo "::error::soothfast-bot needs 'id-token: write' in the job's permissions"
  exit 1
fi

oidc=$(curl -sSf -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=soothfast-bot" | jq -r .value)

response=$(curl -sS -w '\n%{http_code}' -X POST "$BROKER/token" -H "Authorization: Bearer $oidc")
status=${response##*$'\n'}
body=${response%$'\n'*}

if [ "$status" != 200 ]; then
  echo "::error::soothfast-bot refused (HTTP $status): $(jq -r '.reason // .' <<<"$body")"
  exit 1
fi

token=$(jq -r .token <<<"$body")
echo "::add-mask::$token"
{
  echo "token=$token"
  echo "app_slug=$(jq -r .app_slug <<<"$body")"
  echo "expires_at=$(jq -r .expires_at <<<"$body")"
} >>"$GITHUB_OUTPUT"
