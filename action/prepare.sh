#!/usr/bin/env bash
# Work out what this run will do and fetch what it needs: the default-branch
# check, the package list, full history for merge-base and tag lookups, the
# callgrind fallback, and the rustdoc toolchain when anything regenerates.
# A default-branch push with the gate on records a reference run.
# Inputs: GATE REGEN LABELS CACHE (true|false) SHARD PACKAGES RUSTDOC_TOOLCHAIN
# GH_TOKEN (also authenticates the fetches), RUSTDOC_LANE (true|false, optional).
# Outputs: on_default_branch, default_branch, packages, sweep (act on labels),
# records (this leg records the reference), reference (the commit whose
# recorded runs the gate compares against) and cache_runs.
set -euo pipefail

need() {
  local tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null || {
      echo "::error::${tool} must be installed on the runner for this step; see docs/ci.md, Self-hosted runners"
      exit 1
    }
  done
}

need cargo

# Rustdoc JSON comes from the pinned toolchain, whatever else the run does.
install_rustdoc_toolchain() {
  "$(dirname "$0")/rustdoc-toolchain.sh"
}
if command -v jq >/dev/null; then
  default=$(jq -r .repository.default_branch "$GITHUB_EVENT_PATH")
else
  need gh
  default=$(gh api "repos/${GITHUB_REPOSITORY}" --jq .default_branch)
fi
on_default=false
[ "${GITHUB_REF:-}" = "refs/heads/${default}" ] && on_default=true
echo "on_default_branch=$on_default" >>"$GITHUB_OUTPUT"
echo "default_branch=$default" >>"$GITHUB_OUTPUT"
sweep=false
if [ "${LABELS:-false}" = true ] && [ "${GITHUB_EVENT_NAME:-}" != pull_request ] && [ "$on_default" = true ]; then
  sweep=true
  need gh
fi
echo "sweep=$sweep" >>"$GITHUB_OUTPUT"
# Shard 1 of a matrix records every item for all of them.
records=false
if [ -z "${SHARD:-}" ] || [ "${SHARD%%/*}" = 1 ]; then
  records=true
fi
echo "records=$records" >>"$GITHUB_OUTPUT"

gating=false
[ "${GITHUB_EVENT_NAME:-}" = pull_request ] && [ "$GATE" = true ] && gating=true
recording=false
[ "${GITHUB_EVENT_NAME:-}" = push ] && [ "$on_default" = true ] && [ "$GATE" = true ] && recording=true
regenerating=false
[ "${GITHUB_EVENT_NAME:-}" != pull_request ] && [ "$on_default" = true ] && [ "$REGEN" = true ] && regenerating=true
if [ "$gating" = false ] && [ "$recording" = false ] && [ "$regenerating" = false ]; then
  echo "packages=" >>"$GITHUB_OUTPUT"
  [ "${RUSTDOC_LANE:-false}" = true ] && install_rustdoc_toolchain
  exit 0
fi

if [ -z "$PACKAGES" ]; then
  need jq
  PACKAGES=$(cargo metadata --format-version 1 --no-deps |
    jq -r '[.packages[] | select(any(.targets[]; .kind == ["bench"] and .name == "soothfast")) | .name] | join(" ")')
fi
if [ -z "$PACKAGES" ]; then
  echo "::error::no package has a bench target named soothfast; pass the packages input"
  exit 1
fi
echo "packages=$PACKAGES" >>"$GITHUB_OUTPUT"

# A checkout without persisted credentials cannot fetch on its own, so the
# token authenticates this one command and is never written to the config.
fetch() {
  local token="${GH_TOKEN:-${GITHUB_TOKEN:-}}" header
  if [ -z "$token" ]; then
    git fetch --quiet "$@"
    return
  fi
  header=$(printf 'x-access-token:%s' "$token" | base64 | tr -d '\n')
  echo "::add-mask::${header}"
  git -c "http.extraheader=AUTHORIZATION: basic ${header}" fetch --quiet "$@"
}

if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
  fetch --unshallow --tags
fi
if [ "$gating" = true ] && [ -n "${GITHUB_BASE_REF:-}" ]; then
  git rev-parse --verify --quiet "refs/remotes/origin/${GITHUB_BASE_REF}^{commit}" >/dev/null ||
    fetch origin "+refs/heads/${GITHUB_BASE_REF}:refs/remotes/origin/${GITHUB_BASE_REF}"
  reference=$(git merge-base HEAD "origin/${GITHUB_BASE_REF}")
elif [ "$recording" = true ] && git rev-parse --verify --quiet 'HEAD^' >/dev/null; then
  reference=$(git rev-parse 'HEAD^')
fi
cache_runs=false
if [ "${CACHE:-false}" = true ] && [ -n "${reference:-}" ]; then
  cache_runs=true
fi
echo "reference=${reference:-}" >>"$GITHUB_OUTPUT"
echo "cache_runs=$cache_runs" >>"$GITHUB_OUTPUT"

if [ "$(uname -s)" = Linux ] && ! command -v valgrind >/dev/null && command -v apt-get >/dev/null; then
  if [ "$(id -u)" = 0 ]; then
    apt-get update -qq && apt-get install -y -qq valgrind
  elif sudo -n true 2>/dev/null; then
    sudo -n apt-get update -qq && sudo -n apt-get install -y -qq valgrind
  else
    echo "::warning::valgrind is not installed and the runner has no root; a runner without hardware counters cannot fall back to callgrind"
  fi
fi

if [ "$regenerating" = true ] || [ "${RUSTDOC_LANE:-false}" = true ]; then
  install_rustdoc_toolchain
fi
