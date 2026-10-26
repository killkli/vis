#!/usr/bin/env python3
"""Compare default soft-wrapped coordinates with actual C view.c functions.

The C oracle includes the original implementation, manually initializes a view
without opening a terminal, and queries view_coord_get. The corpus covers ASCII
controls/newlines/tabs and representative valid UTF-8, not every locale/width rule.
Known C EOF and wrapped-tab coordinate quirks are counted separately, never
reported as equal. Unknown differences fail. No external Python packages needed.
"""

import argparse
import json
import os
from pathlib import Path
import random
import shlex
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]

C_ORACLE = r'''
#define main original_main
#include MAIN_PATH
#undef main
int main(int argc, char **argv) {
    if (argc != 6) return 2;
    if (!setlocale(LC_ALL, "C.UTF-8")) setlocale(LC_ALL, "en_US.UTF-8");
    Vis session = {0}; Win win = {.vis = &session};
    Text *txt = vis_text_load(&session, 0, TEXT_LOAD_READ);
    if (!txt || !text_insert(&session, txt, 0, argv[1], strlen(argv[1]))) return 3;
    View *view = &win.view;
    view->text = txt; view->tabwidth = atoi(argv[3]); view->breakat = argv[4];
    view->wrapcolumn = atoi(argv[5]);
    if (!vis_view_resize(view, atoi(argv[2]), 512)) return 4;
    for (size_t pos = 0; pos <= strlen(argv[1]); pos++) {
        int row = -1, column = -1;
        view_coord_get(view, pos, 0, &row, &column);
        printf("%zu:%d:%d ", pos, row, column);
    }
    puts("");
}
'''

RUST_DRIVER = r'''
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let data = args[1].as_bytes();
    let width: usize = args[2].parse().unwrap();
    let tab_width: usize = args[3].parse().unwrap();
    let breakat = &args[4];
    let wrapcolumn: usize = args[5].parse().unwrap();
    let wrap_columns = if wrapcolumn == 0 { width } else { width.min(wrapcolumn) };
    for pos in 0..=data.len() {
        let (row, column) = vis::view::screen_position_with_layout(
            data, pos, width, wrap_columns, tab_width, breakat,
        );
        print!("{pos}:{row}:{column} ");
    }
    println!();
}
'''


def coordinates(binary, text, width, tab_width, breakat="", wrapcolumn=0):
    output = subprocess.check_output(
        [str(binary), text, str(width), str(tab_width), breakat, str(wrapcolumn)], timeout=5
    )
    return {position: (row, column) for position, row, column in
            (map(int, entry.split(b":")) for entry in output.split())}


