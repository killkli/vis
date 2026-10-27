# Rust port

The target is the complete vis editor: structural regular expressions, modal
editing, multiple selections, terminal UI, registers, undo history, file handling,
tools, configuration and extension support. This directory is a staged source
port, not yet a replacement for the C executable. The existing C sources and tests
remain the behavioral reference throughout migration.

## Design

* Byte offsets and binary-safe storage preserve the original editor's file model.
* An owned piece table shares immutable byte blocks across branching revisions.
  Rust ownership replaces raw pointers and manually managed lifetimes. Immutable
  read snapshots also share bytes; conflict checks and saves stream through pieces
  instead of allocating another complete document buffer.
* Core modules use the standard library only. `unsafe_code = "forbid"` is enforced
  by Cargo and the crate. The optional Unix terminal feature uses `rustix` for
  safe OS interfaces, `signal-hook` for deferred signal handling, and a narrow
  `nix` signal-mask interface for foreground job control; their low-level
  implementations remain outside this crate's unsafe prohibition. No original C
  editor implementation is linked into the Rust executable. The optional `lua`
  feature uses safe mlua bindings to a vendored Lua VM; third-party native/unsafe
  implementation remains outside this crate. Pure-Lua LPeg avoids adding a native
  extension-loading boundary. Core builds still have no Cargo dependencies.
* Parse structural scripts before execution and collect changes against the
  original document before committing them. Invalid scripts and overlapping
  changes must not partially edit the buffer.
* Retain upstream fixtures and add differential checks as each subsystem becomes
  executable. A passing core suite does not establish editor feature parity.

## Work and acceptance gates

1. Text storage, marks, branching undo/redo and chronological history: test the
   behaviors in `test/core/text-test.c`, binary data and randomized edit sequences.
2. Text motions, objects and regex: test Unicode, invalid UTF-8, line boundaries,
   POSIX leftmost-longest matching, captures and bounded pathological input.
3. Structural commands and file I/O: run the existing sam fixtures, including
   command groups, empty matches, address arithmetic, shell pipes and errors.
4. Editor state: port modes, operators, counts, repeat, registers, marks, mappings
   and selection algebra; execute existing `test/vis` and `test/vim` key fixtures.
5. Terminal: implement input decoding, rendering, resize, suspend and restoration;
   validate real interactions under a PTY, including abnormal exits.
6. Extensions and tools: port Lua-facing behavior/configuration, syntax highlighting,
   digraph/menu tools and helper integration. Decide any unavoidable dependency
   from actual compatibility needs; do not silently remove extension behavior.
7. Release: update default build/install/CI and packaging only after full gates;
   verify supported platforms, file metadata and terminal behavior. Document any
   remaining gaps and do not call the port complete while they remain.

## Current status

Implemented: binary-safe piece-table storage, stable byte marks, grouped branching
history, motions and objects, bounded ERE matching with captures, structural
command groups and address arithmetic, Unix shell pipes, file reads and saves,
a modal editor state machine, and a Unix terminal backend. Normal, insert,
replace, visual and command modes, operators, registers, counts, repeat and
search are available, but key compatibility remains incomplete.

The host now supports multiple command-line files, horizontal/vertical splits,
independent marked cursors into shared buffers, global registers with independent
file histories, per-window tab/expansion options, global autoindent, read-only saves, file reload/open,
quoted paths, startup commands associated with their following files, and stdin
buffers whose `:wq` writes back to stdout. Named-buffer `:w COPY` retains its
original filename. Visual/addressed writes stream a projected byte snapshot
without changing the in-memory document.

Sam sessions retain regex/shell registers and per-document marks, support guard
count/modulo filters, and release closed-file namespaces. Text timestamps and
time-based restore match C duplicate-time, tie and branch semantics.
`:earlier`/`:later` accept timestamp units and native argument validation;
the editor now reports the selected revision's local `HH:MM` time using a
standard-library-only TZif/POSIX time-zone reader.

Oriented selection sets, stable selection marks and range algebra are implemented.
The editor supports simultaneous insertion, paste, motions, find, delete/change/
yank, shift, join, text objects, registers and undo at multiple cursors. Remaining selection
bindings and some combinations still need migration and differential coverage.

Per-window native jump lists are implemented: `gs`, `g<`, `g>`, the
`vis-jumplist-*` actions, JUMP motions (`G`, `gg`, `{`/`}`, `(`/`)`, `%`, `*`/`#`,
`n`/`N`), visual-mode leaves and completed `:` commands record entries. Navigation
restores the stored mode and geometry with the native orientation-preservation,
first-primary and no-normalization rules, and separate windows keep independent
rings across focus changes. Visual `o` flips every selection, as `vis-selection-flip`
already did.

The remaining non-viewport native actions are routed: cross-line
`to_left`/`to_right`/`till_left`/`till_right` character searches (including their
`;`/`,`/`totill` repeats), selection case operators (`gU`, `gu`, `g~` and the
`vis-selections-case-*` actions) with Unicode simple case mapping, and word
searches through the native action names. Wrapped word searches can now match at
position 0, so `n`/`*` from the document start behave like native search.
`vis-suspend` and `<C-z>` now route through the host plan and suspend the
interactive job with the terminal restored; configurations can shadow the
binding with a normal mapping. Screen-line motions and the window-line
`top`/`middle`/`bottom` motions use the soft-wrap cell model plus the body
geometry the UI publishes for every drawn window, keeping the preferred
display column across native actions. `H`/`L`, `gj`/`gk`/`gm`/`g$` and operator
combinations now reach these actions; `M` keeps the selections-restore binding,
which wins over the motion table in the native default order. Window motions
extend operator ranges only to the line start of the destination row (no
`LINEWISE_INCLUSIVE`), matching `dH`/`dL`. Until the first frame is drawn the
motions use native vis's 80-column and 23-body-row no-terminal defaults.
`zt`/`zz`/`zb` and `vis-redraw` request a one-shot first frame row (top, center
or bottom of the cursor row). The page, halfpage and slide actions move the
view and the cursor together like native `view_scroll_page_up`,
`view_scroll_halfpage_down`, `view_slide_up` and their counterparts, and a
cursor that leaves the visible body is centered on the page like native
`view_cursors_to`, while line motions keep scrolling the view row by row.

The view is addressed by a byte, like the native `view->start`: a view may
begin in the middle of a long line, and its first row is cut at that byte. The
start of the view is kept on a text mark, like native `view->start_mark`, so
edits above the view keep the visible text in place instead of moving the
window. The anchor travels with the window while it is unfocused and is
dropped when its line disappears.

