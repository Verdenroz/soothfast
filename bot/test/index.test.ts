import assert from "node:assert/strict";
import test from "node:test";
import {
  GitHubError,
  type GitHubApi,
  type IssueComment,
} from "../src/github.ts";
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

const ctx = { waitUntil: () => {} };

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
    GITHUB_WEBHOOK_SECRET: "hook-secret",
  };
});

interface Fake {
  github: GitHubApi;
  revoked: string[];
  minted: Record<string, string>[];
  compared: string[];
  comments: IssueComment[];
  posted: { kind: "create" | "update"; body: string }[];
  disabled: string[];
  closed: string[];
}

function fakeGitHub(
  opts: {
    installed?: boolean;
    defaultBranch?: string;
    compareStatus?: string;
    comments?: IssueComment[];
    tagObjects?: Record<string, string>;
    toml?: string;
    pulls?: object[];
    forkPull?: object | GitHubError;
    slugFails?: boolean;
  } = {},
): Fake {
  const fake: Fake = {
    revoked: [],
    minted: [],
    compared: [],
    comments: opts.comments ?? [],
    posted: [],
    disabled: [],
    closed: [],
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
      async fileContents() {
        return opts.toml;
      },
      async openPullRequests() {
        return (opts.pulls ?? []) as never;
      },
      async pullRequest() {
        if (opts.forkPull instanceof GitHubError) throw opts.forkPull;
        if (opts.forkPull) return opts.forkPull as never;
        return {
          ...(opts.pulls?.[0] as object),
          state: "open",
          mergeable: false,
        } as never;
      },
      async closePullRequest(_repository, number) {
        fake.closed.push(`close ${number}`);
      },
      async disableAutoMerge(id) {
        fake.disabled.push(id);
      },
      async revoke(token) {
        fake.revoked.push(token);
      },
      async appSlug() {
        if (opts.slugFails) throw new GitHubError(500, "boom");
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
  opts: { route?: string; token?: string; body?: unknown; env?: Env } = {},
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
  const res = await handle(req, opts.env ?? env, deps, ctx);
  return { status: res.status, body: (await res.json()) as Body };
}

const pr = { event_name: "pull_request", ref: "refs/pull/7/merge" };
const run = { event_name: "workflow_run" };
const headSha = "fedcba9876543210fedcba9876543210fedcba98";
const forkPull = {
  state: "open",
  head: {
    sha: headSha,
    ref: "patch-1",
    repo: { id: 999, full_name: "fork/mylib" },
  },
  base: { repo: { id: 12345, full_name: "acme/mylib" } },
};
const bot = { login: "soothfast-bot[bot]" };
const commentBody = {
  pull_request: 7,
  marker: "<!-- soothfast-gate -->",
  body: "## soothfast gate\nok",
};
const runComment = { ...commentBody, head_sha: headSha };

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
  assert.match(body.reason ?? "", /pull_request and workflow_run runs only/);
});

test("a workflow_run job can neither mint a token nor sweep", async () => {
  for (const route of ["/token", "/sweep"]) {
    const fake = fakeGitHub();
    const { status } = await request(run, fake, { route });
    assert.equal(status, 403, route);
    assert.deepEqual(fake.minted, [], route);
  }
});

async function runRequest(
  fake: Fake,
  body: unknown = runComment,
  overrides: Parameters<typeof claims>[0] = {},
) {
  return request({ ...run, ...overrides }, fake, { route: "/comment", body });
}

test("a workflow_run comment on a fork pull request posts and revokes", async () => {
  const fake = fakeGitHub({ forkPull });
  const { status, body } = await runRequest(fake);
  assert.equal(status, 200);
  assert.match(body.comment_url ?? "", /issuecomment-99/);
  assert.deepEqual(fake.minted, [{ pull_requests: "write" }]);
  assert.deepEqual(fake.posted, [
    { kind: "create", body: "<!-- soothfast-gate -->\n## soothfast gate\nok" },
  ]);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a workflow_run comment needs head_sha before any mint", async () => {
  const fake = fakeGitHub({ forkPull });
  for (const body of [commentBody, { ...commentBody, head_sha: 7 }]) {
    const { status, body: reply } = await runRequest(fake, body);
    assert.equal(status, 400);
    assert.match(reply.reason ?? "", /head_sha/);
  }
  assert.deepEqual(fake.minted, []);
});

test("a workflow_run comment rejects a head_sha that is not 40 lowercase hex", async () => {
  const fake = fakeGitHub({ forkPull });
  const bad = [
    headSha.slice(0, 7),
    headSha.slice(0, 39),
    `${headSha}0`,
    headSha.toUpperCase(),
    `${headSha}\n`,
    ` ${headSha}`,
    `g${headSha.slice(1)}`,
    "",
    null,
    [headSha],
  ];
  for (const head_sha of bad) {
    const { status } = await runRequest(fake, { ...commentBody, head_sha });
    assert.equal(status, 400, JSON.stringify(head_sha));
  }
  assert.deepEqual(fake.minted, []);
});

