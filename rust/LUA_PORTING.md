# Lua compatibility and remaining acceptance work

The optional `lua` feature now runs genuine configuration and plugins around the
same Rust editor/workspace. This is an implemented integration, with remaining
compatibility work; it is not yet full extension-API or editor parity. The sources
of truth remain `vis-lua.c`, `lua/vis.lua`, `lua/vis-std.lua`, bundled plugins and
original `test/lua` / `test/vis` fixtures.

## Implemented boundary

`mlua 0.11.6` supports the crate's Rust 1.85 minimum and Lua 5.5. The interpreter
loads only the safe standard libraries (`Lua::new_with(StdLib::ALL_SAFE, …)`,
the same set as `Lua::new()`): arbitrary C-module loading, `package.loadlib`
and the `ffi` library remain unavailable. mlua refuses to open `debug` from its
safe constructor, and the unsafe constructor the crate forbids is the only
alternative. The runtime therefore installs a curated `debug` table instead of
the native one: `debug.getinfo` and `debug.traceback` are served by the
interpreter's own stack inspection through mlua's safe `inspect_stack` and
`traceback` calls, so configuration that reports its own source location (for
example the vis-lspc plugin) keeps working. The remaining `debug` members stay
absent, and `ffi` and C module loading stay unavailable, which
`unsafe_libraries_stay_unavailable_to_configuration` and
`the_curated_debug_library_reports_source_locations` pin.
The vendored Lua VM and mlua dependencies contain native/unsafe implementation;
our Rust code continues to forbid unsafe code. `--no-default-features` still has
zero Cargo dependencies; `--features lua` explicitly opts into the interpreter.
On Unix, Lua process integration also reuses the existing safe `rustix` interface;
this adds no new crate to the default terminal configuration. The original C
editor implementation is never linked into the Rust executable.

The runtime owns `Rc<RefCell<Workspace>>`; userdata holds weak references and
stable, checked file/window/selection IDs. Cached file/window userdata preserves
identity and user fields. Selection lookup creates a fresh wrapper and separate
user fields, matching C's light references, while retained handles still resolve
their checked logical selection. Mutable `vis.types` tables retain native lookup precedence. Selection
identities survive sorting, focus, direct setters and ordinary native edits;
removed/closed objects are invalidated. Invalid byte marks can revive after undo.
Anchored state is independent of geometric collapse and global mode.

Window iteration follows the native newest-first list, while one-line prompts
move to its tail. Focus changes preserve the global mode, unfinished count and
operator. Shared splits expose a fresh window to WIN_OPEN before copying the
source primary cursor, UI flags and local mappings; secondary selections and
window options are not copied. Global options and window options have separate
native lookup scopes, with arbitrary user fields retained beneath that lookup.

Configuration loads `visrc` with the original empty-versus-standard profile
semantics. INIT precedes command-line files, FILE_OPEN precedes WIN_OPEN, and
START follows files and their startup commands. Owned host plans release every
workspace borrow before invoking Lua. Close notifications precede invalidation;
reentrant opens cannot collect a pending file. Reload and file assignment retain
window and primary-selection identity. Internal prompt/message windows stay outside the public window iterator and
notifications; `vis:files()` includes their persistent internal files, matching C.

FILE_SAVE_PRE captures the target independently of focus, permits edits, and
recomputes whole-file writes afterwards. False/errors veto ordinary writes;
force bypasses the veto. Callback edits survive vetoes, and POST runs only after
successful writes. Native marks and window-local last-visual marks are shared
with the Lua interface rather than kept in a separate shadow model.

Binary file content, lines, mutations, snapshots, marks, coordinates, selections,
options and window objects are live. Registered commands/options/actions,
function mappings with negative pending-input results, motion/operator/textobject
callbacks, reentrant feedkeys and binary pipe overloads are implemented. Lua
feedkeys uses the original incremental, non-recording input path. The renderer
receives measured viewport geometry, actual style ranges/cells and status text.
Synchronous children receive foreground terminal ownership and canonical input;
foreground, input state, alternate screen and queued input are restored afterwards.
A narrow optional `nix` signal-mask wrapper blocks SIGTTOU safely during handoff.
It is a terminal dependency, not part of the dependency-free core.

Drawing records ordered range, cell and status operations with callback-time
style snapshots, core-overlay and body-copy boundaries. Inclusive Lua style
endpoints, independent colors and attribute inheritance follow the native UI.
Focused cursors use distinct primary/secondary styles; selection visibility also
includes a focused prompt's parent. Headless C/Rust comparisons cover 90 painted
cells and 961 style parse/merge cases. These are bounded checks: retaining C's
dirty view-style state across frames is still open.