A line motion that moves the cursor out of the view scrolls it the way the
native editor does: the view is scrolled up by its whole height and then down
one row at a time until the cursor is on the last row, like native
`view_redraw_bottom`. Every other motion moves the start of the view to the
cursor and scrolls it back by half a screen, like native `view_cursors_to`.

The width of a character is the width the system reports for it, so the
`wcwidth` of the locale the native editor runs in decides it. A character
that is not wide is merged into the character before it, as the native editor
merges it, and the cell holds at most six bytes, as the native editor's cell
does.

The native `ignorecase` option is wired: the search matches the ASCII letters
regardless of case, like native `REG_ICASE` in the C locale. The named classes
keep the case of the input, and the option reaches the search through the
`:set` command and the Lua option API.

The native `loadmethod` and `change256colors` options are accepted and
reported as well. This port always reads a file as raw bytes, so both load
methods behave the same here, and the alternative color palette is not
implemented.

The native `showspaces`, `showtabs` and `shownewlines` options are wired: the
renderer draws the native replacement symbols (a middle dot, a single right
pointing angle quotation mark and a downwards arrow with corner leftwards) for
the spaces, the tabs and the line endings, like `view_expand_space`,
`view_expand_tab` and `view_expand_newline`.

The native `breakat` option is wired: the characters that may end a line keep
a word together when it does not fit, like the native `view->breakat`. The
core cell model places the cells of a logical line and wraps at the last break
character, so the motions and the rendering agree on the layout.

The remaining native window options are accepted and reported: `cursorline`,
`colorcolumn` and `wrapcolumn`. The `wrapcolumn` option wraps the text at the
smaller of the window width and the option, like native
`view_max_text_width`; the native `cursorline` and `colorcolumn` options are
stored but never drawn by the native editor either.

The native `numbers`, `relativenumbers` and `numberwidth` options draw the
line number sidebar: the number of every visible row, right aligned in front of
the text with one separating space, with the distance to the cursor line for the
relative variant. The sidebar takes its width from the visible line numbers,
like the native editor, and the body is laid out in the remaining columns. The window scoped options
(`statusbar`, `showeof`, `oneline` and the number options) are applied by the
host, which owns the window display, so `:set` reaches them again.

The display widths follow the Unicode character database instead of a coarse
approximation: the generated tables list the zero width and the wide
characters, which fixes several thousand code points that the previous ranges
guessed wrong.

The native `shell` and `escdelay` options are wired: external commands run
through the configured shell (the environment `SHELL` and then `/bin/sh` are
the fallbacks) and the escape delay decides how long an incomplete escape
sequence is collected before it is read as a plain escape key. The `:set`
command now follows the native `set option [value]` shape, so a value may
follow the option name or be attached with `=`.

The native default key tables are wired. A host test parses `config.def.h`,
resolves every one of the 208 bindings to the key sequence it stands for and
drives a fresh editor in the mode the native table applies to, so a missing
default key fails the suite. The bindings added last are the bracket and
parenthesis motions (`[{`, `]}`, `[(`, `])`), the byte and column motions
(`go`, `gH`, `gL`, `g|`), the history jumps (`g-`, `g+`), the selections trim
(`_`), suspend in every mode (`<C-z>`) and the readline and window bindings in
insert mode (`<C-a>`, `<C-e>`, `<S-Left>`, `<S-Right>`, `<PageUp>`,
`<PageDown>`, `<S-PageUp>`, `<S-PageDown>`, `<C-x><C-e>`, `<C-x><C-y>`).

The vis-rs terminal backend restores settings and screen on normal exit, error,
catchable termination signals at event-loop boundaries, and suspend/resume.
Running shell process groups respond to external SIGINT/SIGTERM with bounded
TERM/KILL escalation and pipe cleanup. Foreground terminal handoff restores
canonical input for synchronous children and permits physical Ctrl-C. When stdin
is not a terminal and was not consumed as document data, input is read directly
from the stdin pipe and frames are written to stderr; EOF reopens `/dev/tty` or
fails like the native editor. When stdin was the native `-`/scratch document,
keys come from the available tty instead of the drained pipe. Bracketed paste
stays literal; file bytes, paths and messages cannot inject terminal control
sequences.

Optional Lua configuration, live userdata, reentrant registered callbacks,
function mappings and syntax styles are now implemented; see
[`LUA_PORTING.md`](LUA_PORTING.md) for their bounded verification and remaining
API gaps. Stable logical selection identities and per-window last-visual marks
now survive focus and undo. Real modal prompt windows, mixed editable history
and recursive aliases are also implemented. Still incomplete: option bindings
beyond the fixture corpus and some pending-operator combinations. The visual
mode key table, visual put over an empty register and
linewise yank now follow the native editor (see the visual mode checkpoint
below); paste dot-repeat matches the native editor in the key differential;
`:earlier`/`:later` now display the selected revision's local state time
(see the history notification checkpoint below); and the native `breakat`
and `wrapcolumn` wrapping now matches the C coordinate oracle (see the
custom wrapping checkpoint below). Default build/install/
packaging migration and remaining helper integration are pending. Core piece
lookup/editing is linear in piece count; UI frames still flatten buffers and
repaint visible cells. Existing C builds remain the default.

Language-key translation runs on physical input before macro recording and mode
bindings, with literal insert/replace input and unmodified replay semantics.
Fourteen C/Rust PTY cases verify the mapping, pending literal arguments, truncated
mapping pairs, modifiers and duplicate definitions.

The renderer implements default soft wrapping, configured tab stops,
native `breakat`/`wrapcolumn` wrapping, narrow panes and visible EOF cursors.
Its C-coordinate differential now drives custom break characters and wrap
columns as well, separately reports 1,111 known C EOF/tab-coordinate
discrepancies, and never counts them as equal. This is not a claim of complete
view parity.

`vis-digraph-rs` includes the complete 1,894-entry original table and preserves
lookup order, output and exit codes. Piped input/listing are dependency-free;
interactive raw input uses the existing optional terminal dependency.

`vis-menu-rs` ports candidate ranking, flags, inline terminal interaction and
accept/cancel behavior. Its matching core is dependency-free; interactive mode
uses the same optional OS/signal crates. The query limit is 64 KiB rather than
the original platform-dependent `BUFSIZ`; long candidate lines stay intact.

Known compatibility differences and limits:

* Evaluation-time command failures (shell/read errors, invalid marks or
  addresses) stop the explicit group but are accepted like `vis:command`:
  earlier transcript effects commit and the failure becomes a message. Parse,
  validation and transcript-conflict errors remain atomic. The strict Rust
  `Workspace::command` path still returns the evaluation error without queuing
  its transcript; startup commands and prompt entry use native acceptance.
  External shell effects themselves cannot be rolled back.
