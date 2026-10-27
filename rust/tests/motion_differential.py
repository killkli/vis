#!/usr/bin/env python3
"""Compare safe Rust motions to the original C implementations, without crates.

Run from any directory: python3 rust/tests/motion_differential.py
Requires Python 3, a C compiler (CC, default cc), and rustc. Temporary C and Rust
executables are removed on exit. The original C motion and word-object source
is compiled directly, with minimal contiguous-buffer iterators. Inputs are ASCII
(including whitespace/control bytes), so character iterators move one byte. This
checks motion/object semantics, not the C piece table or locale/Unicode widths.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]

C_PRELUDE = r'''
#define _XOPEN_SOURCE 700
#include <stdio.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <ctype.h>
#include <wchar.h>
#include <errno.h>
#include <limits.h>
#include <locale.h>
typedef struct {const unsigned char *data; size_t len;} Text;
typedef struct {size_t start,end;} Filerange;
typedef struct {const Text *txt; size_t pos; bool valid;} Iterator;
#define ISASCII(c) ((unsigned char)c<128)
#define IsBlank(c) ((c)==' '||(c)=='\t')
#define IsSpace(c) ((c)==' '||(c)=='\t'||(c)=='\n'||(c)=='\r')
#define IsBoundary(c) (isboundary((unsigned char)c))
#define text_range_empty() ((Filerange){SIZE_MAX,SIZE_MAX})
Filerange text_range_new(size_t a,size_t b) {return (Filerange){a,b};}
#define text_range_valid(r) ((r).start<= (r).end)
#define text_range_contains(r,p) ((r).start <=(p)&&(p)<(r).end)
#define EPOS SIZE_MAX
size_t text_size(const Text*t) {return t->len;}
const Text*text_iterator_text(Iterator*i) {return i->txt;}
Iterator text_iterator_get(const Text*t,size_t p){return (Iterator){t,p,p<=t->len};}
bool text_iterator_byte_get(Iterator*i,char*c){if(!i->valid)return false;*c=i->pos<i->txt->len?i->txt->data[i->pos]:0;return true;}
bool text_iterator_byte_next(Iterator*i,char*c){if(!i->valid||i->pos==i->txt->len){i->valid=false;return false;}i->pos++; if(c) text_iterator_byte_get(i,c);return true;}
bool text_iterator_byte_prev(Iterator*i,char*c){if(!i->valid||i->pos==0){i->valid=false;return false;}i->pos--;if(c)text_iterator_byte_get(i,c);return true;}
bool text_iterator_char_next(Iterator*i,char*c){return text_iterator_byte_next(i,c);}
bool text_iterator_char_prev(Iterator*i,char*c){return text_iterator_byte_prev(i,c);}
bool text_iterator_codepoint_next(Iterator*i,char*c){return text_iterator_byte_next(i,c);}
bool text_iterator_codepoint_prev(Iterator*i,char*c){return text_iterator_byte_prev(i,c);}
bool text_iterator_byte_find_next(Iterator*i,char c){while(i->pos<i->txt->len){if(i->txt->data[i->pos]==c)return true;i->pos++;}return false;}
bool text_iterator_byte_find_prev(Iterator*i,char c){while(i->pos>0){i->pos--;if(i->txt->data[i->pos]==c)return true;}return false;}
size_t text_bytes_get(const Text*t,size_t p,size_t n,char*b){if(p>t->len)return 0;if(n>t->len-p)n=t->len-p;memcpy(b,t->data+p,n);return n;}
size_t text_line_blank_next(Text*,size_t);
size_t text_line_end(Text*,size_t);

size_t text_line_blank_prev(Text*,size_t);
// TODO: specify this per file type?
'''

C_MAIN = r'''int main(){unsigned seed=1; unsigned char data[8];const char alphabet[]="a b!_\t\n\r\f\v";setlocale(LC_ALL,"C.UTF-8");for(int t=0;t<100;t++){for(int j=0;j<8;j++){seed=seed*1664525+1013904223;data[j]=alphabet[(seed>>16)%10];}Text txt={data,8};for(size_t p=0;p<=8;p++){
size_t (*fs[])(Text*,size_t)={text_word_start_next,text_word_start_prev,text_word_end_next,text_word_end_prev,text_longword_start_next,text_longword_start_prev,text_longword_end_next,text_longword_end_prev,text_line_begin,text_line_start,text_line_finish,text_line_end,text_line_next,text_line_prev,text_line_up,text_line_down,text_paragraph_next,text_paragraph_prev,text_sentence_next,text_sentence_prev};
printf("%d %zu",t,p);for(size_t f=0;f<sizeof(fs)/sizeof(*fs);f++)printf(" %zu",fs[f](&txt,p));Filerange(*os[])(Text*,size_t)={text_object_word,text_object_word_outer,text_object_longword,text_object_longword_outer};for(size_t f=0;f<4;f++){Filerange r=os[f](&txt,p);printf(" %zu %zu",r.start,r.end);}puts("");}}}
'''

RUST_MAIN = r'''
#[path=WIDTH_PATH] mod width;
#[path=MOTION_PATH] mod motion;
use motion::*;
fn main(){let mut seed=1u32;let alphabet=b"a b!_\t\n\r\x0c\x0b";for t in 0..100{let mut data=[0u8;8];for b in &mut data{seed=seed.wrapping_mul(1664525).wrapping_add(1013904223);*b=alphabet[((seed>>16)%10)as usize];}for p in 0..=8{print!("{t} {p}");for f in [word_start_next,word_start_prev,word_end_next,word_end_prev,longword_start_next,longword_start_prev,longword_end_next,longword_end_prev,line_begin,line_start,line_finish,line_end,line_next,line_prev,line_up,line_down,paragraph_next,paragraph_prev,sentence_next,sentence_prev]{print!(" {}",f(&data,p));}for (outer,big) in [(false,false),(true,false),(false,true),(true,true)]{let r=word_object(&data,p,outer,big);print!(" {} {}",r.start,r.end);}println!();}}}
'''

FIELDS = """word_start_next word_start_prev word_end_next word_end_prev
longword_start_next longword_start_prev longword_end_next longword_end_prev
line_begin line_start line_finish line_end line_next line_prev line_up line_down
paragraph_next paragraph_prev sentence_next sentence_prev
word_inner_start word_inner_end word_outer_start word_outer_end
longword_inner_start longword_inner_end longword_outer_start longword_outer_end""".split()


def run(command):
    result = subprocess.run(command, text=True, capture_output=True, timeout=30)
    if result.returncode:
        raise RuntimeError(
            f"Command failed ({result.returncode}): {shlex.join(command)}\n"
            f"{result.stdout}{result.stderr}"
        )
    return result.stdout


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def main():
    motions = (ROOT / "text-motions.c").read_text().split("size_t text_block_start")[0]
    objects = (ROOT / "text-objects.c").read_text().split("Filerange text_object_word_find_next")[0]
    with tempfile.TemporaryDirectory(prefix="vis-motion-diff-") as folder:
        folder = Path(folder)
        c_source = folder / "oracle.c"
        rs_source = folder / "actual.rs"
        c_binary = folder / "oracle"
        rs_binary = folder / "actual"
        c_source.write_text(C_PRELUDE + motions + objects + C_MAIN)
        rs_source.write_text(
            RUST_MAIN.replace("MOTION_PATH", json.dumps(str(ROOT / "rust/src/motion.rs")))
            .replace("WIDTH_PATH", json.dumps(str(ROOT / "rust/src/width.rs")))
        )
        run(shlex.split(os.environ.get("CC", "cc")) + ["-std=c11", str(c_source), "-o", str(c_binary)])
        run(["rustc", "--edition=2024", "-Awarnings", str(rs_source), "-o", str(rs_binary)])
        expected = run([str(c_binary)]).splitlines()
        actual = run([str(rs_binary)]).splitlines()
    require(len(expected) == len(actual) == 900, "Incomplete oracle or Rust output")
    compared = 0
    for c_line, rs_line in zip(expected, actual):
        c_values = c_line.split()
        rs_values = rs_line.split()
        require(c_values[:2] == rs_values[:2], "Mismatched buffer/position")
        require(len(c_values) == len(rs_values) == len(FIELDS) + 2, "Incomplete comparison fields")
        for field, c_value, rs_value in zip(FIELDS, c_values[2:], rs_values[2:]):
            require(c_value == rs_value, (
                f"buffer {c_values[0]}, byte {c_values[1]}, {field}: "
                f"C={c_value}, Rust={rs_value}"
            ))
            compared += 1
    print(f"PASS: {compared:,} C/Rust motion and word-object values across 900 positions")


if __name__ == "__main__":
    main()
