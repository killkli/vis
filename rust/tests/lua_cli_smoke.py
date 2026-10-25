#!/usr/bin/env python3
"""Exercise the actual optional-Lua CLI, including headless startup and PTYs.

Build with --features lua first. Configurations and binary snapshots live only
in a temporary directory; no existing user configuration or fixture is changed.
"""

import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

from terminal_smoke import ROOT, Session
from terminal_views import cursor, escape, raw, resize, settle


def configuration(directory, name, source):
    path = directory / name
    path.mkdir()
    (path / "visrc.lua").write_text(source)
    return path


def run_headless(binary, config, arguments=(), data=b"", cwd=None, extra_env=None):
    return subprocess.run(
        [str(binary), *map(str, arguments)], input=data, capture_output=True,
        env={**os.environ, "VIS_PATH": str(config), **(extra_env or {})},
        cwd=cwd, timeout=15,
    )


class LuaSession(Session):
    def __init__(self, binary, path, config, rows=24, columns=80):
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.process = subprocess.Popen(
            [str(binary), str(path)], stdin=self.slave, stdout=self.slave,
            stderr=self.slave, start_new_session=True,
            env={**os.environ, "TERM": "xterm-256color", "VIS_PATH": str(config)},
        )


class ControllingLuaSession(LuaSession):
    """A supervisor keeps the controlling session alive through editor exit.

    This lets it inspect restored termios and foreground PGID before the kernel
    revokes the controlling terminal when the session leader finally exits.
    """
    def __init__(self, binary, path, config, audit):
        self.master, self.slave = os.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        self.original = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.audit = audit
        supervisor = r'''
import fcntl,json,os,subprocess,sys,termios,time
os.setsid()
fcntl.ioctl(0,termios.TIOCSCTTY,0)
child=subprocess.Popen(sys.argv[1:3])
def snapshot(code):
    attributes=termios.tcgetattr(0)
    attributes[6]=[value if isinstance(value,int) else value[0] for value in attributes[6]]
    with open(sys.argv[3]+'.tmp','w') as out:
        json.dump(dict(pid=child.pid,code=code,attributes=attributes,foreground=os.tcgetpgrp(0),group=os.getpgrp()),out)
    os.replace(sys.argv[3]+'.tmp',sys.argv[3])
while child.poll() is None:
    snapshot(None)
    time.sleep(0.02)
code=child.wait()
snapshot(code)
sys.exit(code)
'''
        self.process = subprocess.Popen(
            [sys.executable, "-c", supervisor, str(binary), str(path), str(audit)],
            stdin=self.slave, stdout=self.slave, stderr=self.slave,
            env={**os.environ, "TERM": "xterm-256color", "VIS_PATH": str(config)},
        )

    def assert_restored(self):
        result = json.loads(self.audit.read_text())
        expected = self.original.copy()
        expected[6] = [value if isinstance(value, int) else value[0] for value in expected[6]]
        pending = getattr(termios, "PENDIN", 0)
        expected[3] &= ~pending
        result["attributes"][3] &= ~pending
        assert result["attributes"] == expected, result
        assert result["foreground"] == result["group"], result

    def assert_editor_raw(self):
        # tcgetpgrp is only valid from within the controlling session on macOS;
        # the supervisor records this state while retaining session ownership.
        deadline = time.monotonic() + 3
        while True:
            result = json.loads(self.audit.read_text())
            flags = result["attributes"][3]
            if result["foreground"] == result["group"] and not flags & (termios.ICANON | termios.ISIG):
                return
            assert time.monotonic() < deadline, result
            self.read(0.03)

    def close(self):
        if self.process.poll() is None:
            # A failing read test must not leave the editor or its foreground
            # child alive after killing only the supervisor.
            try:
                foreground = json.loads(self.audit.read_text())["foreground"]
                if foreground > 0 and foreground not in (self.process.pid, os.getpgrp()):
                    os.killpg(foreground, signal.SIGKILL)
            except (OSError, ProcessLookupError):
                pass
            try:
                pid = json.loads(self.audit.read_text()).get("pid")
                if pid:
                    os.kill(pid, signal.SIGTERM)
                    self.process.wait(timeout=2)
            except (OSError, ValueError, subprocess.TimeoutExpired):
                pass
        super().close()


def wait_for(session, needle):
    deadline = time.monotonic() + 10
    while needle not in session.output:
        session.read()
        assert session.process.poll() is None, bytes(session.output[-2000:])
        assert time.monotonic() < deadline, bytes(session.output[-2000:])


