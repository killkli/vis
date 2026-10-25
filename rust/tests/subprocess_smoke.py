#!/usr/bin/env python3
"""Verify external SIGINT/SIGTERM interrupt live pipelines and restore the UI."""

import argparse
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
from terminal_smoke import ROOT, Session


def wait_child(session):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        output = subprocess.check_output(["ps", "-Ao", "ppid=,command="], text=True, timeout=3)
        for line in output.splitlines():
            fields = line.strip().split(maxsplit=1)
            if len(fields) == 2 and fields[0] == str(session.process.pid) and "sleep 20" in fields[1]:
                return
        session.read(0.01)
        assert session.process.poll() is None, "editor exited before starting subprocess"
    raise AssertionError("editor's live sleep subprocess was not observed")


def run(binary):
    with tempfile.TemporaryDirectory(prefix="vis-subprocess-") as directory:
        path = Path(directory) / "input"
        path.write_bytes(b"original\n")
        session = Session(binary, path)
        try:
            session.ready()
            session.send_keys(b"iunsaved\x1b:1 | sleep 20\r")
            wait_child(session)
            started = time.monotonic()
            os.kill(session.process.pid, signal.SIGINT)
            deadline = time.monotonic() + 5
            while b"shell command cancelled" not in session.output:
                session.read()
                assert session.process.poll() is None, "SIGINT lost the editor buffer"
                assert time.monotonic() < deadline, "running pipeline was not interrupted"
            session.send_keys(b"\x1b\x1b:wq\r")
            session.finish()
            assert time.monotonic() - started < 5
            assert path.read_bytes() == b"unsavedoriginal\n"
        finally:
            session.close()
        print("PASS external SIGINT interrupts a live pipeline without losing prior edits")

        session = Session(binary, path)
        try:
            session.ready()
            session.send_keys(b":1 | sleep 20\r")
            wait_child(session)
            started = time.monotonic()
            os.kill(session.process.pid, signal.SIGTERM)
            session.finish(success=False)
            assert session.process.returncode != 0
            assert time.monotonic() - started < 5
            assert path.read_bytes() == b"unsavedoriginal\n"
        finally:
            session.close()
        print("PASS external SIGTERM cancels a live pipeline and restores terminal promptly")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    run(parser.parse_args().binary.resolve())
