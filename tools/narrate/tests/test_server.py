import json
import urllib.error
import urllib.request

import pytest

from narrate import NarrateError
from narrate.server import LocalServer, Routes, json_response, serve_clip, start


def call(url, body=None, raw=None):
    data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
    req = urllib.request.Request(url, data=data, method="POST" if data is not None else "GET")
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def _user_error(_body):
    raise NarrateError("bad input")


@pytest.fixture
def server():
    routes = Routes()
    routes.add("GET", "/ok", lambda _b: json_response({"x": float("nan"), "y": 1}))
    routes.add("POST", "/echo", lambda b: json_response(b))
    routes.add("POST", "/user", _user_error)
    routes.add("POST", "/boom", lambda _b: 1 / 0)
    s = LocalServer(routes)
    start(s)
    yield s
    s.shutdown()
    s.server_close()


def test_binds_localhost_only(server):
    assert server.server_address[0] == "127.0.0.1"
    assert server.url == f"http://127.0.0.1:{server.server_address[1]}"


def test_json_is_strict(server):
    status, body = call(server.url + "/ok")
    assert (status, json.loads(body)) == (200, {"x": None, "y": 1})


def test_unknown_route_404(server):
    assert call(server.url + "/nope")[0] == 404


def test_bad_json_400(server):
    assert call(server.url + "/echo", raw=b"{nope")[0] == 400


def test_oversized_body_413(server):
    assert call(server.url + "/echo", raw=b" " * (64 * 1024 + 1))[0] == 413


def test_narrate_error_is_400_and_other_500(server, capsys):
    status, body = call(server.url + "/user", body={})
    assert (status, json.loads(body)) == (400, {"error": "bad input"})
    status, body = call(server.url + "/boom", body={})
    assert (status, json.loads(body)) == (500, {"error": "internal error"})
    assert "ZeroDivisionError" in capsys.readouterr().err


def test_serve_clip_only_known_ids(root):
    (root / "clips").mkdir()
    (root / "clips" / "open.wav").write_bytes(b"RIFF")
    (root / "narration.json").write_text("{}")
    assert serve_clip(root, ["open"], "/clips/open.wav") == (200, "audio/wav", b"RIFF")
    for p in ["/clips/..%2Fnarration.json", "/clips/nope.wav", "/clips/open.wav/../x", "/clips/open.wav.bak"]:
        assert serve_clip(root, ["open"], p)[0] == 404