def latest_frame(session):
    frame = bytes(session.output).rsplit(b"\x1b[?25l", 1)[-1]
    return re.sub(rb"\x1b\[[0-9;]*m", b"", frame)


def run(binary, core_binary=None):
    with tempfile.TemporaryDirectory(prefix="vis-lua-cli-") as temporary:
        directory = Path(temporary).resolve()
        executable = directory / "vis-rs"
        shutil.copy2(binary, executable)
        version = subprocess.run([str(executable), "-v"], capture_output=True, timeout=5)
        assert version.returncode == 0 and b"+lua" in version.stdout, version.stdout

        audit = directory / "events"
        source = r'''
local function log(text) local out=assert(io.open(AUDIT_PATH,'a'));out:write(text,'\n');out:close() end
vis.events={
  init=function()
    assert(vis.win==nil);log('init')
    assert(vis:command_register('cli',function(argv,force,win)
      assert(win==vis.win);log('cmd:'..argv[1]..':'..win.file.name);return true
    end))
  end,
  file_open=function(file) log('file:'..file.name) end,
  win_open=function(win) log('win:'..win.file.name) end,
  start=function() log('start');vis:exit(7) end,
  quit=function() log('quit') end,
}
'''.replace("AUDIT_PATH", json.dumps(str(audit)))
        config = configuration(directory, "events-config", source)
        a, b = directory / "a", directory / "b"
        a.write_bytes(b"one\ntwo\n")
        b.write_bytes(b"second\n")
        result = run_headless(executable, config, ["+cli one", a, "+cli two", b])
        assert result.returncode == 7, result.stderr
        assert not result.stdout
        observed = audit.read_text().splitlines()
        assert observed == ["init", f"file:{a}", f"win:{a}", f"cmd:one:{a}", f"file:{b}", f"win:{b}", f"cmd:two:{b}", "start", "quit"], observed
        print("PASS INIT/files/+commands/START/QUIT ordering and explicit Lua exit status without TTY")

        config = configuration(directory, "early-config", "vis.events={init=function()assert(vis.win==nil);vis:exit(9)end}")
        result = run_headless(executable, config, [directory / "must-not-open"], b"ignored stdin")
        assert result.returncode == 9 and not result.stdout, (result.returncode, result.stderr)
        print("PASS INIT exit bypasses file opening and terminal initialization")

        config = configuration(directory, "binary-config", r'''
vis.events={win_open=function(win)
  local content=win.file:content(0,win.file.size)
  assert(content==string.char(0,255)..'A\n')
  win.file:insert(win.file.size,'Z')
  assert(vis:command('wq'))
end}
''')
        result = run_headless(executable, config, ["-"], b"\0\xffA\n")
        assert result.returncode == 0 and result.stdout == b"\0\xffA\nZ", (result.returncode, result.stdout, result.stderr)
        print("PASS Lua binary stdin/file API and reentrant wq to stdout")

        config = configuration(directory, "readonly-config", r'''
vis.events={win_open=function(win)
  win.file:insert(0,'X')
  assert(vis:command('w')) -- A known command is accepted even if its handler fails.
  local disk=assert(io.open(win.file.path,'rb'))
  assert(disk:read('*a')=='one\ntwo\n');disk:close()
  assert(vis:command('w!'))
  vis:exit(0)
end}
''')
        result = run_headless(executable, config, ["-R", a])
        assert result.returncode == 0 and a.read_bytes() == b"Xone\ntwo\n", (result.returncode, result.stderr)
        print("PASS read-only CLI state reaches Lua save plans and force override")

        forbidden = directory / "cwd"
        forbidden.mkdir()
        (forbidden / "vis_port_cwd_fixture.lua").write_text("return true")
        config = configuration(directory, "paths-config", "assert(not pcall(require,'vis_port_cwd_fixture'));vis.events={start=function()vis:exit(0)end}")
        result = run_headless(executable, config, [], cwd=forbidden, extra_env={"LUA_PATH": "./?.lua"})
        assert result.returncode == 0, result.stderr
        print("PASS VIS_PATH precedence and current-directory package path stripping")

        config = configuration(directory, "styles-config", r'''
local style_id
vis.events={
  init=function() style_id=assert(vis.ui:style_push());assert(vis.ui:style_define(style_id,'fore:#ff0000')) end,
  win_highlight=function(win)
    local view=win.viewport
    assert(view.width>0 and view.height>0)
    assert(view.bytes.start<=view.bytes.finish and view.bytes.finish<=win.file.size)
    win:style(style_id,0,1)
  end,
  win_status=function(win)win:status('LUA_STATUS '..string.char(27)..'[2J','RIGHT')end,
}
''')
        styled = directory / "styled"
        styled.write_bytes(b"abc\n")
        session = LuaSession(executable, styled, config)
        try:
            session.ready()
            wait_for(session, b"LUA_STATUS ^[[2J")
            assert b"38;2;255;0;0" in session.output, bytes(session.output[-2000:])
            assert session.output.count(b"\x1b[2J") == 1, "Lua status injected terminal control"
            session.send(b":q\r")
            session.finish()
        finally:
            session.close()
        print("PASS Lua measured viewport, highlight SGR and safe custom status rendering")

        canonical = directory / "canonical"
        prefix = shlex.quote(sys.executable) + " -c " + shlex.quote("import termios;print(1 if termios.tcgetattr(0)[3]&termios.ICANON else 0)") + " < "
        source = r'''
vis.events={init=function()
  assert(vis:command_register('full',function(argv)
    local status,out,err=vis:pipe(COMMAND_PREFIX..string.format('%q',argv[1]),true)
    assert(status==0,err);assert(out=='1\n',out)
    local file=assert(io.open(AUDIT_PATH,'w'));file:write('canonical');file:close()
    vis:info('fullscreen done');return true
  end))
end}
'''.replace("COMMAND_PREFIX", json.dumps(prefix)).replace("AUDIT_PATH", json.dumps(str(canonical)))
        config = configuration(directory, "fullscreen-config", source)
        session = LuaSession(executable, styled, config)
        try:
            session.ready()
            tty = os.ttyname(session.slave)
            # Queued :q tests preservation of decoder typeahead across resume.
            session.send(f":full {tty}\r:q\r".encode())
            session.finish()
            assert canonical.read_text() == "canonical"
            assert session.output.count(b"\x1b[?1049h") >= 2
            assert b"fullscreen done" in session.output
        finally:
            session.close()
        print("PASS fullscreen Lua child sees canonical TTY, resumes safely and preserves typeahead")

        geometry_audit = directory / "prompt-geometry"
        config = configuration(directory, "prompt-config", r'''
require('vis')
local function geometry()
  local win=vis.win
  local out=assert(io.open(AUDIT_PATH,'a'))
  out:write(string.format('%d,%d,%d,%s\n',win.width,win.height,win.viewport.height,tostring(win.file.name==nil)))
  out:close()
end
assert(vis:map(vis.modes.NORMAL,'<F2>',geometry))
assert(vis:map(vis.modes.INSERT,'<F2>',geometry))
'''.replace("AUDIT_PATH", json.dumps(str(geometry_audit))))
        session = LuaSession(executable, styled, config)
        try:
            session.ready()
            settle(session)
            raw(session, b":pwd\r")
            settle(session)
            raw(session, b":draft")
            settle(session)
            assert cursor(session) == (24, 7), cursor(session)
            escape(session)  # Insert -> Normal in the actual prompt Text.
            raw(session, b"0")
            settle(session)
            assert cursor(session) == (24, 1), cursor(session)
            raw(session, b"\x1bOQ")
            settle(session)
            escape(session)  # Normal Escape closes the internal prompt window.
            raw(session, b":")
            settle(session)
            raw(session, b"\x1b[A")  # First Up expands the real history window.
            settle(session)
            assert cursor(session)[0] >= 13, cursor(session)
            frame = latest_frame(session)
            assert b":pwd" in frame and b":draft" in frame, frame
            raw(session, b"\x1bOQ")
            settle(session)
            escape(session)
            escape(session)
            raw(session, b":vsplit\r")
            settle(session)
            raw(session, b":split-prompt")
            settle(session)
            assert cursor(session) == (24, 14), cursor(session)
            frame = latest_frame(session)
            assert b"\x1b[23;40H|" in frame and b"\x1b[24;40H|" not in frame, frame
            raw(session, b"\x1bOQ")
            settle(session)
            assert geometry_audit.read_text().splitlines() == ["80,1,1,true", "80,12,12,true", "80,1,1,true"], geometry_audit.read_text()
            escape(session)
            escape(session)
            raw(session, b":q!\r:q!\r")
            session.finish()
        finally:
            session.close()
        print("PASS real prompt Normal cursor, expanded editable history and bottom full-width prompt below splits")

        config = configuration(directory, "async-config", r'''
require('vis')
local seen=false
vis.events.subscribe(vis.events.START,function()
  vis:communicate('cli_async','sleep 0.1; printf callback')
end)
vis.events.subscribe(vis.events.PROCESS_RESPONSE,function(name,event,code,msg)
  if event=='STDOUT' then assert(msg=='callback');seen=true end
  if event=='EXIT' then assert(code==0 and seen);vis:info('ASYNC_CALLBACK') end
end)
''')
        session = LuaSession(executable, styled, config)
        try:
            session.ready()
            wait_for(session, b"ASYNC_CALLBACK")
            raw(session, b":q\r")
            session.finish()
            assert b"ASYNC_CALLBACK" in session.output, bytes(session.output[-2000:])
        finally:
            session.close()
        print("PASS asynchronous process callbacks progress and redraw without terminal input")

        config = configuration(directory, "interrupt-config", "vis.events={start=function()vis:exit(0)end};while true do end")
        process = subprocess.Popen(
            [str(executable), str(styled)], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={**os.environ, "VIS_PATH": str(config)},
        )
        try:
            time.sleep(0.15)
            process.send_signal(signal.SIGINT)
            output, errors = process.communicate(timeout=10)
            assert process.returncode == 0 and not output, (process.returncode, output, errors)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        print("PASS SIGINT interrupts configuration Lua before files and terminal initialization")

        for external_signal in (False, True):
            cancellation = directory / ("cancel-signal" if external_signal else "cancel-key")
            config = configuration(directory, cancellation.name + "-config", r'''
require('vis')
vis.events.subscribe(vis.events.START,function()
  vis:communicate('cancel','printf ASYNC_READY; sleep 30')
end)
vis.events.subscribe(vis.events.PROCESS_RESPONSE,function(name,event,code,msg)
  if event=='STDOUT' then vis:info(msg) end
  if event=='EXIT' or event=='SIGNAL' then
    local out=assert(io.open(AUDIT_PATH,'w'));out:write(event);out:close()
    vis:exit(0)
  end
end)
'''.replace("AUDIT_PATH", json.dumps(str(cancellation))))
            session = LuaSession(executable, styled, config)
            try:
                session.ready()
                wait_for(session, b"ASYNC_READY")
                if external_signal:
                    session.process.send_signal(signal.SIGINT)
                else:
                    raw(session, b"\x03")
                session.finish()
                assert cancellation.read_text() == "SIGNAL", cancellation.read_text()
            finally:
                session.close()
        print("PASS terminal Ctrl-C and SIGINT cancel asynchronous process groups and deliver terminal callbacks")

        for cancel in (False, True):
            # This child must actually read a real controlling terminal. Merely
            # inspecting canonical mode cannot expose a background SIGTTIN stop.
            child = "import os,termios; tty=os.open('/dev/tty',os.O_RDWR); assert(os.tcgetpgrp(tty)==os.getpgrp()); assert(termios.tcgetattr(tty)[3]&termios.ICANON); os.write(tty,b'JOB_READY\\n'); line=os.read(tty,4096); assert(line==b'hello\\n'); print('accepted')"
            command = shlex.quote(sys.executable) + " -c " + shlex.quote(child)
            source = r'''
require('vis')
assert(vis:command_register('job',function()
  local status,out,err=vis:pipe(CHILD_COMMAND,true)
  if CANCEL then assert(status~=0);vis:info('JOB_CANCELLED')
  else assert(status==0,err);assert(out=='accepted\n',out);vis:info('JOB_DONE') end
  return true
end))
'''.replace("CHILD_COMMAND", json.dumps(command)).replace("if CANCEL then", "if true then" if cancel else "if false then")
            config = configuration(directory, "job-cancel" if cancel else "job-read", source)
            session = ControllingLuaSession(executable, styled, config, directory / ("job-cancel-audit" if cancel else "job-read-audit"))
            try:
                session.ready()
                raw(session, b":job\r")
                wait_for(session, b"JOB_READY")
                raw(session, b"\x03" if cancel else b"hello\n")
                wait_for(session, b"JOB_CANCELLED" if cancel else b"JOB_DONE")
                session.assert_editor_raw()
                raw(session, b":q\r")
                session.finish()
            finally:
                session.close()
        print("PASS actual controlling-TTY fullscreen reads/cancellation and foreground/raw/final termios restoration")

        source = r'''
require('vis')
local status,out,err=vis:pipe(CHILD_COMMAND,true)
assert(status==0,err);assert(out=='accepted\n',out)
vis.events.subscribe(vis.events.START,function()vis:info('JOB_CONFIG_DONE')end)
'''.replace("CHILD_COMMAND", json.dumps(command))
        config = configuration(directory, "config-job-read", source)
        session = ControllingLuaSession(executable, styled, config, directory / "config-job-audit")
        try:
            wait_for(session, b"JOB_READY")
            raw(session, b"hello\n")
            session.ready()
            wait_for(session, b"JOB_CONFIG_DONE")
            session.assert_editor_raw()
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()
        print("PASS fullscreen controlling-TTY reads during configuration before terminal initialization")

        child = "import os,termios; tty=os.open('/dev/tty',os.O_RDWR); assert(os.tcgetpgrp(tty)==os.getpgrp()); assert(termios.tcgetattr(tty)[3]&termios.ICANON and termios.tcgetattr(tty)[3]&termios.ECHO); os.write(tty,b'OVERLAY_READY'); assert(os.read(tty,4096)==b'x\\n'); print('overlay')"
        command = shlex.quote(sys.executable) + " -c " + shlex.quote(child)
        source = r'''
require('vis')
assert(vis:command_register('overlay',function()
  local status,out,err=vis:pipe(CHILD_COMMAND,false)
  assert(status==0,err);assert(out=='overlay\n',out);vis:info('OVERLAY_DONE');return true
end))
'''.replace("CHILD_COMMAND", json.dumps(command))
        config = configuration(directory, "overlay-job-read", source)
        session = ControllingLuaSession(executable, styled, config, directory / "overlay-job-audit")
        try:
            session.ready()
            raw(session, b":overlay\r")
            wait_for(session, b"OVERLAY_READY")
            raw(session, b"x\n")
            wait_for(session, b"OVERLAY_DONE")
            session.assert_editor_raw()
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()
        print("PASS non-fullscreen overlay reads its controlling-TTY foreground and restores editor ownership")

        child = "import os,termios,time; tty=os.open('/dev/tty',os.O_RDWR); assert(os.tcgetpgrp(tty)==os.getpgrp()); assert(termios.tcgetattr(tty)[3]&termios.ISIG and termios.tcgetattr(tty)[3]&termios.ICANON and termios.tcgetattr(tty)[3]&termios.ECHO); os.write(tty,b'SLOW_READY'); time.sleep(30)"
        command = shlex.quote(sys.executable) + " -c " + shlex.quote(child)
        source = r'''
require('vis')
assert(vis:command_register('slow',function()
  local status=vis:pipe(CHILD_COMMAND,false)
  assert(status~=0);vis:info('SLOW_CANCELLED');return true
end))
'''.replace("CHILD_COMMAND", json.dumps(command))
        config = configuration(directory, "slow-overlay-cancel", source)
        session = ControllingLuaSession(executable, styled, config, directory / "slow-overlay-audit")
        try:
            session.ready()
            raw(session, b":slow\r")
            wait_for(session, b"SLOW_READY")
            raw(session, b"\x03")
            wait_for(session, b"SLOW_CANCELLED")
            session.assert_editor_raw()
            raw(session, b":q\r")
            session.finish()
        finally:
            session.close()
        print("PASS physical Ctrl-C cancels synchronous non-fullscreen children and restores raw signal handling")

        if core_binary is not None:
            core = directory / "vis-core"
            shutil.copy2(core_binary, core)
            session = ControllingLuaSession(core, styled, config, directory / "core-sam-audit")
            before = styled.read_bytes()
            try:
                session.ready()
                # The sleeper itself announces readiness after the shell has
                # spawned it, avoiding an interrupt racing the shell's fork.
                child = "import os,time; os.write(os.open('/dev/tty',os.O_WRONLY),bytes([83,65,77,95,82,69,65,68,89])); time.sleep(20)"
                command = shlex.quote(sys.executable) + " -c " + shlex.quote(child)
                raw(session, (":1|" + command + "\r").encode())
                wait_for(session, b"SAM_READY")
                raw(session, b"\x03")
                session.assert_editor_raw()
                escape(session)
                escape(session)
                raw(session, b":wq\r")
                session.finish()
                assert styled.read_bytes() == before
            finally:
                session.close()
            print("PASS default core Sam physical Ctrl-C, canceled edit preservation and foreground/termios restoration")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    parser.add_argument("--core-binary", type=Path, help="also test default-feature native Sam job control")
    arguments = parser.parse_args()
    run(arguments.binary.resolve(), arguments.core_binary.resolve() if arguments.core_binary else None)
