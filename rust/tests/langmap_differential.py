#!/usr/bin/env python3
"""Compare language translation through physical PTY input, never feedkeys.

Setup uses genuine Lua commands. A mandatory ready marker verifies setup before
physical input; saved bytes and terminal restoration verify the actual action.
"""
import argparse
import json
from pathlib import Path
import shutil
import tempfile
import time

from lua_cli_smoke import LuaSession, configuration
from terminal_smoke import ROOT


CASES = [
    ("normal", b"abc\n", "", "дiX", b"aXbc\n"),
    ("insert-literal", b"abc\n", "", "iд", "дabc\n".encode()),
    ("replace-literal", b"abc\n", "", "Rд", "дbc\n".encode()),
    ("feedkeys-untranslated", b"abc\n", "vis:feedkeys('д')", "iX", b"Xabc\n"),
    ("feedkeys-keeps-disable-until-physical-key", "aдbд\n".encode(), "vis:feedkeys('fд')", "дiX", "aXдbд\n".encode()),
    ("duplicate-keeps-first", b"abc\n", "assert(vis:command('langmap д h'))", "дiX", b"aXbc\n"),
    ("truncated-long-source", b"abc\n", "assert(vis:command('langmap яю h'))", "юiX", b"Xabc\n"),
    ("truncated-long-target", b"abc\n", "assert(vis:command('langmap я lh'))", "яiX", b"aXbc\n"),
    ("find-argument-literal", "aдbд\n".encode(), "", "fдiX", "aXдbд\n".encode()),
    ("replace-argument-literal", b"abc\n", "", "rд", "дbc\n".encode()),
    ("before-mode-mapping", b"abc\n", "vis.win.selection.pos=1;assert(vis:map(vis.modes.NORMAL,'l','h'))", "дiX", b"Xabc\n"),
    ("alt-preserves-modifier", b"abc\n", "assert(vis:map(vis.modes.NORMAL,'<M-l>','l'))", ["\x1bд".encode(), b"iX"], b"aXbc\n"),
    ("operator-pending", b"abc\n", "", "dд", b"bc\n"),
    ("macro-records-translated-key", b"abc\n", "assert(vis:command('langmap l h'))", "qaдq@aiX", b"abXc\n"),
]


def run_case(binary, directory, data, setup, keys, expected):
    directory.mkdir()
    source = directory / "input.txt"
    source.write_bytes(data)
    marker = directory / "ready"
    script = f"""
package.path={json.dumps(str(ROOT / 'lua/?.lua') + ';')}..package.path
require('vis')
vis.events.subscribe(vis.events.START,function()
    local ok,err=pcall(function()
        assert(vis:command('langmap д l'))
        {setup}
    end)
    local out=assert(io.open({json.dumps(str(marker))},'w'))
    out:write(ok and 'READY' or tostring(err));out:close()
    if not ok then vis:exit(1) end
end)
"""
    config = configuration(directory, "config", script)
    session = LuaSession(binary, source, config)
    try:
        session.ready()
        deadline = time.monotonic() + 10
        while not marker.exists():
            session.read()
            assert session.process.poll() is None, bytes(session.output[-3000:])
            assert time.monotonic() < deadline, "missing language-map ready marker"
        assert marker.read_text() == "READY", marker.read_text()
        if isinstance(keys, str):
            session.send_keys(keys.encode())
        else:
            for chunk in keys:
                session.send(chunk)
                session.read(0.08)
        session.send_keys(b"\x1b:wq\r")
        session.finish()
        actual = source.read_bytes()
        assert actual == expected, (expected, actual)
    finally:
        (directory / "terminal.bin").write_bytes(session.output)
        session.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--rust-vis", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    parser.add_argument("--skip-rust", action="store_true")
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("cases", nargs="*")
    arguments = parser.parse_args()
    passed = failed = 0
    with tempfile.TemporaryDirectory(prefix="vis-langmap-") as name:
        temporary = Path(name)
        binaries = [("C", arguments.c_vis)]
        if not arguments.skip_rust:
            binaries.append(("Rust", arguments.rust_vis))
        snapshots = []
        for label, binary in binaries:
            snapshot = temporary / (label + "-vis")
            shutil.copy2(binary.resolve(), snapshot)
            snapshots.append((label, snapshot))
        for name, data, setup, keys, expected in CASES:
            if arguments.cases and name not in arguments.cases:
                continue
            case = temporary / name
            case.mkdir()
            try:
                for label, binary in snapshots:
                    run_case(binary, case / label, data, setup, keys, expected)
                passed += 1
                print("PASS", name, "+".join(label for label, _ in snapshots))
            except (AssertionError, OSError) as error:
                failed += 1
                print("FAIL", name, repr(error))
            finally:
                if arguments.artifacts:
                    shutil.copytree(case, arguments.artifacts / name, dirs_exist_ok=True)
    print(f"{passed} pass, {failed} fail")
    return bool(failed or not passed)


if __name__ == "__main__":
    raise SystemExit(main())
