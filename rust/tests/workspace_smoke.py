#!/usr/bin/env python3
"""Exercise actual multi-file/split-window editing under a headless Unix PTY."""

import argparse
import re
from pathlib import Path
import subprocess
import tempfile
import time
from terminal_smoke import ROOT, Session


def wait_text(session, value):
    deadline = time.monotonic() + 10
    while value not in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', bytes(session.output)):
        session.read()
        assert session.process.poll() is None, bytes(session.output[-2000:])
        assert time.monotonic() < deadline, f"missing rendered text: {value!r}"


def run(binary):
    with tempfile.TemporaryDirectory(prefix="vis-windows-") as directory:
        directory = Path(directory)
        first, second = directory / "first", directory / "second file"
        first.write_bytes(b"alpha\n")
        second.write_bytes(b"beta\n")
        session = Session(binary, [first, second])
        try:
            session.ready()
            wait_text(session, b"alpha")
            wait_text(session, b"beta")
            session.send_keys(b"\x17piA\x1b:w\r\x17wiB\x1b:w\r:qall\r")
            session.finish()
            assert first.read_bytes() == b"Aalpha\n"
            assert second.read_bytes() == b"Bbeta\n"
        finally:
            session.close()
        print("PASS multiple command-line files retain independent contents and saves")

        first.write_bytes(b"a\nb\nc\n")
        session = Session(binary, first)
        try:
            session.ready()
            session.send_keys(b":split\rjiX\x1b\x17piY\x1b:q\r:wq\r")
            session.finish()
            assert first.read_bytes() == b"Ya\nXb\nc\n", first.read_bytes()
        finally:
            session.close()
        print("PASS shared split buffer retains independent cursors and unsaved edits")

        first.write_bytes(b"alpha\n")
        second.write_bytes(b"beta\n")
        session = Session(binary, first)
        try:
            session.ready()
            session.send_keys(f":vsplit '{second}'\r".encode() + b"iB\x1b:w\r:q\riA\x1b:wq\r")
            session.finish()
            assert first.read_bytes() == b"Aalpha\n"
            assert second.read_bytes() == b"Bbeta\n"
        finally:
            session.close()
        print("PASS vertical split and quoted filename return to the original buffer")

        first.write_bytes(b"original\n")
        session = Session(binary, ["-R", first])
        try:
            session.ready()
            session.send_keys(b"iX\x1b:w\r")
            wait_text(session, b"file is read-only")
            assert first.read_bytes() == b"original\n"
            session.send_keys(b"\x1b\x1b:wq!\r")
            session.finish()
            assert first.read_bytes() == b"Xoriginal\n"
        finally:
            session.close()
        print("PASS readonly mode refuses ordinary save and permits explicit forced save")

        session = Session(binary, first)
        try:
            session.ready()
            session.send_keys(b"iUNSAVED\x1b")
            session.send_keys(f":edit '{second}'\r".encode())
            wait_text(session, b"unsaved changes")
            session.send_keys(b"\x1b\x1b" + f":edit! '{second}'\r:q\r".encode())
            session.finish()
            assert first.read_bytes() == b"Xoriginal\n"
        finally:
            session.close()
        print("PASS edit refuses unsaved replacement until force is explicit")

        saved = directory / "stdin saved"
        session = Session(binary, ["-", second], input_data=b"stdin\0\xff\n")
        try:
            session.ready()
            session.send_keys(b"\x17p" + f":w '{saved}'\r".encode() + b"\x17w:q\r:q\r")
            session.finish()
            assert saved.read_bytes() == b"stdin\0\xff\n"
        finally:
            session.close()
        print("PASS stdin and named file windows coexist without copying or losing binary data")

        result = subprocess.run([str(binary), "+qall!", str(first), str(second)],
                                input=b"", capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr
        print("PASS startup commands execute before requiring a terminal")

        first.write_bytes(b"FIRST\n")
        second.write_bytes(b"SECOND\n")
        third = directory / "third"
        third.write_bytes(b"THIRD\n")
        capture_a, capture_b = directory / "a.capture", directory / "b.capture"
        result = subprocess.run([str(binary), f"+> cat > '{capture_a}'", str(first),
                                 f"+> cat > '{capture_b}'", str(second), "+qall!", str(third)],
                                input=b"", capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr
        assert capture_a.read_bytes() == b"FIRST\n"
        assert capture_b.read_bytes() == b"SECOND\n"
        print("PASS startup commands apply to their following file in argument order")

        session = Session(binary, ["-"], input_data=b"stdin\0\xff\n", capture_stdout=True)
        try:
            session.ready()
            session.send_keys(b"iEDIT\x1b:wq\r")
            session.finish()
            assert session.process.stdout.read() == b"EDITstdin\0\xff\n"
        finally:
            session.close()
        print("PASS stdin wq streams edited binary data to redirected stdout")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    run(parser.parse_args().binary.resolve())
