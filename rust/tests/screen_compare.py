#!/usr/bin/env python3
"""Compare the screen the C editor and the Rust port leave on a file.

The two editors write the screen differently: the original writes the whole
screen and the panels over it, the port writes the rows that changed. The
screen they leave behind must be the same, so both are rendered onto a
virtual screen and compared cell by cell.
"""
import argparse
import os
import re
import sys
import time
import unicodedata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "rust" / "tests"))
from terminal_smoke import Session  # noqa: E402

# The sequence that places the cursor.
POSITION = re.compile(rb"\x1b\[(\d*);?(\d*)H")
# The sequence that places the cursor in a row, leaving the column. The
# original editor writes the status bar with it.
ROW = re.compile(rb"\x1b\[(\d*)d")
# The sequence that sets the scrolling region. The original editor scrolls
# the screen with it.
REGION = re.compile(rb"\x1b\[(\d*);(\d*)r")
# The sequence that saves and restores the cursor.
SAVE = re.compile(rb"\x1b[78]")
# The sequence that erases the display.
ERASE = re.compile(rb"\x1b\[(\d*)J")
# The sequence that changes the style, which does not alter the cells.
STYLE = re.compile(rb"\x1b\[[0-9;]*m")


def render(output: bytes, rows: int, columns: int) -> list[str]:
    """The cells the output leaves on the screen."""
    screen = [[b" "] * columns for _ in range(rows)]
    row = column = 1
    index = 0
    length = len(output)
    # The scrolling region. The cursor moves inside it, and a write below it
    # scrolls the region up.
    region = (1, rows)
    while index < length:
        byte = output[index]
        if byte == 0x1b:
            match = POSITION.match(output, index)
            if match:
                row = int(match.group(1) or 1)
                column = int(match.group(2) or 1)
                index = match.end()
                continue
            match = ROW.match(output, index)
            if match:
                row = int(match.group(1) or 1)
                if row > region[1]:
                    # The row is below the screen, so the screen scrolls up.
                    while row > region[1]:
                        del screen[region[0] - 1]
                        screen.insert(region[1] - 1, [b" "] * columns)
                        row -= 1
                index = match.end()
                continue
            match = STYLE.match(output, index)
            if match:
                index = match.end()
                continue
            end = index + 2
            while end < length and not chr(output[end]).isalpha():
                end += 1
            index = min(end + 1, length)
            continue
        if byte == 0x08:
            # A backspace moves the cursor left, which the original editor
            # uses to write the next cell of a line.
            column = max(1, column - 1)
            index += 1
            continue
        if byte == 0x0a:
            if row == region[1]:
                # The region scrolls up, so its first row is dropped.
                del screen[region[0] - 1]
                screen.insert(region[1] - 1, [b" "] * columns)
            else:
                row += 1
            column = 1
            index += 1
            continue
        if byte == 0x0d:
            column = 1
            index += 1
            continue
        if byte == 0x0e:
            # A line feed inside the scrolling region scrolls it up.
            if row == region[1]:
                del screen[region[0] - 1]
                screen.insert(region[1] - 1, [b" "] * columns)
            else:
                row += 1
            index += 1
            continue
        # The editors write whole characters, and a character that is wider
        # than one column covers the columns it spans.
        size = 1
        if byte >= 0xF0:
            size = 4
        elif byte >= 0xE0:
            size = 3
        elif byte >= 0xC0:
            size = 2
        width = 1
        if size > 1:
            character = output[index:index + size]
            if len(character) < size:
                break
            text = character.decode("utf-8", "replace")
            width = 2 if unicodedata.east_asian_width(text) in ("W", "F") else 1
        if 1 <= row <= rows and 1 <= column <= columns:
            screen[row - 1][column - 1] = output[index:index + size]
            for offset in range(1, width):
                if column + offset <= columns:
                    screen[row - 1][column + offset - 1] = b""
        column += width
        index += size
    return [
        b"".join(line).decode("utf-8", "replace").rstrip() for line in screen
    ]


def capture(binary: str, path: str, keys: bytes, rows: int, columns: int) -> list[str]:
    session = Session(binary, path)
    try:
        session.ready()
        time.sleep(1.0)
        if keys:
            # The keys are sent one at a time, so both editors receive all of
            # them. A burst would be coalesced differently by each of them.
            for key in keys:
                session.send(bytes([key]))
                time.sleep(0.05)
            time.sleep(1.2)
        for _ in range(20):
            session.read(0.1)
        return render(bytes(session.output), rows, columns)
    finally:
        session.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path")
    parser.add_argument("c_binary")
    parser.add_argument("rust_binary")
    parser.add_argument("--keys", default="")
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--columns", type=int, default=80)
    options = parser.parse_args()

    keys = options.keys.encode()
    left = capture(options.c_binary, options.path, keys, options.rows, options.columns)
    right = capture(options.rust_binary, options.path, keys, options.rows, options.columns)

    equal = 0
    for index, (one, other) in enumerate(zip(left, right)):
        if one == other:
            equal += 1
            continue
        print(f"row {index + 1}:")
        print(f"  C   : {one!r}")
        print(f"  Rust: {other!r}")
    print(f"identical rows: {equal}/{len(left)}")
    return 0 if equal == len(left) else 1


if __name__ == "__main__":
    raise SystemExit(main())
