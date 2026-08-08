// The embedded-server launcher, against a real spawned binary.
//
// Build the fixture server and the golden first, then run:
//   cargo build -p soothfast-sdk --example embed_server
//   npm --prefix soothfast-sdk/tests/goldens/typescript-embed install
//   npm --prefix soothfast-sdk/tests/goldens/typescript-embed run build
//   node --test soothfast-sdk/tests/typescript/embed.test.mjs
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { createServer } from "node:http";
import test from "node:test";

const golden = new URL("../goldens/typescript-embed/dist/index.js", import.meta.url);
const { Client, EMBED, EmbeddedServer, stopEmbeddedServers } = await import(golden);

const SERVER_BIN = new URL("../../../target/debug/examples/embed_server", import.meta.url).pathname;

if (!existsSync(SERVER_BIN)) {
  throw new Error(
    `missing ${SERVER_BIN} — run: cargo build -p soothfast-sdk --example embed_server`,
  );
}
process.env[EMBED.binEnv] = SERVER_BIN;

test.after(async () => {
  await stopEmbeddedServers();
});

test("a client with no base URL spawns the bundled server and talks to it", async () => {
  const client = new Client();
  const item = await client.getItem("abc");
  assert.equal(item.id, 1);
  assert.equal(item.logoUrl, "a");
  assert.equal(item.note, "abc", "the path segment reached the real server");
});

test("the server is shared, not respawned per client", async () => {
  const a = new Client();
  const b = new Client();
  const [first, second] = await Promise.all([a.getItem("one"), b.getItem("two")]);
  assert.equal(first.note, "one");
  assert.equal(second.note, "two");
});

test("every operation works against the bundled server", async () => {
  const client = new Client();

  const created = await client.createItem({ name: "w" });
  assert.equal(created.id, 7);

  assert.equal(await client.deleteItem("abc"), undefined);

  const stats = await client.getStats();
  assert.equal(stats.status, "ok");

  assert.deepEqual((await client.listItems()).map((i) => i.id), [1, 2]);

  const ids = [];
  for await (const item of client.iterListItems({ limit: 1 })) ids.push(item.id);
  assert.deepEqual(ids, [1, 2]);
});

/** What the spawned server saw in its own environment. */
async function note(client) {
  const { data } = await client.getStats();
  return typeof data === "object" && data !== null ? data.note : data;
}

test("the client environment reaches the process it spawns", async () => {
  const client = new Client(undefined, { serverEnv: { EMBED_SERVER_NOTE: "from-the-client" } });
  assert.equal(await note(client), "from-the-client");
});

test("clients configured differently do not share one server", async () => {
  const a = new Client(undefined, { serverEnv: { EMBED_SERVER_NOTE: "a" } });
  const b = new Client(undefined, { serverEnv: { EMBED_SERVER_NOTE: "b" } });
  assert.deepEqual(await Promise.all([note(a), note(b)]), ["a", "b"]);
});

test("the launch settings beat the ambient environment", async () => {
  // `PORT` belongs to whatever runs the caller's app, not to us.
  process.env.PORT = "8000";
  try {
    const item = await new Client(undefined, { serverEnv: { EMBED_SERVER_NOTE: "ambient" } })
      .getItem("abc");
    assert.equal(item.note, "abc");
  } finally {
    delete process.env.PORT;
  }
});

test("a server that logs every request does not wedge", async () => {
  // 64 KiB of pipe buffer, ~2 KiB of logging per request.
  const client = new Client(undefined, { serverEnv: { EMBED_SERVER_CHATTY: "1" } });
  for (let i = 0; i < 200; i++) {
    const item = await client.getItem(`n${i}`);
    assert.equal(item.note, `n${i}`);
  }
});

test("errors from the bundled server map to the same exception types", async () => {
  const client = new Client();
  await assert.rejects(() => client.getItem("nope"), (err) => {
    assert.equal(err.status, 404);
    assert.equal(err.error, "not_found");
    return true;
  });
});

test("the base-url env override wins and spawns nothing", async () => {
  const hits = [];
  const stub = createServer((req, res) => {
    hits.push(req.url);
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ id: 99 }));
  });
  await new Promise((resolve) => stub.listen(0, "127.0.0.1", resolve));
  const { port } = stub.address();

  process.env[EMBED.baseUrlEnv] = `http://127.0.0.1:${port}`;
  try {
    const item = await new Client().getItem("abc");
    assert.equal(item.id, 99);
    assert.equal(hits.length, 1, "the override was used, not a spawned server");
  } finally {
    delete process.env[EMBED.baseUrlEnv];
    stub.close();
  }
});

test("an explicit base URL bypasses the launcher entirely", async () => {
  const stub = createServer((req, res) => {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ id: 42 }));
  });
  await new Promise((resolve) => stub.listen(0, "127.0.0.1", resolve));
  const { port } = stub.address();
  try {
    const item = await new Client(`http://127.0.0.1:${port}`).getItem("abc");
    assert.equal(item.id, 42);
  } finally {
    stub.close();
  }
});

test("a server that never announces fails with its stderr, not a hang", async () => {
  await assert.rejects(
    () =>
      EmbeddedServer.start(
        { ...EMBED, binary: "/bin/sh", args: ["-c", "echo boom >&2; exit 3"] },
        { bin: "/bin/sh", startupTimeoutMs: 5_000 },
      ),
    (err) => {
      assert.match(err.message, /exited/);
      assert.match(err.message, /boom/, "stderr is surfaced");
      return true;
    },
  );
});

test("a missing binary fails immediately with a usable message", async () => {
  await assert.rejects(
    () => EmbeddedServer.start({ ...EMBED, binary: "soothfast-no-such-binary" }, { bin: "soothfast-no-such-binary" }),
    (err) => {
      assert.match(err.message, /soothfast-no-such-binary/);
      return true;
    },
  );
});

test("a hand-managed server can be started and stopped explicitly", async () => {
  const server = await EmbeddedServer.start(EMBED);
  assert.match(server.baseUrl, /^http:\/\/127\.0\.0\.1:\d+$/);
  const item = await new Client(server.baseUrl).getItem("manual");
  assert.equal(item.note, "manual");
  await server.stop();
});
