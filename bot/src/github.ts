import {
  base64ToBytes,
  bytesToBase64Url,
  textToBase64Url,
  utf8,
} from "./encoding.ts";

const API = "https://api.github.com";
const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };
const TOKEN_PERMISSIONS = { contents: "write", pull_requests: "write" };

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
    appJwt: string,
  ): Promise<InstallationToken>;
  repository(repository: string, token: string): Promise<Repository>;
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
    const json = response.status === 204 ? undefined : await response.json();
    return { status: response.status, json: json as T };
  };
  const expectOk = <T>(result: { status: number; json: T }): T => {
    if (result.status < 200 || result.status >= 300) {
      const message =
        (result.json as { message?: string } | undefined)?.message ?? "";
      throw new GitHubError(result.status, message);
    }
    return result.json;
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
    async mintToken(installationId, repositoryId, jwt) {
      const body = {
        repository_ids: [repositoryId],
        permissions: TOKEN_PERMISSIONS,
      };
      const path = `/app/installations/${installationId}/access_tokens`;
      return expectOk(await call<InstallationToken>("POST", path, jwt, body));
    },
    async repository(repository, token) {
      return expectOk(
        await call<Repository>("GET", `/repos/${repository}`, token),
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
