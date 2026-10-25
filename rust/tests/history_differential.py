#!/usr/bin/env python3
"""Compare the Rust piece-table history with the original C implementation.

Run: python3 rust/tests/history_differential.py
Requires Python 3, a C compiler (CC, default cc), and rustc (RUSTC, default rustc).
No Python packages or Cargo dependencies are needed. All build artifacts and
input files live in an automatically removed temporary directory.

The oracle compiles original util.c, buffer.c and text.c directly. A test-only
clock replaces time(NULL); Rust uses private from_bytes_at/replace_at helpers
in an appended driver. These deterministic test seams do not change production
clock behavior. Checks cover revision identity, Unix seconds, byte content,
modified status, undo/redo cursor results, and stable marks across branches.
"""

import os
from pathlib import Path
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]

C_ORACLE = r'''
#include "util.h"
#include "util.c"
#include "buffer.c"

static time_t clock_value = 100;
static time_t fixed_time(time_t *out) {
    if (out) *out = clock_value;
    return clock_value;
}
#define time fixed_time
#include "text.c"
#undef time

static unsigned char raw[65536];

int main(int argc, char **argv) {
    Text *txt = vis_text_load(NULL, argv[1], TEXT_LOAD_READ);
    if (!txt) return 2;
    Mark mark = EMARK;
    char line[131200], hex[131072];
    size_t start, end, pos;
    long long timestamp;
    while (fgets(line, sizeof(line), stdin)) {
        pos = EPOS;
        switch (line[0]) {
        case 'X': {
            int fields = sscanf(line, "X %lld %zu %zu %131071s",
                                &timestamp, &start, &end, hex);
            size_t len = fields == 4 ? strlen(hex) / 2 : 0;
            for (size_t i = 0; i < len; i++) {
                unsigned value;
                sscanf(hex + 2 * i, "%2x", &value);
                raw[i] = value;
            }
            clock_value = timestamp;
            if (!text_delete(txt, start, end - start) ||
                !text_insert(NULL, txt, start, raw, len)) return 3;
            break;
        }
        case 'S': text_snapshot(txt); break;
        case 'U': pos = text_undo(txt); break;
        case 'R': pos = text_redo(txt); break;
        case 'E': pos = text_earlier(txt); break;
        case 'L': pos = text_later(txt); break;
        case 'T':
            sscanf(line, "T %lld", &timestamp);
            pos = text_restore(txt, timestamp);
            break;
        case 'V': text_mark_current_revision(txt); break;
        case 'M':
            sscanf(line, "M %zu", &start);
            mark = text_mark_set(txt, start);
            break;
        default: return 4;
        }
        printf("%zu %lld %zu %d ", txt->history->seq,
               (long long)text_state(txt), text_size(txt), text_modified(txt));
        if (pos == EPOS) printf("- "); else printf("%zu ", pos);
        pos = text_mark_get(txt, mark);
        if (pos == EPOS) printf("- "); else printf("%zu ", pos);
        size_t length = text_bytes_get(txt, 0, sizeof(raw), (char *)raw);
        for (size_t i = 0; i < length; i++) printf("%02x", raw[i]);
        puts("");
    }
    text_free(txt);
}
'''

# Appended in the text.rs module's scope so deterministic timestamp helpers and
# revision identity remain test seams, without expanding the production API.
RUST_DRIVER = r'''
fn decode_hex(raw: &str) -> Vec<u8> {
    raw.as_bytes().chunks(2).map(|bytes| {
        u8::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap()
    }).collect()
}

fn main() {
    use std::io::BufRead;
    let path = std::env::args_os().nth(1).unwrap();
    let mut text = Text::from_bytes_at(std::fs::read(path).unwrap(), 100);
    let mut mark = None;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut parameters = line.split_whitespace();
        let operation = parameters.next().unwrap();
        let mut pos = None;
        match operation {
            "X" => {
                let time = parameters.next().unwrap().parse().unwrap();
                let start = parameters.next().unwrap().parse().unwrap();
                let end = parameters.next().unwrap().parse().unwrap();
                let bytes = decode_hex(parameters.next().unwrap_or(""));
                text.replace_at(start..end, &bytes, time).unwrap();
            }
            "S" => text.snapshot(),
            "U" => pos = text.undo(),
            "R" => pos = text.redo(),
            "E" => pos = text.earlier(),
            "L" => pos = text.later(),
            "T" => pos = text.restore(parameters.next().unwrap().parse().unwrap()),
            "V" => text.mark_saved(),
            "M" => mark = text.mark(parameters.next().unwrap().parse().unwrap()),
            _ => panic!("invalid history instruction"),
        }
        print!("{} {} {} {} {} {} ", text.current, text.state(), text.len(),
               u8::from(text.modified()),
               pos.map_or("-".into(), |pos| pos.to_string()),
               mark.clone().and_then(|mark| text.mark_get(mark))
                   .map_or("-".into(), |pos| pos.to_string()));
        for byte in text.iter() { print!("{byte:02x}"); }
        println!();
    }
}
'''