* Regex matching uses C-locale named classes, rejects unsupported collation and
  backreferences, and has explicit pattern/program/work/capture limits. Long
  near-miss searches may reach the work limit. Empty regex is accepted by the
  Rust API; macOS libc rejects an empty ERE. This corpus is not complete POSIX
  certification.
* Auto-save writes existing files in place to retain links and metadata. A failed
  in-place write can leave partial data. Explicit atomic replacement preserves
  Unix owner/group/mode, rejects linked files, and does not copy ACLs/xattrs.
  Conflict checks are best effort, not filesystem locks.
* The native `breakat` option wraps a line at the last break character; the
  word itself may overflow the window width, like the native editor.
* The native `cursorline` and `colorcolumn` options are accepted and
  reported but not drawn, like the native editor, which stores them without
  using them.
* Display widths follow the Unicode character database: characters in the
  combining and format categories occupy no column, characters with an East
  Asian width of `Wide` or `Fullwidth` occupy two. The tables are generated
  from one Unicode version, while the native editor asks the system locale,
  so a few code points differ from a system with a different Unicode version.
* A cursor that jumps outside the measured body is centered by half a page,
  like native `view_cursors_to`, while native line motions keep scrolling the
  view row by row and leave the cursor on the last or first visible row.
* After a wrapped multi-cursor word search, C lists the selections in a
  different order from the Rust set. Both expose the same selections and the
  same primary cursor, but ordered comparison (as in the native-action
  differential) can report the list index difference; such a case is kept out
  of the passing corpus rather than forced.
* Native word search treats the word literally; C interpolates it into a
  `[[:<:]]…[[:>:]]` expression, so a word containing regex metacharacters is
  matched literally here. Character case mapping keeps a character unchanged
  when its Unicode mapping would change the byte length, where C leaves the
  selection to its length-preserving `mbstowcs`/`wcstombs` round trip.
* The Lua `debug` and `ffi` libraries stay unavailable: mlua exposes them only
  through its unsafe constructor and this crate forbids unsafe code.
  Configuration that calls `debug.getinfo`, such as vis-lspc, fails while
  loading; see [`LUA_PORTING.md`](LUA_PORTING.md).

Run the interactive editor or a batch transformation:

```sh
cargo run --manifest-path rust/Cargo.toml -- path/to/file
cargo run --manifest-path rust/Cargo.toml -- first-file second-file
printf 'Emacs vi\n' | cargo run --manifest-path rust/Cargo.toml -- --command ',x/Emacs/ c/vis/'
```

For a standard-library-only core and batch executable, build with
`--no-default-features`. Interactive mode requires Unix and the default `terminal`
feature. Shell commands require Unix independently of this feature.

Run local checks with:

```sh
cargo test --manifest-path rust/Cargo.toml
cargo clippy --manifest-path rust/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path rust/Cargo.toml --check
cargo test --manifest-path rust/Cargo.toml --no-default-features
cargo +1.85.0 test --manifest-path rust/Cargo.toml --locked
```

Reproducible headless compatibility checks (Python standard library and native C
and Rust compilers; no Python packages):

```sh
cargo build --manifest-path rust/Cargo.toml --examples
python3 rust/tests/motion_differential.py
python3 rust/tests/regex_differential.py
make vis
python3 rust/tests/differential.py
python3 rust/tests/key_differential.py
python3 rust/tests/key_differential.py --all
cargo build --manifest-path rust/Cargo.toml --features lua
python3 rust/tests/jumplist_native.py --rust-vis rust/target/debug/vis-rs
cargo build --manifest-path rust/Cargo.toml
python3 rust/tests/terminal_smoke.py
python3 rust/tests/workspace_smoke.py
python3 rust/tests/subprocess_smoke.py
python3 rust/tests/terminal_views.py
python3 rust/tests/history_differential.py
python3 rust/tests/sam_session_differential.py
python3 rust/tests/view_differential.py
python3 rust/tools/generate_digraph.py --check
python3 rust/tools/generate_digraph.py --test
python3 rust/tests/menu_differential.py
```

Full sam/key sweeps deliberately fail while differences or unsupported features
remain. Unit tests and selected key fixtures passing do not establish full parity.

## Verified checkpoint (2026-10-25, macOS arm64, second integration)

* All 161 default-feature Rust tests pass; default and dependency-free configurations pass strict
  Clippy. Formatting, Rust 1.85 tests and release builds pass.
* Real PTY gates pass: ten terminal scenarios, nine multi-file/window scenarios,
  two live-subprocess interruption scenarios and seven native-key/view scenarios.
  These cover SIGINT without losing edits, interrupted/oversized paste, file
  conflicts, stdin/stdout, shared splits, termination and suspension.
* Differential checks pass: 25,200 motion/object values, 1,863 regex cases,
  371 digraph cases, 336 menu ranking cases and 16 menu PTY cases.
* History: 92 C/Rust states match. Persistent Sam/count/mark cases: 56/56 match.
* Complete key sweep: 108 pass, zero mismatches/errors, four exclusions: two
  Lua pre-save fixtures, one Lua lexer/syntax fixture and one input disabled
  upstream. The verifier now loads authoritative configuration profiles, asserts
  genuine before/after-Escape observations, and uses native PTYs for macro
  recording. It compares every selection/direction, primary cursor and mode.
  This supersedes the earlier observer-only checkpoint, whose `test/vis`
  before-Escape observation was incomplete.
* View-coordinate corpus: 20,234 equal, 1,111 separately classified C quirks,
  zero unknown differences. Coverage is bounded and excludes complete locale/
  Unicode certification.
* Complete sam sweep: 28/30 match; the two documented failure-handling differences
  remain visible. Linux aarch64 all-target cross-compilation checks pass; Linux/
  Windows runtime CI is configured but has not run in this local verification.

Local command logs and machine-readable results are under
`target/validation/phase2/`. This is a working checkpoint, not full-port acceptance.

The next integration implements the Lua lifecycle/userdata/callback bridge and
safe LPeg strategy described in `LUA_PORTING.md`. Remaining original action,
prompt, mapping, input-pump and view semantics above still require work. Audit file metadata/atomic policy, Unicode
and large-file performance before release integration. Passing the current
fixture corpus does not replace the complete-project acceptance gates above.

## Lua integration checkpoint (2026-10-25, macOS arm64)

The optional Lua integration passes 238 all-feature/all-target tests on both the
current compiler and Rust 1.85. One external LPeg corpus gate is separate from
normal Cargo tests; it was run explicitly and passes 373/373 cases across all
160 bundled lexers. Formatting and strict all-target Clippy pass. The
standard-library-only test configuration passes 176 tests and strict Clippy,
and its Cargo dependency tree contains only this crate.

