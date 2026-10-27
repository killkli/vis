# Handoff: porting completeness audit (2026-10-28)

**Overall port: in progress; complete replacement/release acceptance is not met.**
The actual-example, single-selection performance task is complete; its detailed
2026-10-27 measurements and implementation notes follow this audit. Do not treat
that task's completion, a routed action catalog, or a passing bounded corpus as
proof that all C behavior has been ported. There is no defensible single overall
completion percentage from the current coverage.

## Current acceptance status

| Area | Evidence checked in this audit | Status / remaining work |
|---|---|---|
| Storage, motions, regex, history | 324 core-only all-target tests; 25,200 motion/object values; 1,863 regex cases; 92 history states | Implemented with bounded verification; piece-count scaling, regex resource limits and file-metadata policies remain explicit |
| Structural commands | Full upstream Sam sweep 30/30; persistent session/count/mark sweep 56/56 | Current fixture acceptance passes; not universal command/error equivalence |
| Native actions and keys | All 178 action names routed: 108 implemented, 70 conditional, zero wholly unsupported; full Lua key sweep 142 match, zero mismatches/errors | Conditional context requirements and known pending-operator restrictions remain; one upstream-disabled fixture is excluded |
| Lua and plugins | Eight original Lua scripts pass; external LPeg differential 373/373 across 160 lexers; native loader's two tests pass | Full debug/FFI/C-module API, retained dirty styles and untested argument/error cases remain outside complete parity |
| Terminal and viewport | 66 C/Rust PTY cursor/viewport states; four buffered-input draw cases; terminal 10/10, workspace 9/9, process interruption 2/2 | Single-selection long-line latency is close to C; Unicode joiner rendering and multi-selection/global-row fallbacks remain open |
| Coordinate and style model | 20,804 equal coordinates, 1,111 separately classified C quirks, zero unknown; 961 style parse/merge comparisons | Bounded locale/Unicode/style coverage; do not count classified differences as equal |
| Build and release | 461 root all-feature/all-target tests pass, one external test ignored; strict Clippy and formatting pass | C is still the default Makefile build/install; Lua/native LPeg are opt-in; final packaging, helper and platform acceptance remain pending |

The 461 total includes seven example-target tests; the previous performance
checkpoint's 454 count omitted those targets. The two `vis-lpeg-native` tests
and 324 core-only tests are separate runs, not additional unique root tests.
The ignored external LPeg gate was also run explicitly and passed 373/373.

The strict Lua key sweep returns **exit 1** solely because
`test/vis/selections/out-of-view.keys` is disabled upstream. It is reported as
one unsupported/excluded input, not as a pass. The old 111-pass key and 28/30
Sam checkpoints below are historical; this audit's 142 and 30/30 results
supersede them. The full key corpus still does not cover every mode/argument
combination.

## Known gaps and next work

1. Audit conditional action combinations against C, especially joining with a
   pending operator and direct newline/tab actions under an operator. The 70
   conditional entries include both normal context requirements and real limits;
   they are not a count of 70 missing features. Keep Lua file-name mutation,
   argument/error and persistent dirty-style contracts explicit.
2. Reproduce and fix the previously recorded two-row Unicode joiner difference
   at `50G` (21/23 held rows; G was 23/23). That held-row result was not rerun in
   this audit. The coordinate corpus also intentionally retains 1,111 known C
   quirks, and the new viewport sequence classifies four EOF character-column
   differences rather than hiding them.
3. Port/measure multi-selection and unmeasured global-row viewport fallbacks;
   the native byte-local fast path and latency numbers cover the measured
   single-selection workflow. Continue piece-table scaling and save-metadata
   acceptance work before claiming full large-file equivalence.
4. Collect actual platform CI/runtime evidence and integrate the new viewport
   and batching differentials into acceptance CI. The repository has Linux,
   macOS and Windows-core CI configuration; this local macOS audit did not
   inspect remote runs. The safe process supervisor's WNOWAIT portability limit
   remains documented in `LUA_PORTING.md`.
