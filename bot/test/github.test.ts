import assert from "node:assert/strict";
import test from "node:test";
import { base64UrlToText, utf8 } from "../src/encoding.ts";
import {
  GitHubError,
  appJwt,
  githubApi,
  importPrivateKey,
  wrapPkcs1,
} from "../src/github.ts";
import { NOW, generateKeys, toPem, type TestKeys } from "./keys.ts";

const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };
let keys: TestKeys;
let pkcs8: Uint8Array;

test.before(async () => {
  keys = await generateKeys();
  pkcs8 = new Uint8Array(
    await crypto.subtle.exportKey("pkcs8", keys.privateKey),
  );
});

function derLength(
  bytes: Uint8Array,
  at: number,
): { length: number; next: number } {
  const first = bytes[at];
  if (first < 0x80) return { length: first, next: at + 1 };
  const count = first & 0x7f;
  const length = Array.from(bytes.subarray(at + 1, at + 1 + count)).reduce(
    (n, b) => n * 256 + b,
    0,
  );
  return { length, next: at + 1 + count };
}

// PrivateKeyInfo: SEQUENCE { INTEGER version, AlgorithmIdentifier, OCTET STRING key }
function unwrapPkcs8(bytes: Uint8Array): Uint8Array {
  const outer = derLength(bytes, 1);
  const afterVersion = outer.next + 3;
  const afterAlgorithm = afterVersion + 15;
  assert.equal(bytes[afterAlgorithm], 0x04);
  const octets = derLength(bytes, afterAlgorithm + 1);
  return bytes.subarray(octets.next, octets.next + octets.length);
}

test("wrapPkcs1 reproduces the PKCS#8 encoding WebCrypto exports", () => {
  assert.deepEqual(wrapPkcs1(unwrapPkcs8(pkcs8)), pkcs8);
});

test("wrapPkcs1 encodes short and long DER lengths", () => {
  assert.deepEqual(
    Array.from(wrapPkcs1(Uint8Array.of(1)).subarray(0, 2)),
    [0x30, 0x15],
  );
  const long = wrapPkcs1(new Uint8Array(300));
  assert.deepEqual(Array.from(long.subarray(0, 4)), [0x30, 0x82, 0x01, 0x42]);
});

test("a PKCS#1 PEM as GitHub downloads it signs verifiably", async () => {
  const key = await importPrivateKey(
    toPem("RSA PRIVATE KEY", unwrapPkcs8(pkcs8)),
  );
  const signature = await crypto.subtle.sign(RS256.name, key, utf8("hello"));
  assert.ok(
    await crypto.subtle.verify(
      RS256.name,
      keys.publicKey,
      signature,
      utf8("hello"),
    ),
  );
});

test("a PKCS#8 PEM imports directly", async () => {
  const key = await importPrivateKey(toPem("PRIVATE KEY", pkcs8));
  assert.equal(key.type, "private");
});

test("non-PEM input is refused", async () => {
  await assert.rejects(importPrivateKey("not a key"), /not PEM/);
});

test("appJwt carries the client id and a nine minute window", async () => {
  const jwt = await appJwt("Iv1.abc", keys.privateKey, NOW);
  const [, payload] = jwt.split(".");
  assert.deepEqual(JSON.parse(base64UrlToText(payload)), {
    iat: NOW - 60,
    exp: NOW + 540,
    iss: "Iv1.abc",
  });
});

interface Recorded {
  url: string;
  method?: string;
  auth: string | null;
  body?: unknown;
}

function fakeGitHub(
  responses: Record<string, { status: number; body?: unknown }>,
) {
  const calls: Recorded[] = [];
  const fetchFn = (async (url: string | URL | Request, init?: RequestInit) => {
    const headers = new Headers(init?.headers);
    calls.push({
      url: String(url),
      method: init?.method,
      auth: headers.get("authorization"),
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
    });
    const match = responses[`${init?.method} ${new URL(String(url)).pathname}`];
    return new Response(
      match.body === undefined ? null : JSON.stringify(match.body),
      {
        status: match.status,
      },
    );
  }) as typeof fetch;
  return { api: githubApi(fetchFn), calls };
}

test("installationFor returns undefined on 404", async () => {
  const { api, calls } = fakeGitHub({
    "GET /repos/acme/mylib/installation": {
      status: 404,
      body: { message: "Not Found" },
    },
  });
  assert.equal(await api.installationFor("acme/mylib", "jwt"), undefined);
  assert.equal(calls[0].auth, "Bearer jwt");
});

test("mintToken scopes by repository id and permissions", async () => {
  const minted = { token: "ghs_x", expires_at: "2026-01-01T00:00:00Z" };
  const { api, calls } = fakeGitHub({
    "POST /app/installations/7/access_tokens": { status: 201, body: minted },
  });
  assert.deepEqual(
    await api.mintToken(
      7,
      12345,
      { contents: "write", pull_requests: "write" },
      "jwt",
    ),
    minted,
  );
  assert.deepEqual(calls[0].body, {
    repository_ids: [12345],
    permissions: { contents: "write", pull_requests: "write" },
  });
});

