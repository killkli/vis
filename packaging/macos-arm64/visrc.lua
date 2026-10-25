-- Managed defaults; edit this installed file to customize your editor.
local root = assert(os.getenv('VIS_QUICK_ROOT'), 'Start vis through the installed launcher')
local runtime = root .. '/runtime'
package.path = package.path .. ';' .. runtime .. '/?.lua;' .. runtime .. '/?/init.lua'
require('vis')

vis.events.subscribe(vis.events.INIT, function()
  vis:command('set theme zenburn')
  vis:command('set autoindent true')
end)

vis.events.subscribe(vis.events.WIN_OPEN, function()
  vis:command('set numbers true')
  vis:command('set cursorline true')
  vis:command('set tabwidth 4')
end)

-- Filetype detection and syntax highlighting are provided by vis-std.
-- dkjson handles Unicode escapes in language-server responses.
local lspc = require('plugins/vis-lspc')
lspc.autostart = true
lspc.highlight_diagnostics = 'line'
lspc.universal_root_globs = {'.git'}
lspc.fallback_dirname_as_root = true
lspc.logging = false

-- JSON and other servers use pull diagnostics. Request the initial report as
-- soon as didOpen is sent; subsequent saves/LSP requests refresh it as usual.
vis.events.subscribe(lspc.events.LS_DID_OPEN, function(ls, file)
  if vis.win and vis.win.file == file then
    ls:request_diagnostics(vis.win)
  end
end)

local clangd = {
  name = 'clangd',
  cmd = 'clangd',
  roots = {'compile_commands.json', '.clangd'},
}
local typescript = {
  name = 'typescript',
  cmd = 'typescript-language-server --stdio',
  roots = {'tsconfig.json', 'jsconfig.json', 'package.json'},
  -- Use the bundled tsserver independently of the global Node environment.
  init_options = {tsserver = {path = root .. '/lsp/node_modules/typescript/lib/tsserver.js'}},
}

-- Bundled servers come first on PATH; optional Go/Rust tools are detected.
local servers = {
  c = clangd,
  ansi_c = clangd,
  cpp = clangd,
  lua = {
    name = 'lua-language-server',
    cmd = 'lua-language-server',
    roots = {'.luarc.json', '.luarc.jsonc'},
    settings = {
      Lua = {
        runtime = {version = 'Lua 5.5'},
        diagnostics = {globals = {'vis'}},
        telemetry = {enable = false},
      },
    },
    formatting_options = {tabSize = 2, insertSpaces = true},
  },
  python = {
    name = 'pyright',
    cmd = 'pyright-langserver --stdio',
    roots = {'pyrightconfig.json', 'pyproject.toml', 'setup.py', 'requirements.txt'},
    settings = {python = {analysis = {typeCheckingMode = 'basic', autoSearchPaths = true}}},
  },
  javascript = typescript,
  typescript = typescript,
  go = {name = 'gopls', cmd = 'gopls', roots = {'go.work', 'go.mod'}},
  rust = {name = 'rust-analyzer', cmd = 'rust-analyzer', roots = {'Cargo.toml'}},
  html = {name = 'html', cmd = 'vscode-html-language-server --stdio'},
  css = {name = 'css', cmd = 'vscode-css-language-server --stdio'},
  json = {
    name = 'json', cmd = 'vscode-json-language-server --stdio',
    init_options = {provideFormatter = true},
  },
  yaml = {name = 'yaml', cmd = 'yaml-language-server --stdio'},
}
lspc.ls_map = {}
for syntax, server in pairs(servers) do
  local executable = server.cmd:match('^%S+')
  if os.execute('command -v ' .. executable .. ' >/dev/null 2>&1') then
    lspc.ls_map[syntax] = server
  end
end

-- Ctrl-Space is intercepted by some terminal/IME combinations; Ctrl-L is
-- an additional completion binding in both normal and insert mode.
for _, mode in ipairs({vis.modes.NORMAL, vis.modes.INSERT}) do
  vis:map(mode, '<C-l>', function()
    vis:command('lspc-completion')
    vis.mode = mode
  end, 'LSP completion')
end
