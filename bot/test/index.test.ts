import assert from "node:assert/strict";
import test from "node:test";
import type { GitHubApi } from "../src/github.ts";
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
}

function fakeGitHub(
  opts: { installed?: boolean; defaultBranch?: string } = {},
): Fake {
  const revoked: string[] = [];
  const github: GitHubApi = {
    async installationFor() {
      return opts.installed === false ? undefined : { id: 7 };
    },
    async mintToken(installationId, repositoryId) {
      assert.equal(installationId, 7);
      assert.equal(repositoryId, 12345);
      return { token: "ghs_minted", expires_at: "2026-01-01T01:00:00Z" };
    },
    async repository() {
      return { default_branch: opts.defaultBranch ?? "main" };
    },
    async revoke(token) {
      revoked.push(token);
    },
    async appSlug() {
      return "soothfast-bot";
    },
  };
  return { github, revoked };
}

async function request(
  overrides: Parameters<typeof claims>[0],
  fake: Fake,
  token?: string,
) {
  const jwt =
    token ??
    (await signJwt(
      keys.privateKey,
      { alg: "RS256", kid: KID },
      { ...claims(overrides) },
    ));
  const req = new Request("https://bot.example/token", {
    method: "POST",
    headers: { authorization: `Bearer ${jwt}` },
  });
  const deps = {
    jwks: async (kid: string) => (kid === KID ? keys.jwk : undefined),
    github: fake.github,
    now: () => NOW,
  };
  const res = await handle(req, env, deps);
  return {
    status: res.status,
    body: (await res.json()) as {
      token?: string;
      app_slug?: string;
      reason?: string;
    },
  };
}

test("allowed claims mint a scoped token", async () => {
  const { status, body } = await request({}, fakeGitHub());
  assert.equal(status, 200);
  assert.deepEqual(body, {
    token: "ghs_minted",
    expires_at: "2026-01-01T01:00:00Z",
    app_slug: "soothfast-bot",
  });
});

test("missing environment is refused before any GitHub call", async () => {
  const fake = fakeGitHub();
  fake.github.installationFor = async () =>
    assert.fail("must not reach GitHub");
  const { status, body } = await request({ environment: undefined }, fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /environment/);
});

test("pull_request is refused", async () => {
  const { status } = await request(
    { event_name: "pull_request" },
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

test("an invalid signature is 401", async () => {
  const other = await generateKeys();
  const forged = await signJwt(
    other.privateKey,
    { alg: "RS256", kid: KID },
    { ...claims() },
  );
  const { status, body } = await request({}, fakeGitHub(), forged);
  assert.equal(status, 401);
  assert.match(body.reason ?? "", /signature/);
});

test("other routes are 404 and missing bearer is 401", async () => {
  const fake = fakeGitHub();
  const deps = { jwks: async () => undefined, github: fake.github };
  const get = await handle(new Request("https://bot.example/token"), env, deps);
  assert.equal(get.status, 404);
  const noAuth = await handle(
    new Request("https://bot.example/token", { method: "POST" }),
    env,
    deps,
  );
  assert.equal(noAuth.status, 401);
});
