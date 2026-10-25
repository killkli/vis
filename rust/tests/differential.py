#!/usr/bin/env python3
"""Compare Rust structural editing against the original C vis, without fixture writes.

Build both binaries first. All fixtures are tested by default; unported commands
are failures, not skips. Optional fixture paths allow focused migration checks.
Each program runs in its own temporary working directory, including shell effects.
"""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]


def lua_string(value):
    return '"' + "".join(f"\\{byte:03d}" for byte in value.encode()) + '"'


def reference(binary, data, script, directory):
    source = directory / "input"
    source.write_bytes(data)
    config = directory / "visrc.lua"
    config.write_text(
        "vis.events = {}\n"
        "vis.events.win_open = function(win)\n"
        f"local ok = vis:command({lua_string(script)})\n"
        "local status = assert(io.open('status', 'w'))\n"
        "status:write(tostring(ok)); status:close()\n"
        "local out = assert(io.open('output', 'wb'))\n"
        "out:write(win.file:content(0, win.file.size)); out:close()\n"
        "end\n"
    )
    env = {**os.environ, "VIS_PATH": str(directory), "TERM": "xterm", "LC_ALL": "C"}
    process = subprocess.run(
        [str(binary), "+qall!", str(source)],
        cwd=directory, env=env, input=b"", capture_output=True, timeout=15,
    )
    if process.returncode or not (directory / "status").exists():
        raise RuntimeError(f"C oracle failed: {process.stderr.decode(errors='replace')}")
    return (directory / "status").read_text() == "true", (directory / "output").read_bytes()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--rust-vis", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    parser.add_argument("fixtures", nargs="*", type=Path)
    args = parser.parse_args()
    fixtures = args.fixtures or sorted((ROOT / "test/sam").rglob("*.cmd"))
    passed = 0
    for fixture in fixtures:
        fixture = fixture.resolve()
        data = fixture.with_suffix(".in").read_bytes()
        # Match the implicit whole-file group in upstream test/sam/visrc.lua.
        script = ",{\n" + fixture.read_text() + "\n}"
        with tempfile.TemporaryDirectory(prefix="vis-diff-") as temporary:
            directory = Path(temporary)
            c_dir, rust_dir = directory / "c", directory / "rust"
            c_dir.mkdir()
            rust_dir.mkdir()
            success, expected = reference(args.c_vis.resolve(), data, script, c_dir)
            result = subprocess.run(
                [str(args.rust_vis.resolve()), "--command", script],
                input=data, capture_output=True, cwd=rust_dir, timeout=15,
            )
            same = (result.returncode == 0) == success and (not success or result.stdout == expected)
            name = fixture.relative_to(ROOT) if fixture.is_relative_to(ROOT) else fixture
            if same:
                passed += 1
                print(f"PASS {name}")
            else:
                print(f"FAIL {name}: C success={success}, Rust status={result.returncode}")
                if result.stderr:
                    print(result.stderr.decode(errors="replace").strip())
                elif success:
                    print(f"  expected {expected[:160]!r}\n  received {result.stdout[:160]!r}")
    print(f"{passed}/{len(fixtures)} fixtures match C vis")
    return 0 if passed == len(fixtures) else 1


if __name__ == "__main__":
    raise SystemExit(main())
