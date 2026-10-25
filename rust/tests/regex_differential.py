#!/usr/bin/env python3
"""Compare the Rust ERE engine to the platform C regex used by vis.

Run from any directory: python3 rust/tests/regex_differential.py
Requires python3, rustc and a C compiler (CC, default cc); no Python packages.
Build artifacts live only in a temporary directory. The ASCII corpus fixes
LC_ALL=C so locale-dependent POSIX classes do not obscure engine differences.
This is a bounded compatibility corpus, not proof of all POSIX semantics.
Empty ERE is excluded: macOS libc rejects it, while the Rust API deliberately
accepts it as an empty match (covered by Rust unit tests).
"""

import itertools
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile


ORACLE = r"""
#include <regex.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    regex_t regex;
    regmatch_t matches[65];
    if (regcomp(&regex, argv[1], REG_EXTENDED | REG_NEWLINE)) {
        puts("ERR");
        return 0;
    }
    if (regexec(&regex, argv[2], 65, matches, 0)) {
        puts("NONE");
    } else {
        for (size_t i = 0; i <= regex.re_nsub && i < 65; i++)
            printf("%ld:%ld ", (long)matches[i].rm_so, (long)matches[i].rm_eo);
        puts("");
    }
    regfree(&regex);
}
"""

DRIVER = r"""
#[allow(dead_code)]
#[path = REGEX_PATH]
mod regex;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let regex = match regex::Regex::new(&args[1]) {
        Ok(regex) => regex,
        Err(_) => { println!("ERR"); return; }
    };
    match regex.find(args[2].as_bytes(), 0..args[2].len()) {
        Err(_) => println!("ERR"),
        Ok(None) => println!("NONE"),
        Ok(Some(found)) => {
            for group in found.groups {
                match group {
                    Some(range) => print!("{}:{} ", range.start, range.end),
                    None => print!("-1:-1 "),
                }
            }
            println!();
        }
    }
}
"""


def main():
    source = Path(__file__).resolve().parents[1] / "src" / "regex.rs"
    patterns = [
        "a", ".", "^", "$", "^$", "^.*$", "[^a]*", "[ab]+",
        "[[:alpha:]]+", "a{0,2}", "a{2,}", "a?b+", "a|ab", "(a|aa)*",
        "(a|ab)*(b*)", "((a|aa)*)", "((a?)(a?))*", "(a*)(a*)",
        "(a|aa)(a?)", "((a*)*)", "(a(b)?)+", "((ab)?)*",
        "(a|ab)*(b|ba)*", "(a?)*", "(a*)*", "(a|aa)*a?", "(a|(ab))*",
    ]
    inputs = ["".join(chars) for size in range(6)
              for chars in itertools.product("ab", repeat=size)]
    inputs += ["a\nb", "\n", "a\n\nb\n", " ", "12_ab!", "a\tb"]
    environment = dict(os.environ, LC_ALL="C")
    failures = []
    with tempfile.TemporaryDirectory(prefix="vis-regex-") as directory:
        directory = Path(directory)
        c_source = directory / "oracle.c"
        c_source.write_text(ORACLE)
        rust_source = directory / "driver.rs"
        rust_source.write_text(DRIVER.replace("REGEX_PATH", json.dumps(str(source))))
        oracle, driver = directory / "oracle", directory / "driver"
        subprocess.run(shlex.split(os.environ.get("CC", "cc")) +
                       [str(c_source), "-o", str(oracle)], check=True, timeout=30)
        subprocess.run(["rustc", "--edition=2021", str(rust_source), "-o", str(driver)], check=True, timeout=30)
        for pattern, data in itertools.product(patterns, inputs):
            expected = subprocess.check_output([str(oracle), pattern, data], env=environment, timeout=10).strip()
            actual = subprocess.check_output([str(driver), pattern, data], env=environment, timeout=10).strip()
            if expected != actual:
                failures.append((pattern, data, expected, actual))
    for pattern, data, expected, actual in failures[:10]:
        print(f"{pattern=!r} {data=!r}: C={expected!r}, Rust={actual!r}")
    total = len(patterns) * len(inputs)
    print(f"Regex differential: {total - len(failures)}/{total} cases match platform libc")
    raise SystemExit(bool(failures))


if __name__ == "__main__":
    main()
