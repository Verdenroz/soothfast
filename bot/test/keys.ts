import { bytesToBase64Url, textToBase64Url, utf8 } from "../src/encoding.ts";
import {
  AUDIENCE,
  ENVIRONMENT,
  ISSUER,
  type OidcClaims,
} from "../src/policy.ts";

const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };

export const NOW = 1_800_000_000;
export const KID = "test-kid";

export interface TestKeys {
  privateKey: CryptoKey;
  publicKey: CryptoKey;
  jwk: JsonWebKey;
}

export async function generateKeys(): Promise<TestKeys> {
  const pair = await crypto.subtle.generateKey(
    { ...RS256, modulusLength: 2048, publicExponent: Uint8Array.of(1, 0, 1) },
    true,
    ["sign", "verify"],
  );
  const jwk = await crypto.subtle.exportKey("jwk", pair.publicKey);
  return { ...pair, jwk: { ...jwk, kid: KID } };
}

export async function signJwt(
  privateKey: CryptoKey,
  header: Record<string, unknown>,
  payload: Record<string, unknown>,
): Promise<string> {
  const input = `${textToBase64Url(JSON.stringify(header))}.${textToBase64Url(JSON.stringify(payload))}`;
  const signature = await crypto.subtle.sign(
    RS256.name,
    privateKey,
    utf8(input),
  );
  return `${input}.${bytesToBase64Url(new Uint8Array(signature))}`;
}

export function claims(overrides: Partial<OidcClaims> = {}): OidcClaims {
  return {
    iss: ISSUER,
    aud: AUDIENCE,
    exp: NOW + 300,
    iat: NOW,
    nbf: NOW - 10,
    repository: "acme/mylib",
    repository_id: "12345",
    ref: "refs/heads/main",
    sha: "0123456789abcdef0123456789abcdef01234567",
    event_name: "push",
    environment: ENVIRONMENT,
    ...overrides,
  };
}

export function toPem(label: string, der: Uint8Array): string {
  const base64 = btoa(Array.from(der, (b) => String.fromCharCode(b)).join(""));
  const lines = base64.match(/.{1,64}/g) ?? [];
  return `-----BEGIN ${label}-----\n${lines.join("\n")}\n-----END ${label}-----\n`;
}
