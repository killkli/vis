#!/usr/bin/env python3
"""Compare the port's screen with the rows the native editor holds.

The original editor writes only the cells it marks dirty, so after a scroll
its screen keeps the rows a scroll replaced while its view has moved. The
rows it holds are the behaviour the port reproduces, so the port's screen is
compared with the rows of the native editor's last draw.

The native editor's view is traced by `tools/trace_native_view.sh`, which
adds a trace to the C sources:

    sh rust/tools/trace_native_view.sh
    python3 rust/tests/view_held_rows.py rust/example.txt 50G
    git checkout view.c
"""
import argparse
import os
import re
import select
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "rust" / "tests"))
from terminal_smoke import Session  # noqa: E402
import content_compare  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
TRACE = Path("/tmp/vis_native_view_trace.log")


def run(binary: str, path: str, keys: str, rows: int, columns: int):
    """The screen the editor leaves, and the rows the native editor holds."""
    session = Session(binary, path)
    try:
        session.ready()
        time.sleep(0.5)
        for key in keys:
            session.send(key.encode())
            time.sleep(0.1)
        # The native editor scrolls one row at a time, so it needs time to
        # settle before its view is the one the port shows.
        time.sleep(4.0)
        for _ in range(40):
            session.read(0.05)
        return content_compare.screen_lines(bytes(session.output), rows, columns)
    finally:
        session.close()


def held_rows(trace: Path) -> dict[int, str]:
    """The rows of the native editor's last draw of a whole screen."""
    if not trace.exists():
        return {}
    rows: dict[int, str] = {}
    draws: list[tuple[int, dict[int, str]]] = []
    count = 0
    for line in trace.read_text(encoding="utf-8", errors="replace").split("\n"):
        if line.startswith("DRAW "):
            header = re.match(r"DRAW start=(\d+) end=(\d+) w=(\d+) h=(\d+)", line)
            if not header:
                continue
            count = int(header.group(4))
            rows = {}
            continue
        if line.startswith("ROW "):
            index, _, text = line[4:].partition(" ")
            rows[int(index)] = text
            if len(rows) == count and count > 1:
                # The settled view is the last draw of the biggest screen. The
                # native editor draws a screen of a different size while it
                # starts up, and it scrolls one row at a time, so an earlier
                # draw of the biggest screen is not the view it holds.
                draws.append((count, dict(rows)))
    if not draws:
        return {}
    biggest = max(count for count, _ in draws)
    for count, rows in reversed(draws):
        if count == biggest:
            return rows
    return {}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("path")
    parser.add_argument("--keys", default="")
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--columns", type=int, default=80)
    parser.add_argument("--native", default=str(ROOT / "vis"))
    parser.add_argument("--port", default=str(ROOT / "rust/target/release/vis-rs"))
    parser.add_argument("--trace", default=str(TRACE))
    args = parser.parse_args()

    trace = Path(args.trace)
    if trace.exists():
        trace.unlink()
    # The trace is written by the native editor, so the environment has to
    # reach it.
    os.environ["VIS_DEBUG_TRACE"] = str(trace)
    # The native editor runs the runtime of the repository, as the port does.
    os.environ["LUA_PATH"] = (
        f"{ROOT}/lua/?.lua;{ROOT}/lua/?/init.lua;;"
    )
    session = Session(args.native, args.path)
    try:
        session.ready()
        time.sleep(0.5)
        for key in args.keys:
            session.send(key.encode())
            time.sleep(0.1)
        time.sleep(4.0)
        for _ in range(40):
            session.read(0.05)
    finally:
        session.close()

    held = held_rows(trace)
    port = run(args.port, args.path, args.keys, args.rows, args.columns)
    if not held:
        print("the native editor wrote no trace; run tools/trace_native_view.sh")
        return 1
    same = 0
    for index in sorted(held):
        if index >= len(port):
            break
        native = held[index]
        ours = port[index]
        if native.rstrip() == ours.rstrip():
            same += 1
        else:
            print(f"row {index}:")
            print(f"  the native {len(native)} {native!r}")
            print(f"  the port   {len(ours)} {ours!r}")
    print(f"the port's screen matches the native's rows in {same}/{min(len(held), len(port))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
