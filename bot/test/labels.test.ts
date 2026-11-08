import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULTS } from "../src/config.ts";
import {
  GitHubError,
  type GitHubApi,
  type PullRequest,
} from "../src/github.ts";
import {
  SUBREQUEST_BUDGET,
  decide,
  evaluateOpen,
  evaluatePullRequest,
  gated,
} from "../src/labels.ts";
import type { Labeler } from "../src/webhook.ts";

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
    updated_at: "2026-01-01T00:00:00Z",
    ...rest,
  } satisfies PullRequest;
}

const writers = (...labels: string[]) => new Set(labels);

interface Plan {
  labeler?: { label: string; login: string };
  checked?: number;
  behind?: boolean;
  by?: string[];
  config?: typeof DEFAULTS;
}

function plan(pr: PullRequest, opts: Plan = {}) {
  const { labeler, checked = 1, behind = false, by = [] } = opts;
  const gate = gated(pr, opts.config ?? DEFAULTS, labeler);
  const writes = (label: string) => by.includes(label);
  const verified = {
    ...gate,
    update: gate.update.filter(writes),
    merge: gate.merge.filter(writes),
  };
  return decide(pr, verified, checked, behind);
}

const applied = { label: MERGE, login: "alice" };

test("a writer applying the merge label enables auto-merge", () => {
  const pr = pull({ names: [MERGE] });
  assert.deepEqual(plan(pr, { labeler: applied, by: [MERGE] }), [
    { kind: "enable_auto_merge", label: MERGE },
  ]);
});

test("a merge label applied earlier enables nothing", () => {
  const pr = pull({ names: [MERGE] });
  assert.deepEqual(plan(pr, { by: [MERGE] }), []);
});

test("another label being applied does not enable the merge", () => {
  const pr = pull({ names: [MERGE, UPDATE] });
  const labeler = { label: UPDATE, login: "alice" };
  assert.deepEqual(plan(pr, { labeler, by: [MERGE, UPDATE] }), []);
});

test("a merge label from a non-writer does nothing", () => {
  const pr = pull({ names: [MERGE] });
  assert.deepEqual(plan(pr, { labeler: applied }), []);
});

test("a merge label with no required check is skipped with a reason", () => {
  const pr = pull({ names: [MERGE] });
  const [action] = plan(pr, { labeler: applied, checked: 0, by: [MERGE] });
  assert.equal(action.kind, "skip");
});

test("a draft is not merged", () => {
  const pr = pull({ names: [MERGE], draft: true });
  assert.deepEqual(plan(pr, { labeler: applied, by: [MERGE] }), []);
});

test("a pull request already queued is not queued again", () => {
  const pr = pull({ names: [MERGE], auto_merge: {} });
  assert.deepEqual(plan(pr, { labeler: applied, by: [MERGE] }), []);
});

test("do-not-merge cancels a queued auto-merge and nothing else", () => {
  const pr = pull({ names: [MERGE, HOLD], auto_merge: {} });
  assert.deepEqual(plan(pr, { labeler: applied, by: [MERGE] }), [
    { kind: "disable_auto_merge" },
  ]);
});

test("do-not-merge on an unqueued pull request does nothing", () => {
  const pr = pull({ names: [MERGE, HOLD] });
  assert.deepEqual(plan(pr, { behind: true, by: [MERGE, UPDATE] }), []);
});

test("update acts only when the branch is behind", () => {
  const pr = pull({ names: [UPDATE] });
  assert.deepEqual(plan(pr, { behind: true, by: [UPDATE] }), [
    { kind: "update_branch" },
  ]);
  assert.deepEqual(plan(pr, { by: [UPDATE] }), []);
});

test("update needs no labeler event and runs before merge", () => {
  const pr = pull({ names: [UPDATE, MERGE] });
  const opts = { labeler: applied, behind: true, by: [UPDATE, MERGE] };
  assert.deepEqual(
    plan(pr, opts).map((a) => a.kind),
    ["update_branch", "enable_auto_merge"],
  );
});

