#!/usr/bin/env python3
"""Compare C/Rust drawing callbacks and motion geometry for buffered key bursts."""
import argparse
import json
from pathlib import Path
import tempfile
import time

from example_latency import settle
from lua_cli_smoke import LuaSession, configuration
from terminal_smoke import ROOT

CASES = [b"l" * 20, b"j" * 25 + b"H", b"gg" + b"j" * 40 + b"L", b"gg" + b"j" * 10 + b"ztH"]


def capture(binary, directory):
    audit = directory / "draws"
    config = configuration(directory, "config", """
vis.events={win_status=function(win)
 local f=assert(io.open(AUDIT,'a'))
 f:write(string.format('%d:%d\\n',win.selection.line,win.selection.col))
 f:close()
end}
""".replace("AUDIT", json.dumps(str(audit))))
    path = directory / "input"
    path.write_text("".join(f"line {number:03} abcdefghijklmnopqrstuvwxyz\n" for number in range(100)))
    session = LuaSession(binary, path, config)
    try:
        session.ready()
        settle(session, time.monotonic())
        assert audit.exists(), bytes(session.output[-2000:])
        result = []
        for keys in CASES:
            before = len(audit.read_text().splitlines())
            started = time.monotonic()
            session.send(keys)
            settle(session, started)
            records = audit.read_text().splitlines()[before:]
            assert records, (binary, keys, bytes(session.output[-2000:]))
            result.append((len(records), records[-1]))
        session.send(b":qall!\r")
        session.finish()
        return result
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, default=ROOT / "vis")
    parser.add_argument("--port", type=Path, default=ROOT / "rust/target/release/vis-rs")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="vis-input-batch-") as folder:
        directory = Path(folder)
        native = directory / "native"
        port = directory / "port"
        native.mkdir()
        port.mkdir()
        expected = capture(args.native.resolve(), native)
        actual = capture(args.port.resolve(), port)
        for keys, original, rust in zip(CASES, expected, actual):
            assert original == rust, (keys, original, rust)
        print(f"PASS {len(CASES)} buffered-input draw counts and final line/column values: {actual}")


if __name__ == "__main__":
    main()
