import assert from "node:assert/strict";
import test from "node:test";
import type { GitHubApi, IssueComment } from "../src/github.ts";
import { handle, type Env } from "../src/index.ts";
import {
  KID,
  NOW,
  claims,
  generateKeys,
  signJwt,
  toPem,
  type TestKeys,
} from "./keys.ts";

let keys: TestKeys;
let env: Env;

test.before(async () => {
  keys = await generateKeys();
  const pkcs8 = new Uint8Array(
    await crypto.subtle.exportKey("pkcs8", keys.privateKey),
  );
  env = {
    GITHUB_APP_CLIENT_ID: "Iv1.abc",
    GITHUB_APP_PRIVATE_KEY: toPem("PRIVATE KEY", pkcs8),
  };
});

interface Fake {
  github: GitHubApi;
  revoked: string[];
  minted: Record<string, string>[];
  compared: string[];
  comments: IssueComment[];
  posted: { kind: "create" | "update"; body: string }[];
}

function fakeGitHub(
  opts: {
    installed?: boolean;
    defaultBranch?: string;
    compareStatus?: string;
    comments?: IssueComment[];
    tagObjects?: Record<string, string>;
  } = {},
): Fake {
  const fake: Fake = {
    revoked: [],
    minted: [],
    compared: [],
    comments: opts.comments ?? [],
    posted: [],
    github: {
      async installationFor() {
        return opts.installed === false ? undefined : { id: 7 };
      },
      async mintToken(installationId, repositoryId, permissions) {
        assert.equal(installationId, 7);
        assert.equal(repositoryId, 12345);
        fake.minted.push(permissions);
        return { token: "ghs_minted", expires_at: "2026-01-01T01:00:00Z" };
      },
      async repository() {
        return { default_branch: opts.defaultBranch ?? "main" };
      },
      async compare(_repository, _base, head) {
        fake.compared.push(head);
        return { status: opts.compareStatus ?? "diverged" };
      },
      async tagObject(_repository, sha) {
        return opts.tagObjects?.[sha];
      },
      async listComments() {
        return fake.comments;
      },
      async createComment(_repository, _issue, body) {
        fake.posted.push({ kind: "create", body });
        return {
          id: 99,
          body,
          html_url: "https://github.com/acme/mylib/pull/7#issuecomment-99",
        };
      },
      async updateComment(_repository, id, body) {
        fake.posted.push({ kind: "update", body });
        return {
          id,
          body,
          html_url: `https://github.com/acme/mylib/pull/7#issuecomment-${id}`,
        };
      },
      async revoke(token) {
        fake.revoked.push(token);
      },
      async appSlug() {
        return "soothfast-bot";
      },
    },
  };
  return fake;
}

interface Body {
  token?: string;
  app_slug?: string;
  comment_url?: string;
  reason?: string;
}

async function request(
  overrides: Parameters<typeof claims>[0],
  fake: Fake,
  opts: { route?: string; token?: string; body?: unknown } = {},
) {
  const jwt =
    opts.token ??
    (await signJwt(
      keys.privateKey,
      { alg: "RS256", kid: KID },
      { ...claims(overrides) },
    ));
  const req = new Request(`https://bot.example${opts.route ?? "/token"}`, {
    method: "POST",
    headers: { authorization: `Bearer ${jwt}` },
    body:
      opts.body === undefined
        ? undefined
        : typeof opts.body === "string"
          ? opts.body
          : JSON.stringify(opts.body),
  });
  const deps = {
    jwks: async (kid: string) => (kid === KID ? keys.jwk : undefined),
    github: fake.github,
    now: () => NOW,
  };
  const res = await handle(req, env, deps);
  return { status: res.status, body: (await res.json()) as Body };
}

const pr = { event_name: "pull_request", ref: "refs/pull/7/merge" };
const commentBody = {
  pull_request: 7,
  marker: "<!-- soothfast-gate -->",
  body: "## soothfast gate\nok",
};

test("allowed claims mint a scoped landing token", async () => {
  const fake = fakeGitHub();
  const { status, body } = await request({}, fake);
  assert.equal(status, 200);
  assert.deepEqual(body, {
    token: "ghs_minted",
    expires_at: "2026-01-01T01:00:00Z",
    app_slug: "soothfast-bot",
  });
  assert.deepEqual(fake.minted, [
    { contents: "write", pull_requests: "write" },
  ]);
});

test("missing environment is refused before any GitHub call", async () => {
  const fake = fakeGitHub();
  fake.github.installationFor = async () =>
    assert.fail("must not reach GitHub");
  const { status, body } = await request({ environment: undefined }, fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /environment/);
});

test("a pull request run cannot obtain a token", async () => {
  const fake = fakeGitHub();
  const { status, body } = await request(pr, fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /use \/comment/);
  assert.deepEqual(fake.minted, []);
});

test("a push run cannot use /comment", async () => {
  const { status, body } = await request({}, fakeGitHub(), {
    route: "/comment",
    body: commentBody,
  });
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /pull_request runs only/);
});

