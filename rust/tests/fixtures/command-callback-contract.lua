-- Registered commands receive the native default/addressed range in every mode.
local source = vis.win
local seen = {}
assert(vis:command_register('contractargv', function(argv, force, win, selection, range)
    seen[#seen + 1] = {argv = argv, force = force, win = win, selection = selection, kind = type(range), start = range and range.start, finish = range and range.finish}
    return false
end))
source.selection.pos = 2
assert(vis:command('contractargv "a b" c'))
assert(#seen == 1 and not seen[1].force)
assert(seen[1].win == source)
assert(seen[1].selection.number == source.selection.number and seen[1].selection.pos == source.selection.pos)
assert(seen[1].argv[0] == 'contractargv', tostring(seen[1].argv[0]))
assert(seen[1].argv[1] == 'a b' and seen[1].argv[2] == 'c')
assert(seen[1].start == 0 and seen[1].finish == source.file.size, seen[1].kind)
vis.mode = vis.modes.VISUAL
vis:feedkeys('l')
local visual = source.selection.range
assert(vis:command('contractargv v'))
assert(#seen == 2 and seen[2].start == visual.start and seen[2].finish == visual.finish)
vis.mode = vis.modes.NORMAL
assert(vis:command('#1,#3contractargv! z'))
assert(#seen == 3 and seen[3].force)
assert(seen[3].start == 1 and seen[3].finish == 3 and seen[3].argv[1] == 'z')
assert(vis:command('contractarg q'))
assert(#seen == 3, 'native abbreviated user command is accepted without calling its handler')