The actual CLI passes all eight original `test/lua` scripts and seven additional
Lua lifecycle/stdin/config/style/fullscreen scenarios. The existing 28 PTY
terminal/workspace/signal/view scenarios still pass. Motion (25,200 values),
regex (1,863), history (92 states) and persistent Sam (56) differentials were
rerun successfully. Seventeen native lifecycle tests and three Lua reentrancy
integration tests cover pending opens, invalidation, identity, focused targets
and callbacks that close their origin.

Real Lua key replay now runs original callbacks and Rust digraph helpers. It
reports **110 pass, one failure, one upstream-disabled exclusion, zero errors**.
The failure is genuine command-prompt window identity; the original pre-save,
lexer, digraph/dot, native macro and intentional-error fixtures all execute.
The core-only sweep remains 108 pass with its four explicitly reported
exclusions. Both strict sweeps exit nonzero while these gaps/exclusions remain.

Logs, failure artifacts and a source hash manifest are under
`target/validation/phase3/`. These are bounded local checks, not evidence that
Linux/Windows runtime CI has run or that the complete port is finished. The next
work replaces string command mode with real internal prompt windows and mixed,
editable history, then closes asynchronous process and remaining action/view
semantics before changing default build/install/packaging.

## Prompt/process checkpoint (2026-10-25, macOS arm64)

The real Lua sweep now reports **111 pass, zero mismatches, one upstream-disabled
exclusion, zero errors**. The strict command exits nonzero for that exclusion;
it is never counted as a pass. Both prompt/history and command-result contracts
run unchanged against C and Rust, with explicit native success markers to avoid
C's START exit-status overwrite masking a failed assertion.

The latest minimum-Rust all-feature/all-target suite passes 291 tests with one
separate external LPeg gate. Strict Clippy passes. The expanded CLI gate passes
16 scenarios, including real controlling-terminal fullscreen/config/overlay reads,
physical Ctrl-C in Lua pipes and native Sam, background response events, and
foreground/termios restoration. Ten terminal, nine workspace and two external
subprocess scenarios still pass. Local logs are in `target/validation/phase4/`.

The Lua command adapter preserves native parse/validation/transcript acceptance;
recognized handler failures can return true after reporting a message. The strict
Rust command/Sam APIs still return execution errors and roll back edits. This
separation fixes original error/mapping fixtures without weakening save vetoes.

This is not complete native-surface coverage. The next checks cover global
mode/count state across window focus, actual ordered windows/split initialization,
all symbolic native action names, scoped option semantics, structural workspace
loops and the remaining motion/operator/view families. Existing C remains the
default build until those acceptance gates and platform/release work are done.

## Native surface work in progress

The named-action catalogue now tracks all 178 entries in C's `KEY_ACTION_LIST`,
with typed handlers and arguments rather than key-string aliases. Recognition
and implementation are separate: the current inventory reports 108 implemented,
70 conditional and no unsupported actions; every native name is routed.
Conditional entries are mode restrictions or the measured geometry
screen-line/window-line motions need a drawn frame first.
Bounded native-action cases match document bytes, every selection, orientation,
primary selection and mode before and after Escape, with no unsupported
requests; this does not establish coverage of all 178 actions. Unsupported
requested cases make the strict differential fail.

Linewise visual selections now store their full geometry in the editor rather
than expanding ranges during rendering from the global mode. The shared view
contract checks that switching focus preserves an earlier charwise selection.
Chronological commands support timestamp units and validate arguments before
changing history, including traversal across branches. They retain the native
plain-number count quirk; overflowing timestamp arithmetic reports an error
instead of inheriting signed-overflow behavior.

The dependency-free core remains dependency-free. Ordered drawing, Lua error
windows, process-stream compatibility and cross-file structural continuations
are under active integration; their completion requires fresh combined gates.

## Jump list checkpoint (2026-10-26, macOS arm64)

The per-window jump cache moved from a dormant module into the live host. Saves
now come from the same events as C: JUMP motions after they complete, visual-mode
leaves and visual operators through the shared window-selection path, `gs`/`g<`/
`g>`, the three `vis-jumplist-*` native actions, and the `:`-command epilogue
(which records the focused window after handlers such as `:split`). Navigation
switches mode first so a visual leave re-enters the ring exactly where C does,
then applies ranges with native orientation, anchoring and first-primary rules
without normalizing duplicates. Window focus moves the active ring into and out
of the window's saved view state.

Visual `o` now flips every selection; it previously flipped only the primary,
which also broke native orientation preservation on restore.

Verification: `rust/tests/jumplist_native.py --rust-vis rust/target/debug/vis-rs`
reproduces the C trace byte-for-byte across ring order, empty slots, EOF saves,
full-ring wrap, split seeding, mode/geometry restoration, deletion/undo marks,
duplicate normalization and multi-selection orientation. Five new host-level
tests in `rust/tests/jumplist_actions.rs` run in ordinary `cargo test`. The
all-feature/all-target suite, the standard-library-only suite, strict Clippy
(default and no-default features) and formatting pass; the key sweeps remain
108 pass/4 upstream exclusions (core) and 111 pass/1 exclusion (Lua), and the
native-action differential reports 118 pass with no unsupported cases.

## Native action completion checkpoint (2026-10-26, macOS arm64)

Cross-line character searches, selection case operators, native word-search
actions and `vis-suspend` are routed. The viewport operations are routed as
well (108 implemented, 70 conditional, 0 unsupported). The
native-action differential reports 118 pass, zero failures and zero unsupported
requests against C, covering the new find, case (ASCII, multibyte and invalid
UTF-8) and wrapped-search cases. Word-search helpers now accept a match at the
search origin, matching native wrapped search; the full key sweeps remain 108/4
and 111/1. All-feature and standard-library-only tests plus strict Clippy pass.
Wrapped multi-cursor word-search ordering and literal word search stay
documented above as open differences. The PTY gate now waits for the real INSERT-status transition instead of
a status word the native format never draws, and all ten scenarios pass with an
isolated configuration, including suspend/resume through the new action path.

## Viewport motion checkpoint (2026-10-26, macOS arm64)

The renderer's soft-wrap cell walk moved into `view.rs`, and rendering,
`screen_position` and the new row helpers now share it. The coordinate
differential is unchanged: 20,234 equal values and the same 1,111 documented C
quirks. Each drawn window publishes its body width, height, first wrapped row
and visible range into the window state and focused editor, so motion code uses
the layout the user is actually looking at.