Real internal prompt windows now edit their persistent shared history Text in
ordinary modes. Parent modes/action contexts restore without losing queued keys;
syntax/validation failures stay editable, valid no-match searches close, older history
lines move on successful execution, and reentrant callbacks close only their
original prompt. Their display flags support a full-width one-line prompt,
expanded history without status, actual modal cursor positions and global info.
The identical prompt and command-result assertion scripts are checked against C
with mandatory success markers, then against Rust; C START exit status alone is
not a valid gate. Lua command booleans preserve C's parsing/validation/transcript
status: a recognized handler can report failure and still return true. The strict
Rust `Workspace::command` and Sam APIs continue to return execution errors and
roll back queued edits. Write vetoes still prevent writes and POST events,
independently of the legacy Lua acceptance boolean.

Lua callback exceptions open and focus the persistent internal diagnostic
window. Pending input resumes against that focused window; errors do not become
ordinary info-row messages. A guarded reporting path handles initialization,
events, mappings, registered commands and save hooks without retaining workspace
borrows. A shared C/Rust contract checks focus, resumed insertion, window reuse
and save vetoes. `Runtime::take_lua_errors` supplies a separate diagnostic audit.

`communicate` now owns asynchronous nonblocking stdin/stdout/stderr, bounded
buffers, process response events and cancellation without worker threads.
Synchronous pipes share that supervisor. Leaders remain reserved with
`waitid(NOWAIT)` through group cleanup to prevent PID/PGID reuse before signals;
TERM-resistant descendants, closed pipes and early leader exits have focused
coverage. Global Lua instruction hooks allow interrupting busy configuration and
coroutines, with CLI signal handlers installed before configuration.

## LPeg without native extension loading

Pinned, licensed LuLPeg supplies a pure-Lua matching engine. A safe mlua userdata
adapter preserves the pattern type expected by unmodified lexers; official LPeg
1.1 `re.lua` is preloaded alongside it. Audited fixes cover grammar context,
captures, factoring, accumulator captures, UTF ranges and pattern validation.
See [`vendor/lpeg/PROVENANCE.md`](vendor/lpeg/PROVENANCE.md) for exact upstream
hashes, licenses, patches, resource limits and known semantic limits.

All 160 bundled lexers load and lex in the focused tests. The reproducible native
LPeg differential compares 373 capture/grammar/lexer/numeric cases through the
actual mlua adapter. Passing this bounded corpus does not prove universal LPeg
VM equivalence: stack accounting, locale, work limits and protected metatables
remain explicit differences.

## Remaining work

Complete the original native action/option behavior and remaining view options
and workspace structural commands. Language translation and window-local mapping
commands are implemented with their physical-input and scope rules. A raw input pump for
sessions without a controlling terminal remains distinct from the verified
foreground/canonical terminal protocol. Verify startup geometry, persistent redraw
styles, remaining automatic option overlays, file-name mutations and argument/error semantics
against C. Passing the original eight Lua scripts is not complete API coverage.

The native `debug` library stays outside the safe boundary. The curated
`debug.getinfo`/`debug.traceback` subset covers the common plugin uses; the
remaining members (hooks, local and upvalue access) are absent, so a plugin
that needs them is rejected with an explicit error rather than silently
misbehaving. `debug.getinfo` with a function argument is not served, because
the safe surface cannot locate a function in the call stack.

The safe supervisor explicitly returns Unsupported on targets whose available
safe OS interface cannot reserve an exited leader (`WNOWAIT` unavailable). That
portability work remains open. Communicate userdata is a writable stream;
both `io.input(stream)` and `io.output(stream)` retain it as the selected default.
Reads from its write-only pipe report EBADF and seeks report ESPIPE after native
argument validation. Sequential writes retain earlier valid arguments when a
later argument fails. Real C/Rust PTY contracts cover stream routing, default
restoration, buffering and result arity. Buffered-capacity failure diagnostics
can differ from libc's ambient errno. Resource limits are explicit in
`src/lua/process.rs`.

Keep the strict key differential's failures visible. The new Lua replay gate
executes original fixture callbacks and compares actual saved output as well as
every selection, orientation, primary cursor and mode before/after Escape. It
exposed prompt-object and plugin dot-repeat gaps that core-only replay did not
exercise; those findings drove the actual host/core fixes. Neither skipped fixtures nor known mismatches count as passes.

## Reproduction

```sh
cargo test --manifest-path rust/Cargo.toml --all-features --all-targets
cargo build --manifest-path rust/Cargo.toml --features lua --examples --bins
(cd test/lua && VIS="../../rust/target/debug/vis-rs" sh test.sh)
python3 rust/tests/key_differential.py --lua --all
python3 rust/vendor/lpeg/differential.py
```

The last command needs the pinned upstream archives already present in
`.package-cache`, a Lua 5.5 executable and a C compiler; it downloads/installs
nothing and runs the native oracle only in a test subprocess. The external
corpus test is intentionally separate from normal Cargo tests and reports as
one ignored external gate there.
