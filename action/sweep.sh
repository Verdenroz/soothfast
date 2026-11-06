#!/usr/bin/env bash
# Ask the soothfast-bot broker to reconcile soothfast labels across the open
# pull requests (see the Labels section of docs/ci.md). The rules run in the
# broker, which also acts on label events as they happen; this catches the
# deliveries it missed, so it only annotates and never fails the job.
# Inputs: BOT_TOKEN (set when the caller supplies a token of their own), BROKER (optional).
set -euo pipefail

# shellcheck source=action/oidc.sh
source "$(dirname "$0")/oidc.sh"

skip() {
  echo "::$1::$2"
  exit 0
}

if [ -n "${BOT_TOKEN:-}" ]; then
  skip notice "soothfast labels are acted on by the soothfast-bot broker, which a bot-token has no access to, so nothing was swept; set labels: \"false\" to silence this, see docs/ci.md, Labels"
fi

command -v jq >/dev/null || skip notice "jq must be installed on the runner to sweep labels; see docs/ci.md, Self-hosted runners"
oidc=$(oidc_token) || skip notice "no labels sweep: grant the job 'id-token: write' and the soothfast-bot environment, or set labels: \"false\"; see docs/ci.md, Labels"

BROKER_TIMEOUT=60 broker_post /sweep -H "Authorization: Bearer $oidc" ||
  skip warning "could not reach the broker at $BROKER, so labels were not swept"

if [ "$status" != 200 ]; then
  reason=$(jq -r '.reason // .' <<<"$body" 2>/dev/null) || reason=$body
  skip warning "soothfast-bot refused the sweep (HTTP $status): ${reason//[$'\r\n']/ }"
fi

jq -r '.log[] | gsub("[\r\n]"; " ") | "sweep: " + .' <<<"$body" ||
  skip warning "the broker's sweep reply was unreadable"
