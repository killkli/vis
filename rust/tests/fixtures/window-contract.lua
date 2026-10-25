-- C global action state and actual ordered window/split semantics.
local source = vis.win
source.sequence = 1
assert(vis:command('open'))
local second = vis.win
second.sequence = 2
assert(second.file:insert(0, 'one two\n'))
second.selection.pos = 0
assert(vis:command('open'))
local third = vis.win
third.sequence = 3
assert(third.file:insert(0, 'one two\n'))
third.selection.pos = 0
local sequence = {}
for win in vis:windows() do sequence[#sequence + 1] = win.sequence end
assert(table.concat(sequence, ',') == '3,2,1', 'new windows precede older windows')
vis.mode = vis.modes.INSERT
vis.win = source
assert(vis.mode == vis.modes.INSERT, 'focus preserves the global mode')
vis:feedkeys('Z<Escape>')
assert(source.file:content(0, source.file.size) == 'Zabc xyz\n')
vis.count = 3
vis.win = third
assert(vis.count == 3, 'focus preserves an unfinished count')
vis.count = nil
vis.win = second
vis:feedkeys('d')
assert(vis.mode == vis.modes.OPERATOR_PENDING)
vis.win = third
assert(vis.mode == vis.modes.OPERATOR_PENDING)
vis:feedkeys('w')
assert(third.file:content(0, third.file.size) == 'two\n')
assert(second.file:content(0, second.file.size) == 'one two\n')
vis:feedkeys('<C-w>j')
assert(vis.win == second, 'next follows the native ordered list')
vis:feedkeys('<C-w>k')
assert(vis.win == third, 'previous follows the native ordered list')

vis.win = source
source.options.tabwidth = 4
source.options.expandtab = true
vis.options.autoindent = true
vis.win = second
assert(vis.options.autoindent, 'focus preserves global autoindent')
vis.win = source
assert(vis.options.autoindent)
assert(source.options.autoindent == nil, 'global option is not a native window option')
assert(vis.options.tabwidth == nil, 'window option is not a native global option')
assert(vis:command(',x/[az]/'))
vis.mode = vis.modes.NORMAL
assert(#source.selections == 2)
local position = source.selection.pos
local opened
vis.events.subscribe(vis.events.WIN_OPEN, function(win)
    if win ~= source and win.file == source.file then
        opened = win
        assert(win.selection.pos == 0 and #win.selections == 1)
        assert(win.options.tabwidth == 8 and not win.options.expandtab)
    end
end)
assert(vis:command('split'))
local split = vis.win
assert(split == opened and split ~= source)
assert(#split.selections == 1 and split.selection.pos == position)
assert(split.options.tabwidth == 8 and not split.options.expandtab,
       'split copies UI flags and primary cursor, not tab/expand options')
assert(vis.options.autoindent)
