import json
import subprocess
from pathlib import Path

import numpy as np
import pytest
import soundfile as sf

from narrate import NarrateError
from narrate.cli import main
from narrate.manifest import Manifest, ManifestLine
from narrate.mux import Cue, find_ffmpeg, load_timeline, mux, mux_command, overruns, script_cues
from narrate.script import load_script


def line(id_, duration=1.0, status="approved", clip="clips/{}.wav", at=None):
    return ManifestLine(
        id=id_,
        text="t",
        spoken="t",
        kind="step",
        clip=clip.format(id_) if clip else None,
        duration=duration,
        at=at,
        status=status,
    )


def write_manifest(root: Path, lines, draft=False) -> Path:
    m = Manifest(script="demo.narration.toml", voice="v", draft=draft, sample_rate=24000, lines=list(lines))
    p = root / "narration.json"
    m.write(p)
    for ln in lines:
        if ln.clip:
            (root / ln.clip).parent.mkdir(parents=True, exist_ok=True)
            (root / ln.clip).touch()
    return p


def test_mux_command_is_exact():
    cmd = mux_command("ff", Path("v.mp4"), [(Path("a.wav"), 0.15), (Path("b.wav"), 3.4)], Path("o.mp4"))
    assert cmd == [
        "ff",
        "-y",
        "-loglevel",
        "error",
        "-i",
        "v.mp4",
        "-i",
        "a.wav",
        "-i",
        "b.wav",
        "-filter_complex",
        "[1:a]aresample=48000,adelay=150:all=1[a1];"
        "[2:a]aresample=48000,adelay=3400:all=1[a2];"
        "[a1][a2]amix=inputs=2:normalize=0:dropout_transition=0,"
        "loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000[aout]",
        "-map",
        "0:v",
        "-map",
        "[aout]",
        "-c:v",
        "copy",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-movflags",
        "+faststart",
        "o.mp4",
    ]


def test_overruns():
    m = Manifest("s", "v", False, 24000, lines=[line("a", 3.0), line("b", 1.0)])
    msgs = overruns([Cue("b", 2.5), Cue("a", 0.0)], m)
    assert len(msgs) == 1
    assert "'a'" in msgs[0] and "'b'" in msgs[0] and "0.5" in msgs[0]
    assert overruns([Cue("a", 0.0), Cue("b", 3.0)], m) == []


def test_load_timeline_and_script_cues(root, write_script):
    (root / "t").mkdir()
    tl = root / "t" / "timeline.json"
    tl.write_text(
        json.dumps(
            {
                "version": 1,
                "video": "silent.mp4",
                "narration": "../narration.json",
                "fps": 30,
                "cues": [{"id": "a", "start": 0.5}, {"id": "b", "start": 2}],
            }
        ),
        encoding="utf-8",
    )
    video, manifest, cues = load_timeline(tl)
    assert video == tl.parent / "silent.mp4"
    assert manifest == tl.parent / "../narration.json"
    assert cues == [Cue("a", 0.5), Cue("b", 2.0)]

    s = write_script(root, '[[line]]\nid = "a"\ntext = "One."\nat = 1.5\n\n[[line]]\nid = "b"\ntext = "Two."\nat = 4\n')
    assert script_cues(load_script(s)) == [Cue("a", 1.5), Cue("b", 4.0)]
    s2 = write_script(
        root, '[[line]]\nid = "a"\ntext = "One."\n\n[[line]]\nid = "b"\ntext = "Two."\n', "x.narration.toml"
    )
    with pytest.raises(NarrateError, match="'a', 'b'"):
        script_cues(load_script(s2))


@pytest.mark.parametrize(
    "mutate",
    [
        {"version": 2},
        {"version": "1"},
        {"cues": "no"},
        {"cues": [{"id": "a", "start": -1}]},
        {"cues": [{"id": "a", "start": float("inf")}]},
        {"cues": [{"id": "a", "start": "1"}]},
        {"cues": [{"id": "a", "start": 1}, {"id": "a", "start": 2}]},
        {"cues": [{"start": 1}]},
        {"video": 3},
    ],
)
def test_load_timeline_strict(root, mutate):
    data = {"version": 1, "video": "v.mp4", "narration": "n.json", "fps": 30, "cues": [{"id": "a", "start": 0}]}
    data.update(mutate)
    tl = root / "timeline.json"
    tl.write_text(json.dumps(data), encoding="utf-8")
    with pytest.raises(NarrateError, match="timeline.json"):
        load_timeline(tl)


@pytest.fixture
def fake_ffmpeg(monkeypatch):
    calls = []

    def run(cmd, **kw):
        calls.append(cmd)
        Path(cmd[-1]).write_bytes(b"x")
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr("narrate.mux.subprocess.run", run)
    monkeypatch.setattr("narrate.mux.find_ffmpeg", lambda: "ff")
    return calls


CASES = {
    "draft": ({}, True, [Cue("a", 0)], None, "draft"),
    "unknown": ({}, False, [Cue("zzz", 0)], None, "zzz"),
    "noclip": ({"clip": None}, False, [Cue("a", 0)], None, "no clip"),
    "unapproved": ({"status": "new"}, False, [Cue("a", 0)], "allow_unapproved", "'a'"),
    "overrun": ({"duration": 3.0}, False, [Cue("a", 0), Cue("b", 1)], "allow_overlap", "overlap"),
}


