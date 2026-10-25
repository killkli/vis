#!/usr/bin/env python3
"""Assert native jumplist ring behavior; optionally compare the Rust Lua CLI.

The Lua fixture uses the real gs/g</g> actions and live selection userdata.
Every successful host must write an explicit final marker. This is independent
of the dormant Rust core module, so missing integration cannot count as a pass.
"""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]

FIXTURE = r'''
local running=false
local out
local function trace(label)
  local w=vis.win
  out:write(label,'|',vis.mode,'|',w.selection.number,'|',#w.selections)
  for s in w:selections_iterator() do
    local r=s.range
    out:write('|',tostring(s.pos),',',tostring(r and r.start),',',tostring(r and r.finish),',',s.anchored and 1 or 0)
  end
  out:write('\n');out:flush()
end
local function fresh(data)
  vis.mode=vis.modes.NORMAL
  local previous=vis.win
  local close={}
  for candidate in vis:windows() do if candidate~=previous then close[#close+1]=candidate end end
  for _,candidate in ipairs(close) do assert(candidate:close(true)) end
  assert(vis:command('new'))
  local w=vis.win
  assert(w~=previous,'native window creation did not succeed')
  assert(previous:close(true))
  assert(w.file:insert(0,data or string.rep('abcdefghijklmnopqrstuvwxyz0123456789',4)))
  w.file:snapshot()
  w.selection.pos=0
  return w
end
local function pos(w,p) w.selection.pos=p end
local function at(w,p,label)
  assert(w.selection.pos==p,label..': '..tostring(w.selection.pos)..' ~= '..p)
  trace(label)
end
vis.events={win_open=function()
  if running then return end
  running=true
  out=assert(io.open('trace','w'))

  local w=fresh()
  pos(w,0);vis:feedkeys('gs');pos(w,5);vis:feedkeys('gs')
  vis:feedkeys('g<');at(w,5,'previous-equal-slot')
  vis:feedkeys('g<');at(w,0,'previous-older')
  vis:feedkeys('g>');at(w,0,'next-equal-slot')
  vis:feedkeys('g>');at(w,5,'next-newer')
  vis:feedkeys('g>');at(w,5,'next-empty-slot')

  w=fresh();pos(w,1);vis:feedkeys('gs')
  pos(w,w.file.size);vis:feedkeys('gs');pos(w,2);vis:feedkeys('gs')
  vis:feedkeys('g<');at(w,2,'eof-slot-newest-equal')
  vis:feedkeys('g<');at(w,2,'eof-save-consumes-empty-slot')
  vis:feedkeys('g<');at(w,1,'eof-slot-older-target')

  w=fresh();pos(w,7)
  for i=1,32 do vis:feedkeys('g<');assert(w.selection.pos==7) end
  trace('empty-backward-wrap')
  vis:feedkeys('gs');pos(w,9);vis:feedkeys('g<');at(w,7,'save-after-empty-underflow')

  w=fresh()
  for i=0,32 do pos(w,i);vis:feedkeys('gs') end
  vis:feedkeys('g<');at(w,32,'full-ring-equal')
  for i=31,1,-1 do vis:feedkeys('g<');assert(w.selection.pos==i) end
  trace('full-ring-oldest-retained')
  vis:feedkeys('g<');at(w,32,'full-ring-wraps')

  w=fresh();pos(w,2);vis:feedkeys('gsgs')
  pos(w,9);vis:feedkeys('g<');at(w,2,'duplicate-adjacent-first')
  pos(w,9);vis:feedkeys('g<');at(w,2,'duplicate-adjacent-second')

  w=fresh();pos(w,2);vis:feedkeys('gs');pos(w,5);vis:feedkeys('gs')
  assert(vis:command('split'))
  local split
  for candidate in vis:windows() do
    if candidate~=w and candidate.file==w.file then split=candidate;break end
  end
  assert(split);vis.win=split
  pos(split,9);vis:feedkeys('g<');at(split,5,'split-has-sam-completion-seed')
  pos(split,9);vis:feedkeys('g<');at(split,9,'split-does-not-inherit-old-history')
  vis.win=w;pos(w,8);vis:feedkeys('g<');at(w,5,'original-window-newest-history')
  vis:feedkeys('g<');at(w,2,'original-window-ring-survives-focus')

  w=fresh();pos(w,2);vis:feedkeys('gs');vis:feedkeys('g<')
  vis.mode=vis.modes.VISUAL;vis:feedkeys('gs')
  w.selection.pos=8;vis:feedkeys('g>')
  assert(vis.mode==vis.modes.NORMAL)
  at(w,2,'same-slot-geometry-keeps-old-mode')
  vis:feedkeys('g<')
  assert(vis.mode==vis.modes.VISUAL and w.selection.range.start==2 and w.selection.range.finish==9)
  trace('mode-transition-saves-current-visual')

  w=fresh();pos(w,3);vis:feedkeys('gs')
  assert(w.file:delete(3,1));w.file:snapshot();pos(w,8)
  vis:feedkeys('g<');at(w,8,'deleted-region-is-empty-target')
  vis:feedkeys('u');pos(w,8);vis:feedkeys('g>');at(w,3,'undo-revives-saved-marks')

  w=fresh('abc\ndef\nghi\njkl\n')
  vis:feedkeys('<C-j><C-j>')
  trace('multi-created')
  assert(#w.selections==3 and w.selection.number==3)
  vis.mode=vis.modes.VISUAL
  for i=1,3 do w.selections[i].range={start=(i-1)*4,finish=(i-1)*4+3} end
  vis:feedkeys('gs')
  for i=1,3 do w.selections[i].range={start=(i-1)*4+1,finish=(i-1)*4+3} end
  vis:feedkeys('o');vis:feedkeys('g<')
  assert(w.selection.number==1 and #w.selections==3)
  for i=1,3 do assert(w.selections[i].pos==(i-1)*4 and w.selections[i].anchored) end
  trace('restore-keeps-current-orientation-resets-primary')

  w=fresh('abc\ndef\nghi\njkl\n');vis:feedkeys('<C-j><C-j>')
  vis.mode=vis.modes.VISUAL
  for i=1,3 do w.selections[i].range={start=0,finish=1} end
  vis:feedkeys('gs');vis:feedkeys('g<')
  assert(#w.selections==2)
  trace('duplicate-ranges-native-normalization-quirk')

  w=fresh('a\nb\nc\nd\n');vis:feedkeys('G');at(w,6,'jump-motion-destination')
  pos(w,2);vis:feedkeys('g<');at(w,6,'jump-motion-recorded-after-motion')

  w=fresh();pos(w,2);vis.mode=vis.modes.VISUAL
  w.selection.range={start=2,finish=5};vis.mode=vis.modes.NORMAL
  pos(w,10);vis:feedkeys('g<')
  assert(vis.mode==vis.modes.VISUAL and w.selection.range.start==2 and w.selection.range.finish==5)
  trace('visual-leave-saves-before-clearing')

  out:write('JUMPLIST_NATIVE_COMPLETE\n');out:close();vis:exit(0)
end}
'''