test("a workflow_run comment is refused on a ref that only resembles the default branch", async () => {
  const refs = [
    "refs/heads/main2",
    "refs/heads/feature/main",
    "refs/heads/Main",
    "refs/heads/MAIN",
    "refs/heads/refs/heads/main",
    "refs/heads/main/",
    "refs/heads/",
  ];
  for (const ref of refs) {
    const fake = fakeGitHub({ forkPull });
    const { status, body } = await runRequest(fake, runComment, { ref });
    assert.equal(status, 403, ref);
    assert.match(body.reason ?? "", /not the default branch \(main\)/, ref);
    assert.deepEqual(fake.posted, [], ref);
    assert.deepEqual(fake.revoked, ["ghs_minted"], ref);
  }
  for (const ref of ["refs/tags/main", "refs/pull/7/merge", "main"]) {
    const fake = fakeGitHub({ forkPull, compareStatus: "identical" });
    const { status } = await runRequest(fake, runComment, { ref });
    assert.equal(status, 403, ref);
    assert.deepEqual(fake.minted, [], ref);
  }
});

test("a workflow_run comment revokes when the pull request lookup fails", async () => {
  const fake = fakeGitHub({ forkPull: new GitHubError(500, "boom") });
  await assert.rejects(runRequest(fake), GitHubError);
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("nothing is minted when the app slug lookup fails", async () => {
  const fake = fakeGitHub({ forkPull, slugFails: true });
  const fresh = { ...env, GITHUB_APP_CLIENT_ID: "Iv1.uncached" };
  const opts = { route: "/comment", body: runComment, env: fresh };
  await assert.rejects(request(run, fake, opts), GitHubError);
  assert.deepEqual(fake.minted, []);
});

test("a workflow_run comment from another environment is refused", async () => {
  const fake = fakeGitHub({ forkPull });
  const { status } = await runRequest(fake, runComment, {
    environment: "production",
  });
  assert.equal(status, 403);
  assert.deepEqual(fake.minted, []);
});

test("a workflow_run comment off the default branch is refused and revoked", async () => {
  const fake = fakeGitHub({ forkPull, defaultBranch: "master" });
  const { status, body } = await runRequest(fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not the default branch \(master\)/);
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a workflow_run comment on a closed pull request is refused and revoked", async () => {
  const fake = fakeGitHub({ forkPull: { ...forkPull, state: "closed" } });
  const { status, body } = await runRequest(fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /pull request 7 is not open/);
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a workflow_run comment on a same-repository pull request is refused", async () => {
  const sameRepo = {
    ...forkPull,
    head: { ...forkPull.head, repo: forkPull.base.repo },
  };
  const fake = fakeGitHub({ forkPull: sameRepo });
  const { status, body } = await runRequest(fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /from this repository/);
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a workflow_run comment for a stale head is refused", async () => {
  const fake = fakeGitHub({ forkPull });
  const stale = { ...runComment, head_sha: "0".repeat(40) };
  const { status, body } = await runRequest(fake, stale);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /head moved/);
  assert.deepEqual(fake.posted, []);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a workflow_run comment on an unknown pull request is refused", async () => {
  const fake = fakeGitHub({ forkPull: new GitHubError(404, "Not Found") });
  const { status, body } = await runRequest(fake);
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /pull request 7 does not exist/);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
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
      { id: 1, body: "<!-- coverage -->\nother bot", html_url: "", user: bot },
      { id: 2, body: "<!-- soothfast-gate -->\nold", html_url: "", user: bot },
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

test("/comment never overwrites a marked comment the bot did not write", async () => {
  const marked = { body: "<!-- soothfast-gate -->\nmine", html_url: "" };
  for (const claim of [pr, run]) {
    const fake = fakeGitHub({
      forkPull,
      comments: [
        { ...marked, id: 1, user: { login: "mallory" } },
        { ...marked, id: 2, user: null },
        { ...marked, id: 3, user: { login: "soothfast-bot" } },
      ],
    });
    const { status } = await request(claim, fake, {
      route: "/comment",
      body: runComment,
    });
    assert.equal(status, 200);
    assert.deepEqual(
      fake.posted.map((p) => p.kind),
      ["create"],
    );
  }
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

test("/comment rejects a malformed request before any mint", async () => {
  const numbers = ['"7"', "7.5", "-7", "0", "1e21", "[7]", "null", "true"];
  const bodies = [
    ...numbers.map((n) => JSON.stringify(runComment).replace("7", n)),
    JSON.stringify([runComment]),
    JSON.stringify({ ...runComment, marker: 7 }),
    JSON.stringify({ ...runComment, body: ["x"] }),
    JSON.stringify({ marker: runComment.marker, body: "x" }),
    `{"__proto__":${JSON.stringify(runComment)}}`,
    JSON.stringify({ ...runComment, body: "x".repeat(140000) }),
    "{",
  ];
  for (const claim of [pr, run]) {
    const fake = fakeGitHub({ forkPull });
    for (const body of bodies) {
      const { status } = await request(claim, fake, {
        route: "/comment",
        body,
      });
      assert.equal(status, 400, `${claim.event_name}: ${body.slice(0, 60)}`);
    }
    assert.deepEqual(fake.minted, []);
  }
});

test("/comment takes an integer written as 1e3", async () => {
  const fake = fakeGitHub({ forkPull });
  const body = JSON.stringify(runComment).replace("7", "1e3");
  assert.equal((await runRequest(fake, body)).status, 200);
});

test("a pull_request comment ignores head_sha", async () => {
  for (const head_sha of [7, "nope", "0".repeat(40)]) {
    const fake = fakeGitHub();
    const { status } = await request(pr, fake, {
      route: "/comment",
      body: { ...commentBody, head_sha },
    });
    assert.equal(status, 200);
  }
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
  // action/bot-token.sh matches this exact prefix to warn instead of failing.
  assert.equal(body.reason, "soothfast-bot is not installed on acme/mylib");
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
  const get = await handle(
    new Request("https://bot.example/token"),
    env,
    deps,
    ctx,
  );
  assert.equal(get.status, 404);
  const other = await handle(
    new Request("https://bot.example/other", { method: "POST" }),
    env,
    deps,
    ctx,
  );
  assert.equal(other.status, 404);
  const noAuth = await handle(
    new Request("https://bot.example/comment", { method: "POST" }),
    env,
    deps,
    ctx,
  );
  assert.equal(noAuth.status, 401);
});

test("a broker without its secrets says so", async () => {
  const fake = fakeGitHub();
  const deps = { jwks: async () => undefined, github: fake.github };
  const res = await handle(
    new Request("https://bot.example/token", { method: "POST" }),
    {
      GITHUB_APP_CLIENT_ID: "",
      GITHUB_APP_PRIVATE_KEY: "",
      GITHUB_WEBHOOK_SECRET: "",
    },
    deps,
    ctx,
  );
  assert.equal(res.status, 500);
  assert.match(
    ((await res.json()) as { reason: string }).reason,
    /not configured/,
  );
});

const sweepable = {
  toml: "[bot]\nlabels = true\n",
  pulls: [
    {
      number: 3,
      node_id: "PR_node3",
      draft: false,
      labels: [{ name: "do-not-merge" }],
      auto_merge: {},
      user: { login: "alice" },
      head: { sha: "abc123" },
      updated_at: "2026-01-01T00:00:00Z",
    },
  ],
};

test("a sweep acts on open pull requests, returns its log and revokes", async () => {
  const fake = fakeGitHub(sweepable);
  const { status, body } = await request({}, fake, { route: "/sweep" });
  assert.equal(status, 200);
  assert.deepEqual((body as { log: string[] }).log, [
    "#3: auto-merge cancelled",
  ]);
  assert.deepEqual(fake.disabled, ["PR_node3"]);
  assert.deepEqual(fake.minted, [
    { contents: "write", pull_requests: "write" },
  ]);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a sweep closes a conflicting landing the app opened", async () => {
  const landing = {
    ...sweepable.pulls[0],
    labels: [],
    auto_merge: null,
    user: { login: "soothfast-bot[bot]" },
    head: {
      sha: "abc123",
      ref: "bot/soothfast-update",
      repo: { full_name: "acme/mylib" },
    },
    base: { repo: { full_name: "acme/mylib" } },
  };
  const fake = fakeGitHub({ ...sweepable, pulls: [landing] });
  const { status } = await request({}, fake, { route: "/sweep" });
  assert.equal(status, 200);
  assert.deepEqual(fake.closed, ["close 3"]);
});

test("a sweep from a schedule run is served", async () => {
  const fake = fakeGitHub(sweepable);
  const { status } = await request({ event_name: "schedule" }, fake, {
    route: "/sweep",
  });
  assert.equal(status, 200);
});

test("a sweep refuses pull_request runs without minting", async () => {
  const fake = fakeGitHub(sweepable);
  const { status, body } = await request(pr, fake, { route: "/sweep" });
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /\/sweep serves/);
  assert.deepEqual(fake.minted, []);
});

test("a sweep from a non-default branch is refused and revoked", async () => {
  const fake = fakeGitHub({ ...sweepable, defaultBranch: "master" });
  const { status, body } = await request({}, fake, { route: "/sweep" });
  assert.equal(status, 403);
  assert.match(body.reason ?? "", /not the default branch/);
  assert.deepEqual(fake.disabled, []);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});

test("a sweep revokes its token when evaluation fails", async () => {
  const fake = fakeGitHub(sweepable);
  fake.github.openPullRequests = async () => {
    throw new GitHubError(500, "boom");
  };
  await assert.rejects(request({}, fake, { route: "/sweep" }), GitHubError);
  assert.deepEqual(fake.revoked, ["ghs_minted"]);
});
