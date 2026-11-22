import {
  appJwt,
  GitHubError,
  githubApi,
  importPrivateKey,
  mintRefused,
  type GitHubApi,
  type Repository,
} from "./github.ts";
import {
  evaluateOpen,
  evaluatePullRequest,
  cachedOff,
  type Run,
} from "./labels.ts";
import { githubJwks, OidcError, verifyOidc, type JwksLookup } from "./oidc.ts";
import {
  AUDIENCE,
  COMMENT_MAX_BYTES,
  PERMISSIONS,
  RUN_PERMISSIONS,
  decideBranch,
  decideClaims,
  decideForkPullRequest,
  decideRun,
  decideTag,
  pullRequestNumber,
  type Decision,
  type OidcClaims,
} from "./policy.ts";
import { receive, type Work } from "./webhook.ts";

export interface Env {
  GITHUB_APP_CLIENT_ID: string;
  GITHUB_APP_PRIVATE_KEY: string;
  GITHUB_WEBHOOK_SECRET: string;
}

interface Deps {
  jwks: JwksLookup;
  github: GitHubApi;
  now?: () => number;
}

interface CommentRequest {
  pull_request: number;
  marker: string;
  body: string;
  head_sha?: string;
  run_id?: number;
}

interface Minted {
  token: string;
  expires_at: string;
  slug: string;
  actionsReadable: boolean;
}

type Ctx = Pick<ExecutionContext, "waitUntil">;

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });

function denied(status: number, reason: string, claims?: OidcClaims): Response {
  console.log(
    JSON.stringify({
      denied: reason,
      repository: claims?.repository,
      ref: claims?.ref,
      event_name: claims?.event_name,
      environment: claims?.environment,
    }),
  );
  return json(status, { reason });
}

export async function handle(
  request: Request,
  env: Env,
  deps: Deps,
  ctx: Ctx,
): Promise<Response> {
  if (!env.GITHUB_APP_CLIENT_ID || !env.GITHUB_APP_PRIVATE_KEY) {
    console.error("GITHUB_APP_CLIENT_ID or GITHUB_APP_PRIVATE_KEY is not set");
    return json(500, {
      reason:
        "broker is not configured: set GITHUB_APP_CLIENT_ID and GITHUB_APP_PRIVATE_KEY",
    });
  }
  const route = request.method === "POST" ? new URL(request.url).pathname : "";
  if (route === "/webhook") return webhook(request, env, deps, ctx);
  if (route !== "/token" && route !== "/comment" && route !== "/sweep")
    return json(404, { reason: "not found" });

  const authorization = request.headers.get("authorization") ?? "";
  if (!authorization.startsWith("Bearer "))
    return denied(401, "missing bearer token");
  const verify = verifyOidc(authorization.slice("Bearer ".length), {
    audience: AUDIENCE,
    jwks: deps.jwks,
    now: deps.now?.(),
  });
  const claims = await verify.catch((e: unknown) =>
    e instanceof OidcError ? e : Promise.reject(e),
  );
  if (claims instanceof OidcError) return denied(401, claims.message);

  const policy = decideClaims(claims);
  if (!policy.ok) return denied(403, policy.reason, claims);
  if (route === "/sweep" && policy.mode !== "land") {
    return denied(
      403,
      "/sweep serves push, schedule and workflow_dispatch runs only",
      claims,
    );
  }
  if (route === "/token" && policy.mode !== "land") {
    return denied(
      403,
      `a ${claims.event_name} run cannot hold a token; use /comment`,
      claims,
    );
  }
  if (route === "/comment" && policy.mode !== "comment") {
    return denied(
      403,
      "/comment serves pull_request and workflow_run runs only",
      claims,
    );
  }

  const body =
    route === "/comment"
      ? parseComment(await request.text(), claims)
      : undefined;
  if (body instanceof Response) return body;

  const auth = await authenticate(claims, policy.mode, env, deps);
  if (auth instanceof Response) return auth;
  if (body) return comment(claims, body, auth, deps);
  return route === "/sweep"
    ? sweep(claims, auth, deps)
    : land(claims, auth, deps);
}

async function webhook(
  request: Request,
  env: Env,
  deps: Deps,
  ctx: Ctx,
): Promise<Response> {
  if (!env.GITHUB_WEBHOOK_SECRET) {
    console.error("GITHUB_WEBHOOK_SECRET is not set");
    return json(500, {
      reason: "broker is not configured: set GITHUB_WEBHOOK_SECRET",
    });
  }
  const delivery = await receive(request, env.GITHUB_WEBHOOK_SECRET);
  if (!delivery.ok) {
    console.log(JSON.stringify({ denied: delivery.reason, route: "/webhook" }));
    return json(delivery.status, { reason: delivery.reason });
  }
  if (!delivery.work) return json(200, {});
  const done = act(delivery.work, env, deps).catch((e: unknown) =>
    console.error("webhook work failed", e),
  );
  ctx.waitUntil(done);
  return json(202, {});
}

