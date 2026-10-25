#!/usr/bin/env python3
"""Compare menu ranking and real PTY interactions against original vis-menu.c.

No third-party Python packages. Temporary executables and PTYs are cleaned up.
The original menu has no batch filter option: ranking is tested by compiling its
actual match() function; CLI acceptance/cancellation is tested under a controlling
terminal with stdin/stdout pipes. Build vis-menu-rs first.
"""

import argparse
import base64
import fcntl
import itertools
import json
import os
from pathlib import Path
import random
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]

C_DRIVER = r"""
#define main original_main
#include MENU_PATH
#undef main
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    readstdin(); mw = 80; mh = 24; inputw = inputw < mw/3 ? inputw : mw/3;
    strncpy(text, argv[1], sizeof text - 1);
    if (argv[2][0] == '1') fstrncmp = strncasecmp;
    match();
    for (Item *item = matches; item; item = item->right)
        printf("%td ", item - items);
    puts("");
}
"""

RUST_DRIVER = r"""
#[allow(dead_code)]
#[path = MENU_PATH]
mod menu;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let items = menu::read_candidates(std::io::stdin().lock()).unwrap();
    for index in menu::rank_matches(&items, args[1].as_bytes(), args[2] == "1") {
        print!("{index} ");
    }
    println!();
}
"""

# The wrapper remains the controlling session leader until the parent has
# inspected restored termios. Otherwise macOS revokes the PTY when a session
# leader exits, making cleanup verification impossible on retained slave fds.
WRAPPER = r"""
import base64, json, os, subprocess, sys
report, acknowledge = int(sys.argv[1]), int(sys.argv[2])
program = json.loads(sys.argv[3])
child = subprocess.Popen(program, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
os.write(report, (json.dumps({"pid": child.pid}) + "\n").encode())
output, _ = child.communicate(base64.b64decode(sys.argv[4]))
os.write(report, (json.dumps({"status": child.returncode, "output": output.hex()}) + "\n").encode())
os.read(acknowledge, 1)
"""


def checked(command, **kwargs):
    return subprocess.run(command, check=True, timeout=30, capture_output=True, **kwargs)


def matching(directory):
    c_source, rust_source = directory / "rank.c", directory / "rank.rs"
    c_source.write_text(C_DRIVER.replace("MENU_PATH", json.dumps(str(ROOT / "vis-menu.c"))))
    rust_source.write_text(RUST_DRIVER.replace("MENU_PATH", json.dumps(str(ROOT / "rust/src/bin/vis-menu.rs"))))
    c_driver, rust_driver = directory / "rank-c", directory / "rank-rust"
    checked(shlex.split(os.environ.get("CC", "cc")) + ["-std=c99", '-DVERSION="oracle"', str(c_source), "-o", str(c_driver)])
    checked(["rustc", "--edition=2024", str(rust_source), "-o", str(rust_driver)],
            env={**os.environ, "CARGO_PKG_VERSION": "oracle"})
    randomizer = random.Random(0x564953)
    corpora = [
        b"xfoo bar\nfoo bar\nfoo\nfoobar\nFOO\nbar foo\n\n",
        "界面\n文字界\n面界\n界\n".encode(),
        b"\n".join("".join(randomizer.choice("abAB _-") for _ in range(randomizer.randrange(18))).encode() for _ in range(80)),
    ]
    queries = ["", " ", "  ", "foo", "FOO", "foo bar", " bar foo ", "a", "A", "ab", "a b", "b a", "_", "a-b", "\t", "界", "面 界"]
    queries += ["".join(chars) for size in range(1, 4) for chars in itertools.product("ab ", repeat=size)]
    comparisons = 0
    for corpus, query, insensitive in itertools.product(corpora, queries, [False, True]):
        arguments = [query, str(int(insensitive))]
        expected = subprocess.check_output([str(c_driver), *arguments], input=corpus, timeout=5)
        actual = subprocess.check_output([str(rust_driver), *arguments], input=corpus, timeout=5)
        assert actual == expected, (query, insensitive, expected, actual)
        comparisons += 1
    print(f"PASS {comparisons} C/Rust exact/prefix/substring ranking cases")


def normalized(settings):
    settings = settings.copy()
    settings[3] &= ~getattr(termios, "PENDIN", 0)
    return settings


