import type { MergeMethod } from "./config.ts";
import {
  base64ToBytes,
  bytesToBase64Url,
  textToBase64Url,
  utf8,
} from "./encoding.ts";

const API = "https://api.github.com";
const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };
export interface Installation {
  id: number;
}

export interface InstallationToken {
  token: string;
  expires_at: string;
}

export interface Repository {
  default_branch: string;
}

export interface Comparison {
  status: string;
}

export interface IssueComment {
  id: number;
  body: string;
  html_url: string;
}

export interface PullRequest {
  number: number;
  node_id: string;
  draft: boolean;
  labels: { name: string }[];
  auto_merge: object | null;
  head: { sha: string };
}

interface IssueEvent {
  event: string;
  label?: { name: string };
  actor?: { login: string };
}

interface BranchRule {
  type: string;
}

interface BranchProtection {
  protection?: {
    required_status_checks?: { contexts?: string[]; checks?: unknown[] };
  };
}

export class GitHubError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export interface GitHubApi {
  installationFor(
    repository: string,
    appJwt: string,
  ): Promise<Installation | undefined>;
  mintToken(
    installationId: number,
    repositoryId: number,
    permissions: Record<string, string>,
    appJwt: string,
  ): Promise<InstallationToken>;
  repository(repository: string, token: string): Promise<Repository>;
  compare(
    repository: string,
    base: string,
    head: string,
    token: string,
  ): Promise<Comparison>;
  tagObject(
    repository: string,
    sha: string,
    token: string,
  ): Promise<string | undefined>;
  listComments(
    repository: string,
    issue: number,
    token: string,
  ): Promise<IssueComment[]>;
  createComment(
    repository: string,
    issue: number,
    body: string,
    token: string,
  ): Promise<IssueComment>;
  updateComment(
    repository: string,
    id: number,
    body: string,
    token: string,
  ): Promise<IssueComment>;
  fileContents(
    repository: string,
    path: string,
    ref: string,
    token: string,
  ): Promise<string | undefined>;
  pullRequest(
    repository: string,
    number: number,
    token: string,
  ): Promise<PullRequest>;
  openPullRequests(
    repository: string,
    base: string,
    token: string,
  ): Promise<PullRequest[]>;
  lastLabeler(
    repository: string,
    issue: number,
    label: string,
    token: string,
  ): Promise<string | undefined>;
  collaboratorPermission(
    repository: string,
    login: string,
    token: string,
  ): Promise<string>;
  branchRules(
    repository: string,
    branch: string,
    token: string,
  ): Promise<BranchRule[]>;
  branchProtection(
    repository: string,
    branch: string,
    token: string,
  ): Promise<BranchProtection>;
  updateBranch(
    repository: string,
    number: number,
    expectedHeadSha: string,
    token: string,
  ): Promise<void>;
  enableAutoMerge(
    nodeId: string,
    method: MergeMethod,
    expectedHeadOid: string,
    token: string,
  ): Promise<void>;
  disableAutoMerge(nodeId: string, token: string): Promise<void>;
  mergePullRequest(
    repository: string,
    number: number,
    sha: string,
    method: MergeMethod,
    token: string,
  ): Promise<void>;
  revoke(token: string): Promise<void>;
  appSlug(appJwt: string): Promise<string>;
}

export async function importPrivateKey(pem: string): Promise<CryptoKey> {
  const [label, body] = parsePem(pem);
  const der = base64ToBytes(body);
  const pkcs8 = label === "RSA PRIVATE KEY" ? wrapPkcs1(der) : der;
  return crypto.subtle.importKey("pkcs8", pkcs8, RS256, false, ["sign"]);
}

function parsePem(pem: string): [string, string] {
  const match = /-----BEGIN ([A-Z ]+)-----([\s\S]+?)-----END \1-----/.exec(pem);
  if (!match) throw new Error("private key is not PEM");
  return [match[1], match[2].replace(/\s+/g, "")];
}

// PKCS#8 PrivateKeyInfo around a PKCS#1 RSAPrivateKey: the format GitHub
// hands out versus the only one WebCrypto imports.
const VERSION_ZERO = Uint8Array.of(0x02, 0x01, 0x00);
const RSA_ENCRYPTION = Uint8Array.of(
  0x30,
  0x0d,
  0x06,
  0x09,
  0x2a,
  0x86,
  0x48,
  0x86,
  0xf7,
  0x0d,
  0x01,
  0x01,
  0x01,
  0x05,
  0x00,
);

