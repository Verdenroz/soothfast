import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULTS } from "../src/config.ts";
import {
  GitHubError,
  type GitHubApi,
  type IssueEvent,
  type PullRequest,
} from "../src/github.ts";
import {
  MAX_EVALUATIONS,
  decide,
  evaluateOpen,
  evaluatePullRequest,
  gated,
  lastLabeler,
} from "../src/labels.ts";

const MERGE = "soothfast:merge";
const UPDATE = "soothfast:update";
const HOLD = "do-not-merge";

function pull(overrides: Partial<PullRequest> & { names?: string[] } = {}) {
  const { names = [], ...rest } = overrides;
  return {
    number: 5,
    node_id: "PR_node5",
    draft: false,
    labels: names.map((name) => ({ name })),
    auto_merge: null,
    head: { sha: "abc123" },
    ...rest,
  } satisfies PullRequest;
}

const writers = (...labels: string[]) => new Set(labels);

test("a writer's merge label enables auto-merge", () => {
  const pr = pull({ names: [MERGE] });
  assert.deepEqual(decide(pr, DEFAULTS, 1, false, writers(MERGE)), [
    { kind: "enable_auto_merge" },
  ]);
});

test("a merge label from a non-writer does nothing", () => {
  const pr = pull({ names: [MERGE] });
  assert.deepEqual(decide(pr, DEFAULTS, 1, false, writers()), []);
});

test("a merge label with no required check is skipped with a reason", () => {
  const pr = pull({ names: [MERGE] });
  const [action] = decide(pr, DEFAULTS, 0, false, writers(MERGE));
  assert.equal(action.kind, "skip");
});

test("a draft is not merged", () => {
  const pr = pull({ names: [MERGE], draft: true });
  assert.deepEqual(decide(pr, DEFAULTS, 1, false, writers(MERGE)), []);
});

test("a pull request already queued is not queued again", () => {
  const pr = pull({ names: [MERGE], auto_merge: {} });
  assert.deepEqual(decide(pr, DEFAULTS, 1, false, writers(MERGE)), []);
});

test("do-not-merge cancels a queued auto-merge and nothing else", () => {
  const pr = pull({ names: [MERGE, HOLD], auto_merge: {} });
  assert.deepEqual(decide(pr, DEFAULTS, 1, false, writers(MERGE)), [
    { kind: "disable_auto_merge" },
  ]);
});

test("do-not-merge on an unqueued pull request does nothing", () => {
  const pr = pull({ names: [MERGE, HOLD] });
  assert.deepEqual(decide(pr, DEFAULTS, 1, true, writers(MERGE, UPDATE)), []);
});

test("update acts only when the branch is behind", () => {
  const behind = pull({ names: [UPDATE] });
  assert.deepEqual(decide(behind, DEFAULTS, 0, true, writers(UPDATE)), [
    { kind: "update_branch" },
  ]);
  const clean = pull({ names: [UPDATE] });
  assert.deepEqual(decide(clean, DEFAULTS, 0, false, writers(UPDATE)), []);
});

test("update runs before merge", () => {
  const pr = pull({ names: [UPDATE, MERGE] });
  assert.deepEqual(
    decide(pr, DEFAULTS, 1, true, writers(UPDATE, MERGE)).map((a) => a.kind),
    ["update_branch", "enable_auto_merge"],
  );
});

test("configured label names replace the defaults", () => {
  const config = { ...DEFAULTS, mergeLabels: ["ship"], ignoreLabels: ["hold"] };
  assert.deepEqual(gated(pull({ names: [MERGE] }), config).merge, []);
  assert.deepEqual(gated(pull({ names: ["ship"] }), config).merge, ["ship"]);
  assert.deepEqual(gated(pull({ names: ["ship", "hold"] }), config).merge, []);
});

test("lastLabeler takes the most recent labeled event for the label", () => {
  const events: IssueEvent[] = [
    { event: "labeled", label: { name: MERGE }, actor: { login: "alice" } },
    { event: "labeled", label: { name: "other" }, actor: { login: "carol" } },
    { event: "unlabeled", label: { name: MERGE }, actor: { login: "bob" } },
    { event: "labeled", label: { name: MERGE }, actor: { login: "dave" } },
  ];
  assert.equal(lastLabeler(events, MERGE), "dave");
  assert.equal(lastLabeler(events, "none"), undefined);
});

interface World {
  toml?: string | null;
  pulls?: PullRequest[];
  events?: Record<number, IssueEvent[]>;
  permissions?: Record<string, string>;
  rules?: { type: string }[] | Error;
  protection?: object | Error;
  updateError?: Error;
  compareStatus?: string;
}

