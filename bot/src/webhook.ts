import { utf8 } from "./encoding.ts";

// GitHub never delivers more than 25 MiB.
export const WEBHOOK_MAX_BYTES = 25 * 1024 * 1024;

export const PULL_REQUEST_ACTIONS: readonly string[] = [
  "opened",
  "reopened",
  "labeled",
  "unlabeled",
  "ready_for_review",
  "synchronize",
];

export interface Target {
  installation: number;
  repositoryId: number;
  repository: string;
  defaultBranch: string;
}

export type Work =
  | { kind: "pull_request"; target: Target; number: number; labeler?: Labeler }
  | { kind: "push"; target: Target }
  | { kind: "ignore" };

export interface Labeler {
  label: string;
  login: string;
}

export type Delivery =
  { ok: true; work: Work } | { ok: false; status: number; reason: string };

interface Payload {
  action?: string;
  ref?: string;
  deleted?: boolean;
  label?: { name: string };
  sender: { login: string };
  installation: { id: number };
  repository: { id: number; full_name: string; default_branch: string };
  pull_request?: { number: number; state: string; base: { ref: string } };
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

const IGNORE: Work = { kind: "ignore" };

export function route(event: string, payload: Payload): Work {
  if (event !== "push" && event !== "pull_request") return IGNORE;
  const target: Target = {
    installation: payload.installation.id,
    repositoryId: payload.repository.id,
    repository: payload.repository.full_name,
    defaultBranch: payload.repository.default_branch,
  };
  if (event === "push") {
    const toDefault = payload.ref === `refs/heads/${target.defaultBranch}`;
    return toDefault && !payload.deleted ? { kind: "push", target } : IGNORE;
  }
  const { action, pull_request: pr } = payload;
  if (!pr || pr.state !== "open") return IGNORE;
  if (pr.base.ref !== target.defaultBranch) return IGNORE;
  if (!action || !PULL_REQUEST_ACTIONS.includes(action)) return IGNORE;
  const labeler =
    action === "labeled" && payload.label
      ? { label: payload.label.name, login: payload.sender.login }
      : undefined;
  return { kind: "pull_request", target, number: pr.number, labeler };
}
