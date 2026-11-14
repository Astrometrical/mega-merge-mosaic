from narrate.cli import main


def test_version(capsys):
    assert main(["--version"]) == 0
    assert capsys.readouterr().out.strip() == "narrate 0.1.0"


def test_no_command_prints_usage_and_returns_2(capsys):
    assert main([]) == 2
