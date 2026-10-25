#!/usr/bin/env python3
"""Run identical prompt-object/history assertions against native C and Rust.

The C START event can overwrite an exit status, so success requires a marker
written only after every assertion. No fixture methods or events are replaced.
"""
import argparse
import os
from pathlib import Path
import re
import subprocess
import tempfile
from key_differential import lua_string

ROOT = Path(__file__).resolve().parents[2]


def native_gate(binary, directory, contract):
    source = directory / "input.txt"
    source.write_bytes(b"abc xyz\n")
    marker = directory / "contract-pass"
    configuration = (
        f"package.path={lua_string(str(ROOT / 'lua/?.lua') + ';')}..package.path\n"
        "require('vis')\n"
        "vis.events.subscribe(vis.events.START,function()\n"
        f"  local ok,err=pcall(dofile,{lua_string(str(contract))})\n"
        "  if not ok then io.stderr:write(tostring(err),'\\n');vis:exit(1)\n"
        f"  else local out=assert(io.open({lua_string(str(marker))},'w'))\n"
        "    out:write('PASS');out:close();vis:exit(0) end\n"
        "end)\n"
    )
    (directory / "visrc.lua").write_text(configuration)
    result = subprocess.run(
        [str(binary), str(source)], cwd=directory,
        env={**os.environ, "VIS_PATH": str(directory), "TERM": "xterm-256color"},
        capture_output=True, timeout=30,
    )
    (directory / "c.stdout").write_bytes(result.stdout)
    (directory / "c.stderr").write_bytes(result.stderr)
    if result.returncode != 0 or not marker.exists() or marker.read_bytes() != b"PASS":
        raise RuntimeError(f"C prompt assertions failed (exit {result.returncode}, marker {marker.exists()})\n"
                           + result.stderr.decode(errors="replace")[-2000:])
    print(f"PASS native C {contract.name}, verified success marker")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--skip-rust", action="store_true")
    parser.add_argument("--contract", type=Path,default=ROOT/"rust/tests/fixtures/prompt-contract.lua")
    parser.add_argument("--rust-test", default="lua_prompt")
    parser.add_argument("--rust-filter", help="Run one shared contract within the Rust test target")
    arguments = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="vis-prompt-differential-") as name:
        directory = arguments.artifacts.resolve() if arguments.artifacts else Path(name)
        directory.mkdir(parents=True, exist_ok=True)
        # An earlier successful run must not satisfy the mandatory marker.
        (directory / "contract-pass").unlink(missing_ok=True)
        native_gate(arguments.c_vis.resolve(), directory,arguments.contract.resolve())
        if not arguments.skip_rust:
            rust_command = [
                "cargo", "test", "--manifest-path", str(ROOT / "rust/Cargo.toml"),
                "--features", "lua", "--locked", "--test", arguments.rust_test,
            ]
            if arguments.rust_filter:
                rust_command.append(arguments.rust_filter)
            result = subprocess.run(rust_command + ["--", "--nocapture"],
                                    cwd=ROOT, capture_output=True, timeout=120)
            (directory / "rust.stdout").write_bytes(result.stdout)
            (directory / "rust.stderr").write_bytes(result.stderr)
            exercised = re.search(rb"test result: ok\. ([1-9][0-9]*) passed", result.stdout)
            if result.returncode or not exercised:
                raise RuntimeError("Rust shared prompt assertions failed\n" +
                                   (result.stdout + result.stderr).decode(errors="replace")[-3000:])
            print(f"PASS Rust host runs identical {arguments.contract.name}")


if __name__ == "__main__":
    main()