Screen-line begin/middle/end/up/down and window-line top/middle/bottom are
implemented with native semantics: wrapped row starts and widths, wide-token
column clamping, the preferred display column preserved across native actions,
count handling (including `L`'s bottom-relative default of zero) and clamping to
the final EOF row. Before a frame exists the editor keeps native vis's 80x23
no-terminal geometry, so headless batch and Lua replay actions stay coherent.
The redraw actions (`zt`, `zz`, `zb`, `vis-redraw`) set a one-shot requested
top row on the editor; the workspace exposes it on the next frame and clears it
after rendering, so later cursor motions auto-follow again. The page, halfpage
and slide actions (`vis-window-page-up/down`, `vis-window-halfpage-up/down`,
`vis-window-slide-up/down`) follow native `view_scroll_page_up`,
`view_scroll_halfpage_down`, `view_slide_up` and `view_scroll_up`: they move
the view and the primary cursor together, and they are bound to `<PageUp>`,
`<PageDown>`, `<S-PageUp>`, `<S-PageDown>`, `<C-e>` and `<C-y>` like the native
default table. A cursor that leaves the visible body is now centered on the
page, matching native `view_cursors_to`, while line motions keep the native
row-by-row follow. The native inventory is now 108 implemented, 70 conditional
and no unsupported action.

Window motions clamp to the last row that holds source cells rather than the
virtual EOF cursor row, matching native `L` in short files.

Verification: the option tests in `rust/tests/global_options.rs`,
`rust/tests/numbers_options.rs` and `rust/tests/window_options.rs`, the anchor
tests in `rust/tests/view_anchor.rs`, the default
binding test in `rust/tests/default_bindings.rs`,
eleven host tests in `rust/tests/viewport_motions.rs` plus
view-module unit tests cover row lookup, counts, source-row clamping, wrapped
columns, wide tokens, the `H`/`L`, `gj`/`gk`/`gm`/`g$` and `zt`/`zz`/`zb`
bindings, the page, halfpage and slide geometry, the centered follow of
out-of-view jumps, one-shot scroll requests reaching frames, the renderer honoring and
clamping a request, and the no-terminal default geometry. Real PTY comparisons against C match for `L`/`H` on
short and full windows, `gj`/`gk`, `g$`, `gm`, `zt`/`zz`/`zb`, `dH`, `dL`,
`dj`, `dgj` and counted window motions. Workspace and terminal PTY scenarios pass 9/9 and 10/10
after replacing the harness's fixed post-Escape delay with idle detection.
Cargo tests in both feature configurations, strict Clippy and formatting pass;
key sweeps stay 108/4 and 111/1, and the native-action differential stays at
118 pass. The default key table is still incomplete for the remaining port
options, so other native actions remain reachable only through mappings, Lua and
symbolic keys.

## Visual mode key table checkpoint (2026-10-26, macOS arm64)

The visual mode key table now matches the native default bindings: `x`
aliases the delete operator and `s` aliases the change operator, so they
operate on the selection with the native linewise handling, while `D`,
`C`, `Y`, `S` and `X` are unbound and leave the selection untouched.
`u`/`U` run the selection case operators followed by Escape instead of
reaching history. Keys that no binding in the active mode chain matches
are silently discarded, as in `vis_keys_process`; this now applies to normal
and operator-pending modes as well, not only visual modes. Routed native
actions which are conditional or unsupported still report their own error.

`<C-d>`, `<C-u>`, `<C-j>` and `<C-k>` follow native
`ka_selections_navigate`: with a single selection they scroll half a
page (extending a visual selection like any motion), and only among
several selections do they rotate the primary cursor. The halfpage
scroll keeps an end-of-text cursor verbatim, as `view_cursors_to` does,
instead of rounding it back to the last byte.

The put operator deletes the selection before inserting, so pasting an
empty register over a selection removes it and returns to normal mode,
and a linewise register over a non-linewise selection continues on the
following line (`text_range_is_linewise`). Linewise yanks keep the
cursor on its position (`op_yank` returns `c->pos` for linewise ranges),
and leaving visual mode after an operator collapses the selection
ranges to the cursor endpoints like the native mode leave hook.

Verification: a 30-case visual/put/yank differential against the C
oracle covers `v`/`V` selections with `x`, `s`, `d`, `c`, `y`, the
unbound keys, `u`/`U`, counted variants, paste over selections with
empty and non-empty registers, and dot-repeat sequences such as
`VjYjp.`; every C-reachable case matches, and the feedkeys-only tokens
(`Delete`, `Del` and `Insert`) are reported unsupported by the differential
runner because the native oracle cannot express them as distinct keys;
the replay binary continues to decode those named keys. The strict key
sweep recorded 17 pass with zero mismatches on the default fixture set.
At that checkpoint, the full all-feature suite passed 291 tests, the
standard-library-only suite and strict all-target Clippy passed, and
formatting passed.
One documented notation difference remains: `<C-m>` reaches the editor as `Key::Enter`
because a terminal cannot send it as a distinct key, while the native
feedkeys notation discards it in normal mode.

## Local history notification checkpoint (2026-10-26, macOS arm64)

`:earlier` and `:later` now report the selected revision as
`State from HH:MM`, matching `cmd_earlier_later`'s `localtime_r` and
`strftime` notification. The standard-library-only `time` module reads
the configured `TZ` as either a POSIX rule or TZif file; when `TZ` is
unset it reads `/etc/localtime`. Systems without a usable zone source
fall back to UTC. TZif v1 and v2/v3/v4 transition tables and POSIX
footer rules are supported, including northern/southern DST, `M`, `J`
and zero-based Julian transition dates.

Verification: nine deterministic local-time tests cover fixed-offset
signs, DST transition boundaries, southern-hemisphere rules, Julian
rules, TZif transition tables and footers, a named zoneinfo path and
the host `/etc/localtime` compared with `date`. An interactive PTY
comparison with `TZ=Asia/Taipei` observed the same `State from HH:MM`
message from C vis and vis-rs after `:earlier`. An editor-level
contract test checks both `:earlier` and `:later` messages against the
selected revision. This does not provide Windows local-zone lookup
when `TZ` is unset; that environment falls back to UTC.

## Pending operator edge checkpoint (2026-10-26, macOS arm64)

Six additional C-observable pending-action edges now match. A `;` or
`,` with no prior character search is consumed without cancelling a
pending `d`/`c`/`y`, so the following motion still completes the
operator. A `c` whose resulting half-open range is linewise is treated
linewise even when the motion itself is charwise; newline insertion
matches native `vis_text_insert_nl`, including its two-newline EOF
case. Entering visual-line mode consumes the existing count before the
next motion, while Escape with a pending count clears only the count and
leaves characterwise visual mode active. Visual puts record the native
put action for dot-repeat rather than replaying the preceding visual
selection, and explicit named yanks no longer overwrite the unnamed
register. Counted empty-register `2VjP.` deletes the selected lines once
and leaves the remaining line intact on repeat.

Verification: an 80-case generated pending-operator/visual-count matrix
and a separate 35-case register/put/escape matrix match the C oracle
byte-for-byte. Six permanent differential built-ins and four editor unit
tests protect the new cases. Full key sweeps report 114 pass/4 explicit
core exclusions and 117 pass/1 upstream-disabled Lua exclusion; the
native-action differential reports 118 pass/0 unsupported. The final
all-feature/all-target suite reports 431 passed/1 ignored, the
standard-library-only suite 302 passed, strict all-feature Clippy and
formatting pass. Remaining pending-operator combinations are still
open; these bounded corpora do not establish full modal parity.

## Custom wrapping checkpoint (2026-10-26, macOS arm64)

The coordinate model now distinguishes the physical view width from the
effective text wrap width. C wraps source cells and the EOF cursor against
`view_max_text_width` (the smaller of the window width and `wrapcolumn`) but
reports end-of-text through the physical line table, so the two widths can
differ on the final row. `walk_cells_with_layout` streams cells with both
widths, places the EOF cell through the same breakat wrapping path as source
cells, and backs layout-aware row, line-number, screen-position and last-row
helpers.

The renderer, hardware cursor, overlay cursors, match-bracket highlights and
line-number sidebar use the layout-aware walker, so a `wrapcolumn` narrower
than the window no longer draws rows or cursors at the wrong width. Per-window
`wrapcolumn` travels with the published viewport metrics and is refreshed when
`:set` changes the option, so screen-line, window-line, scroll and view-anchor
motions measure the rows the user actually sees. Window options with no
viewport use the full body width, matching native defaults.

Verification: the C/Rust coordinate differential passes `breakat` and
`wrapcolumn` into both drivers and includes end-of-text positions. Five custom
cases cover hyphen/slash, colon/comma, underscore and wide-character breaks and
a multi-line input at three widths and two tab stops. The corpus reports 20,804
equal values, the same 1,111 documented C EOF/tab quirks and zero unknown
differences. A focused `view` unit test pins the reflowed EOF coordinates and a
terminal panel test pins the hardware cursor under `wrapcolumn`. The
all-feature/all-target suite passes (320 lib tests, all target suites), the
standard-library-only suite passes, strict all-target Clippy passes in both
feature configurations, formatting passes, the default key sweep stays 23/0 and
the native-action differential stays 118/0. Known limits remain: the corpus is
bounded, C locale width rules differ from the generated Unicode tables, and UI
frames still repaint visible cells.

## Native motion range checkpoint (2026-10-26, macOS arm64)

Three native motion rules now match the C editor. The `%` motion first tries
the bracket or quote under the cursor and otherwise scans forward for the
first bracket-like character, so `d%` before any bracket deletes through the
next bracket instead of doing nothing. Linewise operator ranges follow
`text_range_linewise`: the start moves to the line begin, the end only moves
past a line begin, and `LINEWISE_INCLUSIVE` motions (`j`, `k` and `G` without
a count) first extend the end by one character. Counted `G` is the plain line
motion, so `d3G` keeps its target line while `dG` deletes through EOF.
Paragraph motions `{`/`}` are linewise, so `c{` at the start of the text
inserts the native newline before the typed text. A missing text object leaves
the accumulated range (empty at the cursor for the first iteration) for the
operator instead of failing, so `ci(` and `ci"` enter insert mode at the
cursor like native `vis_do`.

Verification: a generated 1,200-case matrix crosses four buffers with five
operators, eighteen motions, three counts and ten text objects; it matches the
C oracle byte-for-byte and by before/after-Escape state (0 failures, 0
unsupported). Five permanent differential built-ins and an editor contract
test pin the new cases. The core key sweep passes 28/0, the Lua sweep 122/0
with one upstream-disabled exclusion, the native-action differential 118/0,
the view-coordinate differential 20,804 equal values with its documented C
quirks and no unknown differences. The all-feature suite passes 322 lib tests
plus every target suite, the standard-library-only suite passes, strict
all-target Clippy and formatting pass. The generated matrix is bounded and
does not cover every operator/motion/count combination.

## Operator and option semantics checkpoint (2026-10-26, macOS arm64)

The operator-pending and selection paths now follow the native tables more
closely. Normal and visual `M` restore the selection mark while the
operator-pending table reaches the window-middle motion; bracket prefixes,
`n`/`N` and `*`/`#` are available before operators. A `COUNT_EXACT` find or
till motion that does not move drops the pending operator (change still opens
insert mode at the cursor). Counted `G` and `gg` use the native line motion,
which lands on the first non-blank and keeps a line-begin range end, while
`gg` without a count is `FILE_BEGIN` and carries the inclusive flag. A single
motion target under an operator uses the operator range path instead of the
multi-selection batch path, matching the native linewise change at EOF.
Selection algebra computes the native range list first: an empty result clears
the remaining selection and its anchoring, and non-empty complements keep the
range byte-for-byte.

Insert mode also follows native put and readline rules: `<C-x><C-e>` and
`<C-x><C-y>` clear their prefix after the slide action, and `<C-r>` inserts a
linewise register at the line begin, appends a missing newline and repeats the
register for the insert count. Word searches only build the native
word-boundary expression for word-like objects, so punctuation searches leave
the pattern and cursor alone. The replay observer now reports the actual
anchored flag instead of the visual mode, which exposed and fixed the empty
complement differences.

The `:set` command accepts only the native grammar: one option name and at most
one whitespace-separated value. There is no `name=value` form and no `no` name
prefix; boolean options are disabled with a value or toggled with `!`. The same
rule applies to window options, structural commands and Lua-registered options.

Verification: a generated 732-case normal/visual operator matrix passes with
zero failures, a 4,608-case register/object/operator/count matrix reports 4,248
equal values with the 360 non-native `it`/`at` sequences excluded, and a
4,992-case motion/operator/count matrix reports 4,896 equal values with the 96
unbound `g^` sequences excluded. Insert and option matrices (54 and 32 cases),
plus focused fixtures for punctuation searches, counted line motions, failed
finds, insert registers, window slides, operator searches, named registers and the
`set` grammar, pass in the key differential. The core key sweep passes 139/0
with four upstream-disabled exclusions and the Lua sweep 142/0 with one. The
native-action differential, the 56-case Sam session differential, the view
coordinate differential and the sixteen PTY cases pass, as do all-target tests
in both feature configurations, strict Clippy and formatting. The matrices are
bounded; the native parser's redispatch of invalid key prefixes (for example
`d[}`), its unsupported visual repeat and bare-dot operator-pending states, and
non-native text objects are not reproduced.

