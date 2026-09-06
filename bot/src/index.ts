import {
  appJwt,
  GitHubError,
  githubApi,
  importPrivateKey,
  type GitHubApi,
} from "./github.ts";
import { githubJwks, OidcError, verifyOidc, type JwksLookup } from "./oidc.ts";
import {
  AUDIENCE,
  COMMENT_MAX_BYTES,
  PERMISSIONS,
  decideBranch,
  decideClaims,
  decideTag,
  pullRequestNumber,
  type Decision,
  type OidcClaims,
} from "./policy.ts";

export interface Env {
  GITHUB_APP_CLIENT_ID: string;
  GITHUB_APP_PRIVATE_KEY: string;
}

export interface Deps {
  jwks: JwksLookup;
  github: GitHubApi;
  now?: () => number;
}

interface CommentRequest {
  pull_request: number;
  marker: string;
  body: string;
}

interface Minted {
  token: string;
  expires_at: string;
  slug: string;
}

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
): Promise<Response> {
  const route = request.method === "POST" ? new URL(request.url).pathname : "";
  if (route !== "/token" && route !== "/comment")
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
  if (route === "/token" && policy.mode !== "land") {
    return denied(
      403,
      `a ${claims.event_name} run cannot hold a token; use /comment`,
      claims,
    );
  }
  if (route === "/comment" && policy.mode !== "comment") {
    return denied(403, "/comment serves pull_request runs only", claims);
  }

  const body =
    route === "/comment"
      ? parseComment(await request.text(), claims)
      : undefined;
  if (body instanceof Response) return body;

  const auth = await authenticate(claims, policy.mode, env, deps);
  if (auth instanceof Response) return auth;
  return body ? comment(claims, body, auth, deps) : land(claims, auth, deps);
}

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
  const minted = await deps.github.mintToken(
    installation.id,
    Number(claims.repository_id),
    PERMISSIONS[mode],
    jwt,
  );
  const slug = await appSlug(deps.github, env.GITHUB_APP_CLIENT_ID, jwt);
  return { ...minted, slug };
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
  const ref = await decideRef(claims, minted.token, deps);
  if (!ref.ok) {
    await revoke(minted.token, deps);
    return denied(403, ref.reason, claims);
  }
  return json(200, {
    token: minted.token,
    expires_at: minted.expires_at,
    app_slug: minted.slug,
  });
}

async function decideRef(
  claims: OidcClaims,
  token: string,
  deps: Deps,
): Promise<Decision> {
  const repository = await deps.github.repository(claims.repository, token);
  if (claims.ref.startsWith("refs/tags/")) {
    const comparison = await deps.github.compare(
      claims.repository,
      repository.default_branch,
      claims.sha,
      token,
    );
    return decideTag(claims.ref, comparison.status);
  }
  return decideBranch(claims.ref, repository.default_branch);
}

// The job never sees this token: the broker posts on its behalf and revokes.
async function comment(
  claims: OidcClaims,
  body: CommentRequest,
  minted: Minted,
  deps: Deps,
): Promise<Response> {
  try {
    const text = `${body.marker}\n${body.body}`;
    const existing = (
      await deps.github.listComments(
        claims.repository,
        body.pull_request,
        minted.token,
      )
    ).find((c) => c.body.startsWith(body.marker));
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
    return json(200, { comment_url: posted.html_url, app_slug: minted.slug });
  } finally {
    await revoke(minted.token, deps);
  }
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
  const { pull_request, marker, body } = parsed as Partial<CommentRequest>;
  if (
    typeof pull_request !== "number" ||
    typeof marker !== "string" ||
    typeof body !== "string"
  ) {
    return json(400, {
      reason: "expected {pull_request: number, marker: string, body: string}",
    });
  }
  if (pull_request !== pullRequestNumber(claims.ref)) {
    return denied(
      403,
      `pull request ${pull_request} is not the one this run belongs to`,
      claims,
    );
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
  return { pull_request, marker, body };
}

const deps: Deps = { jwks: githubJwks(), github: githubApi() };

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    return handle(request, env, deps).catch((e: unknown) => {
      if (e instanceof GitHubError)
        return json(502, { reason: `GitHub API ${e.status}: ${e.message}` });
      console.error(e);
      return json(500, { reason: "internal error" });
    });
  },
} satisfies ExportedHandler<Env>;