5. Migrate default build/install, distribution packaging and helper/runtime
   selection only after those acceptance decisions. `make all`/`make install`
   currently select C; no default-build switch or deployment was performed.

The editor crate still forbids unsafe Rust. Curated safe
`debug.getinfo`/`debug.traceback` support exists (including vis-lspc's ordinary
source reporting); the complete native debug library remains unavailable.
`native-lpeg` is a separate, optional audited unsafe loader boundary for LPeg,
with the pure-Lua matcher as fallback. Arbitrary Lua C-module loading and FFI
remain unavailable. The performance measurements below require native LPeg;
they are not fallback-engine performance claims.

## Reproduction and handoff artifacts

Fresh local logs are under the ignored directory
`rust/target/validation/porting-audit-20261028/`. The audit started from
`07c965d3`; the following commit also includes the previously uncommitted
long-line implementation, its regression tests and these updated documents.
C sources, build defaults and packaging are unchanged.

```sh
cargo test --manifest-path rust/Cargo.toml --all-features --all-targets --locked
cargo test --manifest-path rust/Cargo.toml -p vis-lpeg-native --all-targets --locked
cargo clippy --manifest-path rust/Cargo.toml --all-features --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust/Cargo.toml --check
cargo test --manifest-path rust/Cargo.toml --no-default-features --all-targets --locked
cargo build --manifest-path rust/Cargo.toml --features native-lpeg --bins --examples --locked
python3 rust/tests/key_differential.py --lua --all   # exit 1: one upstream-disabled fixture
python3 rust/tests/differential.py
(cd test/lua && VIS="../../rust/target/debug/vis-rs" sh test.sh)
python3 rust/vendor/lpeg/differential.py
python3 rust/tests/viewport_motion_differential.py
python3 rust/tests/input_batch_differential.py
```

Core-only tests can replace `target/debug/vis-rs` with a non-terminal binary.
Rebuild with the intended features before running debug CLI/PTY gates; the
terminal/workspace/process checks in this audit explicitly used the existing
release/native-LPeg binary. For standalone coordinate/style drivers, first run
`cargo build --manifest-path rust/Cargo.toml --no-default-features --lib --locked`;
example builds do not reliably refresh `target/debug/libvis.rlib`.
The upstream Lua scripts were run in an isolated copy retaining their
`../../lua` runtime layout. The external LPeg gate uses cached pinned archives
and downloads nothing. Platform/runtime and performance results from earlier
dates remain dated observations, not new measurements from this audit.

---

# Performance checkpoint: byte-local viewport actions (2026-10-27)

The actual `rust/example.txt` performance follow-up is complete for the measured
single-selection interactive workflow. Opening is now about 107 ms in both
editors; the remaining half-second screen-row and slide operations now take
roughly 4–6 ms. The earlier checkpoints below are historical, including their
statements that this performance work remained active.

## Final measurements

Release with Lua/native LPeg, the existing user configuration (numbers enabled,
tab width 4), actual 1,993,854-byte fixture, 80x24 headless PTY, three-round
medians in milliseconds. Each row measures the entire key sequence.

| Operation | Previous Rust checkpoint | Final Rust | C |
|---|---:|---:|---:|
| Open | 109.46 | 107.34 | 107.00 |
| `l` x20 | 3.53 | 3.34 | 2.48 |
| `j` x20 | 15.11 | 16.03 | 19.39 |
| `50G` | 4.69 | 4.68 | 3.85 |
| `G` | 6.38 | 6.37 | 3.91 |
| `l` x20 near EOF | 3.49 | 3.37 | 2.41 |
| `gg` | 5.28 | 4.66 | 3.47 |
| Ctrl-B | invalid: missing alias | 3.90 | 3.66 |
| Ctrl-F | invalid: missing alias | 3.43 | 3.29 |
| `gk` x10 | 519.00 | 4.47 | 4.73 |
| `gj` x10 | 518.70 | 3.82 | 4.30 |
| `zz` | 12.96 | 3.52 | 0.56 |
| H | 50.82 | 3.57 | 3.51 |
| L | 50.94 | 3.75 | 3.28 |
| Ctrl-E x20 | 697.35 | 5.56 | 4.49 |