test("update_all updates an unlabelled branch that is behind", () => {
  const config = { ...DEFAULTS, updateAll: true };
  assert.deepEqual(plan(pull(), { config, behind: true }), [
    { kind: "update_branch" },
  ]);
  assert.deepEqual(plan(pull(), { config }), []);
});

test("update_all skips drafts and do-not-merge", () => {
  const config = { ...DEFAULTS, updateAll: true };
  assert.deepEqual(plan(pull({ draft: true }), { config, behind: true }), []);
  assert.deepEqual(plan(pull({ names: [HOLD] }), { config, behind: true }), []);
});

test("configured label names replace the defaults", () => {
  const config = { ...DEFAULTS, mergeLabels: ["ship"], ignoreLabels: ["hold"] };
  const ship = { label: "ship", login: "alice" };
  assert.deepEqual(gated(pull({ names: [MERGE] }), config, applied).merge, []);
  assert.deepEqual(gated(pull({ names: ["ship"] }), config, ship).merge, [
    "ship",
  ]);
  const held = pull({ names: ["ship", "hold"] });
  assert.deepEqual(gated(held, config, ship).merge, []);
});

interface World {
  toml?: string | null;
  pulls?: PullRequest[];
  labelers?: Record<number, Record<string, string>>;
  permissions?: Record<string, string>;
  rules?: { type: string }[] | Error;
  protection?: object | Error;
  updateError?: Error;
  compareStatus?: string;
  compareError?: Error;
  live?: Partial<PullRequest> & { names?: string[] };
  permissionFailures?: number;
  enableError?: Error;
  mergeError?: Error;
  contentsFailures?: number;
}

let nextRepositoryId = 0;

function fakeRun(world: World) {
  const calls: string[] = [];
  const logs: string[] = [];
  const toml = world.toml === undefined ? "[bot]\nlabels = true\n" : world.toml;
  const pulls = world.pulls ?? [];
  let permissionFailures = world.permissionFailures ?? 0;
  let contentsFailures = world.contentsFailures ?? 0;
  const github = {
    async fileContents(_r: string, path: string, ref: string) {
      calls.push(`contents ${path}@${ref}`);
      if (contentsFailures-- > 0) throw new GitHubError(502, "bad gateway");
      return toml ?? undefined;
    },
    async openPullRequests(_r: string, base: string) {
      calls.push(`list ${base}`);
      return pulls;
    },
    async lastLabeler(_r: string, issue: number, label: string) {
      calls.push(`labeler ${issue} ${label}`);
      return world.labelers?.[issue]?.[label];
    },
    async collaboratorPermission(_r: string, login: string) {
      calls.push(`permission ${login}`);
      if (permissionFailures-- > 0) throw new GitHubError(502, "bad gateway");
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
      if (world.compareError) throw world.compareError;
      return { status: world.compareStatus ?? "identical" };
    },
    async updateBranch(_r: string, number: number, sha: string) {
      calls.push(`update ${number}@${sha}`);
      if (world.updateError) throw world.updateError;
    },
    async pullRequest(_r: string, number: number) {
      calls.push(`pull ${number}`);
      const { names, ...live } = world.live ?? {};
      const labels = names?.map((name) => ({ name }));
      const seen = pulls.find((p) => p.number === number)!;
      return { ...seen, ...(labels && { labels }), ...live };
    },
    async enableAutoMerge(id: string, method: string, oid: string) {
      calls.push(`enable ${id} ${method} ${oid}`);
      if (world.enableError) throw world.enableError;
    },
    async mergePullRequest(
      _r: string,
      number: number,
      sha: string,
      method: string,
    ) {
      calls.push(`merge ${number}@${sha} ${method}`);
      if (world.mergeError) throw world.mergeError;
    },
    async disableAutoMerge(id: string) {
      calls.push(`disable ${id}`);
    },
  } as unknown as GitHubApi;
  const run = {
    github,
    token: "ghs_x",
    repositoryId: ++nextRepositoryId,
    repository: "acme/widgets",
    defaultBranch: "main",
    fresh: false,
    log: (message: string) => logs.push(message),
  };
  return { run: { ...run, pulls }, calls, logs };
}

