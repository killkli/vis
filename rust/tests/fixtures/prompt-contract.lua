-- Shared behavioral assertions run unchanged against C vis and the Rust host.
local source = vis.win
local original = source.file:content(0, source.file.size)
local events = 0
for _, event in ipairs({vis.events.FILE_OPEN, vis.events.FILE_CLOSE,
                       vis.events.WIN_OPEN, vis.events.WIN_CLOSE}) do
    vis.events.subscribe(event, function() events = events + 1 end)
end
vis:feedkeys(':')
local prompt, history = vis.win, vis.win.file
assert(prompt ~= source and vis.mode == vis.modes.INSERT)
assert(history:content(0, history.size) == ':\n')
assert(prompt.selection.pos == 1)
local windows, files = 0, 0
for _ in vis:windows() do windows = windows + 1 end
for _ in vis:files() do files = files + 1 end
assert(windows == 1 and files == 3 and events == 0)
vis:feedkeys('<Escape>')
assert(vis.win == prompt and vis.mode == vis.modes.NORMAL)
vis:feedkeys('<Escape>')
assert(vis.win == source and vis.mode == vis.modes.NORMAL)
assert(history.size == 0 and events == 0)

-- An invalid expression retains the same editable prompt; valid no-match hides it.
vis:feedkeys('/[<Enter>')
prompt = vis.win
assert(prompt ~= source and prompt.file == history)
assert(vis.mode == vis.modes.INSERT)
assert(history:content(0, history.size) == '/[\n')
assert(history:delete(0, history.size))
local search = '/definitely_no_match\n'
assert(history:insert(0, search))
prompt.selection.pos = #search - 1
vis:feedkeys('<Enter>')
assert(vis.win == source and vis.mode == vis.modes.NORMAL)
assert(source.selection.pos == 0)
assert(source.file:content(0, source.file.size) == original)

-- One persistent file holds mixed command/search history. Expanded history is
-- ordinary modal text: the selected old line executes and moves to the end.
assert(history:delete(0, history.size))
vis:feedkeys(':set expandtab<Enter>/xyz<Enter>?abc<Enter>')
assert(vis.win == source and source.selection.pos == 0,
       string.format('after mixed history: source=%s pos=%s mode=%s history=%q',
                     tostring(vis.win == source), tostring(source.selection.pos),
                     tostring(vis.mode), history:content(0, history.size)))
assert(source.options.expandtab)
assert(history:content(0, history.size) == ':set expandtab\n/xyz\n?abc\n')
vis:feedkeys(':<Up><Up><Escape>0l')
prompt = vis.win
assert(prompt ~= source and prompt.file == history)
assert(vis.mode == vis.modes.NORMAL)
assert(history:content(prompt.selection.pos, 1) == 'x')
vis:feedkeys('<Enter>')
assert(vis.win == source and source.selection.pos == 4)
assert(history:content(0, history.size) == ':set expandtab\n?abc\n/xyz\n')
assert(events == 0)
assert(source.file:content(0, source.file.size) == original)
