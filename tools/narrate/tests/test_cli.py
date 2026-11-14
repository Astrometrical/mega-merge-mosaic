import pytest

from narrate import fakes
from narrate.build import default_out
from narrate.cli import main
from narrate.fakes import FakeWorld
from narrate.script import load_script

LINE = "Each filter is a group of its own."


@pytest.fixture
def world(monkeypatch):
    w = FakeWorld()
    monkeypatch.setattr(fakes, "_DEFAULT", w)
    return w


def make(root, write_script, voice="test"):
    return write_script(root, f'voice = "{voice}"\n\n[[line]]\nid = "one"\ntext = "{LINE}"\n')


def test_version(capsys):
    assert main(["--version"]) == 0
    assert capsys.readouterr().out.strip() == "narrate 0.1.0"


def test_no_command_prints_usage_and_returns_2(capsys):
    assert main([]) == 2


def test_cli_build_end_to_end(root, write_script, voice_library, world, capsys):
    path = make(root, write_script)
    assert main(["build", str(path)]) == 0
    out = default_out(load_script(path))
    assert (out / "narration.json").is_file()
    printed = capsys.readouterr().out
    assert "one" in printed and str(out / "narration.json") in printed


def test_cli_build_flagged_exit_codes(root, write_script, voice_library, world, capsys):
    path = make(root, write_script)
    world.script(LINE, heard=["garbled"])
    assert main(["build", str(path)]) == 1
    assert "garbled" in capsys.readouterr().out
    assert main(["build", str(path), "--allow-flagged"]) == 0


def test_cli_user_error_exit_2(root, write_script, voice_library, world, capsys):
    path = make(root, write_script, voice="missing")
    assert main(["build", str(path)]) == 2
    assert "missing" in capsys.readouterr().err


def test_cli_draft(root, write_script, capsys):
    path = make(root, write_script)
    assert main(["draft", str(path), "--pace", "4.0"]) == 0
    out = default_out(load_script(path))
    assert (out / "narration.json").is_file()
    assert "draft" in capsys.readouterr().out


def test_cli_missing_extra_exit_2(root, write_script, voice_library, monkeypatch, capsys):
    import builtins

    real = builtins.__import__

    def fake_import(name, *a, **kw):
        if name == "narrate.backends.qwen":
            raise ImportError("no torch")
        return real(name, *a, **kw)

    path = make(root, write_script)
    prof = voice_library / "test" / "voice.toml"
    prof.write_text(prof.read_text(encoding="utf-8").replace('backend = "fake"', 'backend = "qwen3-tts"'))
    monkeypatch.setattr(builtins, "__import__", fake_import)
    assert main(["build", str(path)]) == 2
    assert "uv sync --extra qwen" in capsys.readouterr().err


@pytest.mark.parametrize("pace", ["0", "-2", "nan", "abc"])
def test_cli_bad_pace_exit_2(root, write_script, pace):
    path = make(root, write_script)
    assert main(["draft", str(path), "--pace", pace]) == 2


def test_cli_unexpected_exception_exit_3(root, write_script, voice_library, world, monkeypatch, capsys):
    import narrate.cli

    def boom(*a, **kw):
        raise RuntimeError("kaboom")

    monkeypatch.setattr(narrate.cli, "build", boom)
    assert main(["build", str(make(root, write_script))]) == 3
    err = capsys.readouterr().err
    assert "kaboom" in err and "internal error" in err


def test_cli_review_no_browser(root, write_script, voice_library, world, capsys, monkeypatch):
    import json
    import threading
    import urllib.request

    from narrate import review

    script = make(root, write_script)
    got = {}
    ready = threading.Event()
    opened = []
    real_serve = review.serve

    def on_ready(server):
        got["server"] = server
        ready.set()

    monkeypatch.setattr("narrate.cli.serve", lambda app, port, opener: real_serve(app, port, opener, on_ready))
    monkeypatch.setattr("narrate.cli.webbrowser.open", opened.append)
    result = {}
    t = threading.Thread(
        target=lambda: result.update(code=main(["review", str(script), "--no-browser", "--port", "0"]))
    )
    t.start()
    assert ready.wait(30)
    server = got["server"]
    with urllib.request.urlopen(server.url + "/api/state") as r:
        state = json.loads(r.read())
    assert state["manifest"]["lines"][0]["id"] == "one"
    server.shutdown()
    t.join(10)
    assert result["code"] == 0
    assert opened == []
    assert "Reviewing" in capsys.readouterr().out


def test_cli_review_opens_browser_and_build_errors_skip_server(root, write_script, voice_library, world, monkeypatch):
    opened = []
    monkeypatch.setattr("narrate.cli.serve", lambda app, port, opener: opener("http://x/"))
    monkeypatch.setattr("narrate.cli.webbrowser.open", opened.append)
    script = make(root, write_script)
    assert main(["review", str(script), "--port", "0"]) == 0
    assert opened == ["http://x/"]
    bad = make(root, write_script, voice="nobody")
    called = []
    monkeypatch.setattr("narrate.cli.serve", lambda *a: called.append(1))
    assert main(["review", str(bad)]) == 2
    assert called == []