def wrapped_tab_ends(text, width, tab_width):
    if not text.isascii():
        return set()
    column = 0
    ends = set()
    for pos, character in enumerate(text):
        if character == "\t":
            count = tab_width - column % tab_width
            if column + count > width:
                ends.add(pos + 1)
            for _ in range(count):
                if column == width:
                    column = 0
                column += 1
        elif character == "\n":
            column = 0
        else:
            cells = 2 if ord(character) < 32 or ord(character) == 127 else 1
            if column + cells > width:
                column = 0
            column += cells
    return ends


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library-dir", type=Path, default=ROOT / "rust/target/debug")
    options = parser.parse_args()
    library_dir = options.library_dir.resolve()
    randomizer = random.Random(0x56574557)
    inputs = ["", "abcdefghij", "abcde\nX", "a\tb", "abc\n", "ab界c", "a\u0301bc", "界a界", "ab界", "界a"]
    inputs += ["".join(randomizer.choice("abc \t\n\x01\x7f") for _ in range(randomizer.randrange(1, 40))) for _ in range(60)]
    equal = 0
    known = {"trailing-newline EOF": 0, "wrapped-tab continuation": 0, "C byte/display EOF accounting": 0}
    failures = []
    with tempfile.TemporaryDirectory(prefix="vis-view-diff-") as temporary:
        directory = Path(temporary)
        c_source, rust_source = directory / "view.c", directory / "view.rs"
        # A fresh checkout has no generated config.h. Compile the unchanged C
        # implementation with its shipped default bindings, entirely in temp.
        original_main = (ROOT / "main.c").read_text()
        assert original_main.count('#include "config.h"') == 1
        oracle_main = directory / "original-main.c"
        oracle_main.write_text(original_main.replace('#include "config.h"', '#include "config.def.h"'))
        c_source.write_text(C_ORACLE.replace("MAIN_PATH", json.dumps(str(oracle_main))))
        rust_source.write_text(RUST_DRIVER)
        c_driver, rust_driver = directory / "view-c", directory / "view-rust"
        subprocess.run(shlex.split(os.environ.get("CC", "cc")) + ["-std=c99", '-DVERSION="oracle"', "-DVIS_API=0", "-I", str(ROOT), str(c_source), "-lm", "-o", str(c_driver)], check=True, timeout=30)
        # Freeze the library while other agents/Cargo invocations build features.
        library = directory / "libvis.rlib"
        shutil.copy2(library_dir / "libvis.rlib", library)
        subprocess.run(["rustc", "--edition=2024", str(rust_source), "--extern", f"vis={library}", "-L", f"dependency={library_dir / 'deps'}", "-o", str(rust_driver)], check=True, timeout=30)
        for text in inputs:
            encoded = text.encode()
            # A visual cursor normally uses UTF-8 boundaries; arbitrary offsets
            # inside a multibyte scalar remain a separate binary-view audit.
            positions, offset = [0], 0
            for character in text:
                offset += len(character.encode())
                positions.append(offset)
            for width in [2, 3, 5, 8, 13]:
                for tab_width in [2, 4, 8]:
                    expected = coordinates(c_driver, text, width, tab_width)
                    actual = coordinates(rust_driver, text, width, tab_width)
                    tab_ends = wrapped_tab_ends(text, width, tab_width)
                    for position in positions:
                        if expected[position] == actual[position]:
                            equal += 1
                        elif position == len(encoded) and text.endswith("\n") and actual[position][1] == 0 and expected[position][0] + 1 == actual[position][0] and 1 <= expected[position][1] <= width:
                            known["trailing-newline EOF"] += 1
                        elif position in tab_ends and expected[position][1] in (0, width) and expected[position][0] <= actual[position][0]:
                            known["wrapped-tab continuation"] += 1
                        elif position == len(encoded) and actual[position][1] == 0 and expected[position] == (actual[position][0] - 1, width) and any(ord(c) < 32 or ord(c) > 126 for c in text):
                            # The full row needs an EOF cell on the next row,
                            # but C's byte-count equality hack fails for tabs,
                            # wide glyphs and control spellings of other widths.
                            known["C byte/display EOF accounting"] += 1
                        elif position == len(encoded) and not text.isascii() and expected[position] == (actual[position][0] + 1, 0):
                            # view_draw compares file_byte_count to terminal
                            # width when forcing an EOF cursor to the next row.
                            last_row_bytes = sum(len(character.encode()) for start, character in zip(positions, text) if actual[start][0] == actual[position][0])
                            if last_row_bytes == width:
                                known["C byte/display EOF accounting"] += 1
                            else:
                                failures.append((text, width, tab_width, position, expected[position], actual[position]))
                        else:
                            failures.append((text, width, tab_width, position, expected[position], actual[position]))
        custom_wrap_cases = [
            ("aa bb-cc/dd ee", " -/", 0),
            ("alpha,beta.gamma:delta", ",.:", 6),
            ("one_two three-four", "_ -", 4),
            ("wide界-char words", " -", 5),
            ("base\nnext-line words", " -", 4),
        ]
        for text, breakat, wrapcolumn in custom_wrap_cases:
            positions, offset = [0], 0
            for character in text:
                offset += len(character.encode())
                positions.append(offset)
            for width in [5, 9, 13]:
                for tab_width in [4, 8]:
                    expected = coordinates(c_driver, text, width, tab_width, breakat, wrapcolumn)
                    actual = coordinates(rust_driver, text, width, tab_width, breakat, wrapcolumn)
                    for position in positions:
                        if expected[position] == actual[position]:
                            equal += 1
                        else:
                            failures.append((text, width, tab_width, breakat, wrapcolumn,
                                             position, expected[position], actual[position]))
    print(f"View coordinates: {equal} equal; {sum(known.values())} documented C quirks; {len(failures)} unknown differences")
    for name, count in known.items():
        print(f"  {name}: {count}")
    for failure in failures[:10]:
        print(repr(failure))
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