export function wrapPkcs1(pkcs1: Uint8Array): Uint8Array {
  return derTlv(
    0x30,
    concat(VERSION_ZERO, RSA_ENCRYPTION, derTlv(0x04, pkcs1)),
  );
}

function derTlv(tag: number, value: Uint8Array): Uint8Array {
  return concat(Uint8Array.of(tag), derLength(value.length), value);
}

function derLength(length: number): Uint8Array {
  if (length < 0x80) return Uint8Array.of(length);
  const bytes: number[] = [];
  for (let rest = length; rest > 0; rest = Math.floor(rest / 256))
    bytes.unshift(rest % 256);
  return Uint8Array.of(0x80 | bytes.length, ...bytes);
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  parts.reduce((offset, p) => {
    out.set(p, offset);
    return offset + p.length;
  }, 0);
  return out;
}

export async function appJwt(
  clientId: string,
  key: CryptoKey,
  now = Math.floor(Date.now() / 1000),
): Promise<string> {
  const header = textToBase64Url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const payload = textToBase64Url(
    JSON.stringify({ iat: now - 60, exp: now + 540, iss: clientId }),
  );
  const input = `${header}.${payload}`;
  const signature = await crypto.subtle.sign(RS256.name, key, utf8(input));
  return `${input}.${bytesToBase64Url(new Uint8Array(signature))}`;
}

// GitHub 5xx pages and proxy interstitials are HTML; surface them as the
// error message instead of a JSON parse exception.
function parseBody(text: string): unknown {
  try {
    return text ? JSON.parse(text) : undefined;
  } catch {
    return { message: text.slice(0, 200) };
  }
}

const lastPage = (link: string | null) =>
  Number(/<[^>]*[?&]page=(\d+)[^>]*>;\s*rel="last"/.exec(link ?? "")?.[1] ?? 1);

function labeledBy(events: IssueEvent[], label: string): string | undefined {
  return [...events]
    .reverse()
    .find((e) => e.event === "labeled" && e.label?.name === label)?.actor
    ?.login;
}