test("/comment creates the marked comment with a pull_requests-only token and revokes it", async () => {
  const fake = fakeGitHub();
  const { status, body } = await request(pr, fake, {
    route: "/comment",
    body: commentBody,
  });
  assert.equal(status, 200);
  assert.match(body.comment_url ?? "", /issuecomment-99/);
  assert.equal(body.app_slug, "soothfast-bot");
  assert.deepEqual(fake.minted, [{ pull_requests: "write" }]);
  assert.deepEqual(fake.posted, [
    { kind: "create", body: "<!-- soothfast-gate -->\n## soothfast gate\nok" },
  ]);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("/comment updates an existing marked comment and ignores others", async () => {
  const fake = fakeGitHub({
    comments: [
      { id: 1, body: "<!-- coverage -->\nother bot", html_url: "" },
      { id: 2, body: "<!-- soothfast-gate -->\nold", html_url: "" },
    ],
  });
  const { status } = await request(pr, fake, {
    route: "/comment",
    body: commentBody,
  });
  assert.equal(status, 200);
  assert.deepEqual(fake.posted, [
    { kind: "update", body: "<!-- soothfast-gate -->\n## soothfast gate\nok" },
  ]);
});

test("/comment refuses another pull request's number", async () => {
  const fake = fakeGitHub();
  const { status, body } = await request(pr, fake, {
    route: "/comment",
    body: { ...commentBody, pull_request: 8 },
  });
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not the one this run belongs to/);
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.minted, [], "refused before any mint");
});

test("/comment validates its body", async () => {
  const fake = fakeGitHub();
  assert.equal(
    (await request(pr, fake, { route: "/comment", body: "nope" })).status,
    400,
  );
  assert.equal(
    (
      await request(pr, fake, {
        route: "/comment",
        body: { pull_request: "7" },
      })
    ).status,
    400,
  );
  const badMarker = { ...commentBody, marker: "no marker" };
  assert.equal(
    (await request(pr, fake, { route: "/comment", body: badMarker })).status,
    400,
  );
  const huge = { ...commentBody, body: "x".repeat(70000) };
  assert.equal(
    (await request(pr, fake, { route: "/comment", body: huge })).status,
    400,
  );
  assert.equal(
    (await request(pr, fake, { route: "/comment", body: "null" })).status,
    400,
  );
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.minted, [], "a rejected body must not cost a mint");
});

test("pull_request_target is refused", async () => {
  const { status } = await request(
    { event_name: "pull_request_target", ref: "refs/pull/7/merge" },
    fakeGitHub(),
  );
  assert.equal(status, 403);
});

test("a repo without the app installed is refused", async () => {
  const { status, body } = await request({}, fakeGitHub({ installed: false }));
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not installed on acme\/mylib/);
});

test("a non-default branch is refused and the minted token revoked", async () => {
  const fake = fakeGitHub({ defaultBranch: "master" });
  const { status, body } = await request({}, fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not the default branch \(master\)/);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a tag is judged by the commit that ran, not the tag name", async () => {
  const fake = fakeGitHub({ compareStatus: "behind" });
  const { status, body } = await request({ ref: "refs/tags/v1.0.0" }, fake);
  assert.equal(status, 200);
  assert.equal(body.token, "ghs_minted");
  assert.deepEqual(fake.compared, [claims().sha]);
});

test("an annotated tag's object sha is peeled to its commit before the compare", async () => {
  const objectSha = "9999999999999999999999999999999999999999";
  const fake = fakeGitHub({
    compareStatus: "behind",
    tagObjects: { [objectSha]: "abc123" },
  });
  const { status } = await request(
    { ref: "refs/tags/v1.0.0", sha: objectSha },
    fake,
  );
  assert.equal(status, 200);
  assert.deepEqual(fake.compared, ["abc123"]);
});

test("a tag whose commit is off the default branch is refused and revoked", async () => {
  const fake = fakeGitHub({ compareStatus: "diverged" });
  const { status, body } = await request({ ref: "refs/tags/v1.0.0" }, fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not on the default branch/);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a failing revoke on the wrong branch still refuses with 403", async () => {
  const fake = fakeGitHub({ defaultBranch: "master" });
  fake.github.revoke = async () => {
    throw new Error("network down");
  };
  const { status, body } = await request({}, fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not the default branch/);
});

test("an invalid signature is 401", async () => {
  const other = await generateKeys();
  const forged = await signJwt(
    other.privateKey,
    { alg: "RS256", kid: KID },
    { ...claims() },
  );
  const { status, body } = await request({}, fakeGitHub(), { token: forged });
  assert.equal(status, 401);
  assert.match(body.reason ?? "", /signature/);
});

test("other routes are 404 and missing bearer is 401", async () => {
  const fake = fakeGitHub();
  const deps = { jwks: async () => undefined, github: fake.github };
  const get = await handle(new Request("https://bot.example/token"), env, deps);
  assert.equal(get.status, 404);
  const other = await handle(
    new Request("https://bot.example/other", { method: "POST" }),
    env,
    deps,
  );
  assert.equal(other.status, 404);
  const noAuth = await handle(
    new Request("https://bot.example/comment", { method: "POST" }),
    env,
    deps,
  );
  assert.equal(noAuth.status, 401);
});

test("a broker without its secrets says so", async () => {
  const fake = fakeGitHub();
  const deps = { jwks: async () => undefined, github: fake.github };
  const res = await handle(
    new Request("https://bot.example/token", { method: "POST" }),
    { GITHUB_APP_CLIENT_ID: "", GITHUB_APP_PRIVATE_KEY: "" },
    deps,
  );
  assert.equal(res.status, 500);
  assert.match(
    ((await res.json()) as { reason: string }).reason,
    /not configured/,
  );
});
