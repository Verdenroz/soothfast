"""Runtime behavior of the generated golden SDK, end to end.

Run with: uv run --with httpx --with pytest pytest soothfast-sdk/tests/python/
"""
import sys, threading, time, json
sys.dont_write_bytecode = True
from http.server import BaseHTTPRequestHandler, HTTPServer

sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parents[1] / "goldens" / "python" / "src"))
from acme_items import Client, Item, NewItem, RateLimitError, NotFoundError
from acme_items._runtime import decode, to_wire



def test_generated_sdk_end_to_end():
    item = decode(Item, {"id": 3, "from": "x", "logoUrl": "a", "logo_url": "b"})
    assert item.id == 3 and item.from_ == "x" and item.logo_url == "a" and item.logo_url_ == "b"
    assert to_wire(item) == {"id": 3, "from": "x", "logoUrl": "a", "logo_url": "b"}

    from acme_items import ApiResponse_Item
    r = decode(ApiResponse_Item, {"status": "ok", "data": {"id": 1}})
    assert isinstance(r.data, Item), r.data
    r2 = decode(ApiResponse_Item, {"status": "ok", "data": "plain"})
    assert r2.data == "plain"

    calls = {"n": 0}
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a): pass
        def do_GET(self):
            if self.path.startswith("/v1/items/nope"):
                self.send_response(404); body = b'{"error":"not_found","message":"no such item"}'
            elif self.path.startswith("/v1/items/limited"):
                calls["n"] += 1
                if calls["n"] < 3:
                    self.send_response(429)
                    self.send_header("Retry-After", "0")
                    body = b'{"error":"rate_limited","message":"slow down","retry_after_seconds":0}'
                else:
                    self.send_response(200); body = b'{"id": 9}'
            elif self.path.startswith("/v1/items/glacial"):
                calls["glacial"] = calls.get("glacial", 0) + 1
                self.send_response(429)
                self.send_header("Retry-After", "3600")
                body = b'{"error":"rate_limited","message":"come back later"}'
            elif "cursor=" in self.path:
                self.send_response(200)
                body = json.dumps({"items": [{"id": 2}], "pageInfo": {"endCursor": None, "hasNextPage": False}}).encode()
            elif "limit=" in self.path:
                self.send_response(200)
                body = json.dumps({"items": [{"id": 1}], "pageInfo": {"endCursor": "c1", "hasNextPage": True}}).encode()
            else:
                self.send_response(200); body = json.dumps([{"id": 1}, {"id": 2}]).encode()
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers(); self.wfile.write(body)
        def do_POST(self):
            n = int(self.headers.get("Content-Length", 0))
            payload = json.loads(self.rfile.read(n))
            assert payload == {"name": "w"}, payload
            self.send_response(201)
            body = b'{"id": 7}'
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers(); self.wfile.write(body)

    server = HTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_address[1]}"

    with Client(base) as client:
        assert client.list_items() == [Item(id=1), Item(id=2)]
        assert client.create_item(NewItem(name="w")).id == 7
        try:
            client.get_item("nope"); assert False
        except NotFoundError as e:
            assert e.status == 404 and e.error == "not_found"
        assert client.get_item("limited").id == 9 and calls["n"] == 3, calls
        # A Retry-After longer than we will wait comes back as an error,
        # not as an hour-long sleep inside the call.
        started = time.monotonic()
        try:
            client.get_item("glacial"); assert False
        except RateLimitError as e:
            assert e.retry_after == 3600.0, e.retry_after
        assert time.monotonic() - started < 5, "retried instead of raising"
        assert calls["glacial"] == 1, calls
        items = list(client.iter_list_items(limit=1))
        assert [i.id for i in items] == [1, 2], items
        pages = list(client.iter_list_items(limit=1).pages())
        assert len(pages) == 2 and pages[0].page_info.has_next_page

    import asyncio
    from acme_items import AsyncClient
    async def main():
        async with AsyncClient(base) as ac:
            assert (await ac.list_items()) == [Item(id=1), Item(id=2)]
            got = [i.id async for i in ac.iter_list_items(limit=1)]
            assert got == [1, 2], got
    asyncio.run(main())
    server.shutdown()


