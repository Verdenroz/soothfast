export interface OidcClaims {
  iss: string;
  aud: string | string[];
  exp: number;
  nbf?: number;
  iat: number;
  repository: string;
  repository_id: string;
  ref: string;
  event_name: string;
  environment?: string;
}

export type Decision = { ok: true } | { ok: false; reason: string };

export const ISSUER = "https://token.actions.githubusercontent.com";
export const AUDIENCE = "soothfast-bot";
export const ENVIRONMENT = "soothfast-bot";
export const ALLOWED_EVENTS: readonly string[] = [
  "push",
  "workflow_dispatch",
  "schedule",
];

const allow: Decision = { ok: true };
const deny = (reason: string): Decision => ({ ok: false, reason });

export function decideClaims(claims: OidcClaims): Decision {
  if (!ALLOWED_EVENTS.includes(claims.event_name)) {
    return deny(
      `event ${claims.event_name} cannot mint; allowed: ${ALLOWED_EVENTS.join(", ")}`,
    );
  }
  if (claims.environment !== ENVIRONMENT) {
    return deny(`job must run in the "${ENVIRONMENT}" environment`);
  }
  if (!/^[^/\s]+\/[^/\s]+$/.test(claims.repository)) {
    return deny("repository claim is malformed");
  }
  if (!/^\d+$/.test(claims.repository_id)) {
    return deny("repository_id claim is malformed");
  }
  if (!claims.ref.startsWith("refs/heads/")) {
    return deny(`ref ${claims.ref} is not a branch`);
  }
  return allow;
}

export function decideBranch(ref: string, defaultBranch: string): Decision {
  return ref === `refs/heads/${defaultBranch}`
    ? allow
    : deny(`ref ${ref} is not the default branch (${defaultBranch})`);
}
