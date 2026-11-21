import assert from "node:assert/strict";
import test from "node:test";
import {
  decideBranch,
  decideClaims,
  decideForkPullRequest,
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
    name: "workflow_run on a branch ref comments",
    overrides: { event_name: "workflow_run" },
    mode: "comment",
  },
  {
    name: "workflow_run on a pull ref is refused",
    overrides: { event_name: "workflow_run", ref: "refs/pull/7/merge" },
    reason: /not a branch/,
  },
  {
    name: "workflow_run on a tag is refused",
    overrides: { event_name: "workflow_run", ref: "refs/tags/v1.0.0" },
    reason: /not a branch/,
  },
  {
    name: "workflow_run outside the environment is refused",
    overrides: { event_name: "workflow_run", environment: undefined },
    reason: /"soothfast-bot" environment/,
  },
  {
    name: "merge_group is refused",
    overrides: { event_name: "merge_group" },
    reason: /cannot mint/,
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

const fork = {
  state: "open",
  head: { sha: "abc", repo: { full_name: "fork/lib" } },
  base: { repo: { full_name: "acme/lib" } },
};

test("an open fork pull request at the named head is accepted", () => {
  assert.deepEqual(decideForkPullRequest(7, fork, "abc"), {
    ok: true,
    mode: "comment",
  });
});

test("a fork pull request whose fork was deleted is still a fork", () => {
  const gone = { ...fork, head: { sha: "abc", repo: null } };
  assert.equal(decideForkPullRequest(7, gone, "abc").ok, true);
});

test("closed, same-repository and stale pull requests are refused", () => {
  const closed = { ...fork, state: "closed" };
  const same = { ...fork, head: { sha: "abc", repo: fork.base.repo } };
  const reasons = [
    [decideForkPullRequest(7, closed, "abc"), /not open/],
    [decideForkPullRequest(7, same, "abc"), /from this repository/],
    [decideForkPullRequest(7, fork, "def"), /head moved/],
  ] as const;
  for (const [decision, reason] of reasons) {
    assert.equal(decision.ok, false);
    assert.match(decision.ok ? "" : decision.reason, reason);
  }
});
