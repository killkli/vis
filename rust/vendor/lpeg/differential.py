#!/usr/bin/env python3
"""Compare pure-Lua LPeg with a temporary native build of cached LPeg 1.1.0.

python3 rust/vendor/lpeg/differential.py [--lua /path/to/lua5.5]
Needs a Lua 5.5 interpreter, C compiler and the existing .package-cache archives.
Nothing is downloaded or installed; native code runs only in the test subprocess.
"""
import argparse
import hashlib
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lua', default=os.environ.get('LUA', 'lua'))
    parser.add_argument('--rust-manifest', type=Path, help='alternate manifest for an isolated host harness')
    parser.add_argument('--rust-features', default='lua')
    arguments = parser.parse_args()
    vendor = Path(__file__).resolve().parent
    root = vendor.parents[2]
    archives = {
        'lpeg-1.1.0.tar.gz': '4b155d67d2246c1ffa7ad7bc466c1ea899bbc40fef0257cc9c03cecbaed4352a',
        'lua-5.5.1.tar.gz': '1c4b4068d67061f2a2231ad2b5422e77acea1487ea9890f6320af614f4373dce',
    }
    version = subprocess.check_output([arguments.lua, '-e', 'io.write(_VERSION)'], timeout=10)
    if version != b'Lua 5.5':
        raise SystemExit(f'Lua 5.5 required, found {version!r}')
    names = sorted(path.stem for path in (root / 'lua' / 'lexers').glob('*.lua') if path.stem != 'lexer')
    with tempfile.TemporaryDirectory(prefix='vis-lpeg-') as directory:
        directory = Path(directory)
        for name, digest in archives.items():
            path = root / '.package-cache' / name
            if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
                raise SystemExit(f'archive digest differs: {path}')
            with tarfile.open(path) as archive:
                archive.extractall(directory, filter='data')
        lpeg_source = directory / 'lpeg-1.1.0'
        sources = [lpeg_source / (name + '.c') for name in ('lpvm','lpcap','lptree','lpcode','lpprint','lpcset')]
        flags = ['-bundle', '-undefined', 'dynamic_lookup'] if sys.platform == 'darwin' else ['-shared', '-fPIC']
        subprocess.run(shlex.split(os.environ.get('CC','cc')) + flags + ['-O2', '-std=c99',
                       '-I' + str(directory / 'lua-5.5.1' / 'src'), *map(str,sources),
                       '-o', str(directory / 'lpeg.so')], check=True, timeout=60)
        (directory / 're.lua').write_bytes((lpeg_source / 're.lua').read_bytes())
        corpus = vendor / 'differential.lua'
        def read_lines(output):
            lines = {}
            for line in output.splitlines():
                key, _, value = line.partition(b'\t')
                # Error wording and stack traces differ between implementations;
                # compare whether construction/matching fails, not its message.
                if key.startswith(b'invalid:'):
                    value = value.partition(b'\t')[0]
                lines[key] = value
            return lines
        result = subprocess.run([arguments.lua, str(corpus), 'native', str(directory), str(root), *names],
                                capture_output=True, timeout=60)
        if result.returncode:
            raise SystemExit(f'native corpus failed:\n{result.stderr.decode(errors="replace")}')
        expected = read_lines(result.stdout)
        captured = directory / 'rust-corpus.txt'
        command = ['cargo', 'test', '--manifest-path',
                   str(arguments.rust_manifest or root / 'rust/Cargo.toml'), '--lib']
        if arguments.rust_features:
            command += ['--features', arguments.rust_features]
        command += ['external_differential_corpus', '--', '--ignored']
        subprocess.run(command, env=dict(os.environ,VIS_LPEG_OUTPUT=str(captured)),
                       check=True, timeout=120)
        actual = read_lines(captured.read_bytes())
    failures = [key for key in expected.keys() | actual.keys() if expected.get(key) != actual.get(key)]
    for key in sorted(failures)[:15]:
        if key.startswith(b'lexer:'):
            def tags(value):
                entries = {}
                for index, item in re.findall(rb'n(\d+)=(s[0-9a-f]*|n\d+)', value):
                    entries[int(index)] = bytes.fromhex(item[1:].decode()).decode(errors='replace') if item.startswith(b's') else int(item[1:])
                return [entries[index] for index in sorted(entries)]
            left, right = tags(expected[key]), tags(actual[key])
            difference = next((i for i, pair in enumerate(zip(left, right)) if pair[0] != pair[1]), min(len(left),len(right)))
            print(key.decode(), 'first tag difference', difference,
                  '\n  native:',left[max(0,difference-4):difference+8],
                  '\n  pure:  ',right[max(0,difference-4):difference+8])
        else:
            print(key.decode(), '\n  native:', repr(expected.get(key))[:600], '\n  pure:  ', repr(actual.get(key))[:600])
    print(f'LPeg differential: {len(expected)-len(failures)}/{len(expected)} match, {len(names)} bundled lexers')
    raise SystemExit(bool(failures))


if __name__ == '__main__':
    main()
