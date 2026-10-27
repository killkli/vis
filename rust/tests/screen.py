"""Apply a terminal frame to a virtual screen.

The frames the port writes are compared with the frames the original editor
wrote, by rendering both on a virtual screen and comparing the cells. The
byte order of the two differs, because the port writes a row once while the
original writes it twice, but the screen they leave behind must be the same.

Usage: python3 rust/tests/screen.py < frame
"""
import re
import sys

# The sequence that places the cursor.
POSITION = re.compile(r"\x1b\[(\d*);?(\d*)H")
# The sequences that change the style, which do not alter the cells.
STYLE = re.compile(r"\x1b\[[0-9;]*m")


def render(frame: str, rows: int = 24, columns: int = 80) -> list[str]:
    """The cells a frame leaves on the screen."""
    screen = [[" "] * columns for _ in range(rows)]
    row = column = 1
    index = 0
    length = len(frame)
    while index < length:
        character = frame[index]
        if character == "\x1b":
            match = POSITION.match(frame, index)
            if match:
                row = int(match.group(1) or 1)
                column = int(match.group(2) or 1)
                index = match.end()
                continue
            match = STYLE.match(frame, index)
            if match:
                index = match.end()
                continue
            # An unknown sequence is skipped up to its final byte.
            end = index + 2
            while end < length and not frame[end].isalpha():
                end += 1
            index = min(end + 1, length)
            continue
        if character == "\n":
            row += 1
            column = 1
            index += 1
            continue
        if character == "\r":
            column = 1
            index += 1
            continue
        if 1 <= row <= rows and 1 <= column <= columns:
            screen[row - 1][column - 1] = character
        column += 1
        index += 1
    return ["".join(line).rstrip() for line in screen]


def main() -> int:
    frame = sys.stdin.read()
    for line in render(frame):
        print(line.rstrip())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
