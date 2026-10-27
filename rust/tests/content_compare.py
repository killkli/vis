#!/usr/bin/env python3
"""Compare the behaviour of the C editor and the Rust port.

The two editors write their frames differently, so the bytes on the screen
differ: the original editor draws into a scrolling region and writes past
the last row of the screen, so the screen it leaves behind keeps the content
that was there before the scroll. The port redraws the rows it changes.

This compares what the editors do instead of the bytes they write: the line
the cursor is on, which the status bar reports, and the lines of the document
they show.
"""
import argparse
import re
import sys
import time
import unicodedata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "rust" / "tests"))
from terminal_smoke import Session  # noqa: E402

# The frame of the original editor starts with the sequence that hides the
# cursor. The port writes the same sequence at the start of a frame.
FRAME = b"\x1b[?25l"
# The sequence that changes the style, which does not add text.
STYLE = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")
# The sequence that places the cursor in a row, leaving the column.
ROW = re.compile(rb"\x1b\[(\d+)d")
# The sequence that places the cursor on a row and a column.
POSITION = re.compile(rb"\x1b\[(\d+);(\d+)H")
# The sequence that places the cursor in the first row and the first column.
HOME = re.compile(rb"\x1b\[H")
# The sequence that sets the scrolling region.
REGION = re.compile(rb"\x1b\[(\d*);(\d*)r")


def screen_lines(output: bytes, rows: int, columns: int) -> list[str]:
    """The lines the output leaves on the screen."""
    screen = [[b" "] * columns for _ in range(rows)]
    row = column = 1
    index = 0
    length = len(output)
    region = (1, rows)
    while index < length:
        byte = output[index]
        if byte == 0x1b:
            match = REGION.match(output, index)
            if match:
                region = (max(1, int(match.group(1) or 1)), min(rows, int(match.group(2) or rows)))
                index = match.end()
                continue
            match = POSITION.match(output, index)
            if match:
                row = int(match.group(1))
                column = int(match.group(2))
                index = match.end()
                continue
            match = HOME.match(output, index)
            if match:
                row = 1
                column = 1
                index = match.end()
                continue
            match = ROW.match(output, index)
            if match:
                row = int(match.group(1))
                if row > region[1]:
                    # The row is below the region, so the region scrolls up.
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
            column = max(1, column - 1)
            index += 1
            continue
        if byte == 0x0a:
            if row == region[1]:
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
        # The editors write whole characters, and a character that is wider
        # than one column covers the columns it spans.
        width = 1
        size = 1
        if byte >= 0xF0:
            size = 4
        elif byte >= 0xE0:
            size = 3
        elif byte >= 0xC0:
            size = 2
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
    return [b"".join(line).decode("utf-8", "replace") for line in screen]


def document_lines(output: bytes) -> list[str]:
    """The lines of the document the editor wrote.

    The original editor draws the whole document into its scrolling region,
    so the lines of the document are the ones it wrote, without the status
    bar.
    """
    plain = STYLE.sub(b"", output)
    plain = plain.replace(FRAME, b"")
    plain = re.sub(rb"\x1b\([A-Za-z]", b"", plain)
    plain = re.sub(rb"\x1b=", b"", plain)
    parts = plain.decode("utf-8", "replace").split("\r")
    while parts:
        last = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", parts[-1])
        if last.strip():
            break
        parts = parts[:-1]
    result = []
    for part in parts:
        match = re.match(r"\x1b\[\d+d", part)
        if match:
            part = part[match.end():]
        else:
            match = re.match(r"\x1b\[\d+;\d+H", part)
            if match:
                part = part[match.end():]
        result.append(part)
    return result


def shown(binary: str, path: str, keys: bytes) -> tuple[list[str], str]:
    """The lines the editor shows and the line the cursor is on."""
    session = Session(binary, path)
    try:
        session.ready()
        time.sleep(1.0)
        for key in keys:
            session.send(bytes([key]))
            time.sleep(0.05)
        time.sleep(1.2)
        for _ in range(20):
            session.read(0.05)
        output = bytes(session.output)
        # The content an editor shows is the screen it leaves behind. The
        # port writes the rows that changed, and the original editor writes
        # into a scrolling region, so the screen model follows both.
        lines = screen_lines(output, 24, 80)
        # The status bar reports the percentage and the line of the cursor.
        # The original editor writes it in pieces, so the last report of the
        # output is the current one.
        plain = STYLE.sub(b"", output)
        text = plain.decode("utf-8", "replace")
        match = re.findall(r"(\d+)% « (\d+)", text)
        status = match[-1] if match else ("", "")
        return lines, status
    finally:
        try:
            session.process.kill()
        except Exception:
            pass
        session.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path")
    parser.add_argument("c_binary")
    parser.add_argument("rust_binary")
    parser.add_argument("--keys", default="")
    options = parser.parse_args()

    keys = options.keys.encode()
    left, left_status = shown(options.c_binary, options.path, keys)
    right, right_status = shown(options.rust_binary, options.path, keys)

    print(f"the C editor's status: {left_status!r}, the port's: {right_status!r}")
    # The line the cursor is on, which the status bar reports.
    left_line = left_status[1] if left_status[1] else ""
    right_line = right_status[1] if right_status[1] else ""
    same_line = left_line == right_line
    print(f"the cursor is on line {left_line!r} in the C editor and {right_line!r} in the port")

    # The lines of the document the two show.
    left_set = {line.strip() for line in left if line.strip() and "«" not in line}
    right_set = {line.strip() for line in right if line.strip() and "«" not in line}
    missing = sorted(left_set - right_set)
    extra = sorted(right_set - left_set)
    for line in missing[:6]:
        print(f"  only the C editor: {line[:60]!r}")
    for line in extra[:6]:
        print(f"  only the port: {line[:60]!r}")
    ok = same_line and not missing and not extra
    print("the same behaviour" if ok else "the behaviour differs")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
