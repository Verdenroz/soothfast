// Runtime behavior of the generated golden SDK, end to end.
//
// Build the golden first, then run:
//   npm --prefix soothfast-sdk/tests/goldens/typescript install
//   npm --prefix soothfast-sdk/tests/goldens/typescript run build
//   node --test soothfast-sdk/tests/typescript/runtime.test.mjs
import assert from "node:assert/strict";
import { createServer } from "node:http";
import test from "node:test";

const { Client, NotFoundError, RateLimitError } = await import(
  new URL("../goldens/typescript/dist/index.js", import.meta.url)
);

/** A stub of the Acme Items API the golden fixture describes. */
function handler(state) {
  return (req, res) => {
    const url = new URL(req.url, "http://localhost");
    const send = (status, body) => {
      const payload = body === undefined ? "" : JSON.stringify(body);
      res.writeHead(status, { "content-type": "application/json" });
      res.end(payload);
    };

    if (req.method === "POST" && url.pathname === "/v1/items") {
      let raw = "";
      req.on("data", (chunk) => (raw += chunk));
      req.on("end", () => {
        state.posted = JSON.parse(raw);
        send(201, { id: 7 });
      });
      return;
    }
    if (req.method === "DELETE") {
      res.writeHead(204);
      res.end();
      return;
    }
    if (url.pathname === "/v1/items/nope") {
      return send(404, { error: "not_found", message: "no such item" });
    }
    if (url.pathname === "/v1/items/limited") {
      state.attempts += 1;
      if (state.attempts < 3) {
        res.writeHead(429, { "content-type": "application/json", "retry-after": "0" });
        return res.end(JSON.stringify({ error: "rate_limited", retry_after_seconds: 0 }));
      }
      return send(200, { id: 9 });
    }
    if (url.pathname === "/v1/items/glacial") {
      state.glacial = (state.glacial ?? 0) + 1;
      res.writeHead(429, { "content-type": "application/json", "retry-after": "3600" });
      return res.end(JSON.stringify({ error: "rate_limited" }));
    }
    if (url.pathname.startsWith("/v1/items/")) {
      state.lastPath = url.pathname;
      state.lastQuery = url.search;
      return send(200, { id: 1, logoUrl: "a", logo_url: "b", from: "x" });
    }
    if (url.pathname === "/v1/items") {
      if (url.searchParams.has("cursor") && url.searchParams.get("cursor") !== "") {
        return send(200, { items: [{ id: 2 }], pageInfo: { endCursor: null, hasNextPage: false } });
      }
      if (url.searchParams.has("limit")) {
        state.lastQuery = url.search;
        return send(200, { items: [{ id: 1 }], pageInfo: { endCursor: "c1", hasNextPage: true } });
      }
      state.lastQuery = url.search;
      return send(200, [{ id: 1 }, { id: 2 }]);
    }
    return send(404, { error: "not_found" });
  };
}

async function withServer(run) {
  const state = { attempts: 0, posted: null, lastPath: null, lastQuery: null };
  const server = createServer(handler(state));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    await run(new Client(`http://127.0.0.1:${port}`, { backoffBaseMs: 1 }), state);
  } finally {
    server.close();
  }
}

test("wire property names survive decoding untouched", async () => {
  await withServer(async (client) => {
    const item = await client.getItem("abc");
    assert.equal(item.id, 1);
    assert.equal(item.logoUrl, "a");
    assert.equal(item.logo_url, "b");
    assert.equal(item.from, "x");
  });
});

test("path segments are percent-encoded and query options are forwarded", async () => {
  await withServer(async (client, state) => {
    await client.getItem("a/b", { fields: "id" });
    assert.equal(state.lastPath, "/v1/items/a%2Fb");
    assert.equal(state.lastQuery, "?fields=id");
  });
});

test("undefined query options are dropped rather than sent empty", async () => {
  await withServer(async (client, state) => {
    await client.listItems({ tag: undefined });
    assert.equal(state.lastQuery, "");
  });
});

test("a request body is JSON-encoded", async () => {
  await withServer(async (client, state) => {
    const created = await client.createItem({ name: "w" });
    assert.deepEqual(state.posted, { name: "w" });
    assert.equal(created.id, 7);
  });
});

test("a 204 decodes to nothing", async () => {
  await withServer(async (client) => {
    assert.equal(await client.deleteItem("abc"), undefined);
  });
});

test("a 404 raises the mapped error with its body", async () => {
  await withServer(async (client) => {
    await assert.rejects(() => client.getItem("nope"), (err) => {
      assert.ok(err instanceof NotFoundError);
      assert.equal(err.status, 404);
      assert.equal(err.error, "not_found");
      assert.equal(err.message, "no such item");
      return true;
    });
  });
});

test("a rate limit retries until it succeeds, and reports each one", async () => {
  const seen = [];
  const state = { attempts: 0 };
  const server = createServer(handler(state));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    const client = new Client(`http://127.0.0.1:${port}`, {
      backoffBaseMs: 1,
      onRateLimit: (err) => seen.push(err),
    });
    const item = await client.getItem("limited");
    assert.equal(item.id, 9);
    assert.equal(state.attempts, 3);
    assert.equal(seen.length, 2);
    assert.ok(seen[0] instanceof RateLimitError);
  } finally {
    server.close();
  }
});

test("a Retry-After longer than we will wait raises instead of sleeping", async () => {
  const state = { attempts: 0 };
  const server = createServer(handler(state));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    const client = new Client(`http://127.0.0.1:${port}`, { backoffBaseMs: 1 });
    const started = Date.now();
    await assert.rejects(client.getItem("glacial"), (err) => {
      assert.ok(err instanceof RateLimitError);
      assert.equal(err.retryAfter, 3600);
      return true;
    });
    assert.ok(Date.now() - started < 5000, "retried instead of raising");
    assert.equal(state.glacial, 1);
  } finally {
    server.close();
  }
});

test("retries stop at maxRetries and throw the last error", async () => {
  const state = { attempts: 0 };
  const server = createServer(handler(state));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  try {
    const client = new Client(`http://127.0.0.1:${port}`, { backoffBaseMs: 1, maxRetries: 1 });
    await assert.rejects(() => client.getItem("limited"), RateLimitError);
    assert.equal(state.attempts, 2);
  } finally {
    server.close();
  }
});

test("the pager walks cursors and flattens items", async () => {
  await withServer(async (client, state) => {
    const ids = [];
    for await (const item of client.iterListItems({ limit: 1 })) ids.push(item.id);
    assert.deepEqual(ids, [1, 2]);
    assert.equal(state.lastQuery, "?limit=1");

    const pages = [];
    for await (const page of client.iterListItems().pages()) pages.push(page);
    assert.equal(pages.length, 2);
    assert.equal(pages[0].pageInfo.endCursor, "c1");
    assert.equal(pages[1].pageInfo.hasNextPage, false);

    assert.deepEqual((await client.iterListItems().all()).map((i) => i.id), [1, 2]);
  });
});

test("an unpaginated listing returns the plain array", async () => {
  await withServer(async (client) => {
    assert.deepEqual((await client.listItems()).map((i) => i.id), [1, 2]);
  });
});