test("revoke accepts 204", async () => {
  const { api, calls } = fakeGitHub({
    "DELETE /installation/token": { status: 204 },
  });
  await api.revoke("ghs_x");
  assert.equal(calls[0].auth, "Bearer ghs_x");
});

test("unexpected statuses surface as GitHubError", async () => {
  const { api } = fakeGitHub({
    "GET /app": { status: 401, body: { message: "Bad credentials" } },
  });
  await assert.rejects(
    api.appSlug("jwt"),
    (e: unknown) =>
      e instanceof GitHubError &&
      e.status === 401 &&
      /Bad credentials/.test(e.message),
  );
});

test("a non-JSON error body becomes the GitHubError message", async () => {
  const fetchFn = (async () =>
    new Response("<html>Bad gateway</html>", { status: 502 })) as typeof fetch;
  await assert.rejects(
    githubApi(fetchFn).appSlug("jwt"),
    (e: unknown) =>
      e instanceof GitHubError &&
      e.status === 502 &&
      /Bad gateway/.test(e.message),
  );
});

test("compare encodes both sides and returns the status", async () => {
  const { api, calls } = fakeGitHub({
    "GET /repos/acme/mylib/compare/main...v1.0.0": {
      status: 200,
      body: { status: "behind" },
    },
  });
  assert.deepEqual(await api.compare("acme/mylib", "main", "v1.0.0", "ghs_x"), {
    status: "behind",
  });
  assert.equal(calls[0].auth, "Bearer ghs_x");
});

test("listComments paginates until a short page", async () => {
  const full = Array.from({ length: 100 }, (_, i) => ({
    id: i,
    body: "x",
    html_url: "",
  }));
  const { api, calls } = fakeGitHub({
    "GET /repos/acme/mylib/issues/7/comments": { status: 200, body: full },
  });
  let page = 0;
  const paged = githubApi((async (
    url: string | URL | Request,
    init?: RequestInit,
  ) => {
    page++;
    const body =
      page === 1
        ? full
        : [{ id: 100, body: "<!-- soothfast-gate -->\nhi", html_url: "u" }];
    void init;
    void url;
    return new Response(JSON.stringify(body), { status: 200 });
  }) as typeof fetch);
  const all = await paged.listComments("acme/mylib", 7, "ghs_x");
  assert.equal(all.length, 101);
  assert.equal(page, 2);
  void api;
  void calls;
});

test("createComment and updateComment send the body", async () => {
  const { api, calls } = fakeGitHub({
    "POST /repos/acme/mylib/issues/7/comments": {
      status: 201,
      body: { id: 1, body: "b", html_url: "u1" },
    },
    "PATCH /repos/acme/mylib/issues/comments/1": {
      status: 200,
      body: { id: 1, body: "c", html_url: "u1" },
    },
  });
  assert.equal(
    (await api.createComment("acme/mylib", 7, "b", "ghs_x")).html_url,
    "u1",
  );
  assert.equal(
    (await api.updateComment("acme/mylib", 1, "c", "ghs_x")).body,
    "c",
  );
  assert.deepEqual(
    calls.map((c) => c.body),
    [{ body: "b" }, { body: "c" }],
  );
});

test("tagObject peels an annotated tag and is undefined for a commit", async () => {
  const { api } = fakeGitHub({
    "GET /repos/acme/mylib/git/tags/aaaa": {
      status: 200,
      body: { object: { sha: "bbbb", type: "commit" } },
    },
    "GET /repos/acme/mylib/git/tags/cccc": {
      status: 404,
      body: { message: "Not Found" },
    },
  });
  assert.equal(await api.tagObject("acme/mylib", "aaaa", "ghs_x"), "bbbb");
  assert.equal(await api.tagObject("acme/mylib", "cccc", "ghs_x"), undefined);
});

test("fileContents decodes base64 content and is undefined on 404", async () => {
  const content = Buffer.from("[bot]\nlabels = true\n").toString("base64");
  const { api, calls } = fakeGitHub({
    "GET /repos/acme/mylib/contents/soothfast.toml": {
      status: 200,
      body: {
        content: content.replace(/(.{10})/g, "$1\n"),
        encoding: "base64",
      },
    },
    "GET /repos/acme/mylib/contents/missing.toml": { status: 404 },
  });
  assert.equal(
    await api.fileContents("acme/mylib", "soothfast.toml", "main", "ghs_x"),
    "[bot]\nlabels = true\n",
  );
  assert.match(calls[0].url, /\?ref=main$/);
  assert.equal(
    await api.fileContents("acme/mylib", "missing.toml", "main", "ghs_x"),
    undefined,
  );
});

