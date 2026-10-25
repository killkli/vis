#!/usr/bin/env python3
"""Regenerate checked-in RFC 1345 data from vis-digraph.c; no build-time use.

Usage: python3 rust/tools/generate_digraph.py [--check | --test]
The source ISC copyright and permission notice are retained in digraph.rs.
Only the marked table is replaced, preserving the hand-written lookup API.
"""

import argparse
import ast
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time


def differential(rust_root, rows, binary):
    """Compare complete listing, lookups, error statuses, pipes and raw TTY."""
    if binary is None:
        subprocess.run(["cargo", "build", "--manifest-path", str(rust_root / "Cargo.toml"),
                        "--bin", "vis-digraph-rs"], check=True, timeout=60)
        metadata = json.loads(subprocess.check_output([
            "cargo", "metadata", "--manifest-path", str(rust_root / "Cargo.toml"),
            "--format-version", "1", "--no-deps"], timeout=30))
        binary = Path(metadata["target_directory"]) / "debug" / "vis-digraph-rs"
    binary = str(Path(binary).resolve())
    locales = subprocess.check_output(["locale", "-a"], timeout=10).decode().splitlines()
    utf8 = next((name for name in locales if name.lower() in ("c.utf8", "c.utf-8", "en_us.utf-8")), None)
    if utf8 is None:
        raise SystemExit("UTF-8 locale required to compare the C Unicode output")
    environment = dict(os.environ, LC_ALL=utf8)
    names = [ast.literal_eval(name) for name, _, _ in rows]
    cases = [([], b""), (names, b""), (["SP", "a:", "NU"], b""),
             (["SP", "a"], b""), (["SP", "not-a-digraph"], b""),
             (["not-a-digraph", "a"], b""), (["a", "not-a-digraph"], b""),
             ([""], b""), (["♫"], b""), (["N->", "S.-."], b"")]
    prefixes = sorted({name[:end] for name in names for end in range(1, len(name))})
    cases += [([prefix], b"") for prefix in prefixes if prefix != "-"]
    cases += [(["-"], data) for data in (
        b"", b"a", b"a:", b"a:remaining", b"NU", b"not-a-digraph", b"\xff", b"\0", b"A\0 ", b"N->")]
    cases += [(["-", "ignored"], b"a:")]
    if os.name == "posix":
        cases += [([b"\xff"], b"")]
    with tempfile.TemporaryDirectory(prefix="vis-digraph-") as directory:
        oracle = str(Path(directory) / "oracle")
        subprocess.run(shlex.split(os.environ.get("CC", "cc")) +
                       [str(rust_root.parent / "vis-digraph.c"), "-o", oracle], check=True, timeout=30)
        failures = []
        for arguments, data in cases:
            def run(program):
                result = subprocess.run([program, *arguments], input=data, capture_output=True,
                                        env=environment, timeout=10)
                return result.returncode, result.stdout, result.stderr
            expected, actual = run(oracle), run(binary)
            if expected != actual:
                failures.append((arguments, data, expected, actual))
        for failure in failures[:10]:
            print(repr(failure))
        if failures:
            raise SystemExit(f"{len(failures)}/{len(cases)} digraph cases differ")
        if os.name == "posix":
            for program in (oracle, binary):
                directory_fd = os.open(directory, os.O_RDONLY)
                try:
                    result = subprocess.run([program, "-"], stdin=directory_fd,
                                            capture_output=True, env=environment, timeout=10)
                    assert (result.returncode, result.stdout, result.stderr) == (3, b"", b"I/O error")
                finally:
                    os.close(directory_fd)
                # Neither program may consume bytes following the mnemonic.
                read_fd, write_fd = os.pipe()
                try:
                    os.write(write_fd, b"a:remaining")
                    os.close(write_fd)
                    write_fd = None
                    result = subprocess.run([program, "-"], stdin=read_fd, capture_output=True,
                                            env=environment, timeout=10)
                    assert (result.returncode, result.stdout, os.read(read_fd, 100)) == (0, "ä".encode(), b"remaining")
                finally:
                    os.close(read_fd)
                    if write_fd is not None:
                        os.close(write_fd)
                raw_tty_case(program, environment)
                raw_tty_case(program, environment, b"\x03", (1, b"", b"Unknown digraph: `\x03'\n"))
    print(f"Digraph differential: {len(cases)}/{len(cases)} cases match C; complete {len(rows)}-entry listing, pipe consumption and raw TTY verified")


