#!/usr/bin/env python3
"""Build a self-contained arm64 release from committed vis source and pinned inputs."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
CACHE = REPO / '.package-cache'
DIST = REPO / 'dist'
MINIMUM = '13.5'
INPUTS = {
    'lua-5.5.1.tar.gz': (
        'https://www.lua.org/ftp/lua-5.5.1.tar.gz',
        '1c4b4068d67061f2a2231ad2b5422e77acea1487ea9890f6320af614f4373dce'),
    'lpeg-1.1.0.tar.gz': (
        'https://www.inf.puc-rio.br/~roberto/lpeg/lpeg-1.1.0.tar.gz',
        '4b155d67d2246c1ffa7ad7bc466c1ea899bbc40fef0257cc9c03cecbaed4352a'),
    'vis-lspc-4ae96a9.tar.gz': (
        'https://codeberg.org/muhq/vis-lspc/archive/4ae96a995a77e45231300da7e1b47ccd2d6fd9ca.tar.gz',
        'a3a1d97eafc341c0bb8ff5f9da65ab0df809e33a824c4d6b6f0e71cf5bed4b9d'),
    'dkjson-2.11.lua': (
        'https://dkolf.de/dkjson-lua/dkjson-2.11.lua',
        '197cb50834c642f84b4cf99fe724932c50e6d9c92faec7ad89aa25e91df4d481'),
    'node-v24.22.1-darwin-arm64.tar.gz': (
        'https://nodejs.org/dist/v24.22.1/node-v24.22.1-darwin-arm64.tar.gz',
        '89c682593d76cd44233fa53b1563264e7de9cbb189cad169135c5b4fadfbc0a7'),
    'lua-language-server-3.19.2-darwin-arm64.tar.gz': (
        'https://github.com/LuaLS/lua-language-server/releases/download/3.19.2/lua-language-server-3.19.2-darwin-arm64.tar.gz',
        '79149584320a23bd5fcd5bc31f47a1975acd8a4560a0e89036bb1016fa61687f'),
}


def run(*args, cwd=None, capture=False):
    return subprocess.check_output(args, cwd=cwd, text=True).strip() if capture else subprocess.run(args, cwd=cwd, check=True)


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def fetch(name, url, expected):
    target = CACHE / name
    if not target.exists() or digest(target) != expected:
        temporary = target.with_suffix(target.suffix + '.partial')
        run('curl', '--fail', '--location', '--retry', '3', '--silent', '--show-error', url, '-o', str(temporary))
        if digest(temporary) != expected:
            temporary.unlink()
            raise RuntimeError(f'Checksum mismatch: {name}')
        temporary.replace(target)
    return target


def extract(archive, destination):
    destination.mkdir(parents=True, exist_ok=True)
    # All archives have pinned checksums. The data filter also rejects links
    # escaping the extraction directory and unsafe member names.
    with tarfile.open(archive) as tar:
        tar.extractall(destination, filter='data')


def script(path, content):
    path.write_text('#!/bin/bash\nset -euo pipefail\n' + content)
    path.chmod(0o755)


def main():
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise SystemExit('Build on native Apple Silicon macOS with Xcode Command Line Tools.')
    for tool in ('cc', 'make', 'git', 'npm', 'codesign', 'xcrun'):
        if not shutil.which(tool):
            raise SystemExit(f'Missing build tool: {tool}')
    CACHE.mkdir(exist_ok=True)
    DIST.mkdir(exist_ok=True)
    inputs = {name: fetch(name, *metadata) for name, metadata in INPUTS.items()}
    commit = run('git', 'rev-parse', 'HEAD', cwd=REPO, capture=True)
    version = run('git', 'describe', '--always', cwd=REPO, capture=True) + '-quick1'
    name = 'vis-' + version + '-macos-arm64'
    with tempfile.TemporaryDirectory(prefix='vis-quick-build-') as temporary:
        work = Path(temporary)
        bundle = work / name
        payload = bundle / 'payload'
        for directory in ('bin', 'runtime', 'config', 'tools/bin', 'lsp', 'licenses', 'sources'):
            (payload / directory).mkdir(parents=True)
        archive = payload / 'sources/vis.tar.gz'
        run('git', 'archive', '--format=tar.gz', '-o', str(archive), commit, cwd=REPO)
        source = work / 'vis'
        extract(archive, source)
        shutil.copyfile(source / 'config.def.h', source / 'config.h')
        extract(inputs['lua-5.5.1.tar.gz'], work)
        extract(inputs['lpeg-1.1.0.tar.gz'], work)
        lua = work / 'lua-5.5.1/src'
        lpeg = work / 'lpeg-1.1.0'
        flags = ['-O2', '-arch', 'arm64', '-mmacosx-version-min=' + MINIMUM]
        run('make', '-C', str(lua), '-j4', 'liblua.a', 'CC=cc -std=gnu99',
            'SYSCFLAGS=-DLUA_USE_MACOSX', 'MYCFLAGS=-mmacosx-version-min=' + MINIMUM)
        objects = []
        for stem in ('lpvm', 'lpcap', 'lptree', 'lpcode', 'lpprint', 'lpcset'):
            obj = lpeg / (stem + '.o')
            run('cc', *flags, '-std=c99', '-I' + str(lua), '-c', str(lpeg / (stem + '.c')), '-o', str(obj))
            objects.append(str(obj))
        run('ar', 'rcs', str(lpeg / 'liblpeg.a'), *objects)
        common = [*flags, '-std=c99', '-DNDEBUG', '-D_DARWIN_C_SOURCE', '-DVIS_EXPORT=static',
                  '-DVERSION="' + version + '"', '-DVIS_API=' + run('git', 'rev-list', '--count', commit, cwd=REPO, capture=True)]
        run('cc', *common, '-DCONFIG_LUA=1', '-DCONFIG_LPEG=1', '-DCONFIG_CURSES=1',
            '-DVIS_PATH=""', '-I' + str(lua), 'main.c', str(lpeg / 'liblpeg.a'), str(lua / 'liblua.a'),
            '-lncurses', '-lm', '-o', str(payload / 'bin/vis'), cwd=source)
        for helper in ('vis-menu', 'vis-digraph'):
            run('cc', *common, helper + '.c', '-o', str(payload / 'bin' / helper), cwd=source)
        for helper in ('vis-clipboard', 'vis-complete', 'vis-open'):
            shutil.copy2(source / helper, payload / 'bin' / helper)
        for binary in (payload / 'bin').iterdir():
            binary.chmod(0o755)
            if binary.name in ('vis', 'vis-menu', 'vis-digraph'):
                run('strip', str(binary))
                run('codesign', '--force', '--sign', '-', str(binary))
        for entry in ('vis.lua', 'vis-std.lua', 'lexer.lua', 'lexers', 'themes', 'plugins'):
            src = source / 'lua' / entry
            dst = payload / 'runtime' / entry
            shutil.copytree(src, dst) if src.is_dir() else shutil.copy2(src, dst)
        extract(inputs['vis-lspc-4ae96a9.tar.gz'], work / 'plugins')
        shutil.copytree(work / 'plugins/vis-lspc', payload / 'runtime/plugins/vis-lspc')
        shutil.copy2(inputs['dkjson-2.11.lua'], payload / 'runtime/dkjson.lua')
        extract(inputs['node-v24.22.1-darwin-arm64.tar.gz'], work / 'node')
        node = work / 'node/node-v24.22.1-darwin-arm64'
        (payload / 'node/bin').mkdir(parents=True)
        shutil.copy2(node / 'bin/node', payload / 'node/bin/node')
        shutil.copy2(node / 'LICENSE', payload / 'licenses/Node-LICENSE')
        extract(inputs['lua-language-server-3.19.2-darwin-arm64.tar.gz'], payload / 'lua-language-server')
        for filename in ('package.json', 'package-lock.json'):
            shutil.copy2(HERE / filename, payload / 'lsp' / filename)
        run('npm', 'ci', '--omit=dev', '--ignore-scripts', '--no-audit', '--no-fund', cwd=payload / 'lsp')
        tools = payload / 'tools/bin'
        (tools / 'node').symlink_to('../../node/bin/node')
        entrypoints = {
            'pyright-langserver': 'pyright/langserver.index.js',
            'typescript-language-server': 'typescript-language-server/lib/cli.mjs',
            'vscode-html-language-server': 'vscode-langservers-extracted/bin/vscode-html-language-server',
            'vscode-css-language-server': 'vscode-langservers-extracted/bin/vscode-css-language-server',
            'vscode-json-language-server': 'vscode-langservers-extracted/bin/vscode-json-language-server',
            'yaml-language-server': 'yaml-language-server/bin/yaml-language-server',
        }
        for executable, entrypoint in entrypoints.items():
            assert (payload / 'lsp/node_modules' / entrypoint).is_file(), entrypoint
            script(tools / executable,
                   'root=$(cd -- "$(dirname -- "$0")/../.." && pwd -P)\n'
                   f'exec "$root/node/bin/node" "$root/lsp/node_modules/{entrypoint}" "$@"\n')
        script(tools / 'lua-language-server',
               'root=$(cd -- "$(dirname -- "$0")/../.." && pwd -P)\n'
               'cache="${XDG_CACHE_HOME:-$HOME/.cache}/vis-quick/lua-language-server"\n'
               'exec "$root/lua-language-server/bin/lua-language-server" --logpath="$cache/log" --metapath="$cache/meta" "$@"\n')
        for src, dst in ((HERE / 'launch.sh', payload / 'launch.sh'),
                         (HERE / 'install.sh', bundle / 'install.sh'),
                         (HERE / 'visrc.lua', payload / 'config/visrc.lua'),
                         (HERE / 'README.md', bundle / 'README.md')):
            shutil.copy2(src, dst)
        for executable in (payload / 'launch.sh', bundle / 'install.sh'):
            executable.chmod(0o755)
        shutil.copy2(source / 'LICENSE', payload / 'licenses/vis-LICENSE')
        shutil.copy2(source / 'lua/lexers/LICENSE', payload / 'licenses/Scintillua-LICENSE')
        for filename in ('lua-5.5.1.tar.gz', 'lpeg-1.1.0.tar.gz', 'vis-lspc-4ae96a9.tar.gz'):
            shutil.copy2(inputs[filename], payload / 'sources' / filename)
        shutil.copytree(HERE, payload / 'sources/packaging', ignore=shutil.ignore_patterns('__pycache__', 'node_modules'))
        metadata = {'name': name, 'vis_commit': commit, 'minimum_macos': MINIMUM, 'arch': 'arm64',
                    'inputs': {key: {'url': url, 'sha256': sha} for key, (url, sha) in INPUTS.items()},
                    'npm': json.loads((HERE / 'package.json').read_text())['dependencies']}
        (bundle / 'MANIFEST.json').write_text(json.dumps(metadata, indent=2) + '\n')
        (bundle / 'VERSION').write_text(version + '\n')
        # Fail the build if any core binary acquired a Homebrew/user dependency.
        binaries = [payload / 'bin' / n for n in ('vis', 'vis-menu', 'vis-digraph')]
        binaries += [payload / 'node/bin/node', payload / 'lua-language-server/bin/lua-language-server']
        for binary in binaries:
            deps = run('otool', '-L', str(binary), capture=True).splitlines()[1:]
            if any(not line.strip().startswith(('/usr/lib/', '/System/Library/')) for line in deps):
                raise RuntimeError(f'Non-system dynamic dependency: {binary}: {deps}')
            if 'arm64' not in run('file', str(binary), capture=True):
                raise RuntimeError(f'Not arm64: {binary}')
            build_info = run('xcrun', 'vtool', '-show-build', str(binary), capture=True)
            minimums = re.findall(r'minos (\d+)\.(\d+)', build_info)
            if not minimums or any(tuple(map(int, value)) > (13, 5) for value in minimums):
                raise RuntimeError(f'Unsupported minimum macOS version: {binary}: {build_info}')
        checksums = []
        for entry in sorted(bundle.rglob('*')):
            if entry.is_file() and not entry.is_symlink():
                checksums.append(digest(entry) + '  ' + entry.relative_to(bundle).as_posix())
        (bundle / 'CHECKSUMS.sha256').write_text('\n'.join(checksums) + '\n')
        output = DIST / (name + '.tar.gz')
        run('tar', '-czf', str(output), '-C', str(work), name)
        output.with_suffix(output.suffix + '.sha256').write_text(digest(output) + '  ' + output.name + '\n')
        print(f'Created {output} ({output.stat().st_size / 1024**2:.1f} MiB)', flush=True)


if __name__ == '__main__':
    main()