## Structural acceptance checkpoint (2026-10-26, macOS arm64)

Structural evaluation now separates parse, validation and transcript conflicts
from command-level failures, matching `vis:command`. A failing shell command,
file read, invalid mark or invalid address stops the explicit group, keeps every
transcript effect queued before it and returns accepted status with a message;
an execution carries that diagnostic so the editor can display it after applying
the changes. An interrupted shell keeps its specific `shell command cancelled`
diagnostic instead of being replaced by the generic pre-commit cancellation
check. Conflicts and parse errors remain atomic and return an error.

The strict Rust host API keeps its previous contract: `Workspace::command`,
`Editor::execute_command` and the strict Sam mode reject evaluation errors
without queuing their transcript. Interactive prompts and startup `--command`
values use the native accepted path, while file-loop and host continuations
already classified evaluation diagnostics as messages.

Verification: all 30 structural differential fixtures now match C vis, including
`commands/pipe-out.cmd`, `commands/pipe-in.cmd` and `errors/read.cmd`; the
previous three fixture failures are gone. The library tests cover diagnostic
commit plus undo, strict rejection, register and stable-mark survival, shell
observer cleanup, and conflict/parse rollback. Full default-feature tests,
strict Clippy and formatting pass.

## Prompt macro and default binding checkpoint (2026-10-26, macOS arm64)