def raw_tty_case(program, environment, data=b"a:", expected=(0, "ä".encode(), b"")):
    import pty
    import termios
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    child = subprocess.Popen([program, "-"], stdin=slave, stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, env=environment)
    try:
        deadline = time.monotonic() + 5
        while termios.tcgetattr(slave)[3] & termios.ICANON:
            if child.poll() is not None or time.monotonic() >= deadline:
                raise AssertionError(f"{program} did not enter raw input mode")
            time.sleep(0.01)
        os.write(master, data)
        output, errors = child.communicate(timeout=10)
        assert (child.returncode, output, errors) == expected
        restored = termios.tcgetattr(slave)
        # macOS sets the transient kernel PENDIN bit when returning from raw
        # to canonical mode, including for the original C program.
        restored[3] &= ~getattr(termios, "PENDIN", 0)
        original[3] &= ~getattr(termios, "PENDIN", 0)
        assert restored == original, f"{program}: terminal settings were not restored"
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        os.close(master)
        os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--test", action="store_true", help="compare Rust CLI to a temporary C reference")
    parser.add_argument("--binary", help="existing Rust binary for --test (default: build debug binary)")
    arguments = parser.parse_args()
    rust_root = Path(__file__).resolve().parents[1]
    source = (rust_root.parent / "vis-digraph.c").read_text()
    table = source.split("static const Digraph digraphs[] = {", 1)[1].split("\n};", 1)[0]
    c_string = r'"(?:\\.|[^"\\])*"'
    pattern = re.compile(r'\{\s*(' + c_string + r')\s*,\s*(0x[\da-fA-F]+)\s*,\s*(' + c_string + r')\s*\},')
    rows = pattern.findall(table)
    # Reject silently skipped rows if the source table syntax changes.
    remainder = pattern.sub("", table)
    remainder = re.sub(r'/\*.*?\*/', '', remainder, flags=re.S)
    remainder = re.sub(r'\{\s*""\s*,\s*0\s*,\s*NULL\s*\},', '', remainder)
    if remainder.strip() or not rows:
        raise SystemExit("unrecognized C digraph table syntax")
    generated = ["// BEGIN GENERATED DIGRAPH TABLE", "#[rustfmt::skip]", "pub const DIGRAPHS: &[Digraph] = &["]
    for name, codepoint, description in rows:
        name, description = ast.literal_eval(name), ast.literal_eval(description)
        codepoint = int(codepoint, 16)
        if not 0 <= codepoint <= 0x10FFFF or 0xD800 <= codepoint <= 0xDFFF:
            raise SystemExit(f"non-scalar codepoint {codepoint:x}")
        if not name.isascii() or not 1 <= len(name) <= 7:
            raise SystemExit(f"invalid mnemonic {name!r}")
        generated.append(f"    entry({json.dumps(name)}, '\\u{{{codepoint:x}}}', {json.dumps(description)}),")
    generated += ["];", "// END GENERATED DIGRAPH TABLE"]
    target = rust_root / "src" / "digraph.rs"
    current = target.read_text()
    updated, count = re.subn(r'// BEGIN GENERATED DIGRAPH TABLE.*?// END GENERATED DIGRAPH TABLE',
                            lambda _: "\n".join(generated), current, flags=re.S)
    if count != 1:
        raise SystemExit("expected exactly one generated table marker")
    if arguments.check or arguments.test:
        if current != updated:
            raise SystemExit("generated digraph table is stale")
        print(f"Digraph table up to date: {len(rows)} entries")
    else:
        target.write_text(updated)
        print(f"Generated {len(rows)} digraph entries")
    if arguments.test:
        differential(rust_root, rows, arguments.binary)


if __name__ == "__main__":
    main()
