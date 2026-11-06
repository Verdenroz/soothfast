import assert from "node:assert/strict";
import test from "node:test";
import {
  WEBHOOK_MAX_BYTES,
  receive,
  route,
  verifySignature,
} from "../src/webhook.ts";
import { signWebhook } from "./keys.ts";

const SECRET = "hunter2";

const sign = (body: string, secret = SECRET) => signWebhook(body, secret);

const repository = {
  id: 42,
  full_name: "acme/widgets",
  default_branch: "main",
};

const payload = (extra: Record<string, unknown>) => ({
  sender: { login: "alice" },
  installation: { id: 7 },
  repository,
  ...extra,
});

const target = {
  installation: 7,
  repositoryId: 42,
  repository: "acme/widgets",
  defaultBranch: "main",
};

const pullPayload = (extra: Record<string, unknown> = {}) => ({
  number: 9,
  node_id: "PR_node9",
  draft: false,
  labels: [{ name: "soothfast:merge" }],
  auto_merge: null,
  head: { sha: "abc123" },
  state: "open",
  base: { ref: "main" },
  ...extra,
});

const pr = (action: string, extra: Record<string, unknown> = {}) =>
  payload({ action, pull_request: pullPayload(), ...extra });

async function delivery(
  body: string,
  event = "pull_request",
  signature?: string,
  headers: Record<string, string> = {},
) {
  const request = new Request("https://broker.test/webhook", {
    method: "POST",
    headers: {
      "x-github-event": event,
      "x-hub-signature-256": signature ?? (await sign(body)),
      ...headers,
    },
    body,
  });
  return receive(request, SECRET);
}

test("a correct signature verifies", async () => {
  const bytes = new TextEncoder().encode("{}");
  assert.equal(await verifySignature(SECRET, bytes, await sign("{}")), true);
});

const badSignatures = [
  ["missing header", null],
  ["wrong prefix", "sha1=" + "0".repeat(64)],
  ["short digest", "sha256=abcd"],
  ["uppercase hex", "sha256=" + "A".repeat(64)],
  ["wrong digest", "sha256=" + "0".repeat(64)],
] as const;

for (const [name, header] of badSignatures) {
  test(`signature is refused: ${name}`, async () => {
    const bytes = new TextEncoder().encode("{}");
    assert.equal(await verifySignature(SECRET, bytes, header), false);
  });
}

test("a signature made with another secret is refused", async () => {
  const bytes = new TextEncoder().encode("{}");
  const header = await sign("{}", "other");
  assert.equal(await verifySignature(SECRET, bytes, header), false);
});

test("a signed body that was altered is refused", async () => {
  const signature = await sign('{"a":1}');
  const result = await delivery('{"a":2}', "pull_request", signature);
  assert.deepEqual(result, { ok: false, status: 401, reason: "bad signature" });
});

test("a bad signature is refused before the body is parsed", async () => {
  const result = await delivery("not json", "ping", "sha256=" + "0".repeat(64));
  assert.deepEqual(result, { ok: false, status: 401, reason: "bad signature" });
});

test("a signed body that is not JSON is a 400", async () => {
  const result = await delivery("not json");
  assert.deepEqual(result, {
    ok: false,
    status: 400,
    reason: "body is not JSON",
  });
});

test("a declared length over the cap is refused unread", async () => {
  const result = await delivery("{}", "push", undefined, {
    "content-length": String(WEBHOOK_MAX_BYTES + 1),
  });
  assert.deepEqual(result, {
    ok: false,
    status: 413,
    reason: "payload too large",
  });
});

test("a signed pull_request delivery routes to work", async () => {
  const result = await delivery(JSON.stringify(pr("opened")));
  assert.deepEqual(result, {
    ok: true,
    work: {
      kind: "pull_request",
      target,
      pr: pullPayload(),
      labeler: undefined,
    },
  });
});

for (const action of [
  "opened",
  "reopened",
  "unlabeled",
  "ready_for_review",
  "synchronize",
]) {
  test(`pull_request ${action} evaluates the pull request`, () => {
    assert.deepEqual(route("pull_request", pr(action) as never), {
      kind: "pull_request",
      target,
      pr: pullPayload(),
      labeler: undefined,
    });
  });
}

test("pull_request labeled carries the label and the sender", () => {
  const work = route(
    "pull_request",
    pr("labeled", { label: { name: "soothfast:merge" } }) as never,
  );
  assert.deepEqual(work, {
    kind: "pull_request",
    target,
    pr: pullPayload(),
    labeler: { label: "soothfast:merge", login: "alice" },
  });
});

for (const action of ["closed", "edited", "assigned", "converted_to_draft"]) {
  test(`pull_request ${action} is ignored`, () => {
    assert.equal(route("pull_request", pr(action) as never), undefined);
  });
}

test("a labeled event on a closed pull request is ignored", () => {
  const closed = payload({
    action: "labeled",
    label: { name: "soothfast:merge" },
    pull_request: pullPayload({ state: "closed" }),
  });
  assert.equal(route("pull_request", closed as never), undefined);
});

test("a pull request into another base is ignored", () => {
  const other = pr("labeled", {
    label: { name: "soothfast:merge" },
    pull_request: pullPayload({ base: { ref: "release/1.x" } }),
  });
  assert.equal(route("pull_request", other as never), undefined);
});

test("a pull request with no labels is ignored", () => {
  const bare = pr("synchronize", {
    pull_request: pullPayload({ labels: [] }),
  });
  assert.equal(route("pull_request", bare as never), undefined);
});

test("a push to the default branch re-evaluates the base", () => {
  const work = route("push", payload({ ref: "refs/heads/main" }) as never);
  assert.deepEqual(work, { kind: "push", target, configChanged: true });
});

const pushed = (commits?: object[], forced = false) =>
  route(
    "push",
    payload({ ref: "refs/heads/main", commits, forced }) as never,
  ) as {
    configChanged: boolean;
  };

test("a push reports whether it touches soothfast.toml", () => {
  const commit = (files: object) => ({
    added: [],
    modified: [],
    removed: [],
    ...files,
  });
  assert.equal(
    pushed([commit({ modified: ["soothfast.toml"] })]).configChanged,
    true,
  );
  assert.equal(
    pushed([commit({ removed: ["soothfast.toml"] })]).configChanged,
    true,
  );
  assert.equal(
    pushed([commit({ added: ["src/lib.rs"] })]).configChanged,
    false,
  );
  assert.equal(pushed([]).configChanged, true);
  const unrelated = (length: number) =>
    Array.from({ length }, () => commit({}));
  assert.equal(pushed(unrelated(2047)).configChanged, false);
  assert.equal(pushed(unrelated(2048)).configChanged, true);
  assert.equal(pushed(unrelated(1), true).configChanged, true);
  assert.equal(pushed(undefined).configChanged, true);
});

test("a push to another branch is ignored", () => {
  const work = route("push", payload({ ref: "refs/heads/dev" }) as never);
  assert.equal(work, undefined);
});

test("deleting the default branch is ignored", () => {
  const deleted = payload({ ref: "refs/heads/main", deleted: true });
  assert.equal(route("push", deleted as never), undefined);
});

for (const event of ["ping", "installation", "issues", "check_run"]) {
  test(`${event} is ignored even without a repository`, () => {
    const work = route(event, { zen: "Keep it logically awesome." } as never);
    assert.equal(work, undefined);
  });
}
