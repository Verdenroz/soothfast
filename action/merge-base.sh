#!/usr/bin/env bash
# Print the commit a pull request run compares against: the merge-base of the
# checked-out commit and the base branch. The gate and the docs job both name
# it here, so they read and write the same cached measurement.
# Inputs: GITHUB_BASE_REF.
set -euo pipefail

git merge-base HEAD "origin/${GITHUB_BASE_REF}"
