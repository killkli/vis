-- Semantic corpus used by differential.py against the original native LPeg 1.1.
-- SPDX-License-Identifier: ISC
local backend, vendor, root = arg[1], arg[2], arg[3]
package.path = vendor .. '/?.lua;' .. root .. '/lua/?.lua;' .. root .. '/lua/lexers/?.lua'
if backend == 'pure' then
  local l = assert(loadfile(vendor .. '/compat.lua'))(dofile(vendor .. '/lulpeg.lua'))
  package.loaded.lpeg = l
  l.re = dofile(vendor .. '/re.lua')
  package.loaded.re = l.re
elseif backend == 'native' then
  package.cpath = vendor .. '/?.so'
end
local l, re = require('lpeg'), require('re')
local P,R,S,V,C,Cc,Ct,Cg,Cb,Cp,Cmt,Carg,Cf,Cs = l.P,l.R,l.S,l.V,l.C,l.Cc,l.Ct,l.Cg,l.Cb,l.Cp,l.Cmt,l.Carg,l.Cf,l.Cs
local function serialize(value)
  local kind = type(value)
  if kind == 'string' then return 's' .. (value:gsub('.', function(c) return ('%02x'):format(c:byte()) end)) end
  if kind == 'number' then return 'n' .. tostring(value) end
  if kind == 'nil' then return 'nil' end
  if kind == 'boolean' then return tostring(value) end
  if kind == 'table' then
    local parts = {}
    for key, item in pairs(value) do parts[#parts+1] = serialize(key) .. '=' .. serialize(item) end
    table.sort(parts)
    return '{' .. table.concat(parts, ',') .. '}'
  end
  return kind
end
local function emit(label, ...)
  local items = {}
  for i = 1, select('#', ...) do items[i] = serialize(select(i, ...)) end
  local line = label .. '\t' .. table.concat(items, '\t')
  if _vis_lpeg_emit then _vis_lpeg_emit(line) else print(line) end
end
local function check(label, fn) emit(label, fn()) end
local number = C(R('09')^1)/tonumber
check('captures', function() return Ct(Cc(false,nil,3) * Cg(C('abc'),'named') * Cb('named') * Cp()):match('abc') end)
check('accumulator', function() return (number * (',' * number % function(a,b) return a+b end)^0):match('1,2,3,99') end)
check('accumulator_novalue', function() return (Cc('x') * (P('^') % string.upper)^-1):match('^') end)
check('fold', function() return Cf(number * (',' * number)^0,function(a,b) return a+b end):match('1,2,3') end)
check('substitution', function() return Cs((P('foo')/'bar' + 1)^0):match('foo foo') end)
check('arguments', function() return Carg(3):match('',1,nil,false,'third') end)
check('arguments_201', function() return l.type(Carg(201)) end)
check('named_capture', function() return Ct(Cg(C(R('09')^1),'number') * Cg(C(R('az')^1),'word')):match('12abc') end)
check('division_string', function() return ((C('a')*C('b'))/'%2:%1:%0'):match('ab') end)
check('division_table', function() return (C('a')/{a=false}):match('a') end)
check('matchtime', function() return Cmt(C('abc'),function(s,p,c) return p,c,nil,false,9 end):match('abc') end)
check('behind', function() return (P(2)*l.B('ab')*Cp()):match('ab') end)
check('grammar', function() return P{ C('abc') + 1*V(1) }:match('__abc') end)
check('binary', function() return C(S('\0\255')^1):match('\0\255') end)
check('start_negative', function() return C(P(1)^0):match('abcdef',-3) end)
for _, value in ipairs{'a','♪','😀','\0','\127','\128','\192\128','\244\144\128\128','\237\160\128','\255'} do
  check('utfR:'..serialize(value), function() return l.utfR(0,0x10ffff):match(value) end)
end
check('re_capture', function() return re.match('hello world', "{'hello'} ' ' {'world'}") end)
check('re_substitution', function() return re.gsub('hello hello', "'hello'", 'world') end)
check('re_find', function() return re.find('xyzabc123', "'abc'") end)
check('re_grammar', function() return re.match('((x))', "B <- '(' ( B / 'x' ) ')' ") end)
local invalid = {
  empty_repeat = function() return P('')^0 end,
  left_recursive = function() return P{V(1)*'a'} end,
  missing_rule = function() return P{V('missing')} end,
  empty_grammar_repeat = function() return P{V(2)^0,[2]=P('')} end,
  float_length = function() return P(1.5) end,
  float_repeat = function() return P('a')^1.5 end,
  float_argument = function() return Carg(1.5) end,
  long_behind = function() return l.B(P(256)) end,
  rewind_capture = function() return Cmt('a',function() return 1 end):match('a') end,
}
for name, fn in pairs(invalid) do emit('invalid:'..name, pcall(fn)) end

local lexer = require('lexer')
local generic = 'function foo(x) { return x + 12; } // comment\n\'quoted\' "double"\n'
local language_samples = {
 cpp = '#include <vector>\nint main() { auto x = R"tag(quoted)tag"; // c\n return 0xff; }\n',
 lua = 'local t = {foo = 12}\n--[[long comment]]\nprint([=[long string]=], t.foo)\n',
 python = '@decorator\ndef foo(x: int = 2):\n    return f"hello {x!r}" # comment\n',
 javascript = 'const foo = (x) => `hello ${x}`; // comment\nlet p = /[a-z]+/gi;\n',
 html = '<!DOCTYPE html>\n<div class="x"><script>var x = 2;</script></div>\n',
 yaml = 'key: [null, true, "quoted"]\nother: &anchor {x: 1}\nref: *anchor\n',
}
for index = 4, #arg do
  local name = arg[index]
  local lex = lexer.load(name)
  emit('lexer:generic:'..name, lex:lex(generic))
  local file = assert(io.open(root .. '/lua/lexers/' .. name .. '.lua','rb'))
  local own_source = file:read(4096); file:close()
  emit('lexer:source:'..name, lex:lex(own_source))
  if language_samples[name] then emit('lexer:specific:'..name, lex:lex(language_samples[name])) end
end
local decimal = S('+-')^-1 * lexer.dec_num
local numeric = P{ Cp() * (lexer.hex_num + lexer.oct_num + decimal) * Cp() + 1 * V(1) }
for _, subject in ipairs{'42','-12',' 0xff rest','0755','abc 123','abc','0xA0','+123','3.14'} do
  check('numeric:'..subject, function() return numeric:match(subject) end)
end
