#!/usr/bin/env python3
"""Compare actual C/Rust cursor bytes and viewport ranges after native motions.

Uses headless PTYs and isolated Lua configs. Q maps the named slide-down action
so the sequence exercises that action directly alongside built-in motion keys.
"""
import argparse
import json
from pathlib import Path
import tempfile
import time

from example_latency import settle
from lua_cli_smoke import LuaSession, configuration
from terminal_smoke import ROOT

CASES = [b"G", b"$", b"gk", b"gj", b"H", b"L", b"zz", b"R", b"Q",
         b"3gk", b"2gj", b"zt", b"zb", b"U", b"D", b"\x02", b"\x06",
         b"2H", b"2L", b"gg", b"gj" * 10]


def capture(binary, directory, path, options, columns=80, rows=24):
    audit = directory / "audit"
    source = r'''
vis.events={
 init=function()
  for key,action in pairs{Q='vis-window-slide-down',R='vis-window-slide-up',
    U='vis-window-halfpage-up',D='vis-window-halfpage-down'} do
   assert(vis:map(vis.modes.NORMAL,key,'<'..action..'>'))
  end
 end,
 start=function() for _,command in ipairs(OPTIONS)do assert(vis:command(command))end end,
 win_status=function(w)
  local f=assert(io.open(AUDIT,'a'))
  local v=w.viewport
  f:write(string.format('%d:%d:%d:%d:%d:%d:%d\n',w.selection.pos,w.selection.line,
    w.selection.col,v.bytes.start,v.bytes.finish,v.width,v.height))
  f:close()
 end
}
'''.replace("AUDIT", json.dumps(str(audit))).replace("OPTIONS", "{" + ",".join(json.dumps(option) for option in options) + "}")
    config = configuration(directory, "config", source)
    session = LuaSession(binary, path, config, rows, columns)
    result = []
    try:
        session.ready()
        settle(session, time.monotonic())
        assert audit.exists() and audit.read_text(), bytes(session.output[-2000:])
        result.append(audit.read_text().splitlines()[-1])
        for keys in CASES:
            started = time.monotonic()
            session.send(keys)
            settle(session, started)
            result.append(audit.read_text().splitlines()[-1])
        session.send(b":qall!\r")
        session.finish()
        return result
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, default=ROOT / "vis")
    parser.add_argument("--port", type=Path, default=ROOT / "rust/target/release/vis-rs")
    parser.add_argument("--fixture", choices=["example", "numbers", "wrapped"])
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="vis-viewport-motion-") as folder:
        directory = Path(folder)
        wrapped = directory / "wrapped"
        wrapped.write_bytes((b"aa,bbbb,cc,dddd,12345,\tXYZ\n" * 40))
        fixtures = [("example", ROOT / "rust/example.txt", [], 80, 24),
                    ("numbers", ROOT / "rust/example.txt", ["set numbers"], 80, 24),
                    ("wrapped", wrapped, ["set wrapcolumn 7", "set breakat ,"], 12, 5)]
        total = 0
        eof_column_quirks = 0
        for index, (name, path, options, columns, rows) in enumerate(fixtures):
            if args.fixture and args.fixture != name:
                continue
            observed = []
            for name, binary in [("native", args.native), ("port", args.port)]:
                work = directory / f"{index}-{name}"
                work.mkdir()
                observed.append(capture(binary.resolve(), work, path, options, columns, rows))
            for keys, native, port in zip([b"open", *CASES], *observed):
                n = native.split(':'); r = port.split(':')
                if n[2] != r[2]:
                    # C's line_char_get does not count the final iterator step
                    # into EOF; the existing Rust character-column API does.
                    assert int(n[0]) == path.stat().st_size and int(r[2]) == int(n[2]) + 1, (path.name, options, keys, native, port)
                    eof_column_quirks += 1
                assert n[:2] + n[3:] == r[:2] + r[3:], (path.name, options, keys, native, port)
                total += 1
        print(f"PASS {total} C/Rust cursor/viewport states; {eof_column_quirks} existing EOF character-column quirks")


if __name__ == "__main__":
    main()
