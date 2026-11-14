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
