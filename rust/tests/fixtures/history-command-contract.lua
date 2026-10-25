-- Preserve C command parsing, including its shadowed plain numeric count.
local file = vis.win.file
local original = file:content(0, file.size)
for _, text in ipairs({'A', 'B', 'C'}) do
    assert(file:insert(file.size, text))
    file:snapshot()
end
local function content() return file:content(0, file.size) end
assert(content() == original .. 'ABC')
assert(vis:command('earlier 2'))
assert(content() == original .. 'AB', 'plain numeric count still traverses one revision')
assert(vis:command('earlier 0'))
assert(content() == original .. 'A', 'plain zero retains native one-step behavior')
assert(vis:command('later 0'))
assert(content() == original .. 'AB')
for _, argument in ipairs({'-1', 'xyz', '1q'}) do
    assert(vis:command('earlier ' .. argument), 'handler diagnostics remain accepted')
    assert(content() == original .. 'AB', 'invalid argument must not traverse history')
end
assert(vis:command('earlier 0s'))
assert(content() == original .. 'AB', 'zero seconds restores the current timestamp')
assert(vis:command('earlier 1day'))
assert(content() == original, 'unit parser uses the first suffix character')
assert(vis:command('later 1day'))
assert(content() == original .. 'ABC')