def interaction(binary, arguments, candidates, keys, terminate=False):
    master, slave = os.openpty()
    report_read, report_write = os.pipe()
    acknowledge_read, acknowledge_write = os.pipe()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 60, 0, 0))
    original = termios.tcgetattr(slave)

    def controlling_session():
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    wrapper = subprocess.Popen(
        [sys.executable, "-c", WRAPPER, str(report_write), str(acknowledge_read),
         json.dumps([str(binary), *arguments]), base64.b64encode(candidates).decode()],
        stdin=slave, stdout=slave, stderr=slave, pass_fds=(report_write, acknowledge_read),
        preexec_fn=controlling_session, env={**os.environ, "TERM": "xterm", "LC_ALL": "C"},
    )
    os.close(report_write)
    os.close(acknowledge_read)
    screen, reports = bytearray(), bytearray()
    child = None
    deadline = time.monotonic() + 10
    try:
        while child is None or termios.tcgetattr(slave)[3] & termios.ICANON:
            ready, _, _ = select.select([master, report_read], [], [], 0.05)
            for descriptor in ready:
                if descriptor == master:
                    screen.extend(os.read(master, 65536))
                else:
                    reports.extend(os.read(report_read, 65536))
                    if b"\n" in reports:
                        line, _, rest = reports.partition(b"\n")
                        first_report = json.loads(line)
                        assert "pid" in first_report, (str(binary), arguments, first_report, bytes(screen[-2000:]))
                        child = first_report["pid"]
                        reports[:] = rest
            assert time.monotonic() < deadline, f"menu did not enter raw mode: {bytes(screen)!r}"
        if terminate:
            os.kill(child, signal.SIGTERM)
        else:
            os.write(master, keys)
        while b"\n" not in reports:
            ready, _, _ = select.select([master, report_read], [], [], 0.05)
            for descriptor in ready:
                if descriptor == master:
                    screen.extend(os.read(master, 65536))
                else:
                    reports.extend(os.read(report_read, 65536))
            assert time.monotonic() < deadline, f"menu did not exit: {bytes(screen[-1000:])!r}"
        result = json.loads(reports.partition(b"\n")[0])
        restored = normalized(termios.tcgetattr(slave))
        assert restored == normalized(original), f"menu failed to restore termios: {restored!r} != {original!r}"
        return result["status"], bytes.fromhex(result["output"]), bytes(screen)
    finally:
        os.write(acknowledge_write, b"x")
        try:
            wrapper.wait(timeout=3)
        except subprocess.TimeoutExpired:
            wrapper.kill()
            wrapper.wait(timeout=3)
        for descriptor in [master, slave, report_read, acknowledge_write]:
            os.close(descriptor)


def pty_cases(c_menu, rust_menu):
    candidates = b"prefix foo\nfoo\nfoobar\nFOO\nbar foo\n"
    cases = [
        (["foo"], candidates, b"\r"),
        (["-i", "FOO"], candidates, b"\r"),
        ([], candidates, b"foo\r"),
        ([], candidates, b"\x0e\x0e\x10\r"),
        (["unlisted"], candidates, b"\r"),
        (["foo"], candidates, b"\x0e\t\x1d"),
        (["foo"], candidates, b"\x1c"),
        ([], candidates, b"\x05\r"),
        ([], candidates, b"\x03"),
        ([], candidates, b"\x1b\x1b"),
        (["alpha beta"], candidates, b"\x1bb\x0b\x1d"),
        (["-l", "2", "-p", "Pick"], candidates, b"\x1b[6~\r"),
        (["-t", "-l", "2"], candidates, b"\x1b[B\r"),
        (["-b"], candidates, b"\x1b[C\r"),
        ([], b"\xff\xfe\nvalid\n", b"\r"),
        ([], b"", b"\r"),
    ]
    for arguments, items, keys in cases:
        expected = interaction(c_menu, arguments, items, keys)
        actual = interaction(rust_menu, arguments, items, keys)
        assert actual[:2] == expected[:2], (arguments, keys, expected[:2], actual[:2])
    # Safer Rust behavior: a caught termination signal also restores the tty.
    status, output, _ = interaction(rust_menu, [], candidates, b"", terminate=True)
    assert status > 1 and not output
    # Bracketed paste is literal text, never acceptance/cancellation commands.
    status, output, _ = interaction(rust_menu, [], candidates, b"\x1b[200~not\r\nlisted\x1b[201~\x1d")
    assert (status, output) == (0, b"notlisted\n"), (status, output)
    print(f"PASS {len(cases)} C/Rust PTY cases plus Rust signal/paste restoration")


def cli_cases(c_menu, rust_menu):
    for arguments in [["-p"], ["-l"], ["-unknown"], ["-l", "18446744073709551616"]]:
        results = [subprocess.run([str(binary), *arguments], input=b"", capture_output=True, timeout=5)
                   for binary in [c_menu, rust_menu]]
        assert [result.returncode for result in results] == [2, 2], arguments
        assert all(not result.stdout and result.stderr for result in results), arguments
    for binary in [c_menu, rust_menu]:
        result = subprocess.run([str(binary), "-v"], input=b"", capture_output=True, timeout=5)
        assert result.returncode == 0 and result.stdout.startswith(b"vis-menu ") and not result.stderr
    print("PASS C/Rust CLI version and invalid-argument exit statuses")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-menu", type=Path, default=ROOT / "rust/target/debug/vis-menu-rs")
    parser.add_argument("--c-menu", type=Path)
    options = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="vis-menu-diff-") as temporary:
        directory = Path(temporary)
        # Isolate this run from concurrent Cargo builds replacing a debug
        # executable with a different feature configuration in the workspace.
        rust_menu = directory / "vis-menu-rs"
        shutil.copy2(options.rust_menu.resolve(), rust_menu)
        matching(directory)
        c_menu = options.c_menu or directory / "vis-menu"
        if options.c_menu is None:
            checked(shlex.split(os.environ.get("CC", "cc")) + ["-std=c99", '-DVERSION="oracle"', str(ROOT / "vis-menu.c"), "-o", str(c_menu)])
        cli_cases(c_menu.resolve(), rust_menu)
        pty_cases(c_menu.resolve(), rust_menu)


if __name__ == "__main__":
    main()
