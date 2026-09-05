import { base64UrlToBytes, base64UrlToText, utf8 } from "./encoding.ts";
import { ISSUER, type OidcClaims } from "./policy.ts";

export type JwksLookup = (kid: string) => Promise<JsonWebKey | undefined>;

export interface VerifyOptions {
  audience: string;
  jwks: JwksLookup;
  now?: number;
}

export class OidcError extends Error {}

const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };
const SKEW_SECONDS = 60;
const JWKS_REFETCH_SECONDS = 60;

interface Jws {
  header: { alg?: string; kid?: string };
  payload: OidcClaims;
  signingInput: Uint8Array;
  signature: Uint8Array;
}

function decode(token: string): Jws {
  const parts = token.split(".");
  if (parts.length !== 3) throw new OidcError("token is not a compact JWS");
  const [header, payload, signature] = parts;
  try {
    return {
      header: JSON.parse(base64UrlToText(header)),
      payload: JSON.parse(base64UrlToText(payload)),
      signingInput: utf8(`${header}.${payload}`),
      signature: base64UrlToBytes(signature),
    };
  } catch {
    throw new OidcError("token segments are not base64url JSON");
  }
}

export async function verifyOidc(
  token: string,
  opts: VerifyOptions,
): Promise<OidcClaims> {
  const jws = decode(token);
  if (jws.header.alg !== "RS256")
    throw new OidcError(`unsupported alg ${jws.header.alg}`);
  if (!jws.header.kid) throw new OidcError("token has no kid");
  const jwk = await opts.jwks(jws.header.kid);
  if (!jwk) throw new OidcError(`unknown signing key ${jws.header.kid}`);
  const key = await crypto.subtle.importKey("jwk", jwk, RS256, false, [
    "verify",
  ]);
  const valid = await crypto.subtle.verify(
    RS256.name,
    key,
    jws.signature,
    jws.signingInput,
  );
  if (!valid) throw new OidcError("signature does not verify");

  const claims = jws.payload;
  const now = opts.now ?? Math.floor(Date.now() / 1000);
  if (claims.exp <= now - SKEW_SECONDS)
    throw new OidcError("token has expired");
  if (claims.nbf !== undefined && claims.nbf > now + SKEW_SECONDS) {
    throw new OidcError("token is not yet valid");
  }
  if (claims.iss !== ISSUER)
    throw new OidcError(`issuer ${claims.iss} is not GitHub Actions`);
  const audiences = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
  if (!audiences.includes(opts.audience))
    throw new OidcError(`audience is not ${opts.audience}`);
  return claims;
}

interface JwksDocument {
  keys: (JsonWebKey & { kid: string })[];
}

// One refetch per minute at most: an unknown kid may be a new GitHub key
// or a garbage token, and the second must not turn into a JWKS fetch per request.
export function githubJwks(fetchFn: typeof fetch = fetch): JwksLookup {
  let keys = new Map<string, JsonWebKey>();
  let fetchedAt = 0;
  const load = async () => {
    const config = (await (
      await fetchFn(`${ISSUER}/.well-known/openid-configuration`)
    ).json()) as {
      jwks_uri: string;
    };
    const jwks = (await (
      await fetchFn(config.jwks_uri)
    ).json()) as JwksDocument;
    keys = new Map(jwks.keys.map((k) => [k.kid, k]));
    fetchedAt = Date.now();
  };
  return async (kid) => {
    if (!keys.has(kid) && Date.now() - fetchedAt > JWKS_REFETCH_SECONDS * 1000)
      await load();
    return keys.get(kid);
  };
}
