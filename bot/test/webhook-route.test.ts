import assert from "node:assert/strict";
import test from "node:test";
import { GitHubError, type GitHubApi } from "../src/github.ts";
import { handle, type Env } from "../src/index.ts";
import { generateKeys, signWebhook, toPem } from "./keys.ts";

const SECRET = "hunter2";
let env: Env;

test.before(async () => {
  const keys = await generateKeys();
  const pkcs8 = new Uint8Array(
    await crypto.subtle.exportKey("pkcs8", keys.privateKey),
  );
  env = {
    GITHUB_APP_CLIENT_ID: "Iv1.abc",
    GITHUB_APP_PRIVATE_KEY: toPem("PRIVATE KEY", pkcs8),
    GITHUB_WEBHOOK_SECRET: SECRET,
  };
});

const pullRequest = {
  number: 5,
  node_id: "PR_node5",
  draft: false,
  labels: [{ name: "soothfast:merge" }],
  auto_merge: null,
  head: { sha: "abc123" },
  state: "open",
  base: { ref: "main" },
};

function fakeGitHub(opts: { protection?: Error; open?: object[] } = {}) {
  const calls: string[] = [];
  const minted: { installation: number; repository: number; perms: object }[] =
    [];
  const revoked: string[] = [];
  const github = {
    async mintToken(installation: number, repository: number, perms: object) {
      minted.push({ installation, repository, perms });
      return { token: "ghs_minted", expires_at: "2026-01-01T01:00:00Z" };
    },
    async fileContents() {
      return "[bot]\nlabels = true\n";
    },
    async openPullRequests(_r: string, base: string) {
      calls.push(`list ${base}`);
      return opts.open ?? [];
    },
    async disableAutoMerge(id: string) {
      calls.push(`disable ${id}`);
    },
    async collaboratorPermission(_r: string, login: string) {
      return login === "alice" ? "write" : "read";
    },
    async branchRules() {
      return [{ type: "required_status_checks" }];
    },
    async branchProtection() {
      if (opts.protection) throw opts.protection;
      return {};
    },
    async enableAutoMerge(id: string, method: string) {
      calls.push(`enable ${id} ${method}`);
    },
    async revoke(token: string) {
      revoked.push(token);
    },
  } as unknown as GitHubApi;
  return { github, calls, minted, revoked };
}

const repository = {
  id: 42,
  full_name: "acme/widgets",
  default_branch: "main",
};

const labeled = (login = "alice") => ({
  action: "labeled",
  label: { name: "soothfast:merge" },
  sender: { login },
  installation: { id: 7 },
  repository,
  pull_request: pullRequest,
});

async function deliver(
  fake: ReturnType<typeof fakeGitHub>,
  payload: object,
  opts: { event?: string; secret?: string; env?: Partial<Env> } = {},
) {
  const body = JSON.stringify(payload);
  const request = new Request("https://bot.example/webhook", {
    method: "POST",
    headers: {
      "x-github-event": opts.event ?? "pull_request",
      "x-hub-signature-256": await signWebhook(body, opts.secret ?? SECRET),
    },
    body,
  });
  const pending: Promise<unknown>[] = [];
  const deps = { jwks: async () => undefined, github: fake.github };
  const response = await handle(request, { ...env, ...opts.env }, deps, {
    waitUntil: (p) => void pending.push(p),
  });
  await Promise.all(pending);
  return { status: response.status, pending: pending.length };
}

test("a labeled event by a writer is acknowledged and acted on", async () => {
  const fake = fakeGitHub();
  const result = await deliver(fake, labeled());
  assert.deepEqual(result, { status: 202, pending: 1 });
  assert.deepEqual(fake.calls, ["enable PR_node5 squash"]);
});

test("the token is scoped to the payload's installation and repository", async () => {
  const fake = fakeGitHub();
  await deliver(fake, labeled());
  assert.deepEqual(fake.minted, [
    {
      installation: 7,
      repository: 42,
      perms: { contents: "write", pull_requests: "write" },
    },
  ]);
});

test("the token is revoked after the work", async () => {
  const fake = fakeGitHub();
  await deliver(fake, labeled());
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("the token is revoked when the work fails", async () => {
  const fake = fakeGitHub({ protection: new GitHubError(500, "boom") });
  const result = await deliver(fake, labeled());
  assert.equal(result.status, 202);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
  assert.deepEqual(fake.calls, []);
});

test("a labeler without write access is not acted on", async () => {
  const fake = fakeGitHub();
  await deliver(fake, labeled("mallory"));
  assert.deepEqual(fake.calls, []);
});

test("a synchronize does not re-enable a merge a writer labelled earlier", async () => {
  const fake = fakeGitHub();
  const pushed = { ...labeled(), action: "synchronize", label: undefined };
  const result = await deliver(fake, pushed);
  assert.equal(result.status, 202);
  assert.deepEqual(fake.calls, []);
});

test("a pull request with no labels costs no token", async () => {
  const fake = fakeGitHub();
  const bare = {
    ...labeled(),
    action: "synchronize",
    pull_request: { ...pullRequest, labels: [] },
  };
  const result = await deliver(fake, bare);
  assert.deepEqual(result, { status: 200, pending: 0 });
  assert.deepEqual(fake.minted, []);
});

test("a push to the default branch evaluates open pull requests", async () => {
  const fake = fakeGitHub();
  const push = {
    ref: "refs/heads/main",
    sender: { login: "alice" },
    installation: { id: 7 },
    repository,
  };
  const result = await deliver(fake, push, { event: "push" });
  assert.equal(result.status, 202);
  assert.deepEqual(fake.calls, ["list main"]);
});

test("a push cancels a queued merge on a do-not-merge pull request", async () => {
  const held = {
    ...pullRequest,
    node_id: "PR_held",
    labels: [{ name: "do-not-merge" }],
    auto_merge: {},
  };
  const fake = fakeGitHub({ open: [held] });
  const push = {
    ref: "refs/heads/main",
    sender: { login: "alice" },
    installation: { id: 7 },
    repository,
  };
  await deliver(fake, push, { event: "push" });
  assert.deepEqual(fake.calls, ["list main", "disable PR_held"]);
});

test("a signed ping is acknowledged with no token minted", async () => {
  const fake = fakeGitHub();
  const result = await deliver(fake, { zen: "hi" }, { event: "ping" });
  assert.deepEqual(result, { status: 200, pending: 0 });
  assert.deepEqual(fake.minted, []);
});

test("an event we do not handle is acknowledged without work", async () => {
  const fake = fakeGitHub();
  const closed = { ...labeled(), action: "closed" };
  assert.equal((await deliver(fake, closed)).status, 200);
  assert.deepEqual(fake.minted, []);
});

test("a bad signature is refused with no token minted", async () => {
  const fake = fakeGitHub();
  const result = await deliver(fake, labeled(), { secret: "other" });
  assert.deepEqual(result, { status: 401, pending: 0 });
  assert.deepEqual(fake.minted, []);
});

test("an empty webhook secret is a 500 before any signature work", async () => {
  const fake = fakeGitHub();
  const result = await deliver(fake, labeled(), {
    env: { GITHUB_WEBHOOK_SECRET: "" },
  });
  assert.deepEqual(result, { status: 500, pending: 0 });
});

test("missing App credentials are a 500", async () => {
  const fake = fakeGitHub();
  const result = await deliver(fake, labeled(), {
    env: { GITHUB_APP_PRIVATE_KEY: "" },
  });
  assert.equal(result.status, 500);
});
