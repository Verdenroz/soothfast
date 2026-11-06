#!/usr/bin/env bash
# Ask the soothfast-bot broker to reconcile soothfast labels across the open
# pull requests (see the Labels section of docs/ci.md). The rules run in the
# broker, which also acts on label events as they happen; this catches the
# deliveries it missed.
# Inputs: BOT_TOKEN (set when the caller supplies a token of their own), BROKER (optional).
set -euo pipefail

# shellcheck source=action/oidc.sh
source "$(dirname "$0")/oidc.sh"

if [ -n "${BOT_TOKEN:-}" ]; then
  echo "::warning::soothfast labels are acted on by the soothfast-bot broker, which a bot-token has no access to, so nothing was swept; see docs/ci.md, Labels"
  exit 0
fi

command -v jq >/dev/null || fail "jq must be installed on the runner to sweep labels; see docs/ci.md, Self-hosted runners"
oidc=$(oidc_token) || fail "sweeping labels needs 'id-token: write' in the job's permissions"

BROKER_TIMEOUT=60 broker_post /sweep -H "Authorization: Bearer $oidc" ||
  fail "could not reach the broker at $BROKER"

if [ "$status" != 200 ]; then
  fail "soothfast-bot refused the sweep (HTTP $status): $(jq -r '.reason // .' <<<"$body")"
fi

jq -r '.log[] | "sweep: " + .' <<<"$body"