The modal prompt now accepts counts and replays macro registers with `@` and the
last register with `@@`, using the same replay depth and work limits as normal
mode. Replayed insert keys edit the prompt text without submitting it, and an
empty command register is a consumed no-op. The native `g0` default binding now
routes to `CURSOR_SCREEN_LINE_BEGIN`; it was previously parsed as an unsupported
`g` motion. Wrapped rows therefore move to the row start, not the file line
start.

Verification: focused editor and viewport tests cover macro insertion, counted
repetition, last-register replay and wrapped-row `g0`; strict Clippy and
formatting pass.

## Unbound key dispatch checkpoint (2026-10-26, macOS arm64)

Normal and operator-pending modes now discard a key that has no binding in the
active mode chain, matching `vis_keys_process`; previously only visual modes did
so and other modes returned an editor error. The fallback sits after binding,
mapping and pending-state handling, so routed native actions that are conditional
or unsupported, malformed pending arguments and recursion limits still report
their specific errors.

Verification: the editor unit test for an unbound `!` asserts unchanged text and
a consumed `Continue`, and the language-map test keeps its disabled-translation
non-move as a consumed key before the next mapped key moves. Library tests pass.

## Piped terminal checkpoint (2026-10-26, macOS arm64)

The terminal backend now has an input pump for sessions without a controlling
terminal. When stdin is not a tty and was not consumed as document data, it reads
raw keys from stdin, writes frames to stderr and uses the stderr geometry when
available (otherwise 80x24). At EOF it reopens `/dev/tty`, switches termios to
raw mode and continues; when no terminal can be reopened it fails like native
rather than reporting a clean EOF. The native `-` and implicit scratch paths
already consumed stdin as document bytes, so they deliberately skip the pipe and
read keys from stdout/stderr or the controlling terminal.

Verification: two new integration tests run `vis-rs file` with piped `:q!` and
with empty stdin, asserting a clean exit with unchanged bytes and the native
reopen failure. An actual paced pipe edit (`iX`, Escape, `:wq`) saves `Xabc`
identically under C vis and Rust. GNU screen-style PTY tests still exercise the
ordinary termios path and terminal restoration.

## Frame cost checkpoint (2026-10-26, macOS arm64)

The interactive port was measurably slower than the C editor: a burst of keys
took about 5.3 ms per key at 4 MiB, against 2.9 ms for the C editor, and the
cost grew with the file size. A sampling profile showed 73 percent of the time
in the redraw, dominated by allocation inside the cell walk and by walks that
measured the whole document for every frame.

The native editor keeps `view->start`, the byte offset of the first visible
row, and reads only the visible window; several port helpers instead walked
the whole document. The port now measures the visible window:

* The cell walk reuses its pending-cell buffer instead of allocating a
  vector per source character, and it accepts a start offset, so a frame no
  longer measures the bytes above the viewport.
* The cursor position, the cursor line, the visible line numbers and the
  visible byte range are all measured from the previous frame's visible
  offset.
* The renderer places the overlay cursors from the same offset.

The cost per key is now flat in the file size. The remaining difference was
the syntax highlighter: the `vis-std` module lexes the visible window on every
`WIN_HIGHLIGHT` event, so the lexer runs once per frame. The pure-Lua matcher
cost about 24 ms per frame, while the native LPeg virtual machine costs about
0.15 ms.

Verification: `rust/tests/latency_probe.py` measures the time from a key
burst until the terminal goes quiet, for the C editor and the Rust port at
several file sizes. The view coordinate differential still reports 20,804
equal values, the same 1,111 documented C quirks and zero unknown
differences; the key sweep stays 139/0 with four upstream exclusions, the
native-action differential stays 118/0, the terminal PTY cases pass, and the
all-feature suite passes 439 tests.

## Native LPeg and cached highlighting checkpoint (2026-10-27, macOS arm64)

Two changes close the remaining gap to the native editor.

**Native LPeg.** The optional `native-lpeg` feature loads a native LPeg
library and uses its virtual machine instead of the pure-Lua matcher. The
loading lives in the separate `rust/lpeg-native` crate, which is the port's
single audited boundary to native code: it opens the library with `dlopen`,
calls the module's `luaopen_lpeg` entry point and hands the module table
back. The main crate keeps `unsafe_code = "forbid"`. The feature is
optional, and the pure-Lua matcher remains the fallback, so a system without
a native LPeg behaves exactly as before.

The pure-Lua matcher costs about 24 ms per frame against the native 0.15 ms.
The highlighter runs once per frame, so this was the difference the user
noticed.

**Cached highlighting.** The result of lexing is kept while the bytes are
unchanged. The cache lives on this side of the Lua boundary, so the Lua
modules are unchanged and the native editor keeps its own behaviour. The key
is the lexer's identity together with a digest of the bytes that were lexed,
so a different lexer or different bytes never share an entry. The cache is
installed before the configuration runs, because the `vis` module that creates
the lexers is loaded by the configuration.

**Viewport anchor.** The cell walk now starts at a line boundary that the
previous frame measured, so a frame never walks the text above the viewport.
Before this, the cost of a frame grew with the distance from the start of the
document, which the profile showed as a steadily rising latency while
scrolling down. The anchor belongs to one document, so a window that shows
another file measures from the beginning.