type FakeRun = ReturnType<typeof fakeRun>["run"];

const evaluateOne = (run: FakeRun, number: number, labeler?: Labeler) =>
  evaluatePullRequest(
    run,
    run.pulls.find((p) => p.number === number)!,
    labeler,
  );

const checks = { rules: [{ type: "required_status_checks" }] };
const labeledBy = (login: string, label = MERGE) => ({ label, login });

test("a labeled event by a writer enables auto-merge with the configured method", async () => {
  const { run, calls } = fakeRun({
    toml: '[bot]\nlabels = true\nmerge_method = "rebase"\n',
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    ...checks,
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, [
    "contents soothfast.toml@main",
    "permission alice",
    "rules",
    "protection",
    "pull 5",
    "enable PR_node5 rebase abc123",
  ]);
});

test("an already-mergeable pull request is merged with the labelled head", async () => {
  const { run, calls, logs } = fakeRun({
    toml: '[bot]\nlabels = true\nmerge_method = "squash"\n',
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    enableError: new GitHubError(200, "Pull request is in clean status"),
    ...checks,
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.equal(calls.at(-1), "merge 5@abc123 squash");
  assert.deepEqual(logs, ["#5: merged (checks had already passed)"]);
});

test("an unstable-status rejection also merges", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    enableError: new GitHubError(200, "Pull request is in unstable status"),
    ...checks,
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.equal(calls.at(-1), "merge 5@abc123 squash");
});

test("a refused merge is logged, not thrown", async () => {
  const { run, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    enableError: new GitHubError(200, "Pull request is in clean status"),
    mergeError: new GitHubError(405, "Merge queue is required"),
    ...checks,
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.deepEqual(logs, [
    "#5: could not merge (405: Merge queue is required)",
  ]);
});

test("other enable errors do not merge", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    enableError: new GitHubError(200, "Auto merge is not allowed"),
    ...checks,
  });
  await assert.rejects(evaluateOne(run, 5, labeledBy("alice")));
  assert.ok(!calls.some((c) => c.startsWith("merge")));
});

test("a moved head merges nothing even when it would be clean", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    live: { head: { sha: "def456" } },
    enableError: new GitHubError(200, "Pull request is in clean status"),
    ...checks,
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.ok(!calls.some((c) => c.startsWith("merge")));
  assert.ok(!calls.some((c) => c.startsWith("enable")));
});

test("a draft or queued pull request logs why it is not merged", async () => {
  const draft = fakeRun({ pulls: [pull({ names: [MERGE], draft: true })] });
  await evaluateOne(draft.run, 5, labeledBy("alice"));
  assert.deepEqual(draft.logs, ["#5: not merging, it is a draft"]);
  const queued = fakeRun({ pulls: [pull({ names: [MERGE], auto_merge: {} })] });
  await evaluateOne(queued.run, 5, labeledBy("alice"));
  assert.deepEqual(queued.logs, [
    "#5: not merging, auto-merge is already enabled",
  ]);
});

test("a sweep with nothing actionable logs it", async () => {
  const { run, logs } = fakeRun({ pulls: [pull({ names: [MERGE] })] });
  await evaluateOpen(run);
  assert.deepEqual(logs, ["no pull requests to act on"]);
});

const staleCases = [
  ["the label was removed before evaluation", { names: [] }],
  ["the head moved since it was labelled", { head: { sha: "def456" } }],
  ["auto-merge was queued meanwhile", { auto_merge: {} }],
  ["the pull request became a draft", { draft: true }],
] as const;

