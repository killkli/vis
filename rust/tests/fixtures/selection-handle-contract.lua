-- Selection wrappers are fresh userdata; their checked logical target is live.
local win = vis.win
assert(win == vis.win and win.file == vis.win.file)
assert(win.selections == win.selections)
local first, second = win.selection, win.selection
assert(first ~= second, 'selection lookup creates a fresh userdata wrapper')
first.custom = 'first wrapper'
assert(first.custom == 'first wrapper' and second.custom == nil)
assert(win.selection.custom == nil, 'user fields belong to each wrapper')
first.pos = 2
assert(second.pos == 2 and win.selection.pos == 2)
second.range = {start = 0, finish = 2}
assert(first.anchored and first.range.start == 0 and first.range.finish == 2)
assert(first.pos == second.pos and first.number == second.number)
local array = win.selections[1]
local iterated = win:selections_iterator()()
assert(array ~= first and iterated ~= first and array ~= iterated)
assert(array.number == first.number and iterated.pos == first.pos)
assert(getmetatable(first) == vis.types.selection)
