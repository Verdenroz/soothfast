"""The embedded-server launcher, against a real spawned binary.

Build the fixture server first, then run:
    cargo build -p soothfast-sdk --example embed_server
    uv run --with httpx --with pytest pytest soothfast-sdk/tests/python/test_embed.py
"""
import os
import pathlib
import subprocess
import sys

import pytest

sys.dont_write_bytecode = True

_ROOT = pathlib.Path(__file__).resolve().parents[3]
_SERVER_BIN = _ROOT / "target" / "debug" / "examples" / "embed_server"

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "goldens" / "python-embed" / "src"))

if not _SERVER_BIN.exists():
    pytest.skip(
        f"missing {_SERVER_BIN} — run: cargo build -p soothfast-sdk --example embed_server",
        allow_module_level=True,
    )

from acme_items import Client, EmbeddedServer, NotFoundError, stop_embedded_servers
from acme_items.client import EMBED

os.environ[EMBED.bin_env] = str(_SERVER_BIN)


@pytest.fixture(autouse=True)
def _reap():
    yield
    stop_embedded_servers()
    os.environ.pop(EMBED.base_url_env, None)


def test_a_client_with_no_base_url_spawns_the_bundled_server():
    with Client() as client:
        item = client.get_item("abc")
        assert item.id == 1
        assert item.logo_url == "a"
        assert item.note == "abc", "the path segment reached the real server"


def test_the_server_is_shared_not_respawned_per_client():
    with Client() as a, Client() as b:
        assert a.get_item("one").note == "one"
        assert b.get_item("two").note == "two"


def test_every_operation_works_against_the_bundled_server():
    with Client() as client:
        from acme_items import NewItem

        assert client.create_item(NewItem(name="w")).id == 7
        assert client.delete_item("abc") is None
        assert client.get_stats().status == "ok"
        assert [i.id for i in client.list_items()] == [1, 2]
        assert [i.id for i in client.iter_list_items(limit=1)] == [1, 2]


def _note(client):
    """What the spawned server saw in its own environment."""
    data = client.get_stats().data
    return data.note if hasattr(data, "note") else data["note"]


def test_the_client_environment_reaches_the_process_it_spawns():
    with Client(server_env={"EMBED_SERVER_NOTE": "from-the-client"}) as client:
        assert _note(client) == "from-the-client"


def test_clients_configured_differently_do_not_share_one_server():
    """One process cannot hold two configurations."""
    from acme_items import _server

    with Client(server_env={"EMBED_SERVER_NOTE": "a"}) as a, Client(
        server_env={"EMBED_SERVER_NOTE": "b"}
    ) as b:
        assert _note(a) == "a"
        assert _note(b) == "b"
        assert len(_server._running) == 2


def test_the_launch_settings_beat_the_ambient_environment():
    """`PORT` belongs to whatever runs the caller's app, not to us."""
    os.environ["PORT"] = "8000"
    try:
        with Client() as client:
            assert client.get_item("abc").note == "abc"
    finally:
        os.environ.pop("PORT", None)


def test_a_server_that_logs_every_request_does_not_wedge():
    """64 KiB of pipe buffer, ~2 KiB of logging per request."""
    with Client(server_env={"EMBED_SERVER_CHATTY": "1"}) as client:
        for i in range(200):
            assert client.get_item(f"n{i}").note == f"n{i}"


def test_errors_map_to_the_same_exception_types():
    with Client() as client:
        with pytest.raises(NotFoundError) as excinfo:
            client.get_item("nope")
        assert excinfo.value.status == 404
        assert excinfo.value.error == "not_found"


def test_the_base_url_env_override_wins_and_spawns_nothing():
    server = EmbeddedServer.start(EMBED)
    try:
        os.environ[EMBED.base_url_env] = server.base_url
        with Client() as client:
            assert client.get_item("abc").note == "abc"
        # Nothing was spawned under the shared-server key.
        from acme_items import _server

        assert _server._running == {}
    finally:
        server.stop()


def test_an_explicit_base_url_bypasses_the_launcher():
    server = EmbeddedServer.start(EMBED)
    try:
        with Client(server.base_url) as client:
            assert client.get_item("explicit").note == "explicit"
        from acme_items import _server

        assert _server._running == {}
    finally:
        server.stop()


def test_a_server_that_never_announces_fails_with_its_stderr():
    config = type(EMBED)(
        binary="/bin/sh",
        args=("-c", "echo boom >&2; exit 3"),
        base_url_env=EMBED.base_url_env,
        bin_env="SOOTHFAST_UNSET_BIN_ENV",
    )
    with pytest.raises(RuntimeError) as excinfo:
        EmbeddedServer.start(config)
    assert "exited" in str(excinfo.value)
    assert "boom" in str(excinfo.value), "stderr is surfaced"


def test_a_missing_binary_fails_immediately_with_a_usable_message():
    config = type(EMBED)(
        binary="soothfast-no-such-binary",
        args=(),
        base_url_env=EMBED.base_url_env,
        bin_env="SOOTHFAST_UNSET_BIN_ENV",
    )
    with pytest.raises(RuntimeError) as excinfo:
        EmbeddedServer.start(config)
    assert "soothfast-no-such-binary" in str(excinfo.value)


def test_a_hand_managed_server_can_be_started_and_stopped():
    server = EmbeddedServer.start(EMBED)
    assert server.base_url.startswith("http://127.0.0.1:")
    with Client(server.base_url) as client:
        assert client.get_item("manual").note == "manual"
    server.stop()
    assert server._process.poll() is not None


def test_the_server_dies_with_the_interpreter():
    """atexit reaps the shared server even when nobody stops it."""
    script = (
        "import sys; sys.path.insert(0, %r); sys.dont_write_bytecode = True\n"
        "from acme_items import Client\n"
        "from acme_items import _server\n"
        "c = Client()\n"
        "print(next(iter(_server._running.values()))._process.pid)\n"
        % (
            str(pathlib.Path(__file__).resolve().parents[1] / "goldens" / "python-embed" / "src"),
        )
    )
    out = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        env={**os.environ, EMBED.bin_env: str(_SERVER_BIN)},
        check=True,
    )
    pid = int(out.stdout.strip())
    with pytest.raises(OSError):
        os.kill(pid, 0)
