#!/usr/bin/env bash
# Shared by bot-token.sh and comment.sh: the broker location and the job's
# OIDC token. Source it; do not run it.

BROKER=${BROKER:-https://soothfast-bot.verdenroz.workers.dev}

# Prints the job's OIDC token for the soothfast-bot audience, or nothing when
# the job has no `id-token: write` (a fork pull request, for one).
oidc_token() {
  [ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || return 1
  curl -sSf --max-time 30 -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
    "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=soothfast-bot" | jq -r .value
}