function fakeRun(world: World) {
  const calls: string[] = [];
  const logs: string[] = [];
  const toml = world.toml === undefined ? "[bot]\nlabels = true\n" : world.toml;
  const pulls = world.pulls ?? [];
  const github = {
    async fileContents(_r: string, path: string, ref: string) {
      calls.push(`contents ${path}@${ref}`);
      return toml ?? undefined;
    },
    async pullRequest(_r: string, number: number) {
      calls.push(`pull ${number}`);
      return pulls.find((p) => p.number === number)!;
    },
    async openPullRequests(_r: string, base: string) {
      calls.push(`list ${base}`);
      return pulls;
    },
    async issueEvents(_r: string, issue: number) {
      calls.push(`events ${issue}`);
      return world.events?.[issue] ?? [];
    },
    async collaboratorPermission(_r: string, login: string) {
      calls.push(`permission ${login}`);
      return world.permissions?.[login] ?? "none";
    },
    async branchRules() {
      calls.push("rules");
      if (world.rules instanceof Error) throw world.rules;
      return world.rules ?? [];
    },
    async branchProtection() {
      calls.push("protection");
      if (world.protection instanceof Error) throw world.protection;
      return world.protection ?? {};
    },
    async compare(_r: string, base: string, head: string) {
      calls.push(`compare ${base}...${head}`);
      return { status: world.compareStatus ?? "identical" };
    },
    async updateBranch(_r: string, number: number, sha: string) {
      calls.push(`update ${number}@${sha}`);
      if (world.updateError) throw world.updateError;
    },
    async enableAutoMerge(id: string, method: string) {
      calls.push(`enable ${id} ${method}`);
    },
    async disableAutoMerge(id: string) {
      calls.push(`disable ${id}`);
    },
  } as unknown as GitHubApi;
  const run = {
    github,
    token: "ghs_x",
    repository: "acme/widgets",
    defaultBranch: "main",
    log: (message: string) => logs.push(message),
  };
  return { run, calls, logs };
}

const checks = { rules: [{ type: "required_status_checks" }] };
const labeledBy = (login: string, label = MERGE) => ({ label, login });

test("a labeled event by a writer enables auto-merge with the configured method", async () => {
  const { run, calls } = fakeRun({
    toml: '[bot]\nlabels = true\nmerge_method = "rebase"\n',
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    ...checks,
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, [
    "contents soothfast.toml@main",
    "pull 5",
    "permission alice",
    "rules",
    "protection",
    "enable PR_node5 rebase",
  ]);
});

test("without a labeled payload the last labeler comes from issue events", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    events: {
      5: [
        { event: "labeled", label: { name: MERGE }, actor: { login: "bob" } },
        { event: "labeled", label: { name: MERGE }, actor: { login: "alice" } },
      ],
    },
    permissions: { alice: "maintain", bob: "none" },
    ...checks,
  });
  await evaluatePullRequest(run, 5);
  assert.ok(calls.includes("events 5"));
  assert.ok(calls.includes("permission alice"));
  assert.ok(!calls.includes("permission bob"));
  assert.ok(calls.some((c) => c.startsWith("enable")));
});

test("a triage-level labeler is ignored and logged", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { eve: "triage" },
    ...checks,
  });
  await evaluatePullRequest(run, 5, labeledBy("eve"));
  assert.ok(!calls.some((c) => c.startsWith("enable")));
  assert.match(logs[0], /applied by eve \(triage\)/);
});

test("a repository that did not opt in is left alone", async () => {
  const { run, calls } = fakeRun({
    toml: "[site]\nname = 'x'\n",
    pulls: [pull({ names: [MERGE] })],
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, ["contents soothfast.toml@main"]);
});

test("a missing soothfast.toml is left alone", async () => {
  const { run, calls } = fakeRun({ toml: null });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, ["contents soothfast.toml@main"]);
});

test("a malformed [bot] table acts on nothing and logs why", async () => {
  const { run, calls, logs } = fakeRun({
    toml: "[bot]\nmerge_method = 'ff'\n",
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, ["contents soothfast.toml@main"]);
  assert.match(logs[0], /merge_method must be/);
});

test("a default branch with no required check is not merged", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "admin" },
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.ok(!calls.some((c) => c.startsWith("enable")));
  assert.match(logs[0], /requires no status check/);
});

test("classic protection contexts count as required checks", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    protection: {
      protection: { required_status_checks: { contexts: ["ci"] } },
    },
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.ok(calls.some((c) => c.startsWith("enable")));
});

test("a merge queue rule counts as a required check", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    rules: [{ type: "merge_queue" }],
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.ok(calls.some((c) => c.startsWith("enable")));
});

test("unreadable protection counts as no checks and is logged", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    rules: [],
    protection: new GitHubError(403, "Resource not accessible"),
  });
  await evaluatePullRequest(run, 5, labeledBy("alice"));
  assert.ok(!calls.some((c) => c.startsWith("enable")));
  assert.match(logs[0], /branch protection unreadable \(403\)/);
});

