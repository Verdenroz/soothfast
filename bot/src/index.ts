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
  decideBranch,
  decideClaims,
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
  if (request.method !== "POST" || new URL(request.url).pathname !== "/token") {
    return json(404, { reason: "not found" });
  }
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
  return mint(claims, env, deps);
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

async function mint(
  claims: OidcClaims,
  env: Env,
  deps: Deps,
): Promise<Response> {
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
    jwt,
  );
  const repository = await deps.github.repository(
    claims.repository,
    minted.token,
  );
  const branch = decideBranch(claims.ref, repository.default_branch);
  if (!branch.ok) {
    await deps.github
      .revoke(minted.token)
      .catch((e: unknown) => console.error("revoke failed", e));
    return denied(403, branch.reason, claims);
  }
  const slug = await appSlug(deps.github, env.GITHUB_APP_CLIENT_ID, jwt);
  return json(200, {
    token: minted.token,
    expires_at: minted.expires_at,
    app_slug: slug,
  });
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
