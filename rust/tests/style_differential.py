#!/usr/bin/env python3
"""Compare typed Rust style parsing/merging with original C functions.

The oracle includes unchanged ui-terminal.c through main.c and never opens a
terminal. Rust's generated SGR is decoded independently and compared with the C
merged fields, so attribute clearing and color inheritance are both checked.
"""
import argparse
import itertools
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
C_SOURCE = r'''
#define main original_main
#include MAIN_PATH
#undef main
static unsigned color(VisCellStyle style, bool bg) {
    if (style.properties & (bg ? VisCellProperty_IndexedBG : VisCellProperty_IndexedFG))
        return bg ? VisCellStyleBGIndexGet(&style) : VisCellStyleFGIndexGet(&style);
    return bg ? ((unsigned)style.bg_r<<16)|((unsigned)style.bg_g<<8)|style.bg_b :
                ((unsigned)style.fg_r<<16)|((unsigned)style.fg_g<<8)|style.fg_b;
}
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    Vis session = {0}; session.ui.style_count = 512;
    vis_ui_style_define(&session, 0, str8_from_c_str(argv[1]));
    vis_ui_style_define(&session, 64, str8_from_c_str(argv[2]));
    VisCellStyle s = vis_cell_style_merge(session.ui.styles[0], session.ui.styles[64]);
    printf("%u %u %u %u %u %u\n", s.attributes,
        !!(s.properties & VisCellProperty_KeepAttribute),
        !(s.properties & VisCellProperty_IndexedFG), color(s, false),
        !(s.properties & VisCellProperty_IndexedBG), color(s, true));
}
'''
RUST_SOURCE = r'''
use vis::style::{Color,Style};
fn color(c:Option<Color>)->(usize,usize) { match c.unwrap() {
    Color::Indexed(v)=>(0,usize::from(v)),
    Color::Rgb(r,g,b)=>(1,(usize::from(r)<<16)|(usize::from(g)<<8)|usize::from(b)),
}}
fn main() {
    let args:Vec<_>=std::env::args().collect();
    let s=Style::terminal_default().merge(Style::parse(&args[1])).merge(Style::parse(&args[2]));
    let (fk,f)=color(s.foreground);let (bk,b)=color(s.background);
    println!("{} {} {fk} {f} {bk} {b}",s.attributes.bits(),u8::from(s.keep_attribute));
    for byte in s.to_sgr().bytes(){print!("{byte:02x}");}
}
'''


def decoded_sgr(sequence):
    attributes, foreground, background = 0, None, None
    flags = {1: 8, 2: 32, 3: 16, 4: 1, 5: 4, 7: 2}
    matches = list(re.finditer(rb"\x1b\[([0-9;]*)m", sequence))
    assert b"".join(match[0] for match in matches) == sequence, sequence
    for match in matches:
        values = list(map(int, match[1].split(b";"))) if match[1] else [0]
        index = 0
        while index < len(values):
            value = values[index]
            if value == 0:
                attributes, foreground, background = 0, None, None
            elif value in flags:
                attributes |= flags[value]
            elif 30 <= value <= 37:
                foreground = (0, value - 30)
            elif 40 <= value <= 47:
                background = (0, value - 40)
            elif value in (38, 48):
                if values[index + 1] == 5:
                    color = (0, values[index + 2])
                    index += 2
                else:
                    assert values[index + 1] == 2
                    r, g, b = values[index + 2:index + 5]
                    color = (1, (r << 16) | (g << 8) | b)
                    index += 4
                if value == 38:
                    foreground = color
                else:
                    background = color
            else:
                raise AssertionError((sequence, value))
            index += 1
    assert foreground is not None and background is not None, sequence
    return [attributes, *foreground, *background]


def run(library_dir):
    specs = ["", "bold", "dim", "italics", "reverse", "underlined", "blink",
             "keep_attribute", "bold,italics,fore:red,back:blue",
             "underlined,fore:#010203", "keep_attribute,dim,back:200",
             "fore:green", "back:white", "fore:0,back:255", "fore:256",
             "BoLd, fore: RED,back: BLUE", "fore:red,fore:invalid", "fore:default",
             "nobold", "fore:12junk", "fore:#12zzzz", "fore:#zzzzzz",
             "bold ,dim", ",,,", "fore:0x12", "back:#-12345",
             "fore:#0x1234", "fore:#-0x123", "fore:#-#abcd", "fore:red\x1b[2J", "fore:999999999999999999999999"]
    with tempfile.TemporaryDirectory(prefix="vis-style-diff-") as temporary:
        directory = Path(temporary)
        main = directory / "original-main.c"
        main.write_text((ROOT / "main.c").read_text().replace('#include "config.h"', '#include "config.def.h"'))
        c_source, rust_source = directory / "style.c", directory / "style.rs"
        c_source.write_text(C_SOURCE.replace("MAIN_PATH", json.dumps(str(main))))
        rust_source.write_text(RUST_SOURCE)
        c_binary, rust_binary = directory / "style-c", directory / "style-rust"
        subprocess.run(shlex.split(os.environ.get("CC", "cc")) + ["-std=c99", '-DVERSION="oracle"', "-DVIS_API=0", "-I", str(ROOT), str(c_source), "-lm", "-o", str(c_binary)], check=True, timeout=30)
        library = directory / "libvis.rlib"
        shutil.copy2(library_dir / "libvis.rlib", library)
        subprocess.run(["rustc", "--edition=2024", str(rust_source), "--extern", f"vis={library}", "-L", f"dependency={library_dir / 'deps'}", "-o", str(rust_binary)], check=True, timeout=30)
        count = 0
        for base, overlay in itertools.product(specs, repeat=2):
            expected = list(map(int, subprocess.check_output([str(c_binary), base, overlay], timeout=5).split()))
            result = subprocess.check_output([str(rust_binary), base, overlay], timeout=5).splitlines()
            observed = list(map(int, result[0].split()))
            assert observed == expected, (base, overlay, expected, observed)
            sequence = bytes.fromhex(result[1].decode())
            assert decoded_sgr(sequence) == [expected[0], *expected[2:]], (base, overlay, expected, sequence)
            count += 1
        print(f"PASS {count} native C parse/merge comparisons and independently decoded Rust SGR states")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library-dir", type=Path, default=ROOT / "rust/target/debug")
    run(parser.parse_args().library_dir.resolve())
