#!/usr/bin/env bash
# Work out what this run will do and fetch what it needs: the default-branch
# check, the package list, full history for merge-base and tag lookups, the
# callgrind fallback, and the rustdoc toolchain when anything regenerates.
# A default-branch push with the gate on records a reference run.
# Inputs: GATE REGEN LABELS CACHE (true|false) SHARD PACKAGES RUSTDOC_TOOLCHAIN
# GH_TOKEN.
# Outputs: on_default_branch, default_branch, packages, sweep (act on labels),
# records (this leg records the reference), reference (the commit whose
# recorded runs the gate compares against) and cache_runs.
set -euo pipefail

default=$(gh api "repos/${GITHUB_REPOSITORY}" --jq .default_branch)
on_default=false
[ "${GITHUB_REF:-}" = "refs/heads/${default}" ] && on_default=true
echo "on_default_branch=$on_default" >>"$GITHUB_OUTPUT"
echo "default_branch=$default" >>"$GITHUB_OUTPUT"
sweep=false
if [ "${LABELS:-false}" = true ] && [ "${GITHUB_EVENT_NAME:-}" != pull_request ] && [ "$on_default" = true ]; then
  sweep=true
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
  exit 0
fi

if [ -z "$PACKAGES" ]; then
  PACKAGES=$(cargo metadata --format-version 1 --no-deps |
    jq -r '[.packages[] | select(any(.targets[]; .kind == ["bench"] and .name == "soothfast")) | .name] | join(" ")')
fi
if [ -z "$PACKAGES" ]; then
  echo "::error::no package has a bench target named soothfast; pass the packages input"
  exit 1
fi
echo "packages=$PACKAGES" >>"$GITHUB_OUTPUT"

if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
  git fetch --quiet --unshallow --tags
fi
if [ "$gating" = true ] && [ -n "${GITHUB_BASE_REF:-}" ]; then
  git fetch --quiet origin "+refs/heads/${GITHUB_BASE_REF}:refs/remotes/origin/${GITHUB_BASE_REF}"
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
  sudo apt-get update -qq && sudo apt-get install -y -qq valgrind
fi

if [ "$regenerating" = true ]; then
  rustup toolchain install "$RUSTDOC_TOOLCHAIN" --profile minimal
  echo "SOOTHFAST_RUSTDOC_TOOLCHAIN=$RUSTDOC_TOOLCHAIN" >>"$GITHUB_ENV"
fi
