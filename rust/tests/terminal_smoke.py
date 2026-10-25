#!/usr/bin/env python3
"""Headless Unix PTY checks for editing, terminal restoration and signals.

Build the default-feature binary before running. No GUI or Python packages needed.
"""

import argparse
import fcntl
import os
import re
from pathlib import Path
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]


def wait_until_insert_gone(session, message):
    """Wait for a redrawn status line without the INSERT marker.

    The standard status format renders NORMAL as an empty string, so the
    absence of INSERT from a redraw is the observable transition back to
    normal mode; the follow-up keys prove that interpretation.
    """
    deadline = time.monotonic() + 5
    while True:
        before = len(session.output)
        session.read()
        chunk = bytes(session.output[before:])
        if chunk and b"INSERT" not in chunk:
            return
        assert session.process.poll() is None, "SIGINT discarded unsaved changes"
        assert time.monotonic() < deadline, message


class Session:
    def __init__(self, binary, path, input_data=None, capture_stdout=False):
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        self.output = bytearray()
        arguments = [str(value) for value in path] if isinstance(path, (list, tuple)) else ([str(path)] if path is not None else [])
        self.process = subprocess.Popen(
            [str(binary)] + arguments,
            stdin=self.slave if input_data is None else subprocess.PIPE,
            stdout=subprocess.PIPE if capture_stdout else self.slave,
            stderr=self.slave, start_new_session=True,
            env={**os.environ, "TERM": "xterm-256color"},
        )
        if input_data is not None:
            self.process.stdin.write(input_data)
            self.process.stdin.close()

    def read(self, duration=0.05):
        if select.select([self.master], [], [], duration)[0]:
            self.output.extend(os.read(self.master, 65536))

    def ready(self):
        deadline = time.monotonic() + 10
        while b"\x1b[?1049h" not in self.output:
            self.read()
            if self.process.poll() is not None or time.monotonic() > deadline:
                raise AssertionError(f"editor did not initialize: {bytes(self.output)!r}")

    def send(self, data):
        offset = 0
        deadline = time.monotonic() + 30
        # Drain output while writing: both PTY directions have bounded queues.
        # A blocking multi-MiB write can otherwise deadlock against UI redraws.
        os.set_blocking(self.master, False)
        try:
            while offset < len(data):
                readable, writable, _ = select.select([self.master], [self.master], [], 0.1)
                if readable:
                    self.output.extend(os.read(self.master, 65536))
                if writable:
                    offset += os.write(self.master, memoryview(data)[offset:offset + 65536])
                assert time.monotonic() < deadline, "timed out sending terminal input"
        finally:
            os.set_blocking(self.master, True)

    def send_keys(self, data):
        # Match upstream test/util/keys: a standalone Escape is separated from
        # the next key so a native terminal can distinguish it from Meta input.
        # Wait for the editor to go idle after each such fragment instead of a
        # fixed delay: a slow frame (for example one that just opened a split)
        # can otherwise coalesce Escape and the following byte into a Meta key.
        start = 0
        for match in re.finditer(rb'\x1b(?![\[\]OP^_])', data):
            self.send(data[start:match.end()])
            quiet = time.monotonic() + 0.04
            deadline = time.monotonic() + 1.0
            while time.monotonic() < deadline:
                before = len(self.output)
                self.read(0.01)
                if len(self.output) != before:
                    quiet = time.monotonic() + 0.04
                elif time.monotonic() >= quiet:
                    break
            start = match.end()
        if start < len(data):
            self.send(data[start:])

    def finish(self, success=True):
        deadline = time.monotonic() + 10
        while self.process.poll() is None:
            self.read()
            if time.monotonic() > deadline:
                raise AssertionError(f"editor did not exit: {bytes(self.output[-2000:])!r}")
        self.read(0)
        if success:
            assert self.process.returncode == 0, bytes(self.output[-2000:])
        self.assert_restored()
        assert b"\x1b[?1049l" in self.output, "alternate screen not restored"

    def assert_restored(self):
        restored = termios.tcgetattr(self.slave)
        original = self.original.copy()
        # macOS sets PENDIN when restoring canonical input after raw mode. This
        # kernel-maintained retype flag is not a user terminal setting.
        pending = getattr(termios, "PENDIN", 0)
        restored[3] &= ~pending
        original[3] &= ~pending
        assert restored == original, f"terminal attributes not restored: before={original!r}, after={restored!r}"

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)
        os.close(self.slave)


