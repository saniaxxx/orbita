"""The command line as systemd runs it (ExecStartPre --check, then the service)."""

import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def orbita(*args, **kw):
    return subprocess.run(
        [sys.executable, "-m", "orbita", *args], cwd=ROOT, capture_output=True, text=True, **kw
    )


def config(tmp_path, extra=""):
    p = tmp_path / "config.yaml"
    p.write_text(
        "satellites:\n  - name: a\n    host: 127.0.0.1\n    port: 1\n"
        "http:\n  host: 127.0.0.1\n  port: 0\n" + extra
    )
    return p


def test_check_example():
    r = orbita("-c", "config.example.yaml", "--check")
    assert r.returncode == 0, r.stderr
    assert "1 satellite(s)" in r.stdout


def test_check_errors(tmp_path):
    assert orbita("-c", str(tmp_path / "missing.yaml"), "--check").returncode == 2
    bad = config(tmp_path, "intents:\n  - type: nope\n")
    r = orbita("-c", str(bad), "--check")
    assert r.returncode == 2 and "unknown type" in r.stderr


def test_runs_and_stops_on_sigterm(tmp_path):
    p = subprocess.Popen(
        [sys.executable, "-m", "orbita", "-c", str(config(tmp_path))],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )  # fmt: skip
    time.sleep(2)
    assert p.poll() is None, p.stdout.read()
    p.send_signal(signal.SIGTERM)
    out, _ = p.communicate(timeout=10)
    assert p.returncode == 0, out
    assert "orbita: 1 satellite(s)" in out
    assert "cannot connect" in out  # nothing listens on port 1; it keeps retrying