def run(binary, directory):
    directory.mkdir()
    (directory / "visrc.lua").write_text(FIXTURE)
    source = directory / "input"
    source.write_bytes(b"initial document\n")
    result = subprocess.run(
        [str(binary), "+qall!", str(source)], cwd=directory,
        env={**os.environ, "VIS_PATH": str(directory), "TERM": "xterm"},
        input=b"", capture_output=True, timeout=15,
    )
    (directory / "stderr").write_bytes(result.stderr)
    trace = directory / "trace"
    assert result.returncode == 0 and trace.is_file(), (result.returncode, result.stderr[-1500:])
    output = trace.read_text()
    assert output.endswith("JUMPLIST_NATIVE_COMPLETE\n"), output
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--rust-vis", type=Path)
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    if args.artifacts:
        args.artifacts.mkdir(parents=True, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix="jumplist-", dir=args.artifacts)).resolve()
        print(f"Artifacts: {directory}")
        expected = run(args.c_vis.resolve(), directory / "c")
        if args.rust_vis:
            assert run(args.rust_vis.resolve(), directory / "rust") == expected
    else:
        with tempfile.TemporaryDirectory(prefix="vis-jumplist-") as temporary:
            directory = Path(temporary).resolve()
            expected = run(args.c_vis.resolve(), directory / "c")
            if args.rust_vis:
                assert run(args.rust_vis.resolve(), directory / "rust") == expected
    print("PASS native jumplist ring/mode/marks/orientation success-marker oracle")
    if args.rust_vis:
        print("PASS actual Rust jumplist action trace matches C")


if __name__ == "__main__":
    main()