test("collaboratorPermission reads the permission and maps 404 to none", async () => {
  const { api } = fakeGitHub({
    "GET /repos/acme/mylib/collaborators/alice/permission": {
      status: 200,
      body: { permission: "maintain" },
    },
    "GET /repos/acme/mylib/collaborators/ghost/permission": { status: 404 },
  });
  assert.equal(
    await api.collaboratorPermission("acme/mylib", "alice", "ghs_x"),
    "maintain",
  );
  assert.equal(
    await api.collaboratorPermission("acme/mylib", "ghost", "ghs_x"),
    "none",
  );
});

test("updateBranch sends the expected head sha", async () => {
  const { api, calls } = fakeGitHub({
    "PUT /repos/acme/mylib/pulls/7/update-branch": {
      status: 202,
      body: { message: "Updating pull request branch." },
    },
  });
  await api.updateBranch("acme/mylib", 7, "abc123", "ghs_x");
  assert.deepEqual(calls[0].body, { expected_head_sha: "abc123" });
});

test("enableAutoMerge sends the merge method as a GraphQL enum", async () => {
  const { api, calls } = fakeGitHub({
    "POST /graphql": { status: 200, body: { data: {} } },
  });
  await api.enableAutoMerge("PR_node", "squash", "abc123", "ghs_x");
  const body = calls[0].body as { query: string; variables: object };
  assert.match(body.query, /enablePullRequestAutoMerge/);
  assert.match(body.query, /expectedHeadOid: \$oid/);
  assert.deepEqual(body.variables, {
    id: "PR_node",
    method: "SQUASH",
    oid: "abc123",
  });
});

test("mergePullRequest binds the merge to the head sha", async () => {
  const { api, calls } = fakeGitHub({
    "PUT /repos/acme/widgets/pulls/5/merge": { status: 200, body: {} },
  });
  await api.mergePullRequest("acme/widgets", 5, "abc123", "squash", "ghs_x");
  assert.deepEqual(calls[0].body, { sha: "abc123", merge_method: "squash" });
});

test("a GraphQL error in a 200 response becomes a GitHubError", async () => {
  const { api } = fakeGitHub({
    "POST /graphql": {
      status: 200,
      body: { errors: [{ message: "Pull request is in clean status" }] },
    },
  });
  await assert.rejects(
    api.disableAutoMerge("PR_node", "ghs_x"),
    (e: unknown) => e instanceof GitHubError && /clean status/.test(e.message),
  );
});

function pagedEvents(pages: Record<number, object[]>, last: number) {
  const requested: number[] = [];
  const fetchFn = (async (url: string | URL | Request) => {
    const page = Number(new URL(String(url)).searchParams.get("page"));
    requested.push(page);
    const link = `<https://api.github.com/x?per_page=100&page=2>; rel="next", <https://api.github.com/x?per_page=100&page=${last}>; rel="last"`;
    return new Response(JSON.stringify(pages[page] ?? []), {
      status: 200,
      headers: last > 1 ? { link } : {},
    });
  }) as typeof fetch;
  return { api: githubApi(fetchFn), requested };
}

const labeled = (label: string, login: string) => ({
  event: "labeled",
  label: { name: label },
  actor: { login },
});

test("lastLabeler reads only the first page when there is one", async () => {
  const { api, requested } = pagedEvents(
    { 1: [labeled("a", "bob"), labeled("a", "alice")] },
    1,
  );
  assert.equal(await api.lastLabeler("acme/mylib", 7, "a", "ghs_x"), "alice");
  assert.deepEqual(requested, [1]);
});

test("lastLabeler jumps to the last page and stops when it finds the label", async () => {
  const { api, requested } = pagedEvents(
    { 1: [labeled("a", "old")], 9: [labeled("a", "dave"), labeled("b", "x")] },
    9,
  );
  assert.equal(await api.lastLabeler("acme/mylib", 7, "a", "ghs_x"), "dave");
  assert.deepEqual(requested, [1, 9]);
});

test("lastLabeler falls back to the page before the last, then gives up", async () => {
  const { api, requested } = pagedEvents(
    { 1: [], 8: [labeled("a", "carol")], 9: [labeled("b", "x")] },
    9,
  );
  assert.equal(await api.lastLabeler("acme/mylib", 7, "a", "ghs_x"), "carol");
  assert.deepEqual(requested, [1, 9, 8]);
  const none = pagedEvents({ 1: [], 8: [], 9: [] }, 9);
  assert.equal(
    await none.api.lastLabeler("acme/mylib", 7, "a", "ghs_x"),
    undefined,
  );
  assert.deepEqual(none.requested, [1, 9, 8]);
});

test("lastLabeler ignores unlabeled events and other labels", async () => {
  const { api } = pagedEvents(
    {
      1: [
        labeled("a", "alice"),
        { event: "unlabeled", label: { name: "a" }, actor: { login: "bob" } },
        labeled("b", "carol"),
      ],
    },
    1,
  );
  assert.equal(await api.lastLabeler("acme/mylib", 7, "a", "ghs_x"), "alice");
});