def run(command, **kwargs):
    result = subprocess.run(
        command, capture_output=True, text=True, timeout=30, **kwargs
    )
    if result.returncode:
        raise RuntimeError(
            f"Command failed ({result.returncode}): {shlex.join(command)}\n"
            f"{result.stdout}{result.stderr}"
        )
    return result.stdout


def cases():
    # Topology from test/core/text-test.c: 0--1--2--3, with 1--4--5--6--7.
    operations = []
    for index, value in enumerate(b"0123"):
        operations += [
            f"X {101 + index} 0 {0 if index == 0 else 1} {value:02x}", "S"
        ]
    operations += ["V", "U", "U", "M 0"]
    for index, value in enumerate(b"4567"):
        operations += [f"X {105 + index} 0 1 {value:02x}", "S"]
    operations += [f"T {time}" for time in range(101, 109)]
    operations += [f"T {time}" for time in range(108, 100, -1)]
    operations += ["R", "R", "R", "R", "T 90", "T 200"]
    yield "original branching scenario, saved state and marks", b"", operations

    yield "branch cursors, duplicate seconds and nearest-time ties", b"abcdef", [
        "X 110 2 2 58", "S", "X 120 5 5 59", "S", "U",
        "X 130 0 0 5a", "S", "E", "L", "T 100", "T 115",
        "T 119", "T 129", "T 101", "X 105 0 0 51", "S",
        "X 105 0 0 52", "S", "T 105", "U", "T 105",
        "T 104", "T 106", "T 90", "T 200",
    ]

    yield "backward clock jumps preserve creation order", b"", [
        "X 110 0 0 61", "X 999 1 1 62", "S", "X 110 2 2 63", "S",
        "X 90 3 3 64", "S", "T 110", "T 100", "T 95", "T 115",
        "T 89", "T 111", "T 90", "T 100", "T 110",
    ]

    yield "no-movement history requests preserve live undo groups", b"", [
        "X 110 0 0 61", "T 110", "X 120 1 1 62", "L",
        "X 130 2 2 63", "U", "R", "E", "L",
    ]


def main():
    with tempfile.TemporaryDirectory(prefix="vis-history-differential-") as name:
        directory = Path(name)
        c_source = directory / "oracle.c"
        rust_source = directory / "probe.rs"
        c_source.write_text(C_ORACLE)
        rust_source.write_text((ROOT / "rust/src/text.rs").read_text() + RUST_DRIVER)
        oracle = directory / "oracle"
        probe = directory / "probe"
        run(shlex.split(os.environ.get("CC", "cc")) + [
            "-std=c99", "-Wno-unused-function", "-I", str(ROOT),
            str(c_source), "-o", str(oracle),
        ])
        run(shlex.split(os.environ.get("RUSTC", "rustc")) + [
            "--edition=2024", "-A", "dead_code", str(rust_source), "-o", str(probe),
        ])
        count = 0
        for label, initial, operations in cases():
            path = directory / "input"
            path.write_bytes(initial)
            instructions = "\n".join(operations) + "\n"
            expected = run([str(oracle), str(path)], input=instructions).splitlines()
            actual = run([str(probe), str(path)], input=instructions).splitlines()
            if len(expected) != len(operations) or len(actual) != len(expected):
                raise AssertionError(f"{label}: missing history output")
            for index, (wanted, got) in enumerate(zip(expected, actual)):
                if wanted != got:
                    raise AssertionError(
                        f"{label}, instruction {index}: {operations[index]!r}\n"
                        f"Fields: revision timestamp length modified cursor mark hexbytes\n"
                        f"C:    {wanted}\nRust: {got}\n"
                        f"Instructions:\n{instructions}"
                    )
            count += len(operations)
            print(f"PASS {label}: {len(operations)} states")
        print(f"PASS {count} C/Rust history operation states")


if __name__ == "__main__":
    main()
