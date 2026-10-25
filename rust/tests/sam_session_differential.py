#!/usr/bin/env python3
"""Compare guard-count domains, persistent registers and marks with actual C vis.

Compiles the Rust core directly, so independently unfinished UI modules cannot
hide Sam regressions. C and Rust run in isolated temporary directories. Commands
are compared for success, resulting bytes, and requested anchored selections.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def lua_string(value):
    return '"' + "".join(f"\\{byte:03d}" for byte in value.encode()) + '"'


def build_harness(directory):
    source = directory / "main.rs"
    modules = "".join(f"#[path={json.dumps(str(ROOT / 'rust/src' / (name + '.rs')))}] mod {name};\n"
                      for name in ["text", "motion", "regex", "sam"])
    source.write_text("#![forbid(unsafe_code)]\n#![allow(dead_code)]\n" + modules + r'''
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mut text = text::Text::from_bytes(std::fs::read(&args[1]).unwrap());
    let mut session = sam::Session::new();
    for line in std::fs::read_to_string(&args[3]).unwrap().lines() {
        let (name, positions) = line.split_once(' ').unwrap();
        let marks = positions.split(',').map(|p| text.mark(p.parse().unwrap()).unwrap()).collect();
        session.set_marks(&text, name.chars().next().unwrap(), marks).unwrap();
    }
    let reset = args[4] == "reset";
    let mut dots = vec![0..text.len()];
    let mut selected = Vec::new();
    let mut statuses = String::new();
    let scripts = std::fs::read(&args[2]).unwrap();
    for script in scripts.split(|&b| b == 0) {
        if script.is_empty() { continue; }
        let script = std::str::from_utf8(script).unwrap();
        let len = text.len();
        let result = if args[4] == "cursor" { session.execute_many(&mut text, script, &[0..0]) }
            else if reset { session.execute(&mut text, script, 0..len) }
            else { session.execute_in_mode(&mut text, script, &dots, !selected.is_empty()) };
        match result {
            Ok(result) => {
                statuses.push_str("ok\n");
                selected = result.selections;
                if !selected.is_empty() { dots = selected.clone(); }
            },
            Err(_) => { statuses.push_str("error\n"); selected.clear(); },
        }
    }
    statuses.push_str("ranges\n");
    for range in selected { statuses.push_str(&format!("{} {}\n",range.start,range.end)); }
    std::fs::write("output", text.to_vec()).unwrap();
    std::fs::write("state", statuses).unwrap();
}
''')
    binary = directory / "sam-harness"
    subprocess.run(["rustc", "--edition=2024", str(source), "-o", str(binary)], check=True, timeout=60)
    return binary


def reference(binary, data, scripts, marks, reset, directory):
    source = directory / "input"
    source.write_bytes(data)
    setup = "vis.events={}\nvis.events.win_open=function(win)\nlocal statuses={}\n"
    for name, positions in marks.items():
        ranges = ",".join(f"{{start={pos},finish={pos + 1}}}" for pos in positions)
        setup += f"win.marks[{lua_string(name)}]={{{ranges}}}\n"
    for script in scripts:
        if reset:
            setup += "vis:feedkeys('<Escape><Escape>')\n"
        setup += f"statuses[#statuses+1]=vis:command({lua_string(script)}) and 'ok\\n' or 'error\\n'\n"
    setup += (
        "local out=assert(io.open('output','wb'));out:write(win.file:content(0,win.file.size));out:close()\n"
        "local state=assert(io.open('state','w'));state:write(table.concat(statuses),'ranges\\n')\n"
        "for selection in win:selections_iterator() do if selection.anchored then local r=selection.range;"
        "state:write(string.format('%d %d\\n',r.start,r.finish)) end end\nstate:close()\nend\n"
    )
    (directory / "visrc.lua").write_text(setup)
    env = {**os.environ, "VIS_PATH": str(directory), "TERM": "xterm", "LC_ALL": "C.UTF-8"}
    result = subprocess.run([str(binary), "+qall!", str(source)], cwd=directory, env=env,
                            input=b"", capture_output=True, timeout=15)
    if result.returncode or not (directory / "state").exists():
        raise RuntimeError(f"C oracle failed: {result.returncode} {result.stderr[:200]!r}")


def state(directory, compare_ranges):
    statuses, _, ranges = (directory / "state").read_text().partition("ranges\n")
    return ((directory / "output").read_bytes(), statuses,
            ranges if compare_ranges else None)


def cases():
    for count in ["1", "2", "2,3", ",2", "3,", "%2", "%3", "-1", "-2", "1,-1", "-3,-1"]:
        for guard in ["g", "v"]:
            yield f"count-{guard}{count}", b"a\nb\nc\nd\n", [f",x {guard}{count} c/X\\n/"], {}, True, False
            yield f"select-{guard}{count}", b"a\nb\nc\nd\n", [f",x {guard}{count}"], {}, True, True
    yield "modulo-regex-xor", b"a\nb\nc\nd\n", [",x v2/b/ c/X\\n/"], {}, True, False
    yield "nested-negative-domain", b"abc\ndefgh\n", [",x x/./ g-1 c/X/"], {}, True, False
    yield "group-domain", b"a\nb\nc\nd\n", [",x {g%2 i/+/ v%2 i/-/}"], {}, True, False
    yield "across-visual-inputs", b"a\nb\nc\nd\n", [",x", "v-1"], {}, False, True
    yield "regex-register", b"one two one", [",x/one/", ",x// c/X/"], {}, True, False
    yield "regex-after-parse-failure", b"one two one", [",x/two/ {", ",x/[/ d", ",x// c/Y/"], {}, True, False
    yield "shell-after-parse-failure", b"original", [",< printf first\n{", ",<\n"], {}, True, False
    yield "stable-marks", b"abcdef", ["'a,'b {i/</ a/>/}"], {"a": [1], "b": [4]}, True, False
    yield "per-input-marks", b"abc def", [",x/[^ ]+/", "'ai/!/"], {"a": [1, 5]}, False, False
    yield "normal-cursor-x-default", b"a\nb\nc\nd\n", ["x g%2"], {}, "cursor", True
    yield "normal-cursor-a-default", b"a\nb\nc\nd\n", ["a/>/"], {}, "cursor", False
    yield "normal-cursor-r-default", b"a\nb\n", ["r input\n"], {}, "cursor", False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    args = parser.parse_args()
    passed = total = 0
    with tempfile.TemporaryDirectory(prefix="vis-sam-session-diff-") as temporary:
        directory = Path(temporary)
        harness = build_harness(directory)
        for name, data, scripts, marks, reset, compare_ranges in cases():
            total += 1
            c_dir, rust_dir = directory / f"c-{total}", directory / f"rust-{total}"
            c_dir.mkdir()
            rust_dir.mkdir()
            reference(args.c_vis.resolve(), data, scripts, marks, reset, c_dir)
            source, commands, mark_file = rust_dir / "input", rust_dir / "scripts", rust_dir / "marks"
            source.write_bytes(data)
            commands.write_bytes(b"\0".join(script.encode() for script in scripts))
            mark_file.write_text("".join(f"{name} {','.join(map(str, positions))}\n" for name, positions in marks.items()))
            context = "cursor" if reset == "cursor" else "reset" if reset else "keep"
            subprocess.run([str(harness), str(source), str(commands), str(mark_file), context],
                           cwd=rust_dir, check=True, timeout=15)
            expected, actual = state(c_dir, compare_ranges), state(rust_dir, compare_ranges)
            if expected == actual:
                passed += 1
                print(f"PASS {name}")
            else:
                print(f"FAIL {name}\n  C {expected!r}\n  Rust {actual!r}")
    print(f"{passed}/{total} session/count/mark cases match C vis")
    return int(passed != total)


if __name__ == "__main__":
    raise SystemExit(main())