async function act(work: Work, env: Env, deps: Deps): Promise<void> {
  const { target } = work;
  const fresh = work.kind === "push" && work.configChanged;
  const off = fresh
    ? undefined
    : cachedOff(target.repositoryId, target.defaultBranch);
  if (off) {
    console.log(`${target.repository}: ${off}`);
    return;
  }
  const jwt = await appJwt(
    env.GITHUB_APP_CLIENT_ID,
    await privateKey(env.GITHUB_APP_PRIVATE_KEY),
    deps.now?.(),
  );
  const { token } = await deps.github.mintToken(
    target.installation,
    target.repositoryId,
    PERMISSIONS.land,
    jwt,
  );
  const run = makeRun(
    deps,
    token,
    target.repositoryId,
    target.repository,
    target.defaultBranch,
    fresh,
    undefined,
    (m) => console.log(`${target.repository}: ${m}`),
  );
  try {
    if (work.kind === "pull_request")
      await evaluatePullRequest(run, work.pr, work.labeler);
    else await evaluateOpen(run);
  } finally {
    await revoke(token, deps);
  }
}

const makeRun = (
  deps: Deps,
  token: string,
  repositoryId: number,
  repository: string,
  defaultBranch: string,
  fresh: boolean,
  botLogin: string | undefined,
  log: Run["log"],
): Run => ({
  github: deps.github,
  token,
  repositoryId,
  repository,
  defaultBranch,
  fresh,
  botLogin,
  log,
});

let cachedKey: { pem: string; key: CryptoKey } | undefined;
let cachedSlug: { clientId: string; slug: string } | undefined;

async function privateKey(pem: string): Promise<CryptoKey> {
  if (cachedKey?.pem !== pem)
    cachedKey = { pem, key: await importPrivateKey(pem) };
  return cachedKey.key;
}

async function appSlug(
  github: GitHubApi,
  clientId: string,
  jwt: string,
): Promise<string> {
  if (cachedSlug?.clientId !== clientId)
    cachedSlug = { clientId, slug: await github.appSlug(jwt) };
  return cachedSlug.slug;
}

async function authenticate(
  claims: OidcClaims,
  mode: keyof typeof PERMISSIONS,
  env: Env,
  deps: Deps,
): Promise<Minted | Response> {
  const jwt = await appJwt(
    env.GITHUB_APP_CLIENT_ID,
    await privateKey(env.GITHUB_APP_PRIVATE_KEY),
    deps.now?.(),
  );
  const installation = await deps.github.installationFor(
    claims.repository,
    jwt,
  );
  if (!installation) {
    return denied(
      403,
      `soothfast-bot is not installed on ${claims.repository}`,
      claims,
    );
  }
  const slug = await appSlug(deps.github, env.GITHUB_APP_CLIENT_ID, jwt);
  const mint = (permissions: Record<string, string>) =>
    deps.github.mintToken(
      installation.id,
      Number(claims.repository_id),
      permissions,
      jwt,
    );
  if (claims.event_name === "workflow_run") {
    const withActions = await mint(RUN_PERMISSIONS).catch((e: unknown) =>
      mintRefused(e) ? undefined : Promise.reject(e),
    );
    if (withActions) return { ...withActions, slug, actionsReadable: true };
  }
  const minted = await mint(PERMISSIONS[mode]);
  return { ...minted, slug, actionsReadable: false };
}

async function revoke(token: string, deps: Deps): Promise<void> {
  await deps.github
    .revoke(token)
    .catch((e: unknown) => console.error("revoke failed", e));
}

async function land(
  claims: OidcClaims,
  minted: Minted,
  deps: Deps,
): Promise<Response> {
  const { decision } = await resolveRef(claims, minted.token, deps);
  if (!decision.ok) {
    await revoke(minted.token, deps);
    return denied(403, decision.reason, claims);
  }
  return json(200, {
    token: minted.token,
    expires_at: minted.expires_at,
    app_slug: minted.slug,
  });
}

async function resolveRef(
  claims: OidcClaims,
  token: string,
  deps: Deps,
): Promise<{ decision: Decision; repository: Repository }> {
  const repository = await deps.github.repository(claims.repository, token);
  if (!claims.ref.startsWith("refs/tags/"))
    return {
      decision: decideBranch(claims.ref, repository.default_branch),
      repository,
    };
  // An annotated tag's sha names the tag object; judge the commit under it.
  // Tag objects are immutable, so peeling keeps the sha claim authoritative.
  const commit =
    (await deps.github.tagObject(claims.repository, claims.sha, token)) ??
    claims.sha;
  const comparison = await deps.github.compare(
    claims.repository,
    repository.default_branch,
    commit,
    token,
  );
  return { decision: decideTag(claims.ref, comparison.status), repository };
}

async function sweep(
  claims: OidcClaims,
  minted: Minted,
  deps: Deps,
): Promise<Response> {
  try {
    const { decision, repository } = await resolveRef(
      claims,
      minted.token,
      deps,
    );
    if (!decision.ok) return denied(403, decision.reason, claims);
    const log: string[] = [];
    const run = makeRun(
      deps,
      minted.token,
      repository.id,
      claims.repository,
      repository.default_branch,
      true,
      `${minted.slug}[bot]`,
      (message) => log.push(message),
    );
    await evaluateOpen(run);
    return json(200, { log });
  } finally {
    await revoke(minted.token, deps);
  }
}

