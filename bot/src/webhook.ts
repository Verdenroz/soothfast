import { utf8 } from "./encoding.ts";
import type { PullRequest } from "./github.ts";

// GitHub never delivers more than 25 MiB.
export const WEBHOOK_MAX_BYTES = 25 * 1024 * 1024;

const PULL_REQUEST_ACTIONS: readonly string[] = [
  "opened",
  "reopened",
  "labeled",
  "unlabeled",
  "ready_for_review",
  "synchronize",
];

interface Target {
  installation: number;
  repositoryId: number;
  repository: string;
  defaultBranch: string;
}

export type Work =
  | { kind: "pull_request"; target: Target; pr: PullRequest; labeler?: Labeler }
  | { kind: "push"; target: Target; configChanged: boolean };

export interface Labeler {
  label: string;
  login: string;
}

type Delivery =
  { ok: true; work?: Work } | { ok: false; status: number; reason: string };

interface Payload {
  action?: string;
  ref?: string;
  deleted?: boolean;
  forced?: boolean;
  commits?: { added: string[]; modified: string[]; removed: string[] }[];
  label?: { name: string };
  sender: { login: string };
  installation: { id: number };
  repository: { id: number; full_name: string; default_branch: string };
  pull_request?: PullRequest & { state: string; base: { ref: string } };
}

export async function receive(
  request: Request,
  secret: string,
): Promise<Delivery> {
  const declared = Number(request.headers.get("content-length") ?? 0);
  if (declared > WEBHOOK_MAX_BYTES) return reject(413, "payload too large");
  const body = new Uint8Array(await request.arrayBuffer());
  if (body.length > WEBHOOK_MAX_BYTES) return reject(413, "payload too large");
  const signature = request.headers.get("x-hub-signature-256");
  if (!(await verifySignature(secret, body, signature)))
    return reject(401, "bad signature");
  let payload: unknown;
  try {
    payload = JSON.parse(new TextDecoder().decode(body));
  } catch {
    return reject(400, "body is not JSON");
  }
  const event = request.headers.get("x-github-event") ?? "";
  return { ok: true, work: route(event, payload as Payload) };
}

const reject = (status: number, reason: string): Delivery => ({
  ok: false,
  status,
  reason,
});

export async function verifySignature(
  secret: string,
  body: Uint8Array,
  header: string | null,
): Promise<boolean> {
  const match = /^sha256=([0-9a-f]{64})$/.exec(header ?? "");
  if (!match) return false;
  const key = await crypto.subtle.importKey(
    "raw",
    utf8(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"],
  );
  const expected = Uint8Array.from(match[1].match(/../g) ?? [], (h) =>
    parseInt(h, 16),
  );
  return crypto.subtle.verify("HMAC", key, expected, body);
}

// A push payload lists at most 2048 commits, so a full list may hide the one
// that edits it; a force-push can drop that commit from the list entirely.
const MAX_LISTED_COMMITS = 2048;

const touchesConfig = ({ commits, forced }: Payload) =>
  forced === true ||
  !commits?.length ||
  commits.length >= MAX_LISTED_COMMITS ||
  commits.some((c) =>
    [...c.added, ...c.modified, ...c.removed].includes("soothfast.toml"),
  );

export function route(event: string, payload: Payload): Work | undefined {
  if (event !== "push" && event !== "pull_request") return undefined;
  const target: Target = {
    installation: payload.installation.id,
    repositoryId: payload.repository.id,
    repository: payload.repository.full_name,
    defaultBranch: payload.repository.default_branch,
  };
  if (event === "push") {
    const toDefault = payload.ref === `refs/heads/${target.defaultBranch}`;
    return toDefault && !payload.deleted
      ? { kind: "push", target, configChanged: touchesConfig(payload) }
      : undefined;
  }
  const { action, pull_request: pr } = payload;
  const relevant =
    pr &&
    action &&
    PULL_REQUEST_ACTIONS.includes(action) &&
    pr.state === "open" &&
    pr.base.ref === target.defaultBranch &&
    pr.labels.length > 0;
  if (!relevant) return undefined;
  const labeler =
    action === "labeled" && payload.label
      ? { label: payload.label.name, login: payload.sender.login }
      : undefined;
  return { kind: "pull_request", target, pr, labeler };
}
