import http.client
import json
import urllib.error
import urllib.request

import pytest

from narrate import NarrateError
from narrate.server import LocalServer, Routes, json_response, serve_clip, start


def call(url, body=None, raw=None):
    data = raw if raw is not None else (None if body is None else json.dumps(body).encode())
    headers = {"Content-Type": "application/json"} if data is not None else {}
    req = urllib.request.Request(url, data=data, method="POST" if data is not None else "GET", headers=headers)
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
    for p in [
        "/clips/..%2Fnarration.json",
        "/clips/nope.wav",
        "/clips/open.wav/../x",
        "/clips/open.wav.bak",
        "/clips/..\narration.json",
        "/clips/%6Fpen.wav",
    ]:
        assert serve_clip(root, ["open"], p)[0] == 404


def raw_request(server, method, path, headers, body=None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1])
    conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
    for k, v in headers.items():
        conn.putheader(k, v)
    if body is not None:
        conn.putheader("Content-Length", str(len(body)))
    conn.endheaders(body)
    r = conn.getresponse()
    status = r.status
    conn.close()
    return status


def test_foreign_host_is_421(server):
    port = server.server_address[1]
    assert raw_request(server, "GET", "/ok", {"Host": f"evil.example:{port}"}) == 421
    assert raw_request(server, "GET", "/ok", {"Host": "127.0.0.1"}) == 421
    assert raw_request(server, "GET", "/ok", {"Host": f"localhost:{port}"}) == 200
    assert raw_request(server, "GET", "/ok", {"Host": f"127.0.0.1:{port}"}) == 200


def test_post_needs_json_content_type_415(server):
    host = {"Host": f"127.0.0.1:{server.server_address[1]}"}
    assert raw_request(server, "POST", "/echo", host | {"Content-Type": "text/plain"}, b"{}") == 415
    assert raw_request(server, "POST", "/echo", host, b"{}") == 415
    ok = host | {"Content-Type": "application/json; charset=utf-8"}
    assert raw_request(server, "POST", "/echo", ok, b"{}") == 200


def test_deeply_nested_json_is_400(server):
    assert call(server.url + "/echo", raw=b"[" * 30000 + b"]" * 30000)[0] == 400
