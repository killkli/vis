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
the local-time state notification still needs migration.

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

The first visible row is anchored to its content with a text mark, like native
`view->start_mark`, so edits above the view keep the visible text in place
instead of moving the window. The anchor travels with the window while it is
unfocused and is dropped when its line disappears.

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
canonical input for synchronous children and permits physical Ctrl-C. A raw
input pump for sessions without a controlling terminal still needs work. Bracketed paste stays literal;
file bytes, paths and messages cannot inject terminal control sequences.

Optional Lua configuration, live userdata, reentrant registered callbacks,
function mappings and syntax styles are now implemented; see
[`LUA_PORTING.md`](LUA_PORTING.md) for their bounded verification and remaining
API gaps. Stable logical selection identities and per-window last-visual marks
now survive focus and undo. Real modal prompt windows, mixed editable history and recursive aliases are
also implemented. Still incomplete: option bindings beyond the fixture corpus,
some pending-operator combinations, case/completion operations,
paste dot-repeat and the history state notification. Custom wrapping and precise
Unicode widths also require work. Default build/install/packaging
migration and remaining helper integration are pending. Core piece lookup/editing
is linear in piece count; UI frames still flatten buffers and repaint visible
cells. Existing C builds remain the default.

Language-key translation runs on physical input before macro recording and mode
bindings, with literal insert/replace input and unmodified replay semantics.
Fourteen C/Rust PTY cases verify the mapping, pending literal arguments, truncated
mapping pairs, modifiers and duplicate definitions.

The renderer now implements default soft wrapping and configured tab stops,
including narrow panes and visible EOF cursors. Its C-coordinate differential
separately reports 1,111 known C EOF/tab-coordinate discrepancies, never counting
them as equal. This is not a claim of complete view parity.

`vis-digraph-rs` includes the complete 1,894-entry original table and preserves
lookup order, output and exit codes. Piped input/listing are dependency-free;
interactive raw input uses the existing optional terminal dependency.

`vis-menu-rs` ports candidate ranking, flags, inline terminal interaction and
accept/cancel behavior. Its matching core is dependency-free; interactive mode
uses the same optional OS/signal crates. The query limit is 64 KiB rather than
the original platform-dependent `BUFSIZ`; long candidate lines stay intact.

Known compatibility differences and limits:

* Shell/read failure aborts queued document edits and returns an error. Current C
  vis hides these failures and commits prior edits in two sam fixtures:
  `commands/pipe-out.cmd` and `errors/read.cmd`. External shell effects themselves
  cannot be rolled back. The differential gate reports both differences.
* Regex matching uses C-locale named classes, rejects unsupported collation and
  backreferences, and has explicit pattern/program/work/capture limits. Long
  near-miss searches may reach the work limit. Empty regex is accepted by the
  Rust API; macOS libc rejects an empty ERE. This corpus is not complete POSIX
  certification.
* Auto-save writes existing files in place to retain links and metadata. A failed
  in-place write can leave partial data. Explicit atomic replacement preserves
  Unix owner/group/mode, rejects linked files, and does not copy ACLs/xattrs.
  Conflict checks are best effort, not filesystem locks.
* Width and combining tables approximate the original `wcwidth` behavior. The
  motion differential corpus verifies ASCII/control inputs, not full Unicode.
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

Verification: the option tests in `rust/tests/global_options.rs`, the anchor
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