// The job never sees this token: the broker posts on its behalf and revokes.
async function comment(
  claims: OidcClaims,
  body: CommentRequest,
  minted: Minted,
  deps: Deps,
): Promise<Response> {
  let runVerified: boolean | undefined;
  try {
    if (claims.event_name === "workflow_run") {
      const checked = await decideWorkflowRun(claims, body, minted, deps);
      if (!checked.decision.ok)
        return denied(403, checked.decision.reason, claims);
      runVerified = checked.runVerified;
    }
    const text = `${body.marker}\n${body.body}`;
    const author = `${minted.slug}[bot]`;
    const existing = (
      await deps.github.listComments(
        claims.repository,
        body.pull_request,
        minted.token,
      )
    ).find((c) => c.user?.login === author && c.body.startsWith(body.marker));
    const posted = existing
      ? await deps.github.updateComment(
          claims.repository,
          existing.id,
          text,
          minted.token,
        )
      : await deps.github.createComment(
          claims.repository,
          body.pull_request,
          text,
          minted.token,
        );
    return json(200, {
      comment_url: posted.html_url,
      app_slug: minted.slug,
      ...(runVerified === undefined ? {} : { run_verified: runVerified }),
    });
  } finally {
    await revoke(minted.token, deps);
  }
}

async function decideWorkflowRun(
  claims: OidcClaims,
  body: CommentRequest,
  minted: Minted,
  deps: Deps,
): Promise<{ decision: Decision; runVerified: boolean }> {
  const { token } = minted;
  const { decision } = await resolveRef(claims, token, deps);
  if (!decision.ok) return { decision, runVerified: false };
  const found = await deps.github
    .pullRequest(claims.repository, body.pull_request, token)
    .catch((e: unknown) =>
      e instanceof GitHubError && e.status === 404
        ? undefined
        : Promise.reject(e),
    );
  const forked = decideForkPullRequest(body.pull_request, found, body.head_sha);
  if (!forked.ok || !found || !minted.actionsReadable || !body.run_id)
    return { decision: forked, runVerified: false };
  const triggering = await deps.github.workflowRun(
    claims.repository,
    body.run_id,
    token,
  );
  const checked = decideRun(
    triggering,
    Number(claims.repository_id),
    body.head_sha,
    found,
  );
  return { decision: checked, runVerified: checked.ok };
}

function parseComment(
  raw: string,
  claims: OidcClaims,
): CommentRequest | Response {
  if (raw.length > COMMENT_MAX_BYTES * 2)
    return json(400, { reason: "request body too large" });
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return json(400, { reason: "body is not JSON" });
  }
  if (typeof parsed !== "object" || parsed === null)
    return json(400, { reason: "body is not an object" });
  const { pull_request, marker, body, head_sha, run_id } =
    parsed as Partial<CommentRequest>;
  if (
    typeof pull_request !== "number" ||
    !Number.isSafeInteger(pull_request) ||
    pull_request <= 0 ||
    typeof marker !== "string" ||
    typeof body !== "string"
  ) {
    return json(400, {
      reason:
        "expected {pull_request: positive integer, marker: string, body: string}",
    });
  }
  const forFork = claims.event_name === "workflow_run";
  if (forFork) {
    if (typeof head_sha !== "string" || !/^[0-9a-f]{40}$/.test(head_sha)) {
      return json(400, {
        reason:
          "workflow_run comments require head_sha: the 40-hex pull request head",
      });
    }
  } else if (pull_request !== pullRequestNumber(claims.ref)) {
    return denied(
      403,
      `pull request ${pull_request} is not the one this run belongs to`,
      claims,
    );
  }
  if (
    run_id !== undefined &&
    (typeof run_id !== "number" || !Number.isSafeInteger(run_id) || run_id <= 0)
  ) {
    return json(400, { reason: "run_id must be a positive integer" });
  }
  if (!/^<!-- [\w-]+ -->$/.test(marker)) {
    return json(400, {
      reason: "marker must be an HTML comment like <!-- soothfast-gate -->",
    });
  }
  if (
    new TextEncoder().encode(`${marker}\n${body}`).length > COMMENT_MAX_BYTES
  ) {
    return json(400, { reason: `comment exceeds ${COMMENT_MAX_BYTES} bytes` });
  }
  return {
    pull_request,
    marker,
    body,
    head_sha: forFork ? head_sha : undefined,
    run_id: forFork ? run_id : undefined,
  };
}

const deps: Deps = { jwks: githubJwks(), github: githubApi() };

export default {
  async fetch(
    request: Request,
    env: Env,
    ctx: ExecutionContext,
  ): Promise<Response> {
    return handle(request, env, deps, ctx).catch((e: unknown) => {
      if (e instanceof GitHubError)
        return json(502, { reason: `GitHub API ${e.status}: ${e.message}` });
      console.error(e);
      return json(500, { reason: "internal error" });
    });
  },
} satisfies ExportedHandler<Env>;
