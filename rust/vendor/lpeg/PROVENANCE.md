# Pure-Lua LPeg backend

The optional Rust Lua host keeps `mlua::Lua::new()` safety restrictions and C
module loading disabled. It preloads this implementation as `lpeg` and `re`.
The matcher and capture evaluator are Lua; Rust supplies safe pattern userdata
and forwards constructors/operators through the already-used mlua API. No new
Cargo dependency, native LPeg binary, runtime download or `unsafe` block is used.

## Source and licenses

- `lulpeg.lua`: Pierre-Yves Gérardy's [LuLPeg](https://github.com/pygy/LuLPeg),
  pinned to commit `f07f5be09d0461b1e83a8f811ca2c9cb79a69ab2`.
  The original file's SHA-256 is
  `387a5923de54d27f4969f86845d579ad9d057da8db5a90f872c28bc050ebae55`.
  `vis.patch` records all changes against that exact upstream file.
- `LICENSE` is the upstream Romantic WTF Public License, including the MIT
  notice for upstream re and tests. Its original SHA-256 is
  `78e4d3f4786fc3d51854f42cbcf235660b07136ab213e7abcf0e85e970ececdb`.
- `UPSTREAM_README.md` and `upstream-0.12-test.lua` are unmodified from the same
  commit. The latter's SHA-256 is
  `feabb2e29de6e7fa12efed2a4cb09e7127be97151d6f9602764aa0b0affd8f7b`.
  It documents the old baseline; it is not presented as a passing LPeg 1.1 gate.
- `re.lua` is the unmodified module from Roberto Ierusalimschy's official
  [LPeg 1.1.0 archive](https://www.inf.puc-rio.br/~roberto/lpeg/lpeg-1.1.0.tar.gz),
  already cached by this project. SHA-256:
  `528f74b290e0fccde47eb6ce322338e51275fd71886d1484b745cf46d01d2511`.
  Its MIT license is preserved in `LICENSE-LPEG`.
- `compat.lua`, the Rust adapter and differential harness are project code under
  the repository's ISC license.

## Audited changes

LuLPeg upstream implements LPeg 0.12, so simply aliasing `require('lpeg')` is
insufficient. The adapter adds LPeg 1.1 accumulator captures (`patt % function`),
UTF-8 range patterns, the string-valued version field, Carg's current index
range, grammar validation and explicit resource bounds. Official LPeg 1.1's
`re.lua` provides its matching current grammar, including accumulator syntax.

The C differential uncovered and now tests these additional upstream defects:

- compiled captures leaked nonterminal bindings across grammar scopes, causing
  JSP and Ruby lexers to use tags from earlier lexers;
- distributing ordered choice over repetition, negation or match-time captures
  changed matches, including Markdown, reStructuredText and YAML whitespace;
- false table-capture values were discarded;
- match-time captures accepted backward or fractional positions;
- missing factorizer constructor aliases broke C++ and YAML loading.

Pattern userdata is necessary for existing `lexer.range` callers that distinguish
patterns from option arguments using `type(value) == 'userdata'`. The underlying
table is stored as the userdata's Lua-visible uservalue so Lua can trace cycles.
The original global `type` and `getmetatable` functions are unchanged. Official
`re.lua` receives a private environment exposing the safe operator forwarding
functions; mlua's actual userdata metatable remains protected.

## Verification

`cargo test --manifest-path rust/Cargo.toml --features lua lua::lpeg --lib`
checks safe Lua 5.5 loading, captures, grammars, ordered choice, numeric search
patterns, UTF-8/binary cases, malformed grammars and all 160 bundled lexers.

`python3 rust/vendor/lpeg/differential.py` builds a temporary native LPeg 1.1
oracle using the project's cached LPeg and Lua header archives. It runs the same
corpus through that native oracle and the actual safe mlua adapter, comparing
capture values, every tag and byte position from all 160 lexers (generic input,
the first 4096 bytes of each lexer's source, plus language-specific examples),
the numeric plugin's search pattern, error acceptance and `re` behavior.
The corpus currently has **373 cases; all match**. C code is compiled and loaded
only in the test subprocess. Archives are SHA-256 checked; each subprocess has a
timeout. No network access is used by this verifier.

## Explicit compatibility limits

These tests cover the project's shipped behavior, not every possible LPeg
program. The backend is intentionally identified as LuLPeg in `lpeg.version`.

- `setmaxstack` bounds active Lua grammar rule calls, not C LPeg VM backtracking
  instructions. The accepted range is 1..8192, default 400; bundled Scintillua
  requests 2048. Tail-recursive grammars can reach this bound sooner than the C
  VM. Ten million matcher-node steps per match bound pathological work. Errors
  are propagated; matching or lexing is never silently disabled.
- Named character classes use the C locale. Process locale changes do not alter
  them. Binary patterns match bytes; `utfR` decodes only its own UTF-8 input.
- Error messages, GC timing, pattern identity across separately constructed
  patterns and access to the native debug metatable can differ. `P(pattern)`
  preserves identity. `ptree` and `pcode` raise the original release build's
  "function only implemented in debug mode" error.
- The unchanged upstream 0.12 suite reports known error/stack/GC differences in
  its README. No claim is made that all upstream assertions pass under this
  newer bounded host; use the recorded semantic corpus as the current gate.
