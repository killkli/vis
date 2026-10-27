#!/usr/bin/env python3
"""Measure opening and navigating the actual long-line example through a PTY.

Build both release editors first. Unlike the short-line latency probe, this
also checks process CPU activity: a slow layout may produce no terminal output
for longer than the usual quiet period. Returning during that interval would
incorrectly report the next key as taking zero time.

    python3 rust/tests/example_latency.py --rounds 3 --output /tmp/example.json
"""
import argparse
import json
from pathlib import Path
import statistics
import subprocess
import time

from terminal_smoke import ROOT, Session

CASES = [
    ("right20", b"l" * 20),
    ("down20", b"j" * 20),
    ("50G", b"50G"),
    ("G", b"G"),
    ("right_at_end20", b"l" * 20),
    ("gg", b"gg"),
]


EXTENDED_CASES = [
    ("G_again", b"G"),
    ("page_up", b"\x02"),
    ("page_down", b"\x06"),
    ("screen_up10", b"gk" * 10),
    ("screen_down10", b"gj" * 10),
    ("center", b"zz"),
    ("window_top", b"H"),
    ("window_bottom", b"L"),
    ("slide_up20", b"\x05" * 20),
    ("slide_down20", b"\x19" * 20),
]


def cpu_time(session):
    return subprocess.check_output(
        ["ps", "-p", str(session.process.pid), "-o", "time="], text=True
    ).strip()


def settle(session, started, quiet=0.3, timeout=120):
    last_output = started
    last_activity = started
    last_cpu = None
    next_poll = started
    while time.monotonic() - started < timeout:
        before = len(session.output)
        session.read(0.002)
        now = time.monotonic()
        if len(session.output) != before:
            last_output = last_activity = now
        if session.process.poll() is not None:
            raise RuntimeError(f"editor exited: {bytes(session.output[-1000:])!r}")
        if now >= next_poll:
            current_cpu = cpu_time(session)
            if current_cpu != last_cpu:
                last_activity = now
            last_cpu = current_cpu
            next_poll = now + 0.05
        if now - last_activity >= quiet:
            return (last_output - started) * 1000
    raise TimeoutError("editor did not finish processing keys")


def measure(binary, path, cases=CASES):
    started = time.monotonic()
    session = Session(binary, path)
    try:
        session.ready()
        result = {"open": settle(session, started)}
        for name, keys in cases:
            started = time.monotonic()
            session.send(keys)
            result[name] = settle(session, started)
        return result
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, nargs="+", default=[ROOT / "vis", ROOT / "rust/target/release/vis-rs"])
    parser.add_argument("--file", type=Path, default=ROOT / "rust/example.txt")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--extended", action="store_true", help="also time page, screen-row and window motions near EOF")
    args = parser.parse_args()
    results = {}
    for binary in args.binaries:
        binary = binary.resolve()
        runs = []
        for index in range(args.rounds):
            run = measure(binary, args.file.resolve(), CASES + (EXTENDED_CASES if args.extended else []))
            runs.append(run)
            print(f"{binary.name} round {index + 1}: " + " ".join(f"{name}={value:.2f}ms" for name, value in run.items()), flush=True)
        results[str(binary)] = runs
        print("median: " + " ".join(f"{name}={statistics.median(run[name] for run in runs):.2f}ms" for name in runs[0]), flush=True)
    if args.output:
        args.output.write_text(json.dumps({"file": str(args.file.resolve()), "results_ms": results}, indent=2) + "\n")


if __name__ == "__main__":
    main()