for (const [name, live] of staleCases) {
  test(`auto-merge is not enabled when ${name}`, async () => {
    const { run, calls, logs } = fakeRun({
      pulls: [pull({ names: [MERGE] })],
      permissions: { alice: "write" },
      live,
      ...checks,
    });
    await evaluateOne(run, 5, labeledBy("alice"));
    assert.ok(calls.includes("pull 5"));
    assert.ok(!calls.some((c) => c.startsWith("enable")));
    assert.match(logs[0], /changed since soothfast:merge was applied/);
  });
}

test("a merge label applied earlier is not acted on by a later event", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    labelers: { 5: { [MERGE]: "alice" } },
    permissions: { alice: "maintain" },
    ...checks,
  });
  await evaluateOne(run, 5);
  assert.ok(!calls.some((c) => c.startsWith("enable")));
  assert.ok(!calls.some((c) => c.startsWith("labeler")));
});

test("without a labeled payload the last update labeler is looked up", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [UPDATE] })],
    labelers: { 5: { [UPDATE]: "alice" } },
    permissions: { alice: "maintain" },
    compareStatus: "behind",
  });
  await evaluateOne(run, 5);
  assert.ok(calls.includes(`labeler 5 ${UPDATE}`));
  assert.ok(calls.includes("permission alice"));
  assert.ok(calls.includes("update 5@abc123"));
});

test("a triage-level labeler is ignored and logged", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { eve: "triage" },
    ...checks,
  });
  await evaluateOne(run, 5, labeledBy("eve"));
  assert.ok(!calls.some((c) => c.startsWith("enable")));
  assert.match(logs[0], /applied by eve \(triage\)/);
});

test("a repository that did not opt in is left alone", async () => {
  const { run, calls } = fakeRun({
    toml: "[site]\nname = 'x'\n",
    pulls: [pull({ names: [MERGE] })],
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, ["contents soothfast.toml@main"]);
});

test("a repository without labels = true is told how to turn them on", async () => {
  const { run, logs } = fakeRun({ toml: "[gate]\nwalltime = 'advisory'\n" });
  await evaluateOpen(run);
  assert.deepEqual(logs, [
    "labels are off: set [bot] labels = true in soothfast.toml on main",
  ]);
});

test("a missing soothfast.toml is left alone", async () => {
  const { run, calls } = fakeRun({ toml: null });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, ["contents soothfast.toml@main"]);
});

test("a malformed [bot] table acts on nothing and logs why", async () => {
  const { run, calls, logs } = fakeRun({
    toml: "[bot]\nmerge_method = 'ff'\n",
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.deepEqual(calls, ["contents soothfast.toml@main"]);
  assert.match(logs[0], /merge_method must be/);
});

test("a default branch with no required check is not merged", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "admin" },
  });
  await evaluateOne(run, 5, labeledBy("alice"));
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
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.ok(calls.some((c) => c.startsWith("enable")));
});

test("a merge queue rule counts as a required check", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    rules: [{ type: "merge_queue" }],
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.ok(calls.some((c) => c.startsWith("enable")));
});

test("unreadable protection counts as no checks and is logged", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    rules: [],
    protection: new GitHubError(403, "Resource not accessible"),
  });
  await evaluateOne(run, 5, labeledBy("alice"));
  assert.ok(!calls.some((c) => c.startsWith("enable")));
  assert.match(logs[0], /branch protection unreadable \(403\)/);
});

test("other protection read failures propagate", async () => {
  const { run } = fakeRun({
    pulls: [pull({ names: [MERGE] })],
    permissions: { alice: "write" },
    protection: new GitHubError(500, "boom"),
  });
  await assert.rejects(evaluateOne(run, 5, labeledBy("alice")));
});

test("do-not-merge from anyone cancels a queued auto-merge", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [MERGE, HOLD], auto_merge: {} })],
  });
  await evaluateOne(run, 5, labeledBy("eve", HOLD));
  assert.deepEqual(calls.slice(-1), ["disable PR_node5"]);
  assert.ok(!calls.some((c) => c.startsWith("permission")));
});

