#!/usr/bin/env bash
# Render a fork pull request's verdict from its run's result files and post it
# as the pull request's comment. The fork's run controls everything it
# uploaded, so nothing in it decides the comment's structure: the number is
# trusted only when it names an open pull request of this repository from the
# fork branch and commit GitHub reports the run was for (the commit alone is
# not enough: a fork can push another pull request's head commit to a branch),
# expectations are cut to a strict grammar, and the results are read by the
# pinned CLI, which fences their text. The conclusion and link are the run's
# own, from the event.
# Inputs: CLI ARTIFACT_DIR (pr-number, expect_*, landing) RESULTS HEAD_SHA
# HEAD_REPO HEAD_BRANCH RUN_ATTEMPT RUN_CONCLUSION RUN_URL GH_TOKEN
# GITHUB_REPOSITORY, LABELS_DIR (a checkout of the default branch; default: here).
set -euo pipefail

max_names=256
max_shards=64
max_name_len=64
max_file_bytes=16384
max_results_bytes=$((20 * 1024 * 1024))
max_results_files=1000

skip() {
  echo "::notice::no comment posted: $1"
  exit 0
}

[ -f "$ARTIFACT_DIR/pr-number" ] || skip "the run uploaded no pull request number"
number=$(head -n 1 "$ARTIFACT_DIR/pr-number")
[[ $number =~ ^[1-9][0-9]{0,9}$ ]] || skip "the pull request number is not an integer"

pull=$(gh api "repos/${GITHUB_REPOSITORY}/pulls/${number}" \
  --jq '[.state, .head.sha, .head.repo.full_name // "-", .head.ref] | @tsv') ||
  skip "could not read #$number of ${GITHUB_REPOSITORY}"
IFS=$'\t' read -r state head repo branch <<<"$pull"
[ "$state" = open ] || skip "#$number is not open"
[ "$head" = "$HEAD_SHA" ] || skip "#$number has moved past the commit this run tested"
[ "$repo" = "$HEAD_REPO" ] && [ "$repo" != "$GITHUB_REPOSITORY" ] || skip "#$number is not from the fork this run belongs to"
[ "$branch" = "$HEAD_BRANCH" ] || skip "#$number is not from the branch this run belongs to"

dropped=0
# expectation FILE PATTERN LIMIT: sets kept to the tokens of the first line of
# FILE that match PATTERN, at most LIMIT of them. The rest are counted, never echoed.
expectation() {
  local file=$1 pattern=$2 limit=$3 token
  local -a tokens=()
  kept=()
  if [ -f "$file" ]; then
    read -ra tokens < <(head -c "$max_file_bytes" "$file" | head -n 1) || true
  fi
  for token in "${tokens[@]}"; do
    if [[ $token =~ $pattern ]] && [ "${#kept[@]}" -lt "$limit" ]; then
      kept+=("$token")
    else
      dropped=$((dropped + 1))
    fi
  done
}

name="^[A-Za-z0-9._+-]{1,${max_name_len}}\$"
for kind in rust import docs test check; do
  expectation "$ARTIFACT_DIR/expect_$kind" "$name" "$max_names"
  export "EXPECT_${kind^^}=${kept[*]-}"
done
expectation "$ARTIFACT_DIR/expect_shards" '^[0-9]{1,3}/[0-9]{1,3}$' "$max_shards"
export EXPECT_SHARDS="${kept[*]-}"
LANDING=false
[ "$(head -n 1 "$ARTIFACT_DIR/landing" 2>/dev/null || true)" != true ] || LANDING=true
export LANDING
[ "$dropped" -eq 0 ] || echo "::notice::dropped $dropped uploaded expectations that were malformed or over the limit"

# none, large or ok
results_state=ok
if [ ! -d "$RESULTS" ]; then
  results_state=none
else
  find "$RESULTS" ! -type d ! -type f -delete
  for dir in "$RESULTS"/*-attempt-*; do
    [ -d "$dir" ] || continue
    attempt=${dir##*-attempt-}
    if [[ ! $attempt =~ ^[0-9]{1,9}$ ]] || [ "$attempt" -gt "$RUN_ATTEMPT" ]; then
      rm -rf "$dir"
    fi
  done
  size=$(du -sb "$RESULTS" | cut -f 1)
  files=$(find "$RESULTS" -type f | wc -l)
  if [ "$size" -gt "$max_results_bytes" ] || [ "$files" -gt "$max_results_files" ]; then
    results_state=large
    echo "::notice::the run's results are over the size limit and were not rendered"
  else
    while IFS= read -r -d '' file; do
      if [[ ! ${file#"$RESULTS"/} =~ ^[A-Za-z0-9._@+=/-]+$ ]]; then
        rm -f "$file"
        echo "::notice::dropped a result file with an unusual name"
      fi
    done < <(find "$RESULTS" -type f -print0)
    [ -n "$(find "$RESULTS" -type f -name '*.json' -print -quit)" ] || results_state=none
  fi
fi

body="${RUNNER_TEMP:-/tmp}/soothfast-fork-verdict.md"
verdict="${RUNNER_TEMP:-/tmp}/soothfast-fork-verdict-body.md"
rc=0
case $results_state in
none) echo "No results were found for this run, so there is no verdict. Read the run." >"$verdict" ;;
large) echo "The results of this run are too large to render here. Read the run." >"$verdict" ;;
ok)
  OUT="$verdict" GH_REPO="$GITHUB_REPOSITORY" PR_NUMBER="$number" LABELS_REF=HEAD \
    "$(dirname "$0")/render-verdict.sh" || rc=$?
  [ "$rc" -le 1 ] || exit "$rc"
  ;;
esac

{
  echo "Verdict of [the fork's run]($RUN_URL), which concluded \`$RUN_CONCLUSION\`."
  if [ "$results_state" = ok ] && [ "$rc" -eq 0 ] && [ "$RUN_CONCLUSION" != success ]; then
    echo "The run did not succeed, so this verdict may be missing sections. Read the run."
  fi
  echo
  cat "$verdict"
} >"$body"

MARKER='<!-- soothfast-verdict -->' BODY_FILE="$body" PR_NUMBER="$number" "$(dirname "$0")/comment.sh"
