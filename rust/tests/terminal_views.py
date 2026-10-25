#!/usr/bin/env python3
"""Headless PTY checks for native modified keys, split panels and resizing.

Unlike replay, these checks deliver actual terminal byte sequences. Escape keys
are separated from following text by the decoder timeout; Alt prefixes are not.
The binary is snapshotted to avoid concurrent Cargo feature builds replacing it.
"""

import argparse
import fcntl
import os
from pathlib import Path
import re
import shutil
import struct
import tempfile
import termios
import time

from terminal_smoke import ROOT, Session


def settle(session, duration=0.15):
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        session.read(min(0.03, max(0, deadline - time.monotonic())))


def raw(session, data):
    # These inputs are small enough to fit in the PTY queue. The base suite's
    # higher-level helper may deliberately delay Escape for ordinary commands.
    while data:
        data = data[os.write(session.master, data):]


def escape(session):
    raw(session, b"\x1b")
    settle(session, 0.08)


def save_quit(session):
    escape(session)
    raw(session, b":wq\r")
    session.finish()


def resize(session, rows, columns):
    fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
    settle(session, 0.4)


def cursor(session):
    addresses = re.findall(rb"\x1b\[(\d+);(\d+)H", session.output)
    assert addresses, "no screen cursor position emitted"
    return tuple(map(int, addresses[-1]))


def run(binary):
    with tempfile.TemporaryDirectory(prefix="vis-views-") as temporary:
        directory = Path(temporary)
        executable = directory / "vis-rs"
        shutil.copy2(binary, executable)

        # This fixture needs genuine ESC+Ctrl, not the old C keys.c helper,
        # which accidentally discarded the ALT modifier in Unicode key output.
        fixture = ROOT / "test/vis/selections/new-above-first-below-last"
        path = directory / "meta"
        path.write_bytes(fixture.with_suffix(".in").read_bytes())
        session = Session(executable, path)
        try:
            session.ready()
            raw(session, b"/3\r2\x1b\x0b2\x1b\x0ai>")
            save_quit(session)
            assert path.read_bytes() == fixture.with_suffix(".ref").read_bytes(), path.read_bytes()
        finally:
            session.close()

        path = directory / "wrapped"
        path.write_bytes(b"abcdefghijklmnop\n")
        session = Session(executable, path)
        try:
            session.ready()
            resize(session, 6, 5)
            raw(session, b"14l")
            settle(session)
            assert cursor(session) == (3, 5), cursor(session)
            raw(session, b"l")
            settle(session)
            assert cursor(session) == (4, 1), cursor(session)
            resize(session, 6, 8)
            assert cursor(session) == (2, 8), cursor(session)
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()

        path = directory / "wide"
        path.write_text("ab界c")
        session = Session(executable, path)
        try:
            session.ready()
            resize(session, 5, 3)
            raw(session, b"2l")
            settle(session)
            assert cursor(session) == (2, 1), cursor(session)
            raw(session, b"l")
            settle(session)
            assert cursor(session) == (2, 3), cursor(session)
            assert "界c".encode() in session.output
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()

        path = directory / "tabs"
        path.write_bytes(b"a\tb")
        session = Session(executable, path)
        try:
            session.ready()
            resize(session, 5, 8)
            raw(session, b"2l")
            settle(session)
            assert cursor(session) == (2, 1), cursor(session)
            raw(session, b":set tabwidth=4\r")
            settle(session)
            assert cursor(session) == (1, 5), cursor(session)
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()

        path = directory / "shared"
        path.write_bytes(b"one\ntwo\n")
        session = Session(executable, path)
        try:
            session.ready()
            raw(session, b":split\r")
            settle(session)
            assert b"\x1b[12;1H" in session.output, "horizontal status row missing"
            raw(session, b":vsplit\riX")
            settle(session)
            assert b"\x1b[1;26H|" in session.output and b"\x1b[1;52H|" in session.output, "vertical separators missing"
            escape(session)
            raw(session, b":w\r:qall!\r")
            session.finish()
            assert path.read_bytes() == b"Xone\ntwo\n", path.read_bytes()
            assert session.output.count(b"Xone") >= 3, "shared buffer update missing in another pane"
        finally:
            session.close()

        fixture = ROOT / "test/vis/selections/align-indent"
        path = directory / "backtab"
        path.write_bytes(fixture.with_suffix(".in").read_bytes())
        session = Session(executable, path)
        try:
            session.ready()
            raw(session, b"vGIi\x1b[Z|")
            save_quit(session)
            assert path.read_bytes() == fixture.with_suffix(".ref").read_bytes(), path.read_bytes()
        finally:
            session.close()

        path = directory / "controls"
        path.write_bytes(b"left\x1b[2J\n1234567890\n")
        session = Session(executable, path)
        try:
            session.ready()
            settle(session)
            visible = re.sub(rb"\x1b\[[0-9;]*m", b"", session.output)
            assert b"left^[[2J" in visible, "file escape was not displayed literally"
            assert session.output.count(b"\x1b[2J") == 1, "file content injected screen clearing"
            mark = len(session.output)
            fcntl.ioctl(session.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 4, 9, 0, 0))
            settle(session, 0.65)
            addresses = re.findall(rb"\x1b\[(\d+);(\d+)H", session.output[mark:])
            assert addresses, "resize did not repaint"
            assert all(1 <= int(row) <= 4 and 1 <= int(column) <= 9 for row, column in addresses), addresses
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()

    print("PASS 7 PTY modified-key, split/shared-buffer, softwrap/Unicode/tabwidth, resize and safe-display checks")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-vis", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    run(parser.parse_args().rust_vis.resolve())
