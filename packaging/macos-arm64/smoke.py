#!/usr/bin/env python3
"""Exercise the archive offline in an isolated home; never install into the real account."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import termios
import time

AUDIT = r'''
local lsp = require('plugins/vis-lspc')
local json = require('dkjson')
vis:command_register('quick-audit', function()
  local win = vis.win
  local state = {
    syntax = win.syntax, lexer = vis.lexers.load(win.syntax) ~= nil,
    theme = vis.lexers.STYLE_DEFAULT, line = win.selection.line,
    numbers = win.options.numbers, cursorline = win.options.cursorline,
    tabwidth = win.options.tabwidth, autoindent = vis.options.autoindent,
    json_unicode = json.decode([["\u4e2d\u6587"]]),
    did_open = lsp.open_files[win.file.path] ~= nil,
    initialized = false, diagnostics = 0,
  }
  local server = lsp.ls_map[win.syntax]
  local ls = server and lsp.running[server.name]
  if ls then state.initialized = ls.initialized; state.server = ls.name end
  local opened = lsp.open_files[win.file.path]
  if opened then
    for _, diagnostics in pairs(opened.diagnostics) do
      state.diagnostics = state.diagnostics + #diagnostics
    end
  end
  local f = assert(io.open(os.getenv('VIS_AUDIT_FILE'), 'w'))
  f:write(assert(json.encode(state)))
  f:close()
end)
'''

CASES = {
    'c': ('check.c', 'int answer(void) { return 42; }\nint main(void) { return answer(); }\nint issue = unknown_name;\n', True),
    'lua': ('check.lua', 'local n = 42\nprint(n + missing_name)\n', True),
    'python': ('check.py', 'n: int = "wrong"\nprint(missing_name)\n', True),
    'typescript': ('check.ts', 'const n: number = "wrong";\nconsole.log(missing_name);\n', True),
    'javascript': ('check.js', 'const n = 1;\nconsole.log(n);\n', False),
    'json': ('check.json', '{"a": }\n', True),
    'html': ('check.html', '<!doctype html><html><body>Hello</body></html>\n', False),
    'css': ('check.css', 'a { color: red; }\n', False),
    'yaml': ('check.yaml', 'a: [\n', True),
}


def run_installer(bundle, env, prefix, *flags, success=True):
    result = subprocess.run(['/bin/bash', str(bundle/'install.sh'), '--prefix', str(prefix), *flags],
                            env=env, capture_output=True, text=True, timeout=120)
    if (result.returncode == 0) != success:
        raise AssertionError(result.stdout + result.stderr)
    return result


def check_case(language, case, launcher, folder, env):
    filename, content, needs_diagnostics = case
    folder.mkdir()
    target = folder/filename
    target.write_text(content)
    if language in ('typescript', 'javascript'):
        (folder/'tsconfig.json').write_text('{"compilerOptions":{"strict":true,"allowJs":true},"include":["check.*"]}')
    audit = folder/'audit.json'
    child_env = dict(env, TERM='xterm-256color', VIS_AUDIT_FILE=str(audit))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
    proc = subprocess.Popen([str(launcher), str(target)], cwd=folder, env=child_env,
                            stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    transcript = bytearray()
    def drain(duration):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            ready, _, _ = select.select([master], [], [], min(.1, max(0, end-time.monotonic())))
            if ready:
                try:
                    transcript.extend(os.read(master, 65536))
                    (folder/'terminal.log').write_bytes(transcript)
                except OSError: break
    state = {}
    try:
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            drain(.4)
            if proc.poll() is not None: break
            os.write(master, b':quick-audit\r')
            drain(.15)
            if audit.exists():
                state = json.loads(audit.read_text())
                if state.get('initialized') and state.get('did_open') and (not needs_diagnostics or state['diagnostics']):
                    break
        assert state.get('initialized') and state.get('did_open'), state
        assert state['syntax'] == language and state['lexer'], state
        assert state['numbers'] and state['cursorline'] and state['autoindent'] and state['tabwidth'] == 4, state
        assert state['theme'] == 'fore:#d7d7d7,back:#3a3a3a' and state['json_unicode'] == '中文', state
        if needs_diagnostics: assert state['diagnostics'] > 0, state
        if language == 'c':
            os.write(master, b'2G0/answer\rll')
            drain(.3)
            os.write(master, b'gd')
            drain(.7)
            os.write(master, b':quick-audit\r')
            drain(.2)
            assert json.loads(audit.read_text())['line'] == 1, 'C definition jump failed'
            state['definition_jump'] = True
        return dict(passed=True, **state)
    finally:
        if proc.poll() is None:
            os.write(master, b'\x1b')
            drain(.15)
            os.write(master, b':q!\r')
            drain(.5)
        # Release the PTY before reaping: macOS can otherwise keep an exiting
        # terminal process blocked while closing its slave device.
        os.close(master)
        if proc.poll() is None: proc.terminate()
        try: proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=3)
        (folder/'terminal.log').write_bytes(transcript)


def main():
    if len(sys.argv) != 2: raise SystemExit('Usage: smoke.py dist/vis-…-macos-arm64.tar.gz')
    archive = Path(sys.argv[1]).resolve()
    report = archive.parent / (archive.name.removesuffix('.tar.gz') + '-smoke')
    report.mkdir(exist_ok=True)
    results = {}
    with tempfile.TemporaryDirectory(prefix='vis quick smoke ') as tmp:
        root = Path(tmp)
        with tarfile.open(archive) as tar: tar.extractall(root, filter='data')
        bundle = next(root.glob('vis-*-macos-arm64'))
        home = root/'Test User'
        home.mkdir()
        prefix = home/'Local Apps'
        config_dir = home/'Preferences/vis'
        config_dir.mkdir(parents=True)
        rc = home/'.zshrc'
        shell_target = home/'dotfiles/zshrc'
        shell_target.parent.mkdir()
        shell_original = '# preserve shell settings\nexport VIS_USER_SENTINEL=yes\n'
        shell_target.write_text(shell_original)
        rc.symlink_to(shell_target)
        old_config = '-- preserve editor settings\n'
        (config_dir/'visrc.lua').write_text(old_config)
        (prefix/'bin').mkdir(parents=True)
        old_executable = home/'existing-vis'
        old_executable.write_text('do not overwrite this target\n')
        (prefix/'bin/vis').symlink_to(old_executable)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(('XDG_', 'VIS_', 'LUA_', 'NVM_', 'NODE_')) and key != 'ZDOTDIR'}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(config_dir.parent),
                   PATH='/usr/bin:/bin:/usr/sbin:/sbin', SHELL='/bin/zsh')
        # A corrupt package must fail before replacing any existing user file.
        template = bundle/'payload/config/visrc.lua'
        original_template = template.read_bytes()
        template.write_bytes(original_template + b'\n-- corruption\n')
        run_installer(bundle, env, prefix, success=False)
        assert (config_dir/'visrc.lua').read_text() == old_config and rc.read_text() == shell_original
        template.write_bytes(original_template)
        install = run_installer(bundle, env, prefix)
        (report/'install.log').write_text(install.stdout + install.stderr)
        base = prefix/'share/vis-quick'
        first_backup = next((base/'backups').iterdir())
        assert (first_backup/'visrc.lua').read_text() == old_config
        assert (first_backup/'zshrc').read_text() == shell_original and not (first_backup/'zshrc').is_symlink()
        assert rc.is_symlink() and old_executable.read_text() == 'do not overwrite this target\n'
        assert (first_backup/'vis').is_symlink()
        run_installer(bundle, env, prefix)
        assert len(list((base/'backups').iterdir())) == 2
        assert rc.read_text().count('# >>> vis-quick >>>') == 1 and shell_original in rc.read_text()
        before_no_shell = rc.read_bytes()
        run_installer(bundle, env, prefix, '--no-shell')
        assert rc.read_bytes() == before_no_shell
        result = subprocess.run(['/bin/zsh', '-ic', '[[ $VIS_USER_SENTINEL = yes ]] && vis -v && vise -v'], env=env,
                                capture_output=True, text=True)
        assert result.returncode == 0 and result.stdout.count('+lua') == 2, result.stdout + result.stderr
        results['installer'] = {'passed': True, 'checksum_rejection': True, 'backups': True,
                                'symlink_targets_preserved': True, 'repeat_install': True,
                                'no_shell': True, 'aliases': True, 'spaces_in_paths': True}
        audit_module = config_dir/'audit.lua'
        audit_module.write_text(AUDIT)
        with (config_dir/'visrc.lua').open('a') as f:
            f.write('\ndofile(' + json.dumps(str(audit_module)) + ')\n')
        for language, case in CASES.items():
            folder = root/language
            try:
                results[language] = check_case(language, case, prefix/'bin/vise', folder, env)
            except Exception as error:
                results[language] = {'passed': False, 'error': str(error)}
            if folder.exists():
                target = report/language
                shutil.copytree(folder, target, dirs_exist_ok=True)
            (report/'results.json').write_text(json.dumps(results, indent=2, ensure_ascii=False) + '\n')
            print(language + ': ' + ('PASS' if results[language]['passed'] else 'FAIL ' + results[language]['error']), flush=True)
    assert all(result['passed'] for result in results.values()), f'See {report}/results.json'
    print(f'PASS: isolated offline install and {len(CASES)} language sessions. Report: {report}', flush=True)


if __name__ == '__main__': main()
