export interface OidcClaims {
  iss: string;
  aud: string | string[];
  exp: number;
  nbf?: number;
  iat: number;
  repository: string;
  repository_id: string;
  ref: string;
  sha: string;
  event_name: string;
  environment?: string;
}

export type Mode = "land" | "comment";

export type Decision = { ok: true; mode: Mode } | { ok: false; reason: string };

export const ISSUER = "https://token.actions.githubusercontent.com";
export const AUDIENCE = "soothfast-bot";
export const ENVIRONMENT = "soothfast-bot";
export const LAND_EVENTS: readonly string[] = [
  "push",
  "workflow_dispatch",
  "schedule",
];
export const COMMENT_EVENTS: readonly string[] = ["pull_request"];

// A landing token is handed to the job. A pull request runs code nobody has
// merged yet, so it never receives a token: the broker posts the comment
// itself with a short-lived token of its own.
export const PERMISSIONS: Record<Mode, Record<string, string>> = {
  land: { contents: "write", pull_requests: "write" },
  comment: { pull_requests: "write" },
};

export const COMMENT_MAX_BYTES = 65536;

const deny = (reason: string): Decision => ({ ok: false, reason });

export function decideClaims(claims: OidcClaims): Decision {
  const mode = modeFor(claims.event_name);
  if (mode === undefined) {
    const allowed = [...LAND_EVENTS, ...COMMENT_EVENTS].join(", ");
    return deny(`event ${claims.event_name} cannot mint; allowed: ${allowed}`);
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
  if (mode === "comment" && pullRequestNumber(claims.ref) === undefined) {
    return deny(`ref ${claims.ref} is not a pull request merge ref`);
  }
  if (
    mode === "land" &&
    !claims.ref.startsWith("refs/heads/") &&
    !claims.ref.startsWith("refs/tags/")
  ) {
    return deny(`ref ${claims.ref} is neither a branch nor a tag`);
  }
  return { ok: true, mode };
}

function modeFor(event: string): Mode | undefined {
  if (LAND_EVENTS.includes(event)) return "land";
  if (COMMENT_EVENTS.includes(event)) return "comment";
  return undefined;
}

export function decideBranch(ref: string, defaultBranch: string): Decision {
  return ref === `refs/heads/${defaultBranch}`
    ? { ok: true, mode: "land" }
    : deny(`ref ${ref} is not the default branch (${defaultBranch})`);
}

// Compare status of default...sha for the commit the job actually ran:
// "behind" or "identical" means it is already on the default branch. The
// tag name is never consulted, since a tag can be moved after the run starts.
export function decideTag(ref: string, compareStatus: string): Decision {
  return compareStatus === "behind" || compareStatus === "identical"
    ? { ok: true, mode: "land" }
    : deny(
        `tag ${ref} points at a commit that is not on the default branch (compare status ${compareStatus})`,
      );
}

export function pullRequestNumber(ref: string): number | undefined {
  const match = /^refs\/pull\/(\d+)\/merge$/.exec(ref);
  return match ? Number(match[1]) : undefined;
}