def run(binary):
    with tempfile.TemporaryDirectory(prefix="vis-terminal-") as directory:
        path = Path(directory) / "input"
        path.write_bytes(b"start\n")
        session = Session(binary, path)
        try:
            session.ready()
            session.send_keys(b"iRust \x1b:wq\r")
            session.finish()
            assert path.read_bytes() == b"Rust start\n", path.read_bytes()
        finally:
            session.close()
        print("PASS insert/save/quit and terminal restoration")

        session = Session(binary, path)
        try:
            session.ready()
            before = path.read_bytes()
            session.send_keys(b"iunsaved\x1b:q\r")
            deadline = time.monotonic() + 5
            while b"unsaved changes" not in session.output:
                session.read()
                assert session.process.poll() is None, "quit discarded unsaved changes"
                assert time.monotonic() < deadline, "missing unsaved-change message"
            # Clear any remaining prompt/mode state before the forced quit.
            session.send_keys(b"\x1b\x1b:q!\r")
            session.finish()
            assert path.read_bytes() == before
        finally:
            session.close()
        print("PASS quit refuses unsaved changes and forced quit leaves disk untouched")

        session = Session(binary, path)
        try:
            session.ready()
            session.send_keys(b"i\x1b[200~literal\x1b:q!\n\x1b[201~\x1b:wq\r")
            session.finish()
            assert path.read_bytes() == b"literal\x1b:q!\nRust start\n", path.read_bytes()
        finally:
            session.close()
        print("PASS bracketed paste preserves literal command/control bytes")

        piped = path.with_name("piped")
        session = Session(binary, None, input_data=b"piped\0\xff\n")
        try:
            session.ready()
            session.send_keys(f":w {piped}\r:q\r".encode())
            session.finish()
            assert piped.read_bytes() == b"piped\0\xff\n"
        finally:
            session.close()
        print("PASS piped input uses terminal events and saves binary content")

        session = Session(binary, path)
        try:
            session.ready()
            os.kill(session.process.pid, signal.SIGTERM)
            session.finish(success=False)
            assert session.process.returncode != 0
        finally:
            session.close()
        print("PASS SIGTERM restores terminal")

        interrupted = path.with_name("interrupted")
        interrupted.write_bytes(b"original\n")
        session = Session(binary, interrupted)
        try:
            session.ready()
            session.send_keys(b"iunsaved")
            # Wait for rendering to prove the input was handled before signalling.
            deadline = time.monotonic() + 5
            while b"unsaved" not in session.output:
                session.read()
                assert time.monotonic() < deadline
            os.kill(session.process.pid, signal.SIGINT)
            wait_until_insert_gone(session, "SIGINT did not cancel insert mode")
            # Enter after SIGINT must be interpreted in normal mode, not inserted.
            session.send_keys(b":wq\r")
            session.finish()
            assert interrupted.read_bytes() == b"unsavedoriginal\n"
        finally:
            session.close()
        print("PASS SIGINT cancels input without losing unsaved edits")

        session = Session(binary, path)
        try:
            session.ready()
            session.send_keys(b"\x1a")
            deadline = time.monotonic() + 10
            while True:
                pid, status = os.waitpid(session.process.pid, os.WNOHANG | os.WUNTRACED)
                if pid and os.WIFSTOPPED(status):
                    break
                session.read()
                assert time.monotonic() < deadline, "Ctrl-Z did not suspend editor"
            session.assert_restored()
            os.kill(session.process.pid, signal.SIGCONT)
            session.send_keys(b":q\r")
            session.finish()
        finally:
            session.close()
        print("PASS suspend/resume restores terminal before stopping")

        conflict = path.with_name("conflict")
        conflict.write_bytes(b"original\n")
        session = Session(binary, conflict)
        try:
            session.ready()
            conflict.write_bytes(b"external\n")
            session.send_keys(b"iX\x1b:w\r")
            deadline = time.monotonic() + 5
            while b"file changed on disk" not in session.output:
                session.read()
                assert session.process.poll() is None, "save conflict unexpectedly exited editor"
                assert time.monotonic() < deadline, "missing save conflict message"
            assert conflict.read_bytes() == b"external\n"
            session.send_keys(b"\x1b\x1b:w!\r:q\r")
            session.finish()
            assert conflict.read_bytes() == b"Xoriginal\n"
        finally:
            session.close()
        print("PASS external save conflict preserves disk and editor buffer until forced")

        large_paste = path.with_name("large-paste")
        large_paste.write_bytes(b"original\n")
        session = Session(binary, large_paste)
        try:
            session.ready()
            session.send_keys(b"iunsaved\x1b[200~" + b"x" * (8 * 1024 * 1024 + 16)
                         + b":q!\r\x1b[201~\x1b:wq\r")
            session.finish()
            assert b"paste discarded" in session.output
            assert large_paste.read_bytes() == b"unsavedoriginal\n"
        finally:
            session.close()
        print("PASS oversized paste is rejected without exiting or losing prior edits")

        interrupted_paste = path.with_name("interrupted-paste")
        interrupted_paste.write_bytes(b"original\n")
        session = Session(binary, interrupted_paste)
        try:
            session.ready()
            session.send_keys(b"iunsaved")
            session.send(b"\x1b[200~partial")
            deadline = time.monotonic() + 0.15
            while time.monotonic() < deadline:
                session.read(0.01)
            os.kill(session.process.pid, signal.SIGINT)
            wait_until_insert_gone(session, "SIGINT did not cancel paste editing")
            session.send(b":q!\r\x1b[201~")
            session.send_keys(b"iY\x1b:wq\r")
            session.finish()
            assert interrupted_paste.read_bytes() == b"unsavedYoriginal\n"
        finally:
            session.close()
        print("PASS interrupted paste keeps discarding its remaining command bytes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    args = parser.parse_args()
    run(args.binary.resolve())


if __name__ == "__main__":
    main()