The previous Ctrl-B/F measurements were no-ops in Rust: those default aliases
were missing. They now invoke the existing page actions and pass C state
comparisons. The corrected timing must not be described as a speedup over the
old no-op.

The extended sequence's physical Ctrl-Y emits no C terminal output, so its
0 ms result is not a valid execution-time comparison. A separate probe keeps
the existing user configuration, maps Q to `<vis-window-slide-down>`, runs G
then L to place the cursor at the bottom, and sends Q x20: Rust **5.56 ms**,
C **5.06 ms**. Its configuration and sequence differ from the main probe.

```sh
cargo build --manifest-path rust/Cargo.toml --release --features native-lpeg --bins
python3 rust/tests/example_latency.py --extended --rounds 3 --output /tmp/example-latency.json
python3 rust/tests/viewport_motion_differential.py
```

Authoritative current artifacts are `/tmp/vis-core-perf/final-latency.json`,
`mapped-slide-latency.json`, `mapped-slide-latency.py`, and the validation logs
in the same directory. Older baseline binaries/results remain in
`/tmp/vis-perf/` and `/tmp/vis-core-perf/vis-rs-before`. Timings are local PTY
observations, with a few milliseconds of host/render overhead still visible;
this does not claim cycle-for-cycle equality or identical cold launches.

## Core algorithms now match the byte-local C view

* New `rust/src/view/native.rs` mirrors the C viewport operations using a
  bounded matrix of current rows and per-column source-byte counts. Screen-row
  motions, H/M/L, zt/zz/zb, page and slide actions no longer lay out the document
  prefix. Backward scrolling uses C's bounded screenful-byte scan.
* The model preserves native tab-fill/continuation cells, EOF accounting,
  preferred columns, count behavior and view-start mark restoration. Measured
  single-selection motions use it; API consumers without measured byte anchors
  and multi-selection view motions retain the existing row-based fallback.
* `view.rs` reports the native consumed read offset separately from retained
  cells. A word moved off the last row still contributes to `view->end`; a tab
  that cannot finish expanding can have the opposite effect. Word wrapping now
  clears the emitted row before moving its suffix, preventing duplicated cells.
* `main.rs` publishes the actual text width after the sidebar, and Lua separates
  window width from viewport width. Following runs before sidebar sizing, so
  returning from line 180 to line 1 immediately restores the correct width.
* Byte anchors now update when their byte changes within the same global row.
  Tracked text marks restore before cursor following; inserted text at the
  first visible byte stays visible. An unchanged cursor at the byte start does
  not move the viewport merely because the next key is a numeric count.
* Ctrl-B/F now alias page up/down in normal and visual modes, matching C.

## Final validation and remaining scope

* All-feature tests: **454 passed, 1 ignored, 0 failed**. Clippy with
  `-D warnings`, formatting and diff whitespace checks pass. Release/native-LPeg
  and no-default binary builds pass. Rust 1.85 locked all-feature build passes
  with the eight pre-existing cygwin cfg warnings.
* The native view unit oracle performs **864 matching C/Rust states** across
  16 operations, six fixtures (including the actual example), three geometries
  and three initial positions. Degenerate one-column/one-row and binary input
  are covered for Rust bounds safety separately; C can assert on those cases.
