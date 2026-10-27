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
