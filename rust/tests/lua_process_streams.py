#!/usr/bin/env python3
"""Verify communicate FILE* contracts against actual C and Rust Lua CLIs.

Build vis-rs with --features lua first. Each host must write explicit success
markers after its assertions and after the child receives a partial write.
The C oracle runs first; successful process exit alone never counts as proof.
"""

import argparse
import errno
import fcntl
import os
from pathlib import Path
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]

CONTRACT = r'''
local function mark(text)
  local out=assert(io.open('success','a'));out:write(text,'\n');out:close()
end
local partial
local received=''
vis.events={
  init=function()
    local returned=table.pack(vis:communicate('file-api','cat >/dev/null'))
    assert(returned.n==1)
    local stream=assert(returned[1])
    local numeric=assert(vis:communicate(123,456))
    assert(numeric:close())
    assert(not pcall(vis.communicate,vis))
    assert(not pcall(vis.communicate,vis,{},'true'))
    local input,output=io.input(),io.output()
    assert(io.type(stream)=='file')
    assert(io.input(stream)==stream and io.input()==stream)
    assert(io.output()==output)
    local result,message,code=io.read()
    assert(result==nil and type(message)=='string' and code==9)
    result,message,code=stream:read('*l',{})
    assert(result==nil and code==9)
    assert(not pcall(stream.read,stream,'*a',{}))
    assert(not pcall(stream.read,stream,'invalid'))
    assert(not pcall(stream.read,stream,true))
    assert(not pcall(stream.read,stream,-1))
    local iterator=io.lines()
    assert(type(iterator)=='function' and not pcall(iterator))
    iterator=stream:lines('invalid')
    assert(type(iterator)=='function' and not pcall(iterator))
    result,message,code=stream:seek()
    assert(result==nil and type(message)=='string' and code==29)
    assert(not pcall(stream.seek,stream,'invalid'))
    assert(not pcall(stream.seek,stream,'cur','invalid'))
    assert(not pcall(stream.seek,stream,'cur',1.5))
    assert(stream:setvbuf('no',-1))
    assert(stream:setvbuf('full',-1)==nil)
    assert(stream:setvbuf('line',-1)==nil)
    assert(stream:setvbuf('no','17'))
    assert(stream:setvbuf('full',0))
    assert(not pcall(stream.setvbuf,stream,'invalid'))
    assert(not pcall(stream.setvbuf,stream,'full',1.5))
    assert(stream:write()==stream and stream:write('bin\0',123)==stream)
    assert(not pcall(stream.write,stream,{}))
    assert(stream:flush('ignored'))
    local native=assert(io.tmpfile())
    assert(native:write('native') and native:seek('set',0)==0)
    assert(io.input(native)==native and io.read('*a')=='native')
    assert(io.input(stream)==stream and native:close())
    assert(io.output(stream)==stream and io.write('global')==stream)
    assert(io.flush() and io.close())
    assert(io.output(output)==output)
    assert(io.type(stream)=='closed file' and io.input()==stream)
    assert(not pcall(io.read) and not pcall(io.input,stream))
    assert(not pcall(stream.close,stream))
    assert(not pcall(stream.write,stream,'closed'))
    assert(not pcall(stream.setvbuf,stream,'no'))
    assert(io.input(input)==input)
    mark('STREAM_FILE_API_COMPLETE')
  end,
  start=function()
    partial=assert(vis:communicate('partial','head -c 6'))
    assert(partial:setvbuf('full',8192))
    assert(not pcall(partial.write,partial,'prefix',{}))
    assert(partial:flush())
  end,
  process_response=function(name,kind,code,data)
    if name=='partial' and kind=='STDOUT' then
      received=received..data
      if #received>=6 then
        assert(received=='prefix')
        mark('PARTIAL_WRITE_COMPLETE')
        if io.type(partial)=='file' then partial:close() end
        vis:exit(0)
      end
    end
  end,
}
'''


def verify(binary, directory):
    directory.mkdir()
    (directory / "visrc.lua").write_text(CONTRACT)
    source = directory / "input"
    source.write_bytes(b"process stream fixture\n")
    master, slave = os.openpty()
    process = None
    transcript = bytearray()
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        process = subprocess.Popen(
            [str(binary), str(source)], cwd=directory,
            env={**os.environ, "VIS_PATH": str(directory), "TERM": "xterm"},
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
        )
        deadline = time.monotonic() + 10
        while process.poll() is None:
            if time.monotonic() >= deadline:
                raise AssertionError(f"stream oracle timed out: {bytes(transcript[-1500:])!r}")
            if select.select([master], [], [], 0.05)[0]:
                try:
                    transcript.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
        assert process.returncode == 0, (process.returncode, bytes(transcript[-1500:]))
        marker = directory / "success"
        assert marker.is_file(), f"stream assertions did not finish: {bytes(transcript[-1500:])!r}"
        assert marker.read_text() == "STREAM_FILE_API_COMPLETE\nPARTIAL_WRITE_COMPLETE\n", marker.read_text()
    finally:
        (directory / "terminal.log").write_bytes(transcript)
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--rust-vis", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    if args.artifacts is not None:
        args.artifacts.mkdir(parents=True, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix="stream-contract-", dir=args.artifacts)).resolve()
        print(f"Artifacts: {directory}")
        verify(args.c_vis.resolve(), directory / "c")
        print("PASS native C success-marker stream oracle")
        verify(args.rust_vis.resolve(), directory / "rust")
    else:
        with tempfile.TemporaryDirectory(prefix="vis-process-streams-") as temporary:
            directory = Path(temporary).resolve()
            verify(args.c_vis.resolve(), directory / "c")
            print("PASS native C success-marker stream oracle")
            verify(args.rust_vis.resolve(), directory / "rust")
    print("PASS Rust writable stream IO routing, validation, and partial-write contract")


if __name__ == "__main__":
    main()