test("other protection read failures propagate", async () => {
  const { run } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    protection: new GitHubError(500, "boom"),
  });
  await assert.rejects(evaluatePullRequest(run, 5, labeledBy("alice")));
});

test("do-not-merge from anyone cancels a queued auto-merge", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE, HOLD], auto_merge: {} })],
  });
  await evaluatePullRequest(run, 5, labeledBy("eve", HOLD));
  assert.deepEqual(calls.slice(-1), ["disable PR_node5"]);
  assert.ok(!calls.some((c) => c.startsWith("permission")));
});

test("update passes the head sha it saw", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [UPDATE] })],
    permissions: { alice: "write" },
    compareStatus: "behind",
  });
  await evaluatePullRequest(run, 5, labeledBy("alice", UPDATE));
  assert.deepEqual(calls.slice(-2), [
    "compare main...abc123",
    "update 5@abc123",
  ]);
});

for (const [status, updated] of [
  ["diverged", true],
  ["behind", true],
  ["ahead", false],
  ["identical", false],
] as const) {
  test(`compare ${status} ${updated ? "updates" : "leaves"} the branch`, async () => {
    const { run, calls } = fakeRun({
      pulls: [pull({ names: [UPDATE] })],
      permissions: { alice: "write" },
      compareStatus: status,
    });
    await evaluatePullRequest(run, 5, labeledBy("alice", UPDATE));
    assert.equal(
      calls.some((c) => c.startsWith("update")),
      updated,
    );
  });
}

test("the branch is compared only for a writer's update label", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [UPDATE] })],
    permissions: { eve: "triage" },
    compareStatus: "behind",
  });
  await evaluatePullRequest(run, 5, labeledBy("eve", UPDATE));
  assert.ok(!calls.some((c) => c.startsWith("compare")));
});

test("a refused update is logged and does not fail the run", async () => {
  const { run, logs } = fakeRun({
    pulls: [pull({ names: [UPDATE] })],
    permissions: { alice: "write" },
    compareStatus: "behind",
    updateError: new GitHubError(422, "merge conflict"),
  });
  await evaluatePullRequest(run, 5, labeledBy("alice", UPDATE));
  assert.match(logs.at(-1)!, /could not update \(422: merge conflict\)/);
});

test("a push evaluates only open pull requests with an update label", async () => {
  const { run, calls } = fakeRun({
    pulls: [
      pull({ number: 1, names: [UPDATE] }),
      pull({ number: 2, names: [MERGE] }),
      pull({ number: 3 }),
    ],
    permissions: { alice: "write" },
    compareStatus: "behind",
    events: {
      1: [
        {
          event: "labeled",
          label: { name: UPDATE },
          actor: { login: "alice" },
        },
      ],
    },
  });
  await evaluateOpen(run, "update");
  assert.deepEqual(
    calls.filter((c) => c.startsWith("pull ") || c.startsWith("update")),
    ["update 1@abc123"],
  );
});

test("a sweep evaluates every pull request carrying a bot label", async () => {
  const { run, calls } = fakeRun({
    pulls: [
      pull({ number: 1, names: [HOLD], auto_merge: {} }),
      pull({ number: 2, names: [MERGE] }),
      pull({ number: 3 }),
    ],
    events: {
      2: [
        { event: "labeled", label: { name: MERGE }, actor: { login: "alice" } },
      ],
    },
    permissions: { alice: "write" },
    ...checks,
  });
  await evaluateOpen(run, "any");
  assert.ok(calls.includes("disable PR_node5"));
  assert.ok(calls.includes("enable PR_node5 squash"));
  assert.equal(calls.filter((c) => c.startsWith("contents")).length, 1);
  assert.equal(calls.filter((c) => c.startsWith("pull ")).length, 0);
});

test("a sweep is bounded and logs the truncation", async () => {
  const total = MAX_EVALUATIONS + 3;
  const pulls = Array.from({ length: total }, (_, i) =>
    pull({ number: i + 1, names: [HOLD], auto_merge: {} }),
  );
  const { run, calls, logs } = fakeRun({ pulls });
  await evaluateOpen(run, "any");
  assert.equal(
    calls.filter((c) => c.startsWith("disable")).length,
    MAX_EVALUATIONS,
  );
  assert.match(logs[0], new RegExp(`${MAX_EVALUATIONS} of ${total}`));
});

test("one failing pull request does not stop the sweep", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [
      pull({ number: 1, names: [MERGE] }),
      pull({ number: 2, names: [HOLD], auto_merge: {} }),
    ],
    events: {
      1: [
        { event: "labeled", label: { name: MERGE }, actor: { login: "alice" } },
      ],
    },
    permissions: { alice: "write" },
    protection: new GitHubError(500, "boom"),
  });
  await evaluateOpen(run, "any");
  assert.match(logs[0], /#1: boom/);
  assert.ok(calls.some((c) => c.startsWith("disable")));
});