The port also stopped measuring the text above the viewport. The cell walk
starts at a line boundary that the previous frame measured, so a frame costs
the same wherever the viewport is. The anchor belongs to one document
revision, so a window whose text changed measures from the beginning, like the
native editor does when its mark no longer fits.

Verification: `rust/tests/latency_probe.py` measures the time from a key
burst until the terminal goes quiet. At 1 MiB the port with the native
matcher measures about 2.2 ms per key, and a cursor move that leaves the
visible window unchanged costs about 1.5 ms, because the cached highlighting
serves the repeated window instead of lexing it again. Scrolling changes the
window, so it costs about 13 ms with the pure-Lua matcher and about 2.2 ms
with the native one. The cost no longer grows with the file size the way it
did. The first frame after a change costs more while the cache is filled. The
LPeg corpus differential reports 373/373 matching cases with the native
matcher as well as the pure-Lua one, over all 160 bundled lexers. The view
coordinate differential still reports 20,804 equal values and zero unknown
differences; the key sweep stays 139/0 with four upstream exclusions, the Lua
sweep 142/0 with one upstream exclusion, and the native-action differential
118/0.

The cached highlighting is checked against the uncached one: every bundled
lexer is run over several inputs, and the tags the cache returns must be
identical to the tags the matcher produces without the cache. The test also
counts the lookups, so a cache that is never used fails the check. The
comparison is made the way the highlighter makes it, through the loader with
its cache flag, because the loader builds a new lexer table without it.

The native matcher reports itself as `LPeg 1.1.0` and the pure-Lua matcher
as `LuLPeg 0.1.0 (vis LPeg 1.1 API adapter)`, so a configuration can tell
which one is in use. Two behaviors differ between them and are covered by the
tests: the native matcher rejects an empty loop body when the pattern is
built, while the pure-Lua matcher accepts the pattern and rejects the match;
and the native matcher bounds the stack by its own rule, so a recursive
pattern may still match where the pure-Lua matcher rejects it.

## Long lines checkpoint (2026-10-27, macOS arm64)

A file with a line that is longer than the screen showed a difference between
the two editors. The example file of the port, `rust/example.txt`, holds a
line of 158 kilobytes among its 179 lines, and the editors disagreed about
the screen after a motion.

The original editor addresses its view by a byte offset. A jump to a line
beyond the visible window places the start of the view at the byte of the
cursor and scrolls back by half a screen, which for a line that holds no
newline in that range leaves the view in the middle of the line. The
original editor then leaves the cursor below the visible body, and the screen
keeps the content it had before the jump, because it only writes the rows
that changed.

The port keeps the cursor on the last visible row and redraws the window, so
its screen shows the content of the view. The two agree on the line the
cursor is on, which the status bar reports, and the screen of the original
editor holds the content of the previous view.

The behaviour is compared with `rust/tests/content_compare.py`, which
renders the screen of both editors and compares the line the cursor is on
with the content the screen shows. The comparison of the frames themselves
is in `rust/tests/screen_compare.py`.

## Incremental frame checkpoint (2026-10-27, macOS arm64)

The port wrote the whole screen on every frame. The native editor keeps a
cell buffer and blits only the dirty cells, so a keystroke that changes one
line wrote the entire screen to the terminal. The port now keeps the rows of
the previous frame and writes only the rows that changed.

The frame is composed as rows rather than as one flat string, because two
windows can share a row and a separator is drawn between them. A row holds
every write made to it, so the separator does not erase the windows beside
it, and the order of the writes inside a row is kept. The blanking of a row
that no window covers is part of the row, so a closed or hidden window leaves
nothing behind.

The screen the two editors leave behind is the same. A run of keys that
inserts, moves, deletes and undoes text produces the same rows in both, and
the terminal output shrinks by more than an order of magnitude: a scripted
session that writes about 198 KB to the terminal writes about 16 KB.

The byte order differs, because the port writes a row once while the original
writes the screen and then the panels over it. The tests therefore compare the
screen the frames leave behind, rendered onto a virtual screen, rather than
the bytes. `rust/tests/screen.py` renders a frame, and the Rust unit tests
use the same rule.

The frame still begins by hiding the cursor and ends by placing it, so a
frame is identifiable in the terminal output.

## Frame cost profile checkpoint (2026-10-27, macOS arm64)

A sampling profile of the interactive loop showed where the time of a frame
went. The native editor takes about 0.26 ms per key; the port took about
6 ms. Four causes were found and fixed.

**The Lua interrupt hook.** The port installs a Lua instruction hook so that
a configuration that never returns can be interrupted. The Lua interpreter
traces every instruction while a hook is set, and the profile showed
`luaG_traceexec` at 31 percent of the work. The native editor installs no
hook at all. The hook is only needed while the configuration runs, so it is
removed once the configuration has finished, before the first frame is
drawn. The interrupt still works: the test that interrupts a configuration
with `while true do end` passes.

**A debug artifact.** A leftover statement opened a file and wrote a line to
it on every frame, which showed up as `open` and `write` in the profile.

**One walk per cursor.** The overlay placed the cell of every cursor with a
separate walk of the visible window, so a window with several selections
walked it once per selection. The native editor places the cell of every
cursor during the single pass that fills its cell buffer. The port now
measures all the positions in one walk.

**A copy of the document per frame.** The profile showed `_platform_memmove`
at 42 percent of the work on a four megabyte file. The port copied the
bytes of the document on every frame, in the paths that read the text: the
selection set, the motions, the anchor, the Lua objects and the frame
builder. The text now keeps a shared copy of its bytes, which a reader
borrows instead of copying the document, and the frame builder keeps the
copy of a buffer while its text does not change.

The copy of a buffer is keyed by the identity of its text, which counts the
edits. The revision time of the text groups the edits of an undo step, so it
does not change for every edit and it cannot serve as the key.

The cost of a key is now flat in the size of the document, as it is in the
native editor:

| Document | Native editor | The port |
| --- | --- | --- |
| 64 KiB | 0.16 ms | 1.10 ms |
| 1 MiB | 0.09 ms | 1.12 ms |
| 4 MiB | 0.21 ms | 1.53 ms |

Before these changes the port took 1.15 ms on the same file of 64 KiB and
2.26 ms on the file of 4 MiB, and with the pure-Lua matcher about 22 ms per
key on a file of long lines instead of about 10 ms.

The measurements are in `rust/tests/latency_probe.py`, which reports the
time from a key burst until the terminal goes quiet. The number of cells the
cell walk visits is a deterministic measure of the work and does not depend
on the load of the machine.
