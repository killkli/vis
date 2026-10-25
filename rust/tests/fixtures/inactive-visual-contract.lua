-- Linewise mode materializes geometry in the affected view, never in every
-- anchored selection merely because the editor's mode is global.
local source = vis.win
assert(vis:command('open'))
local second = vis.win
assert(second.file:insert(0, 'one\nlast\n'))

vis.win = source
source.selection.pos = 1
vis.mode = vis.modes.VISUAL
vis:feedkeys('l')
assert(source.selection.anchored)
assert(source.selection.range.start == 1 and source.selection.range.finish == 3)

vis.win = second
assert(vis.mode == vis.modes.VISUAL)
vis.mode = vis.modes.NORMAL
second.selection.pos = 1
vis.mode = vis.modes.VISUAL_LINE
assert(second.selection.range.start == 0 and second.selection.range.finish == 4)
assert(second.selection.pos == 3, 'linewise mode materializes the newline endpoint')
assert(source.selection.range.start == 1 and source.selection.range.finish == 3,
       'inactive charwise selection retains its exact byte geometry')

vis.win = source
assert(vis.mode == vis.modes.VISUAL_LINE)
assert(source.selection.range.start == 1 and source.selection.range.finish == 3,
       'bare focus does not apply a linewise motion')
vis:feedkeys('l')
assert(source.selection.range.start == 0 and source.selection.range.finish == 8,
       'an actual linewise motion materializes full-line geometry')
assert(source.selection.pos == 7)

vis.win = second
vis.mode = vis.modes.NORMAL
assert(source.selection.anchored)
assert(source.selection.range.start == 0 and source.selection.range.finish == 8)
vis.win = source
assert(vis.mode == vis.modes.NORMAL and source.selection.anchored)
assert(source.selection.range.start == 0 and source.selection.range.finish == 8,
       'retained inactive linewise geometry survives another view leaving visual mode')
