#!/usr/bin/env python3
"""Compare real C VT100 and Rust callback painting through headless PTYs.

Requires the C compiler and Lua development flags used by the original editor.
The C binary is built entirely in a temporary directory. No GUI is involved.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import time
import unicodedata

from lua_cli_smoke import LuaSession, configuration
from terminal_smoke import ROOT
from terminal_views import settle


def screen(transcript, rows=24, columns=80):
    cells = [[(" ", 0, None, None) for _ in range(columns)] for _ in range(rows)]
    row = column = attributes = 0
    foreground = background = None
    position = 0
    flags = {1: 8, 2: 32, 3: 16, 4: 1, 5: 4, 7: 2}
    while position < len(transcript):
        if transcript[position:position + 2] == b"\x1b[":
            match = re.match(rb"\x1b\[([0-9;?]*)([@-~])", transcript[position:])
            assert match, transcript[position:position + 40]
            values, command = match.groups()
            parameters = [int(value) if value else 0 for value in values.lstrip(b"?").split(b";")]
            if command in (b"H", b"f"):
                row = max(0, parameters[0] - 1)
                column = max(0, (parameters[1] if len(parameters) > 1 else 1) - 1)
            elif command == b"J":
                if parameters[0] == 2:
                    cells = [[(" ", attributes, foreground, background) for _ in range(columns)] for _ in range(rows)]
                elif parameters[0] == 0:
                    for clear_row in range(row, rows):
                        for clear_column in range(column if clear_row == row else 0, columns):
                            cells[clear_row][clear_column] = (" ", attributes, foreground, background)
                else:
                    raise AssertionError((values, command))
            elif command == b"m":
                index = 0
                while index < len(parameters):
                    value = parameters[index]
                    if value == 0:
                        attributes, foreground, background = 0, None, None
                    elif value in flags:
                        attributes |= flags[value]
                    elif value in (22, 23, 24, 25, 27):
                        attributes &= ~{22: 8 | 32, 23: 16, 24: 1, 25: 4, 27: 2}[value]
                    elif 30 <= value <= 37:
                        foreground = (0, value - 30)
                    elif 40 <= value <= 47:
                        background = (0, value - 40)
                    elif value in (39, 49):
                        if value == 39: foreground = None
                        else: background = None
                    elif value in (38, 48):
                        if parameters[index + 1] == 5:
                            color = (0, parameters[index + 2]); index += 2
                        else:
                            assert parameters[index + 1] == 2
                            color = (1, *parameters[index + 2:index + 5]); index += 4
                        if value == 38: foreground = color
                        else: background = color
                    else:
                        raise AssertionError((value, transcript[position:position + 80]))
                    index += 1
            elif command in (b"h", b"l") and values.startswith(b"?") and parameters[0] in (7,25,1049,2004):
                pass
            else:
                raise AssertionError((values, command))
            position += match.end()
        elif transcript[position] == 13:
            column = 0; position += 1
        elif transcript[position] == 10:
            row += 1; position += 1
        else:
            byte = transcript[position]
            length = 1 if byte < 128 else 2 if byte < 224 else 3 if byte < 240 else 4
            character = transcript[position:position + length].decode()
            assert ord(character) >= 32, (byte, transcript[position:position + 40])
            width = 0 if unicodedata.combining(character) else 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1
            if column >= columns:
                row += 1; column = 0
            if row < rows:
                cells[row][column] = (character, attributes, foreground, background)
                for next_column in range(column + 1, min(columns, column + width)):
                    cells[row][next_column] = ("", attributes, foreground, background)
            column += width; position += length
    return cells


SOURCE = r'''
local range,post,early,italic,keep,late,blank
vis.events={
 init=function()
   assert(vis.ui:style_define(0,'bold,fore:blue,back:white'))
   local function style(s)local id=assert(vis.ui:style_push());assert(vis.ui:style_define(id,s));return id end
   range=style('italics,fore:green');post=style('underlined,back:red')
   early=style('fore:yellow');italic=style('italics,fore:green')
   keep=style('keep_attribute,underlined,back:red');late=style('fore:magenta');blank=style('fore:cyan')
 end,
 win_highlight=function(win)
   assert(vis.ui:style_define(range,'italics,fore:green'))
   win:style(range,1,2)
   win:style_pos(early,2,0)
   assert(vis.ui:style_define(range,'fore:red')) -- old cells retain the prior style
 end,
 win_status=function(win)
   win:style(post,2,2)
   win:style_pos(early,0,win.height-1)
   win:status('L','R')
   win:style_pos(italic,1,win.height-1)
   win:style_pos(keep,1,win.height-1)
 end,
 ui_draw=function()
   local win=vis.win;if not win then return end
   win:style_pos(early,2,0)
   win:style_pos(blank,10,0)
   win:style(late,3,3) -- after body copy; must not change this frame
   local ready=assert(io.open(READY_PATH,'w'));ready:write('ready');ready:close()
 end,
}
'''


def capture(binary, directory, source=SOURCE, data=b"abcd\n", rows=24, columns=80):
    ready = directory / "ready"
    config = configuration(directory, "config", source.replace("READY_PATH", json.dumps(str(ready))))
    path = directory / "input"
    path.write_bytes(data)
    session = LuaSession(binary, path, config, rows, columns)
    try:
        session.ready()
        deadline = time.monotonic() + 10
        while not ready.exists():
            session.read()
            assert session.process.poll() is None, bytes(session.output[-3000:])
            assert time.monotonic() < deadline, bytes(session.output[-3000:])
        settle(session, 0.3)
        observed = screen(bytes(session.output), rows, columns)
        session.send_keys(b"\x1b\x1b:qall!\r")
        session.finish()
        return observed
    finally:
        session.close()


def run(binary):
    with tempfile.TemporaryDirectory(prefix="vis-style-render-") as temporary:
        directory = Path(temporary).resolve()
        main = directory / "original-main.c"
        main.write_text((ROOT / "main.c").read_text().replace('#include "config.h"', '#include "config.def.h"'))
        flags = shlex.split(os.environ.get("VIS_C_LUA_FLAGS", ""))
        if not flags:
            flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "lua"], text=True))
        if "-lm" not in flags:
            flags.append("-lm")
        c_binary, rust_binary = directory / "vis-c", directory / "vis-rust"
        subprocess.run(shlex.split(os.environ.get("CC", "cc")) + ["-std=c99", '-DVERSION="style-oracle"', "-DCONFIG_CURSES=0", "-DCONFIG_LUA=1", "-I", str(ROOT), str(main), *flags, "-o", str(c_binary)], check=True, timeout=40)
        shutil.copy2(binary, rust_binary)
        c_directory, rust_directory = directory / "c", directory / "rust"
        c_directory.mkdir(); rust_directory.mkdir()
        expected, observed = capture(c_binary, c_directory), capture(rust_binary, rust_directory)
        positions = [(0, 1), (0, 2), (0, 3), (0, 10), (23, 0), (23, 1), (23, 2), (23, 79)]
        for row, column in positions:
            assert observed[row][column] == expected[row][column], ((row, column), expected[row][column], observed[row][column])
        print(f"PASS {len(positions)} actual C/Rust painted-cell states: snapshot styles, range/core/copy phases, blank cells and ordered status merges")
        template = r'''
local paint
vis.events={
 init=function()
   assert(vis.ui:style_define(0,'fore:white,back:black'))
   paint=assert(vis.ui:style_push());assert(vis.ui:style_define(paint,'bold,fore:red'))
 end,
 win_highlight=function(win) RANGE_PAINT end,
 ui_draw=function()
   CELL_PAINT
   local ready=assert(io.open(READY_PATH,'w'));ready:write('ready');ready:close()
 end,
}
'''
        cases = [
            ("tab-exact", "win:style(paint,1,1)", "", b"A\t\xe7\x95\x8cZ", 80, [(0,x) for x in range(1,12)]),
            ("tab-through-wide", "win:style(paint,1,2)", "", b"A\t\xe7\x95\x8cZ", 80, [(0,x) for x in range(1,12)]),
            ("wide-start-inside", "win:style(paint,3,3)", "", b"A\t\xe7\x95\x8cZ", 80, [(0,x) for x in range(8,12)]),
            ("wide-leading-byte", "win:style(paint,2,2)", "", b"A\t\xe7\x95\x8cZ", 80, [(0,x) for x in range(8,12)]),
            ("wide-end-byte", "win:style(paint,3,5)", "", b"A\t\xe7\x95\x8cZ", 80, [(0,x) for x in range(8,12)]),
            ("eof-range", "win:style(paint,6,6)", "", b"A\t\xe7\x95\x8cZ", 80, [(0,11)]),
            ("wide-continuation-cell", "", "vis.win:style_pos(paint,9,0)", b"A\t\xe7\x95\x8cZ", 80, [(0,x) for x in range(8,12)]),
            ("wrapped-tab", "win:style(paint,1,1)", "", b"A\t\xe7\x95\x8cZ", 5, [(y,x) for y in range(3) for x in range(5) if (y,x)!=(0,0)]),
            ("control-endpoint", "win:style(paint,1,1)", "", b"A\x01Z", 80, [(0,x) for x in range(1,5)]),
            ("control-continuation-cell", "", "vis.win:style_pos(paint,2,0)", b"A\x01Z", 80, [(0,x) for x in range(1,5)]),
        ]
        failures = []
        samples = 0
        for name, range_paint, cell_paint, data, columns, positions in cases:
            c_directory, rust_directory = directory / (name+"-c"), directory / (name+"-rust")
            c_directory.mkdir(); rust_directory.mkdir()
            source = template.replace("RANGE_PAINT", range_paint).replace("CELL_PAINT", cell_paint)
            expected = capture(c_binary, c_directory, source, data, columns=columns)
            observed = capture(rust_binary, rust_directory, source, data, columns=columns)
            for row,column in positions:
                if observed[row][column] != expected[row][column]:
                    failures.append((name,(row,column),expected[row][column],observed[row][column]))
                samples += 1
        assert not failures, failures
        print(f"PASS {samples} actual C/Rust tab, wide-character, EOF and continuation-cell states")
        crossing = r'''
local red,green,first
vis.events={
 init=function()
   assert(vis.ui:style_define(0,'fore:white,back:black'))
   red=assert(vis.ui:style_push());assert(vis.ui:style_define(red,'bold,fore:red'))
   green=assert(vis.ui:style_push());assert(vis.ui:style_define(green,'italics,fore:green'))
 end,
 start=function()assert(vis:command('vsplit'))end,
 win_highlight=function(win)
   if win==vis.win then
     first=win
     for target in vis:windows() do
       if target~=win then target:style_pos(red,1,0) end
     end
   elseif first then
     first:style_pos(green,1,0)
     first:status('X','Y')
     first:style_pos(red,1,first.height-1)
   end
 end,
 win_status=function(win)win:status('L','R')end,
 ui_draw=function()
   local ready=assert(io.open(READY_PATH,'w'));ready:write('ready');ready:close()
 end,
}
'''
        c_directory, rust_directory = directory / "cross-c", directory / "cross-rust"
        c_directory.mkdir(); rust_directory.mkdir()
        expected, observed = capture(c_binary,c_directory,crossing), capture(rust_binary,rust_directory,crossing)
        positions = [(0,1),(0,41),(23,0),(23,1),(23,38),(23,40),(23,79)]
        for row,column in positions:
            assert observed[row][column] == expected[row][column], ("cross-window",(row,column),expected[row][column],observed[row][column])
        print(f"PASS {len(positions)} cross-window early/late body copy and ordered status states")
        visibility = r'''
vis.events={
 init=function()
   assert(vis.ui:style_define(0,'fore:white,back:black'))
   assert(vis.ui:style_define(1,'italics,fore:red'))
   assert(vis.ui:style_define(2,'bold,fore:green'))
   assert(vis.ui:style_define(4,'underlined,fore:blue'))
 end,
 start=function() START_ACTION end,
 ui_draw=function()
   local ready=assert(io.open(READY_PATH,'w'));ready:write('ready');ready:close()
 end,
}
'''
        cursor_cases = [
            ("primary-secondary-match", "vis:feedkeys('<C-j>')", b"(a)\n(b)\n", [(y,x) for y in range(2) for x in range(3)]),
            ("inactive-hidden", "vis:feedkeys('v2l');assert(vis:command('vsplit'))", b"abcd\n", [(0,x) for x in [0,1,2,40,41,42]]),
            ("retained-anchors-normal", "vis:feedkeys('v2l');assert(vis:command('vsplit'));vis:feedkeys('<Escape><C-w>j')", b"abcd\n", [(0,x) for x in [0,1,2,40,41,42]]),
            ("prompt-parent-ranges", "vis:feedkeys('v2l:')", b"abcd\n", [(0,x) for x in range(4)]),
        ]
        samples=0
        for name,action,data,positions in cursor_cases:
            c_directory,rust_directory=directory/(name+"-c"),directory/(name+"-rust")
            c_directory.mkdir();rust_directory.mkdir()
            source=visibility.replace("START_ACTION",action)
            expected,observed=capture(c_binary,c_directory,source,data),capture(rust_binary,rust_directory,source,data)
            for row,column in positions:
                assert observed[row][column] == expected[row][column], (name,(row,column),expected[row][column],observed[row][column])
                samples+=1
        print(f"PASS {samples} primary/secondary/matching cursor and native selection visibility states")
        allocation = r'''
local bare,empty
vis.events={
 init=function()
   assert(vis.ui:style_define(0,'bold,fore:blue,back:white'))
   bare=assert(vis.ui:style_push())
   empty=assert(vis.ui:style_push(''))
 end,
 win_highlight=function(win)win:style(bare,1,1);win:style(empty,2,2)end,
 ui_draw=function()
   local ready=assert(io.open(READY_PATH,'w'));ready:write('ready');ready:close()
 end,
}
'''
        c_directory,rust_directory=directory/"allocation-c",directory/"allocation-rust"
        c_directory.mkdir();rust_directory.mkdir()
        expected,observed=capture(c_binary,c_directory,allocation),capture(rust_binary,rust_directory,allocation)
        for column in (1,2):
            assert observed[0][column] == expected[0][column], ("style allocation",column,expected[0][column],observed[0][column])
        print("PASS native bare/explicit-empty style allocation color inheritance")
        whitespace = r'''
vis.events={
 init=function()
   assert(vis.ui:style_define(0,'bold,fore:blue,back:white'))
   assert(vis.ui:style_define(13,'italics,fore:red'))
 end,
 ui_draw=function()
   local ready=assert(io.open(READY_PATH,'w'));ready:write('ready');ready:close()
 end,
}
'''
        c_directory,rust_directory=directory/"whitespace-c",directory/"whitespace-rust"
        c_directory.mkdir();rust_directory.mkdir()
        expected,observed=capture(c_binary,c_directory,whitespace,b"A \tZ\n"),capture(rust_binary,rust_directory,whitespace,b"A \tZ\n")
        # Compare source whitespace and same-row padding. The already tracked
        # trailing-newline EOF-row geometry belongs to the next view stage.
        positions=[(0,x)for x in range(1,11)]
        for row,column in positions:
            assert observed[row][column] == expected[row][column], ("whitespace snapshot",(row,column),expected[row][column],observed[row][column])
        print(f"PASS {len(positions)} native space/tab/newline style states")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "rust/target/debug/vis-rs")
    run(parser.parse_args().binary.resolve())