export function githubApi(fetchFn: typeof fetch = fetch): GitHubApi {
  const call = async <T>(
    method: string,
    path: string,
    auth: string,
    body?: unknown,
  ) => {
    const response = await fetchFn(`${API}${path}`, {
      method,
      headers: {
        accept: "application/vnd.github+json",
        authorization: `Bearer ${auth}`,
        "content-type": "application/json",
        "user-agent": "soothfast-bot",
        "x-github-api-version": "2022-11-28",
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return {
      status: response.status,
      json: parseBody(await response.text()) as T,
      link: response.headers.get("link"),
    };
  };
  const expectOk = <T>(result: { status: number; json: T }): T => {
    if (result.status < 200 || result.status >= 300) {
      const message =
        (result.json as { message?: string } | undefined)?.message ?? "";
      throw new GitHubError(result.status, message);
    }
    return result.json;
  };
  const paginate = async <T>(path: string, token: string) => {
    const pages: T[][] = [];
    for (let page = 1; ; page++) {
      const separator = path.includes("?") ? "&" : "?";
      const url = `${path}${separator}per_page=100&page=${page}`;
      const batch = expectOk(await call<T[]>("GET", url, token));
      pages.push(batch);
      if (batch.length < 100) break;
    }
    return pages.flat();
  };
  const graphql = async (
    query: string,
    variables: Record<string, string>,
    token: string,
  ) => {
    const result = await call<{ errors?: { message: string }[] }>(
      "POST",
      "/graphql",
      token,
      { query, variables },
    );
    const errors = expectOk(result)?.errors;
    if (errors?.length) throw new GitHubError(result.status, errors[0].message);
  };
  return {
    async installationFor(repository, jwt) {
      const result = await call<Installation>(
        "GET",
        `/repos/${repository}/installation`,
        jwt,
      );
      return result.status === 404 ? undefined : expectOk(result);
    },
    async mintToken(installationId, repositoryId, permissions, jwt) {
      const body = { repository_ids: [repositoryId], permissions };
      const path = `/app/installations/${installationId}/access_tokens`;
      return expectOk(await call<InstallationToken>("POST", path, jwt, body));
    },
    async repository(repository, token) {
      return expectOk(
        await call<Repository>("GET", `/repos/${repository}`, token),
      );
    },
    async compare(repository, base, head, token) {
      const path = `/repos/${repository}/compare/${encodeURIComponent(base)}...${encodeURIComponent(head)}`;
      return expectOk(await call<Comparison>("GET", path, token));
    },
    async listComments(repository, issue, token) {
      return paginate<IssueComment>(
        `/repos/${repository}/issues/${issue}/comments`,
        token,
      );
    },
    async createComment(repository, issue, body, token) {
      const path = `/repos/${repository}/issues/${issue}/comments`;
      return expectOk(await call<IssueComment>("POST", path, token, { body }));
    },
    async updateComment(repository, id, body, token) {
      const path = `/repos/${repository}/issues/comments/${id}`;
      return expectOk(await call<IssueComment>("PATCH", path, token, { body }));
    },
    async tagObject(repository, sha, token) {
      const result = await call<{ object: { sha: string } }>(
        "GET",
        `/repos/${repository}/git/tags/${sha}`,
        token,
      );
      return result.status === 404 ? undefined : expectOk(result).object.sha;
    },
    async fileContents(repository, path, ref, token) {
      const result = await call<{ content: string }>(
        "GET",
        `/repos/${repository}/contents/${path}?ref=${encodeURIComponent(ref)}`,
        token,
      );
      if (result.status === 404) return undefined;
      return new TextDecoder().decode(base64ToBytes(expectOk(result).content));
    },
    async pullRequest(repository, number, token) {
      const path = `/repos/${repository}/pulls/${number}`;
      return expectOk(await call<PullRequest>("GET", path, token));
    },
    // One page of 100 covers the evaluation cap.
    async openPullRequests(repository, base, token) {
      const path = `/repos/${repository}/pulls?state=open&sort=updated&direction=desc&per_page=100&base=${encodeURIComponent(base)}`;
      return expectOk(await call<PullRequest[]>("GET", path, token));
    },
    async lastLabeler(repository, issue, label, token) {
      const path = `/repos/${repository}/issues/${issue}/events?per_page=100`;
      const first = await call<IssueEvent[]>("GET", `${path}&page=1`, token);
      const pages = new Map([[1, expectOk(first)]]);
      const last = lastPage(first.link);
      for (const page of [last, last - 1].filter((n) => n >= 1)) {
        const url = `${path}&page=${page}`;
        const events =
          pages.get(page) ??
          expectOk(await call<IssueEvent[]>("GET", url, token));
        pages.set(page, events);
        const login = labeledBy(events, label);
        if (login) return login;
      }
      return undefined;
    },
    async collaboratorPermission(repository, login, token) {
      const path = `/repos/${repository}/collaborators/${encodeURIComponent(login)}/permission`;
      const result = await call<{ permission: string }>("GET", path, token);
      return result.status === 404 ? "none" : expectOk(result).permission;
    },
    async branchRules(repository, branch, token) {
      const path = `/repos/${repository}/rules/branches/${encodeURIComponent(branch)}`;
      return expectOk(await call<BranchRule[]>("GET", path, token));
    },
    async branchProtection(repository, branch, token) {
      const path = `/repos/${repository}/branches/${encodeURIComponent(branch)}`;
      return expectOk(await call<BranchProtection>("GET", path, token));
    },
    async updateBranch(repository, number, expectedHeadSha, token) {
      const path = `/repos/${repository}/pulls/${number}/update-branch`;
      expectOk(
        await call<undefined>("PUT", path, token, {
          expected_head_sha: expectedHeadSha,
        }),
      );
    },
    async enableAutoMerge(nodeId, method, expectedHeadOid, token) {
      await graphql(
        `
          mutation (
            $id: ID!
            $method: PullRequestMergeMethod!
            $oid: GitObjectID!
          ) {
            enablePullRequestAutoMerge(
              input: {
                pullRequestId: $id
                mergeMethod: $method
                expectedHeadOid: $oid
              }
            ) {
              clientMutationId
            }
          }
        `,
        { id: nodeId, method: method.toUpperCase(), oid: expectedHeadOid },
        token,
      );
    },
    async disableAutoMerge(nodeId, token) {
      await graphql(
        `
          mutation ($id: ID!) {
            disablePullRequestAutoMerge(input: { pullRequestId: $id }) {
              clientMutationId
            }
          }
        `,
        { id: nodeId },
        token,
      );
    },
    async mergePullRequest(repository, number, sha, method, token) {
      const path = `/repos/${repository}/pulls/${number}/merge`;
      expectOk(
        await call<undefined>("PUT", path, token, {
          sha,
          merge_method: method,
        }),
      );
    },
    async revoke(token) {
      expectOk(await call<undefined>("DELETE", "/installation/token", token));
    },
    async appSlug(jwt) {
      return expectOk(await call<{ slug: string }>("GET", "/app", jwt)).slug;
    },
  };
}
