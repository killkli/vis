-- Rust vis adapter for the pinned pure-Lua LuLPeg implementation.
-- SPDX-License-Identifier: ISC
local lpeg = ...
local original_P, original_match, original_pow = lpeg.P, lpeg.match, lpeg.__pow
local validated = setmetatable({}, {__mode = 'k'})

local function nullable(pattern, rules, values)
  local kind = pattern.pkind
  if kind == 'ref' then return values and values[pattern.aux] or false end
  if kind == 'grammar' then
    local own = pattern.aux
    local current = {}
    local changed = true
    while changed do
      changed = false
      for key, rule in pairs(own) do
        if not current[key] and nullable(rule, own, current) then current[key], changed = true, true end
      end
    end
    return nullable(own[1], own, current)
  end
  if kind == 'choice' then return nullable(pattern[1], rules, values) or nullable(pattern[2], rules, values) end
  if kind == 'sequence' then return nullable(pattern[1], rules, values) and nullable(pattern[2], rules, values) end
  if kind == 'at most' then return true end
  if kind == 'at least' then return pattern.aux == 0 or nullable(pattern.pattern, rules, values) end
  if kind == 'true' or kind == 'lookahead' or kind == 'unm' or kind == 'behind' then return true end
  if kind == 'Cp' or kind == 'Cc' or kind == 'Cb' or kind == 'Carg' then return true end
  -- A match-time function can advance beyond its child's match; check its
  -- actual progress in the unbounded repetition matcher instead.
  if kind == 'Cmt' then return false end
  if pattern.pattern then return nullable(pattern.pattern, rules, values) end
  if kind == 'any' then return pattern.aux == 0 end
  if kind == 'string' then return #pattern.aux == 0 end
  return false
end

local validate
local function validate_grammar(grammar)
  local rules = grammar.aux
  for key, rule in pairs(rules) do
    if lpeg.type(rule) ~= 'pattern' then error("rule '" .. tostring(key) .. "' is not a pattern", 3) end
  end
  local values = {}
  local changed = true
  while changed do
    changed = false
    for key, rule in pairs(rules) do
      if not values[key] and nullable(rule, rules, values) then values[key], changed = true, true end
    end
  end
  local seen = {}
  local function inspect(pattern)
    if seen[pattern] then return end
    seen[pattern] = true
    if pattern.pkind == 'grammar' then validate(pattern); return end
    if pattern.pkind == 'ref' then
      if not rules[pattern.aux] then error("rule '" .. tostring(pattern.aux) .. "' undefined in given grammar", 3) end
    elseif pattern.pkind == 'at least' and nullable(pattern.pattern, rules, values) then
      error('loop body may accept empty string', 3)
    end
    if pattern.pattern then inspect(pattern.pattern) end
    if pattern[1] then inspect(pattern[1]); inspect(pattern[2]) end
  end
  for _, rule in pairs(rules) do inspect(rule) end
  -- Follow only calls reachable without consuming a byte to find direct and
  -- indirect left recursion. Nested grammars establish their own rule scope.
  local function left(pattern, active)
    local kind = pattern.pkind
    if kind == 'ref' then
      local key = pattern.aux
      if active[key] then error("rule '" .. tostring(key) .. "' may be left recursive", 3) end
      active[key] = true
      left(rules[key], active)
      active[key] = nil
    elseif kind == 'sequence' then
      left(pattern[1], active)
      if nullable(pattern[1], rules, values) then left(pattern[2], active) end
    elseif kind == 'choice' then left(pattern[1], active); left(pattern[2], active)
    elseif kind ~= 'grammar' and pattern.pattern then left(pattern.pattern, active) end
  end
  for key, rule in pairs(rules) do left(rule, {[key] = true}) end
end

validate = function(pattern)
  if validated[pattern] then return end
  if pattern.pkind == 'grammar' then
    validate_grammar(pattern)
  else
    if pattern.pattern then validate(pattern.pattern) end
    if pattern[1] then validate(pattern[1]); validate(pattern[2]) end
  end
  validated[pattern] = true
end

function lpeg.P(value)
  local pattern = original_P(value)
  validate(pattern)
  return pattern
end

function lpeg.__pow(pattern, count)
  count = tonumber(count)
  if not count or count % 1 ~= 0 then error('integer repetition count expected', 2) end
  if count >= 0 and nullable(pattern) then error('loop body may accept empty string', 2) end
  return original_pow(pattern, count)
end

function lpeg.match(pattern, ...)
  pattern = lpeg.P(pattern)
  return original_match(pattern, ...)
end

function lpeg.setmaxstack(count)
  if type(count) ~= 'number' or count % 1 ~= 0 or count < 1 or count > 8192 then
    error('LPeg grammar call stack bound must be an integer in 1..8192', 2)
  end
  -- Counts grammar rule calls in this backend, not C LPeg VM instructions.
  lpeg._maxstack = count
end

-- LPeg 1.x exposes version as a string. Keep the backend identity honest.
lpeg.version = 'LuLPeg 0.1.0 (vis LPeg 1.1 API adapter)'

function lpeg.utfR(first, last)
  if type(first) ~= 'number' or type(last) ~= 'number' or first % 1 ~= 0 or last % 1 ~= 0 or
      first < 0 or last > 0x10ffff or first > last then error('invalid UTF-8 code point range', 2) end
  if last <= 127 then return lpeg.R(string.char(first, last)) end
  return lpeg.Cmt(lpeg.P(1), function(subject, position)
    local start = position - 1
    local byte = subject:byte(start)
    local width, value
    if byte < 128 then width, value = 1, byte
    elseif byte >= 0xc2 and byte <= 0xdf then width, value = 2, byte - 0xc0
    elseif byte >= 0xe0 and byte <= 0xef then width, value = 3, byte - 0xe0
    elseif byte >= 0xf0 and byte <= 0xf4 then width, value = 4, byte - 0xf0
    else return false end
    for index = 1, width - 1 do
      local continuation = subject:byte(start + index)
      if not continuation or continuation < 0x80 or continuation > 0xbf then return false end
      value = value * 64 + continuation - 0x80
    end
    if (width == 2 and value < 128) or (width == 3 and value < 0x800) or
        (width == 4 and (value < 0x10000 or value > 0x10ffff)) then return false end
    if value >= first and value <= last then return start + width end
    return false
  end)
end

return lpeg
