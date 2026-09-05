import assert from "node:assert/strict";
import test from "node:test";
import { OidcError, githubJwks, verifyOidc } from "../src/oidc.ts";
import { AUDIENCE } from "../src/policy.ts";
import {
  KID,
  NOW,
  claims,
  generateKeys,
  signJwt,
  type TestKeys,
} from "./keys.ts";

const header = { alg: "RS256", typ: "JWT", kid: KID };
let keys: TestKeys;

test.before(async () => {
  keys = await generateKeys();
});

const verify = (token: string, now = NOW) =>
  verifyOidc(token, {
    audience: AUDIENCE,
    now,
    jwks: async (kid) => (kid === KID ? keys.jwk : undefined),
  });

const rejects = async (token: string, pattern: RegExp, now = NOW) => {
  await assert.rejects(
    verify(token, now),
    (e: unknown) => e instanceof OidcError && pattern.test(e.message),
  );
};

test("valid token returns its claims", async () => {
  const token = await signJwt(keys.privateKey, header, { ...claims() });
  assert.deepEqual(await verify(token), claims());
});

test("aud may be an array containing the audience", async () => {
  const token = await signJwt(keys.privateKey, header, {
    ...claims({ aud: ["other", AUDIENCE] }),
  });
  assert.equal((await verify(token)).repository, "acme/mylib");
});

test("tampered payload fails signature check", async () => {
  const token = await signJwt(keys.privateKey, header, { ...claims() });
  const [h, , s] = token.split(".");
  const forged = Buffer.from(
    JSON.stringify(claims({ repository: "evil/repo" })),
  ).toString("base64url");
  await rejects(`${h}.${forged}.${s}`, /signature/);
});

test("wrong audience is refused", async () => {
  await rejects(
    await signJwt(keys.privateKey, header, {
      ...claims({ aud: "someone-else" }),
    }),
    /audience/,
  );
});

test("wrong issuer is refused", async () => {
  await rejects(
    await signJwt(keys.privateKey, header, {
      ...claims({ iss: "https://evil" }),
    }),
    /issuer/,
  );
});

test("expired token is refused past the skew", async () => {
  const token = await signJwt(keys.privateKey, header, { ...claims() });
  await rejects(token, /expired/, NOW + 400);
});

test("token within skew of expiry is accepted", async () => {
  const token = await signJwt(keys.privateKey, header, { ...claims() });
  await verify(token, NOW + 330);
});

test("unknown kid is refused", async () => {
  await rejects(
    await signJwt(
      keys.privateKey,
      { ...header, kid: "other" },
      { ...claims() },
    ),
    /unknown signing key/,
  );
});

test("alg none is refused", async () => {
  const token = await signJwt(
    keys.privateKey,
    { ...header, alg: "none" },
    { ...claims() },
  );
  await rejects(token, /unsupported alg/);
});

test("garbage is refused", async () => {
  await rejects("not.a.jwt", /base64url JSON/);
  await rejects("nope", /compact JWS/);
});

test("githubJwks fetches discovery then keys and caches by kid", async () => {
  const calls: string[] = [];
  const fetchFn = (async (url: string | URL | Request) => {
    calls.push(String(url));
    const body = String(url).endsWith("openid-configuration")
      ? { jwks_uri: "https://jwks.example/keys" }
      : { keys: [keys.jwk] };
    return new Response(JSON.stringify(body));
  }) as typeof fetch;
  const lookup = githubJwks(fetchFn);
  assert.deepEqual(await lookup(KID), keys.jwk);
  assert.deepEqual(await lookup(KID), keys.jwk);
  assert.equal(await lookup("missing"), undefined);
  assert.deepEqual(calls, [
    "https://token.actions.githubusercontent.com/.well-known/openid-configuration",
    "https://jwks.example/keys",
  ]);
});
