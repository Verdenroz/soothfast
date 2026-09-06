import assert from "node:assert/strict";
import test from "node:test";
import {
  decideBranch,
  decideClaims,
  decideTag,
  pullRequestNumber,
} from "../src/policy.ts";
import { claims } from "./keys.ts";

const cases = [
  {
    name: "push to a branch in the environment lands",
    overrides: {},
    mode: "land",
  },
  {
    name: "workflow_dispatch lands",
    overrides: { event_name: "workflow_dispatch" },
    mode: "land",
  },
  {
    name: "schedule lands",
    overrides: { event_name: "schedule" },
    mode: "land",
  },
  {
    name: "push of a tag is a landing candidate",
    overrides: { ref: "refs/tags/v1.0.0" },
    mode: "land",
  },
  {
    name: "pull_request on a merge ref comments",
    overrides: { event_name: "pull_request", ref: "refs/pull/7/merge" },
    mode: "comment",
  },
  {
    name: "pull_request_target is refused",
    overrides: { event_name: "pull_request_target", ref: "refs/pull/7/merge" },
    reason: /cannot mint/,
  },
  {
    name: "issue_comment is refused",
    overrides: { event_name: "issue_comment" },
    reason: /cannot mint/,
  },
  {
    name: "pull_request on a branch ref is refused",
    overrides: { event_name: "pull_request" },
    reason: /not a pull request/,
  },
  {
    name: "push on a pull ref is refused",
    overrides: { ref: "refs/pull/7/merge" },
    reason: /neither a branch nor a tag/,
  },
  {
    name: "missing environment is refused",
    overrides: { environment: undefined },
    reason: /"soothfast-bot" environment/,
  },
  {
    name: "other environment is refused",
    overrides: { environment: "production" },
    reason: /"soothfast-bot" environment/,
  },
  {
    name: "malformed repository is refused",
    overrides: { repository: "acme" },
    reason: /repository claim/,
  },
  {
    name: "non-numeric repository_id is refused",
    overrides: { repository_id: "abc" },
    reason: /repository_id claim/,
  },
];

for (const c of cases) {
  test(c.name, () => {
    const decision = decideClaims(claims(c.overrides));
    if ("mode" in c) {
      assert.deepEqual(decision, { ok: true, mode: c.mode });
    } else {
      assert.equal(decision.ok, false);
      assert.match(decision.ok ? "" : decision.reason, c.reason);
    }
  });
}

test("default branch matches", () => {
  assert.deepEqual(decideBranch("refs/heads/main", "main"), {
    ok: true,
    mode: "land",
  });
});

test("feature branch is not the default branch", () => {
  const decision = decideBranch("refs/heads/feature", "main");
  assert.equal(decision.ok, false);
  assert.match(
    decision.ok ? "" : decision.reason,
    /not the default branch \(main\)/,
  );
});

test("default branch name is not a prefix match", () => {
  assert.equal(decideBranch("refs/heads/main-old", "main").ok, false);
});

test("a tag whose commit is on the default branch lands", () => {
  assert.deepEqual(decideTag("refs/tags/v1.0.0", "behind"), {
    ok: true,
    mode: "land",
  });
  assert.deepEqual(decideTag("refs/tags/v1.0.0", "identical"), {
    ok: true,
    mode: "land",
  });
});

test("a tag ahead of or diverged from the default branch is refused", () => {
  for (const status of ["ahead", "diverged"]) {
    const decision = decideTag("refs/tags/v1.0.0", status);
    assert.equal(decision.ok, false);
    assert.match(
      decision.ok ? "" : decision.reason,
      /not on the default branch/,
    );
  }
});

test("pullRequestNumber reads the merge ref only", () => {
  assert.equal(pullRequestNumber("refs/pull/7/merge"), 7);
  assert.equal(pullRequestNumber("refs/pull/7/head"), undefined);
  assert.equal(pullRequestNumber("refs/heads/main"), undefined);
});