* New PTY differential: **66 matching cursor-byte/source-line/viewport states**
  across the actual example with/without numbers and a narrow word-wrapped/tab
  fixture. Four existing EOF character-column differences are explicitly
  classified (C's final iterator-step accounting); other differences fail.
* Fresh standalone coordinate oracle: **20,804 equal, 1,111 documented C quirks,
  0 unknown**. Rebuild `--no-default-features --lib` before running it; examples
  do not necessarily refresh the standalone rlib used by the driver.
* Native actions **118**, key cases **48**, four buffered-input drawing cases,
  terminal editing/signals/paste, seven split/resize/Unicode checks, Lua CLI and
  controlling-TTY/job-control checks pass. Style/overlay painting passes
  22 cursor/selection states plus 10 whitespace states.
* This does not claim that the old two-row Unicode joiner rendering discrepancy
  is fixed; its previously recorded held-row result is 21/23 at 50G and 23/23 at
  G. Multi-selection/global-row fallback performance was not the measured
  workflow. C sources in the worktree remain unmodified.

---

# Earlier checkpoint: buffered input (superseded by the byte-local actions above)

The goal is **still active**. Opening and ordinary navigation of
`rust/example.txt` now have latency comparable to C, but the extended probe
found large remaining costs in screen-line/window/slide motions near EOF.
Do not call the whole performance goal complete based on the short probe.

## Authoritative measurements

Release with Lua/native LPeg, existing configuration (line numbers enabled),
80x24 headless PTY, actual 1,993,854-byte fixture, three-round medians in ms.
Each row is the entire key sequence, not one key. Occasional first-launch
outliers remain in the raw JSON; the opening value is the median, not a claim
about every cold launch.

| Operation | Starting Rust | Current Rust | C |
|---|---:|---:|---:|
| Open | 120.19 | 109.46 | 104.69 |
| `l` x20 | 88.45 | 3.53 | 2.40 |
| `j` x20 | 4094.77 | 15.11 | 19.18 |
| `50G` | 1204.69 | 4.69 | 3.95 |
| `G` | 1044.33 | 6.38 | 4.21 |
| `l` x20 near EOF | 18300.84 | 3.49 | 2.38 |
| `gg` | 957.02 | 5.28 | 3.74 |

The extended cases run after the above sequence, starting with another `G`:

| Extended operation | Rust | C |
|---|---:|---:|
| `page_up` | 3.34 | 4.14 |
| `page_down` | 3.38 | 3.71 |
| `screen_up10` | 519.00 | 5.03 |
| `screen_down10` | 518.70 | 4.47 |
| `center` | 12.96 | 0.54 |
| `window_top` | 50.82 | 3.59 |
| `window_bottom` | 50.94 | 3.48 |
| `slide_up20` | 697.35 | 4.58 |
| `slide_down20` | 256.11 | 0.00 |

`page_up/down` are Ctrl-B/F; `screen_up/down10` are `gk`/`gj` x10;
`center` is `zz`; `window_top/bottom` are H/L; `slide_up/down20` are
Ctrl-E/Y x20. A zero C time means no new terminal output was emitted, not a
claim that the key had no processing cost.

```sh
cargo build --manifest-path rust/Cargo.toml --release --features native-lpeg --bins
python3 rust/tests/example_latency.py --extended --rounds 3 --output /tmp/example-latency.json
```

The probe checks CPU activity as well as terminal silence and drains output
continuously. Output-only quiet timing can stop while a long computation is
still running. Artifacts under `/tmp/vis-perf/` include `vis-rs-before`,
`vis-rs-checkpoint2`, `before-final.json`, `after-measure.json`,
`batch-final-latency.json`, sampling profiles, and test logs.

## Changes currently in the dirty worktree

* `view.rs`: lazy logical-line tokens; reused pending-cell storage; a scalar
  coordinate path for empty `breakat`, with arithmetic ASCII runs rather than
  allocating display cells. Word wrapping and malformed UTF-8 use the existing
  cell walk. Randomized tests compare the scalar path against the cell walk.
* `terminal.rs`: byte-local cursor/overlay/geometry/sidebar measurements;
  incremental source line numbers; document-row checkpoints; at most two
  viewport-sized cell layouts shared across geometry, sidebar, cursor and
  painting. Cache keys include position, height, widths, tab width and breakat;
  exact retained Arc ownership invalidates them on every content change.
* Ordinary displayed text uses `Cow<str>` to borrow document bytes. Body/grid
  copies no longer allocate a String for every ordinary character. Control
  spellings, status labels and merged marks remain owned where necessary.
* Geometry measurement skips constructing the discarded styled body. Line
  following is reused between geometry and painting for an unchanged query.
* `lua/objects.rs`: per-file incremental line counting and duplicate line/col
  lookup caching, invalidated by exact content identity and cleaned on close.
* `editor.rs`: centered following is byte-local; global row resolution is
  deferred to rendering; ordinary motion completion borrows the bytes; removed
  the unconditional `/tmp/follow.log` debug writes.
* `main.rs`/terminal decoder: drain complete events from one terminal read
  before drawing, as C's main loop does. Motion geometry and Lua viewport
  metadata still update between keys. Incomplete Escape/paste input yields to
  painting; messages force painting even with queued input, preserving
  fullscreen-child feedback and queued quit behavior.
* `input_batch_differential.py`: C and Rust both draw once for four buffered
  key sequences and agree on final line/column, including j+H/L and zt+H.
* Anchor test fixtures now publish byte anchors separately, like the host.
  The three old anchor failures were reproduced from HEAD before updating the
  fixture. Jump tests assert the requested byte rather than an eager row.
* `view_held_rows.py` now settles while draining and excludes the sidebar.
  C tracing/building was done only under `/tmp/vis-perf/native`; C sources in
  this worktree remain unmodified.

## Verification

* All-feature tests: **447 passed, 1 ignored, 0 failed**; Clippy `-D warnings`,
  formatting and diff whitespace checks pass. Default and no-default builds
  pass. Rust 1.85 locked all-feature build passes with existing cygwin cfg
  warnings in unchanged code.
* C coordinates: **20,804 equal, 1,111 documented quirks, 0 unknown**.
  Important: rebuild the standalone rlib before this gate. Examples/tests do
  not necessarily update `target/debug/libvis.rlib`, and a Lua-enabled rlib
  needs native linker search flags that the standalone oracle does not pass:
  `cargo build --manifest-path rust/Cargo.toml --no-default-features --lib`.
* The fresh current and HEAD baseline initially both showed 15 old coordinate
  differences for byte offsets inside a combining cell. These were then fixed:
  C advances to the right edge of the containing cell, rather than its start.
  Both scalar and cell coordinate paths now implement this rule, and the
  fresh standalone oracle is green. This corrects the earlier stale-library
  validation claim in this handoff.
* Native actions 118, key cases 48, motion/object values 25,200, regex cases
  1,863, Sam sessions 56, history states 92, langmap cases 14 and menu PTY
  cases 16 pass. Style/overlay/whitespace comparison, release terminal smoke,
  split/resize checks, Lua CLI/job-control, workspace and subprocess checks
  also pass. Fullscreen feedback needed explicit message painting under batching;
  that regression was fixed and the Lua CLI suite rerun successfully.
* Held rows remain 23/23 for G and 21/23 for 50G. The previously documented
  two-row Unicode joiner rendering discrepancy remains; see older handoff below.

## Next work: core row lookup paths

The extended latency table proves this work is unfinished. In
`rust/src/editor/actions.rs`, `native_view_motion` eagerly calls
`last_source_row_with_layout`, `position_at_with_breakat`, row width/start
helpers and sometimes scans the prefix repeatedly for one action.
`native_scroll` and `native_window` also use row-addressed helpers; each slide
may repeat them. These still materialize cells from byte zero, although native
C uses its current view's rows and byte start.

Relevant helpers in `view.rs`: `last_source_row_with_layout`,
`row_start_with_breakat`, `row_width_with_breakat`, `position_at_with_breakat`.
`last_row_with_layout` and global position already have the scalar fast path.
Prefer native byte-local viewport geometry for motions, or share an exact row
lookup/index so repeated operations do not rescan the prefix. Preserve tab-fill,
wide/control cells, combining bytes, EOF/wrapcolumn and motion-count semantics;
use C PTY comparisons as well as the short-file unit fixtures.


---

# Handoff: the Rust port's view model

The port's view is now addressed by a byte, like the native editor's
`view->start`, and the port's rendering matches the native's held rows for
every motion that was checked. One narrow difference is left; it is described
at the end.

Everything below was measured against the C editor in this repository. The C
sources are the reference and are unmodified.

## What the native does, and what the port now does

| Native | Port |
|---|---|
| `view->start` is a byte; a view may begin in the middle of a long line | `Viewport::view_start` is a byte; `walk_cells_from_layout` starts there |
| `view->start_mark` keeps the view on a text mark, so the view follows the text edited above it | `Editor::view_anchor_start` marks the byte the view starts at; the terminal applies it only when the view did not move |
| `view_cursors_to`: the start moves to the cursor and scrolls back by half a screen | `view_start_for_cursor` in `rust/src/view.rs` |
| `view_redraw_bottom`: the view scrolls up by its whole height, then down one row at a time until the cursor is on the last row | `view_start_for_line_motion` in `rust/src/view.rs` |
| `view_viewport_down` advances the start by the bytes of the first row | `row_bytes` in `rust/src/view.rs` |
| `view->end` is the byte after the last cell of the last row | `view_end` in `rust/src/view.rs` |
| `text_lineno_by_pos` reports the line of a position | `line_number_at_byte` in `rust/src/view.rs` |
| A character that is not wide is merged into the cell before it | `LineCell::merged_end` in `rust/src/view.rs`; the cell holds at most six bytes, as `VisCellData` does |
| `wcwidth` decides the width of a character | `rust/src/width.rs`; the `ZERO_WIDTH` table now matches the system's `wcwidth` for the locale the native uses |

## Where the code lives

* `rust/src/view.rs` — the cell model, the row layout and the view's start.
  `walk_cells_from_layout` streams the cells from a byte; `place_line` lays a
  line out; `view_start_for_cursor`, `view_start_for_line_motion`,
  `viewport_up`, `view_end` and `row_bytes` reproduce the native's scrolling.
* `rust/src/terminal.rs` — the renderer. `Viewport::view_start` holds the byte
  the view starts at, `view_start_last` tells a scroll from a view that stayed,
  and `panel_decorated` applies the native's follow rules.
* `rust/src/editor.rs` — `scroll_start`, `line_motion` and the anchor. The
  editor tells the renderer how the cursor left the view.
* `rust/src/editor/actions.rs` — the line motions set `line_motion`.
* `rust/src/width.rs` — the width tables.

## Verification

```sh
cargo test --manifest-path rust/Cargo.toml --all-features
cargo clippy --manifest-path rust/Cargo.toml --all-targets --all-features
cargo fmt --manifest-path rust/Cargo.toml --check
```

All pass: 330 tests, no clippy warnings, the formatting is clean. The
default, `--no-default-features` and `+1.85.0 --locked` builds pass too.

Every differential gate passes:

```sh
python3 rust/tests/key_differential.py            # 48 pass
python3 rust/tests/native_action_differential.py   # 118 pass
python3 rust/tests/motion_differential.py          # 25,200 values
python3 rust/tests/regex_differential.py           # 1,863 cases
python3 rust/tests/view_differential.py            # 20,804 equal, 0 unknown
python3 rust/tests/sam_session_differential.py     # 56 cases
python3 rust/tests/history_differential.py         # 92 states
python3 rust/tests/{prompt,langmap,style_render,menu}_differential.py
python3 rust/tests/{terminal,workspace,subprocess,terminal_views,lua_cli}_smoke.py
```

The gates need the example binaries:

```sh
cargo build --manifest-path rust/Cargo.toml --release --features native-lpeg --bins
cargo build --manifest-path rust/Cargo.toml --examples --features native-lpeg
```

## The native's screen is stale after a scroll

The original editor writes only the cells it marks dirty, so after a scroll
its screen keeps the rows a scroll replaced while its view has moved. The
port redraws the rows it changes, so the port's screen is the *new* content and
the native's screen is a mix of the old and the new.

This is why `rust/tests/screen_compare.py` reports fewer identical rows than
the editors really agree on. The comparison that matters compares the port's
screen with the rows the native *holds*:

```sh
sh rust/tools/trace_native_view.sh          # adds a trace to the C sources
python3 rust/tests/view_held_rows.py rust/example.txt --keys 50G
git checkout view.c                          # the C sources are the reference
```

The trace writes the cell data of every draw to a file, and the script
compares the port's screen with the rows of the native editor's last draw of
the biggest screen. The native editor scrolls one row at a time, so it needs
a few seconds to settle; the script waits.

With that comparison the port matches the native's held rows:

| keys | rows that match |
|---|---|
| *(none)* | 23/23 |
| `j` | 23/23 |
| `G` | 23/23 |
| `gg` | 23/23 |
| `50G` | 21/23 |

## What is left

`50G` on `rust/example.txt`: the port's rows 6 and 7 are one and two
characters shorter than the native's. The port's *cells* are identical to the
native's for those rows — the same positions, the same widths, the same merged
characters — and the port's body holds the full text of the row, so the loss
happens between the body and the screen.

The rows hold a zero-width joiner next to a zero-width non-joiner:

```
986461: "\u{1fff}" "\u{200c}" "\u{200d}"
```

The native's cell 43 holds six bytes (`"\u{1fff}\u{200c}"`); the joiner is
dropped because the cell's six-byte buffer is full, and the native's
`file_byte_count` does not grow either, so the joiner is read again and
merges into the cell after it. The port merges the joiner into cell 43 as
well, but its cell's length is nine bytes, and the renderer writes the cell's
text from `cell.pos` to `cell.merged_end` limited to six bytes.

What is measured, and where to look next:

* The port's body row holds the full text — the row ends with the `-` the
  native's ends with. The body is built in `ordered_panel` in
  `rust/src/terminal.rs`.
* The body is copied into the `ui` grid by `copy_body`, which uses both
  `width` and `text_width`. They are the same here, because the line-number
  sidebar is off; with a sidebar they differ.
* The `ui` grid is written to the screen by the loop that follows
  `copy_body`. It skips the cells whose width is zero, which is how a
  character that is not wide shares the column of the character before it.

So the character is lost in one of those three places. To find it, print the
body row, the `ui` row and the port's screen row for the frame in question
and compare them; the first of the three that is short is where the
character goes.

The port's cells for that row were measured and match the native's:

```
the native's cell 43: width 1, six bytes, "\u{1fff}\u{200c}"
the port's cell 43:   width 1, nine bytes, "\u{1fff}\u{200c}"
```

The nine bytes are the base character, the non-joiner and the joiner; the
native's six bytes are the base character and the non-joiner, because its
cell's buffer is full when the joiner arrives.

## Two things that were wrong and are easy to get wrong again

* **The width tables must match the system's `wcwidth`.** The port used a
  Unicode-derived table that called U+00AD and U+FEFF zero-width; the native's
  `wcwidth` calls them one column wide. The `ZERO_WIDTH` table in
  `rust/src/width.rs` was regenerated from the system.
* **A character that is not wide is merged into the cell before it, and the
  merge is resolved before the row is emitted.** The native places a character
  only when the next one arrives, so a character that is not wide at the start
  of a row joins the cell before it, on the row before.