@pytest.mark.parametrize("name", list(CASES))
def test_mux_refusals(root, fake_ffmpeg, name):
    opts, draft, cues, flag, needle = CASES[name]
    manifest = write_manifest(root, [line("a", **opts), line("b")], draft=draft)
    video = root / "v.mp4"
    video.write_bytes(b"v")
    out = root / "out.mp4"
    with pytest.raises(NarrateError, match=needle):
        mux(video, manifest, cues, out)
    assert not out.exists() and fake_ffmpeg == []
    if flag:
        mux(video, manifest, cues, out, **{flag: True})
        assert out.exists()
        assert not (root / "out.mp4.tmp.mp4").exists()


def test_mux_ffmpeg_failure_leaves_no_output(root, monkeypatch):
    manifest = write_manifest(root, [line("a")])
    video = root / "v.mp4"
    video.write_bytes(b"v")
    err = "\n".join(f"e{i}" for i in range(30))
    monkeypatch.setattr("narrate.mux.find_ffmpeg", lambda: "ff")
    monkeypatch.setattr("narrate.mux.subprocess.run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, 1, "", err))
    out = root / "out.mp4"
    with pytest.raises(NarrateError) as ei:
        mux(video, manifest, [Cue("a", 0)], out)
    text = str(ei.value)
    assert "e29" in text and "e10" in text and "e9\n" not in text
    assert not out.exists()


def test_cli_mux_argument_rules(root):
    assert main(["mux", "-o", str(root / "o.mp4")]) == 2
    assert main(["mux", "--timeline", "t.json", "--video", "v.mp4", "-o", "o.mp4"]) == 2
    assert main(["mux", "--video", "v.mp4", "-o", "o.mp4"]) == 2


def test_cli_mux_script_mode(root, write_script, fake_ffmpeg):
    s = write_script(root, '[[line]]\nid = "a"\ntext = "One."\nat = 0.5\n')
    out_dir = root / "out" / "demo"
    out_dir.mkdir(parents=True)
    write_manifest(out_dir, [line("a")])
    video = root / "v.mp4"
    video.write_bytes(b"v")
    out = root / "o.mp4"
    assert main(["mux", "--video", str(video), "--script", str(s), "-o", str(out)]) == 0
    assert out.exists() and len(fake_ffmpeg) == 1
    assert str(out_dir / "clips" / "a.wav") in fake_ffmpeg[0]


def test_mux_end_to_end(root):
    try:
        ff = find_ffmpeg()
    except NarrateError:
        pytest.skip("ffmpeg not available")
    video = root / "silent.mp4"
    subprocess.run(
        [
            ff,
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x240:d=3",
            "-pix_fmt",
            "yuv420p",
            str(video),
        ],
        check=True,
    )
    (root / "clips").mkdir()
    t = np.arange(24000) / 24000
    for name, f in (("a", 440), ("b", 660)):
        sf.write(root / "clips" / f"{name}.wav", (0.3 * np.sin(2 * np.pi * f * t)).astype("float32"), 24000)
    manifest = write_manifest(root, [line("a"), line("b")])
    out = root / "out.mp4"
    mux(video, manifest, [Cue("a", 0.2), Cue("b", 1.5)], out)
    assert out.exists() and out.stat().st_size > 0
    info = subprocess.run([ff, "-i", str(out)], capture_output=True, text=True, encoding="utf-8", errors="replace")
    assert "Audio:" in info.stderr and "Video:" in info.stderr
    assert not (root / "out.mp4.tmp.mp4").exists()


def test_overrun_tolerance_boundary():
    m = Manifest("s", "v", False, 24000, lines=[line("a", 1.0), line("b", 1.0)])
    assert overruns([Cue("a", 0.0), Cue("b", 0.9995)], m) == []
    assert len(overruns([Cue("a", 0.0), Cue("b", 0.998)], m)) == 1


@pytest.mark.parametrize("at", ["nan", "inf", "-1"])
def test_script_cues_rejects_bad_at(root, write_script, at):
    s = write_script(root, f'[[line]]\nid = "a"\ntext = "One."\nat = {at}\n')
    with pytest.raises(NarrateError, match=r"demo\.narration\.toml.*'a'"):
        script_cues(load_script(s))


def test_mux_paths_absolute_with_relative_inputs(root, fake_ffmpeg, monkeypatch):
    write_manifest(root, [line("a")])
    (root / "v.mp4").write_bytes(b"v")
    monkeypatch.chdir(root)
    mux(Path("v.mp4"), Path("narration.json"), [Cue("a", 0)], Path("-o.mp4"))
    cmd = fake_ffmpeg[0]
    ins = [cmd[i + 1] for i, a in enumerate(cmd) if a == "-i"]
    assert all(Path(p).is_absolute() for p in ins) and Path(cmd[-1]).is_absolute()
    assert (root / "-o.mp4").exists()
