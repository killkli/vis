-- Native error handlers open the persistent internal message window before
-- the interrupted input callback resumes. Errors do not become info-row text.
local source = vis.win
local original = source.file:content(0, source.file.size)
local opened, inputs = 0, 0
vis.events.subscribe(vis.events.WIN_OPEN, function() opened = opened + 1 end)
vis.events.subscribe(vis.events.INPUT, function(key)
    inputs = inputs + 1
    if key == 'z' then error('input contract failure') end
end)
vis:feedkeys('iz<Escape>')
local message = vis.win
assert(message ~= source, 'Lua callback error focuses message window')
assert(opened == 0, 'internal message window has no WIN_OPEN event')
assert(inputs == 1, 'internal insertion does not emit another INPUT')
assert(source.file:content(0, source.file.size) == original,
       'resumed input follows the focused window')
local diagnostic = message.file:content(0, message.file.size)
assert(diagnostic:find('input contract failure', 1, true), diagnostic)
assert(diagnostic:sub(1, 1) == 'z', 'input resumes at the message cursor')
assert(vis.mode == vis.modes.NORMAL)
local count = 0
for win in vis:windows() do count = count + 1; assert(win == source) end
assert(count == 1, 'internal message window is excluded from vis.windows')

vis.win = source
assert(vis:map(vis.modes.NORMAL, 'Q', function() error('mapping contract failure') end))
vis:feedkeys('Q')
assert(vis.win == message, 'mapping error reuses the message window')
assert(message.file:content(0, message.file.size):find('mapping contract failure', 1, true))

vis.win = source
assert(vis:command_register('contractfail', function() error('command contract failure') end))
assert(vis:command('contractfail'), 'handler failure remains accepted')
assert(vis.win == message, 'command error focuses the message window')
assert(message.file:content(0, message.file.size):find('command contract failure', 1, true))

local path = os.tmpname()
os.remove(path)
local saves, posts = 0, 0
vis.events.subscribe(vis.events.FILE_SAVE_PRE, function()
    saves = saves + 1
    error('save contract failure')
end)
vis.events.subscribe(vis.events.FILE_SAVE_POST, function() posts = posts + 1 end)
vis.win = source
assert(vis:command('w ' .. path))
assert(vis.win == message, 'save event error focuses the message window')
assert(saves == 1 and posts == 0, 'save exception vetoes actual write')
local disk = io.open(path, 'rb')
if disk then disk:close(); os.remove(path); error('save veto wrote a file') end
assert(source.file:content(0, source.file.size) == original)