test("update passes the head sha it saw", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [UPDATE] })],
    permissions: { alice: "write" },
    compareStatus: "behind",
  });
  await evaluateOne(run, 5, labeledBy("alice", UPDATE));
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
    await evaluateOne(run, 5, labeledBy("alice", UPDATE));
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
  await evaluateOne(run, 5, labeledBy("eve", UPDATE));
  assert.ok(!calls.some((c) => c.startsWith("compare")));
});

test("a refused update is logged and does not fail the run", async () => {
  const { run, logs } = fakeRun({
    pulls: [pull({ names: [UPDATE] })],
    permissions: { alice: "write" },
    compareStatus: "behind",
    updateError: new GitHubError(422, "merge conflict"),
  });
  await evaluateOne(run, 5, labeledBy("alice", UPDATE));
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
    labelers: { 1: { [UPDATE]: "alice" } },
  });
  await evaluateOpen(run);
  assert.deepEqual(
    calls.filter((c) => c.startsWith("pull ") || c.startsWith("update")),
    ["update 1@abc123"],
  );
});

test("a push with update_all updates every open pull request without a label lookup", async () => {
  const { run, calls } = fakeRun({
    toml: "[bot]\nlabels = true\nupdate_all = true\n",
    pulls: [pull({ number: 1 }), pull({ number: 2, draft: true })],
    compareStatus: "behind",
  });
  await evaluateOpen(run);
  assert.deepEqual(
    calls.filter((c) => c.startsWith("update") || c.startsWith("labeler")),
    ["update 1@abc123"],
  );
});

test("a sweep cancels queued merges but never enables one", async () => {
  const { run, calls } = fakeRun({
    pulls: [
      pull({ number: 1, node_id: "N1", names: [HOLD], auto_merge: {} }),
      pull({ number: 2, names: [MERGE] }),
      pull({ number: 3 }),
    ],
    labelers: { 2: { [MERGE]: "alice" } },
    permissions: { alice: "write" },
    ...checks,
  });
  await evaluateOpen(run);
  assert.deepEqual(
    calls.filter((c) => /^(enable|disable)/.test(c)),
    ["disable N1"],
  );
  assert.equal(calls.filter((c) => c.startsWith("contents")).length, 1);
  assert.equal(calls.filter((c) => c.startsWith("pull ")).length, 0);
});

test("pull requests that cannot act do not use up the sweep's slots", async () => {
  const idle = Array.from({ length: 60 }, (_, i) =>
    pull({ number: i + 1, names: i % 2 ? [MERGE] : [HOLD] }),
  );
  const queued = pull({
    number: 99,
    node_id: "N99",
    names: [HOLD],
    auto_merge: {},
  });
  const { run, calls, logs } = fakeRun({ pulls: [...idle, queued] });
  await evaluateOpen(run);
  assert.deepEqual(
    calls.filter((c) => c.startsWith("disable")),
    ["disable N99"],
  );
  assert.deepEqual(logs, ["#99: auto-merge cancelled"]);
});

test("a draft with an update label is left alone", async () => {
  const { run, calls } = fakeRun({
    pulls: [pull({ names: [UPDATE], draft: true })],
  });
  await evaluateOpen(run);
  assert.ok(!calls.some((c) => c.startsWith("labeler")));
});

test("a sweep logs how many pull requests it reached", async () => {
  const pulls = Array.from({ length: 30 }, (_, i) => pull({ number: i + 1 }));
  const { run, calls, logs } = fakeRun({
    toml: "[bot]\nlabels = true\nupdate_all = true\n",
    pulls,
    compareStatus: "behind",
  });
  await evaluateOpen(run);
  const reached = calls.filter((c) => c.startsWith("update")).length;
  assert.equal(logs.at(-1), `evaluating ${reached} of 30 pull requests`);
});

