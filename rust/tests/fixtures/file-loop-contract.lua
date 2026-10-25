-- Genuine X/Y window iteration, typed nested children and lifecycle callbacks.
local source = vis.win
local original = source.file:content(0, source.file.size)
local directory = assert(source.file.path):match('^(.*)/[^/]+$')
local alpha_path = directory .. '/loop-alpha.txt'
local beta_path = directory .. '/loop-beta.txt'
local function quote(value) return '"' .. value:gsub('\\', '\\\\'):gsub('"', '\\"') .. '"' end
local function put(path, value)
    local file = assert(io.open(path, 'wb')); assert(file:write(value)); assert(file:close())
end
local function content(file) return file:content(0, file.size) end
put(alpha_path, 'aba\n'); put(beta_path, 'baa\n')
assert(vis:command('open ' .. quote(alpha_path))); local alpha = vis.win
assert(vis:command('open ' .. quote(beta_path))); local beta = vis.win
local pattern = '/loop-(alpha|beta)\\.txt$/'

local calls = {}
local probe_error
assert(vis:command_register('loopprobe', function(argv, force, win, selection, range)
    if argv[0] ~= 'loopprobe' or force then probe_error = 'argv or force' end
    if selection.pos ~= win.selection.pos then probe_error = 'selection position' end
    if type(range) ~= 'table' or range.start ~= 0 or range.finish ~= win.file.size then
        probe_error = 'range '..tostring(range)..' '..tostring(type(range)=='table' and range.start)..' '..tostring(type(range)=='table' and range.finish)..' size '..win.file.size
    end
    calls[#calls + 1] = win
    return true
end))
assert(vis:command('loopprobe'))
assert(not probe_error, probe_error)
assert(#calls == 1 and calls[1] == beta)
calls = {}
vis.win = source
assert(vis:command('X' .. pattern .. ' loopprobe'))
assert(not probe_error, probe_error)
assert(#calls == 2 and calls[1] == beta and calls[2] == alpha)
assert(vis.win == source, 'file iteration does not focus its explicit target')

assert(vis:command('X' .. pattern .. ' ,x/a/ {i/</ a/>/}'))
assert(content(alpha.file) == '<a>b<a>\n')
assert(content(beta.file) == 'b<a><a>\n')
assert(vis:command('X/loop-alpha\\.txt$/ X/loop-beta\\.txt$/ ,c/nested/'))
assert(content(beta.file) == 'nested')
assert(vis:command('Y/loop-alpha\\.txt$/ ,g/nested/ c/negative/'))
assert(content(beta.file) == 'negative')
assert(content(source.file) == original)

-- A duplicated view is a second visit to the same file/transcript.
vis.win = beta
assert(vis:command('split')); local duplicate = vis.win
vis.win = source
assert(vis:command('X/loop-beta\\.txt$/ ,c/conflict/') == false)
assert(content(beta.file) == 'negative')
assert(duplicate:close(true))
assert(vis:command('X' .. pattern .. ' ,x/./ q!') == false)
assert(content(alpha.file) == '<a>b<a>\n' and content(beta.file) == 'negative')

local saved, closed = {}, {}
vis.events.subscribe(vis.events.FILE_SAVE_PRE, function(file, path)
    if file == alpha.file or file == beta.file then
        assert(vis.win == source, 'save callbacks retain global focus')
        assert(path == file.path)
        assert(file:insert(file.size, '!'))
        saved[#saved + 1] = path
    end
end)
vis.events.subscribe(vis.events.WIN_CLOSE, function(win)
    if win == alpha or win == beta then
        assert(vis.win == source, 'closing an inactive target does not focus it')
        closed[#closed + 1] = win.file.path
    end
end)
assert(vis:command('X' .. pattern .. ' w'))
assert(#saved == 2 and saved[1] == beta_path and saved[2] == alpha_path)
local file = assert(io.open(alpha_path, 'rb')); assert(file:read('*a') == '<a>b<a>\n!'); file:close()
file = assert(io.open(beta_path, 'rb')); assert(file:read('*a') == 'negative!'); file:close()
assert(vis:command('X' .. pattern .. ' q'))
assert(#closed == 2 and closed[1] == beta_path and closed[2] == alpha_path)
assert(vis.win == source and content(source.file) == original)
assert(os.remove(alpha_path)); assert(os.remove(beta_path))
