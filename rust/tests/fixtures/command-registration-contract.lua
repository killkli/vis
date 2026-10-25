-- Command registry collisions use exact native names, not Rust's extra aliases.
local native = {'a','c','d','g','i','p','s','v','x','y','X','Y','>','<','|','!',
                'w','r','{','}','e','q','cd','help','map','map-window','unmap',
                'unmap-window','langmap','new','open','qall','set','split',
                'vnew','vsplit','wq','earlier','later'}
for _, name in ipairs(native) do
    assert(not vis:command_register(name, function() end), 'native collision: ' .. name)
end
for _, name in ipairs({'write','quit','edit','o','b','B','D','buffer','bdelete',
                      'chdir','pwd','files','ls','wincmd'}) do
    assert(vis:command_register(name, function() end), 'available exact name: ' .. name)
    assert(not vis:command_register(name, function() end))
end
