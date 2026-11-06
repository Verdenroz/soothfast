#!/usr/bin/env bash
# Shared by bot-token.sh, comment.sh and sweep.sh: the broker location, the
# job's OIDC token and the POST to the broker. Source it; do not run it.

BROKER=${BROKER:-https://soothfast-bot.verdenroz.workers.dev}

fail() {
  echo "::error::$1"
  exit 1
}

# broker_post ROUTE [CURL_ARG...]: POST to the broker, setting `status` (000
# when it cannot be reached, which also fails the call) and `body`.
# shellcheck disable=SC2034
broker_post() {
  local route=$1 response
  shift
  status=000 body=
  response=$(curl -sS --max-time "${BROKER_TIMEOUT:-30}" -w '\n%{http_code}' -X POST "$BROKER$route" "$@") || return 1
  status=${response##*$'\n'}
  body=${response%$'\n'*}
}

# Prints the job's OIDC token for the soothfast-bot audience, or nothing when
# the job has no `id-token: write` (a fork pull request, for one).
oidc_token() {
  [ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || return 1
  curl -sSf --max-time 30 -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
    "${ACTIONS_ID_TOKEN_REQUEST_URL}&audience=soothfast-bot" | jq -r .value
}
