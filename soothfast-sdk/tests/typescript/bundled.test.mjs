// The platform-package path: no env vars, no PATH — the binary the
// package shipped for this machine.
//
// Setup (what `cargo soothfast sdk build` does for real):
//   cargo build -p soothfast-sdk --example embed_server
//   cd soothfast-sdk/tests/goldens/typescript-embed
//   mkdir -p platforms/acme-items-linux-x64/bin
//   cp ../../../../target/debug/examples/embed_server platforms/acme-items-linux-x64/bin/
//   npm install && npm install --no-save ./platforms/acme-items-linux-x64
//   npm run build
// Then: node --test soothfast-sdk/tests/typescript/bundled.test.mjs
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import test from "node:test";

const root = new URL("../goldens/typescript-embed/", import.meta.url);
const { Client, EMBED, stopEmbeddedServers } = await import(new URL("dist/index.js", root));

// Neither override may be set: this test is about the bundled binary.
delete process.env[EMBED.binEnv];
delete process.env[EMBED.baseUrlEnv];

const installed = new URL("node_modules/acme-items-linux-x64/bin/embed_server", root);
if (!existsSync(installed)) {
  throw new Error(`platform package not installed — see the setup block in ${import.meta.url}`);
}

test.after(async () => {
  await stopEmbeddedServers();
});

test("the packaged binary is spawned with nothing configured", async () => {
  const client = new Client();
  const item = await client.getItem("bundled");
  assert.equal(item.note, "bundled");
});

test("the platform suffix names a package npm would actually install", () => {
  // Mirrors Target::npm_suffix on the Rust side; a mismatch here means
  // optionalDependencies point at names that were never published.
  const expected = `${EMBED.packagePrefix}-${process.platform}-${process.arch}`;
  assert.equal(expected, "acme-items-linux-x64");
});