test("a sweep with update_all reaches more than six pull requests within the subrequest cap", async () => {
  const pulls = Array.from({ length: 30 }, (_, i) => pull({ number: i + 1 }));
  const { run, calls } = fakeRun({
    toml: "[bot]\nlabels = true\nupdate_all = true\n",
    pulls,
    compareStatus: "behind",
  });
  await evaluateOpen(run);
  const updates = calls.filter((c) => c.startsWith("update")).length;
  assert.ok(updates > 6, `${updates} updated`);
  assert.ok(updates < 30, "the budget stops the sweep");
  assert.ok(calls.length <= SUBREQUEST_BUDGET, `${calls.length} subrequests`);
});

test("a sweep evaluates the least recently updated pull requests first", async () => {
  const day = (n: number) => `2026-01-${String(n).padStart(2, "0")}T00:00:00Z`;
  const pulls = Array.from({ length: 20 }, (_, i) =>
    pull({ number: i + 1, updated_at: day(20 - i) }),
  );
  const { run, calls } = fakeRun({
    toml: "[bot]\nlabels = true\nupdate_all = true\n",
    pulls,
    compareStatus: "behind",
  });
  await evaluateOpen(run);
  const updated = calls
    .filter((c) => c.startsWith("update"))
    .map((c) => Number(c.slice(7, c.indexOf("@"))));
  assert.deepEqual(updated.slice(0, 3), [20, 19, 18]);
});

test("one failing pull request does not stop the sweep", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [
      pull({ number: 1, names: [UPDATE] }),
      pull({ number: 2, names: [HOLD], auto_merge: {} }),
    ],
    labelers: { 1: { [UPDATE]: "alice" } },
    permissions: { alice: "write" },
    compareError: new GitHubError(500, "boom"),
  });
  await evaluateOpen(run);
  assert.match(logs[0], /#1: boom/);
  assert.ok(calls.some((c) => c.startsWith("disable")));
});

test("a failed lookup is retried by the next pull request, not cached", async () => {
  const { run, calls, logs } = fakeRun({
    pulls: [
      pull({ number: 1, names: [UPDATE] }),
      pull({ number: 2, names: [UPDATE] }),
    ],
    labelers: { 1: { [UPDATE]: "alice" }, 2: { [UPDATE]: "alice" } },
    permissions: { alice: "write" },
    compareStatus: "behind",
    permissionFailures: 1,
  });
  await evaluateOpen(run);
  assert.match(logs[0], /#1: bad gateway/);
  assert.ok(calls.includes("update 2@abc123"));
  assert.equal(calls.filter((c) => c === "permission alice").length, 2);
});

test("a successful lookup is shared across pull requests", async () => {
  const { run, calls } = fakeRun({
    pulls: [
      pull({ number: 1, names: [UPDATE] }),
      pull({ number: 2, names: [UPDATE] }),
    ],
    labelers: { 1: { [UPDATE]: "alice" }, 2: { [UPDATE]: "alice" } },
    permissions: { alice: "write" },
  });
  await evaluateOpen(run);
  assert.equal(calls.filter((c) => c === "permission alice").length, 1);
});

const fetches = (calls: string[]) =>
  calls.filter((c) => c.startsWith("contents")).length;

test("deliveries within the ttl read soothfast.toml once", async (t) => {
  let now = 1_000_000;
  t.mock.method(Date, "now", () => now);
  const { run, calls } = fakeRun({ toml: null });
  await evaluateOpen(run);
  now += 59_000;
  await evaluateOpen(run);
  assert.equal(fetches(calls), 1);
  now += 2_000;
  await evaluateOpen(run);
  assert.equal(fetches(calls), 2);
});

test("a fresh run reads the config again", async () => {
  const { run, calls } = fakeRun({});
  await evaluateOpen(run);
  await evaluateOpen({ ...run, fresh: true });
  assert.equal(fetches(calls), 2);
});

test("a failed read is not cached", async () => {
  const { run, calls } = fakeRun({ contentsFailures: 1 });
  await assert.rejects(evaluateOpen(run));
  await evaluateOpen(run);
  assert.equal(fetches(calls), 2);
});
