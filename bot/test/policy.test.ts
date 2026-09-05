import assert from "node:assert/strict";
import test from "node:test";
import { decideBranch, decideClaims } from "../src/policy.ts";
import { claims } from "./keys.ts";

const cases = [
  {
    name: "push to a branch in the environment is allowed",
    overrides: {},
    reason: undefined,
  },
  {
    name: "workflow_dispatch is allowed",
    overrides: { event_name: "workflow_dispatch" },
    reason: undefined,
  },
  {
    name: "schedule is allowed",
    overrides: { event_name: "schedule" },
    reason: undefined,
  },
  {
    name: "pull_request is refused",
    overrides: { event_name: "pull_request" },
    reason: /event pull_request cannot mint/,
  },
  {
    name: "pull_request_target is refused",
    overrides: { event_name: "pull_request_target" },
    reason: /cannot mint/,
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
    name: "tag ref is refused",
    overrides: { ref: "refs/tags/v1.0.0" },
    reason: /not a branch/,
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
    if (c.reason === undefined) {
      assert.deepEqual(decision, { ok: true });
    } else {
      assert.equal(decision.ok, false);
      assert.match(decision.ok ? "" : decision.reason, c.reason);
    }
  });
}

test("default branch matches", () => {
  assert.deepEqual(decideBranch("refs/heads/main", "main"), { ok: true });
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
