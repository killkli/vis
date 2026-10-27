//! Byte-addressed viewport operations, corresponding to native view.c.
use super::{CellKind, ViewportMetrics, walk_cells_with_row_end};
use crate::editor::actions::{NativeMotion, NativeScroll, NativeWindow};
use crate::motion;

struct Row {
    start: usize,
    bytes: usize,
    width: usize,
    counts: Vec<usize>,
}

pub(crate) struct NativeView<'a> {
    data: &'a [u8],
    metrics: ViewportMetrics,
    tab_width: usize,
    breakat: &'a str,
    pub start: usize,
    last_start: usize,
    marked_start: usize,
    end: usize,
    last: usize,
    rows: Vec<Row>,
}

impl<'a> NativeView<'a> {
    pub fn new(
        data: &'a [u8],
        start: usize,
        metrics: ViewportMetrics,
        tab_width: usize,
        breakat: &'a str,
    ) -> Self {
        let mut view = Self {
            data,
            metrics,
            tab_width,
            breakat,
            start: start.min(data.len()),
            last_start: 0,
            marked_start: 0,
            end: start.min(data.len()),
            last: 0,
            rows: Vec::new(),
        };
        view.draw();
        view
    }

    fn draw(&mut self) {
        if self.start != self.last_start {
            self.marked_start = self.start;
        } else {
            self.start = self.marked_start;
        }
        self.last_start = self.start;
        let width = self.metrics.width.max(1);
        let height = self.metrics.height.max(1);
        self.rows = (0..height)
            .map(|_| Row {
                start: self.start,
                bytes: 0,
                width: 0,
                counts: vec![0; width],
            })
            .collect();
        let mut previous = None;
        let mut last_source = None;
        let mut last_kind = None;
        let mut read_end = self.start;
        walk_cells_with_row_end(
            self.data,
            self.start,
            0,
            1,
            width,
            self.metrics.wrap_width(),
            self.tab_width,
            self.breakat,
            |cell| {
                if cell.row >= height {
                    return false;
                }
                if cell.kind == CellKind::Eof {
                    return false;
                }
                let bytes = if previous == Some(cell.pos) {
                    0
                } else {
                    cell.len
                };
                previous = Some(cell.pos);
                let row = &mut self.rows[cell.row];
                if cell.column < width {
                    row.counts[cell.column] = bytes;
                }
                row.bytes += bytes;
                row.width = row.width.max(cell.column + cell.width);
                last_source = Some(cell.row);
                last_kind = Some(cell.kind);
                true
            },
            |row, end| {
                if row < height {
                    read_end = end;
                }
            },
        );
        let mut position = self.start;
        for row in &mut self.rows {
            row.start = position;
            position += row.bytes;
        }
        self.end = read_end.min(self.data.len());
        self.last = if self.end < self.data.len() {
            height - 1
        } else {
            let row = last_source.unwrap_or(0);
            // An empty line after a newline is excluded before native's
            // EOF byte-count equality rule is considered.
            if row > 0 && self.rows[row].bytes == 0 {
                row - 1
            } else if last_kind == Some(CellKind::Newline) {
                row
            } else {
                (row + usize::from(self.rows[row].bytes == width)).min(height - 1)
            }
        };
    }

    pub fn coordinate(&self, position: usize) -> Option<(usize, usize)> {
        if position < self.start || position > self.end {
            return None;
        }
        let mut row = 0;
        let mut current = self.start;
        while row < self.last && current < position {
            if current + self.rows[row].bytes > position {
                break;
            }
            current += self.rows[row].bytes;
            row += 1;
        }
        let limit = self.rows[row].width.min(self.metrics.width.max(1));
        let mut column = 0;
        while current < position && column < limit {
            current += self.rows[row].counts[column];
            column += 1;
            while column < limit && self.rows[row].counts[column] == 0 {
                column += 1;
            }
        }
        Some((row, column))
    }

    fn cursor_set(&self, row: usize, column: usize) -> usize {
        let width = self.metrics.width.max(1);
        let row = row.min(self.rows.len() - 1);
        let mut column = column.min(width);
        // Wide-cell continuations, tab fill and blank padding have no bytes.
        let count_at = |column: usize| {
            self.rows
                .get(row + column / width)
                .map_or(0, |line| line.counts[column % width])
        };
        while column > 0 && count_at(column) == 0 {
            column -= 1;
        }
        (self.rows[row].start + self.rows[row].counts.iter().take(column).sum::<usize>())
            .min(self.data.len())
    }

    fn up(&mut self, count: usize) -> bool {
        if self.start == 0 {
            return false;
        }
        self.start = super::viewport_up(
            self.data,
            self.start,
            count,
            self.metrics.width.max(1) * self.metrics.height.max(1),
        );
        self.draw();
        true
    }

    fn down(&mut self, count: usize) -> bool {
        if self.end >= self.data.len() {
            return false;
        }
        if count >= self.rows.len() {
            self.start = self.end;
        } else {
            self.start += self
                .rows
                .iter()
                .take(count)
                .map(|row| row.bytes)
                .sum::<usize>();
        }
        self.draw();
        true
    }

    pub fn follow(&mut self, position: usize) -> usize {
        let position = position.min(self.data.len());
        if self.start == position {
            self.last_start = 0;
        }
        if position == self.end && self.last == self.rows.len() - 1 {
            self.start += self.rows[0].bytes;
            self.draw();
        }
        if position < self.start || position > self.end {
            self.start = position;
            self.up(self.rows.len() / 2);
        }
        if position <= self.start || position > self.end {
            self.start = motion::line_begin(self.data, position);
            self.draw();
        }
        if position <= self.start || position > self.end {
            self.start = position;
            self.draw();
        }
        position
    }

    fn coordinate_or_first(&self, position: usize) -> (usize, usize) {
        self.coordinate(position).unwrap_or((0, 0))
    }

    fn screen_up(&mut self, position: usize, desired: usize) -> usize {
        let (mut row, _) = self.coordinate_or_first(position);
        let mut position = position;
        if row == 0 {
            position = self.scroll(position, NativeScroll::ScrollUp, 1);
            row = self.coordinate_or_first(position).0;
        }
        if row > 0 {
            self.cursor_set(row - 1, desired)
        } else {
            position
        }
    }

    fn screen_down(&mut self, position: usize, desired: usize) -> usize {
        let (mut row, _) = self.coordinate_or_first(position);
        let mut position = position;
        if row == self.rows.len() - 1 {
            position = self.scroll(position, NativeScroll::ScrollDown, 1);
            row = self.coordinate_or_first(position).0;
        }
        if row != self.rows.len() - 1 {
            self.cursor_set(row + 1, desired)
        } else {
            position
        }
    }

    pub fn motion(
        &mut self,
        mut position: usize,
        movement: NativeMotion,
        count: usize,
        bottom_count: usize,
        preferred: Option<usize>,
    ) -> (usize, Option<usize>) {
        use NativeMotion as M;
        let (row, column) = self.coordinate_or_first(position);
        let desired = preferred.filter(|column| *column != 0).unwrap_or(column);
        match movement {
            M::ScreenLineUp | M::ScreenLineDown => {
                for _ in 0..count {
                    let before = (position, self.start);
                    position = if movement == M::ScreenLineUp {
                        self.screen_up(position, desired)
                    } else {
                        self.screen_down(position, desired)
                    };
                    if before == (position, self.start) {
                        break;
                    }
                }
                (position, Some(desired))
            }
            M::ScreenLineBegin => (self.cursor_set(row, 0), preferred),
            M::ScreenLineMiddle => (self.cursor_set(row, self.rows[row].width / 2), preferred),
            M::ScreenLineEnd => (
                self.cursor_set(row, self.rows[row].width.saturating_sub(1)),
                preferred,
            ),
            M::WindowLineTop | M::WindowLineMiddle | M::WindowLineBottom => {
                let wanted = match movement {
                    M::WindowLineTop => count.max(1),
                    M::WindowLineMiddle => self.rows.len() / 2,
                    _ => self.rows.len().saturating_sub(bottom_count),
                };
                (
                    self.rows[wanted.saturating_sub(1).min(self.last)].start,
                    preferred,
                )
            }
            _ => (position, preferred),
        }
    }

    pub fn window(&mut self, position: usize, window: NativeWindow) {
        let row = self.coordinate_or_first(position).0;
        match window {
            NativeWindow::Top => {
                self.start += self
                    .rows
                    .iter()
                    .take(row)
                    .map(|row| row.bytes)
                    .sum::<usize>();
                self.draw();
                self.follow(position);
            }
            NativeWindow::Center => {
                let center = self.rows.len() / 2;
                if row < center {
                    self.scroll(position, NativeScroll::SlideDown, center - row);
                } else {
                    self.start += self
                        .rows
                        .iter()
                        .take(row - center)
                        .map(|row| row.bytes)
                        .sum::<usize>();
                }
                self.draw();
                self.follow(position);
            }
            NativeWindow::Bottom => {
                self.up(self.rows.len());
                while position >= self.end && self.down(1) {}
            }
        }
    }

    pub fn scroll(&mut self, position: usize, scroll: NativeScroll, count: usize) -> usize {
        use NativeScroll as S;
        let height = self.rows.len();
        let (row, column) = self.coordinate_or_first(position);
        match scroll {
            S::ScrollUp => {
                if self.up(count) {
                    let (row, column) = self.coordinate_or_first(position);
                    self.cursor_set(row.min(self.last), column)
                } else {
                    self.follow(0)
                }
            }
            S::ScrollDown => {
                if self.down(count) {
                    let (row, column) = self.coordinate_or_first(position);
                    self.cursor_set(row, column)
                } else {
                    self.follow(self.data.len())
                }
            }
            S::SlideUp => {
                if self.down(count) {
                    if self.coordinate_or_first(position).0 == 0 {
                        self.cursor_set(0, self.coordinate_or_first(position).1)
                    } else {
                        self.follow(position)
                    }
                } else {
                    self.screen_down(position, column)
                }
            }
            S::SlideDown => {
                let was_last = row == self.last;
                if self.up(count) {
                    if was_last {
                        self.cursor_set(self.last, column)
                    } else {
                        self.follow(position)
                    }
                } else {
                    self.screen_up(position, column)
                }
            }
            S::PageUp | S::HalfPageUp => {
                let position = if self.start == 0 {
                    self.follow(0)
                } else {
                    let position = self.follow(self.start - 1);
                    self.window(
                        position,
                        if scroll == S::PageUp {
                            NativeWindow::Bottom
                        } else {
                            NativeWindow::Center
                        },
                    );
                    position
                };
                self.cursor_set(self.coordinate_or_first(position).0, 0)
            }
            S::PageDown => {
                let position = self.scroll(position, S::ScrollDown, height);
                self.cursor_set(self.coordinate_or_first(position).0, 0)
            }
            S::HalfPageDown => {
                let end = self.end;
                let position = self.scroll(position, S::ScrollDown, height / 2);
                if position < self.data.len() {
                    self.follow(end)
                } else {
                    position
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn tiny_and_binary_views_keep_all_motion_positions_in_bounds() {
        for data in [
            b"a\x01b\x7f\xff\nlast".as_slice(),
            b"abcdefghij",
            b"a\tb\n",
            b"",
        ] {
            for (width, height) in [(1, 3), (8, 1), (4, 3)] {
                let mut view = NativeView::new(
                    data,
                    0,
                    ViewportMetrics {
                        width,
                        height,
                        top: 0,
                        wrapcolumn: 0,
                    },
                    8,
                    "",
                );
                let mut position = view.follow(data.len());
                for _ in 0..4 {
                    for movement in [
                        NativeMotion::ScreenLineUp,
                        NativeMotion::ScreenLineDown,
                        NativeMotion::WindowLineTop,
                        NativeMotion::WindowLineBottom,
                    ] {
                        position = view.motion(position, movement, 2, 0, None).0;
                        position = view.follow(position);
                        assert!(
                            position <= data.len()
                                && view.start <= view.end
                                && view.end <= data.len()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn viewport_operations_match_native_c_rows_and_bytes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let directory = std::env::temp_dir().join(format!("vis-byte-view-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let main = std::fs::read_to_string(root.join("main.c"))
            .unwrap()
            .replace("#include \"config.h\"", "#include \"config.def.h\"");
        std::fs::write(directory.join("main.c"), main).unwrap();
        let oracle = r#"
#define main original_main
#include "main.c"
#undef main
int main(int argc, char **argv) {
    setlocale(LC_ALL, "C.UTF-8") || setlocale(LC_ALL, "en_US.UTF-8");
    FILE *file=fopen(argv[1],"rb");fseek(file,0,SEEK_END);size_t len=ftell(file);rewind(file);
    char *data=malloc(len+1);fread(data,1,len,file);fclose(file);
    Vis vis={0}; Win win={.vis=&vis}; Text *text=vis_text_load(&vis,0,TEXT_LOAD_READ);
    text_insert(&vis,text,0,data,len);
    View *view=&win.view;view->text=text;view->tabwidth=atoi(argv[4]);view->breakat=argv[5];view->wrapcolumn=atoi(argv[6]);
    vis_view_resize(view,atoi(argv[2]),atoi(argv[3]));view_selections_new(view,0);
    Selection *sel=view->selection;view_cursors_to(sel,strtoull(argv[7],NULL,10));
    for(int i=8;i<argc;i++) {
        if(!strcmp(argv[i],"up"))view_screenline_up(sel);
        else if(!strcmp(argv[i],"down"))view_screenline_down(sel);
        else if(!strcmp(argv[i],"begin"))view_screenline_begin(sel);
        else if(!strcmp(argv[i],"middle"))view_screenline_middle(sel);
        else if(!strcmp(argv[i],"end"))view_screenline_end(sel);
        else if(!strcmp(argv[i],"top"))view_cursors_to(sel,view_screenline_goto(view,1));
        else if(!strcmp(argv[i],"bottom"))view_cursors_to(sel,view_screenline_goto(view,view->height));
        else if(!strcmp(argv[i],"zt"))view_redraw_top(view);
        else if(!strcmp(argv[i],"zz"))view_redraw_center(view);
        else if(!strcmp(argv[i],"zb"))view_redraw_bottom(view);
        else if(!strcmp(argv[i],"slideup"))view_slide_up(view,1);
        else if(!strcmp(argv[i],"slidedown"))view_slide_down(view,1);
        else if(!strcmp(argv[i],"pageup"))view_scroll_page_up(view);
        else if(!strcmp(argv[i],"pagedown"))view_scroll_page_down(view);
        else if(!strcmp(argv[i],"halfup"))view_scroll_halfpage_up(view);
        else if(!strcmp(argv[i],"halfdown"))view_scroll_halfpage_down(view);
        printf("%zu:%zu:%zu ",sel->pos,view->start,view->end);
    }
}
"#;
        std::fs::write(directory.join("oracle.c"), oracle).unwrap();
        let binary = directory.join("oracle");
        let built = Command::new("cc")
            .current_dir(&directory)
            .args(["-std=c99", "-DVERSION=\"oracle\"", "-DVIS_API=0", "-I"])
            .arg(&root)
            .args(["oracle.c", "-lm", "-o"])
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let commands = [
            "up",
            "down",
            "top",
            "bottom",
            "zz",
            "slideup",
            "slidedown",
            "zt",
            "zb",
            "pageup",
            "pagedown",
            "halfup",
            "halfdown",
            "begin",
            "middle",
            "end",
        ];
        let fixtures = [
            b"".as_slice(),
            b"abcdefghij",
            b"abcde\nX\n",
            b"a\tb\nlong word here\n",
            "ab界a\u{301}xyz\nlast\n".as_bytes(),
            include_bytes!("../../example.txt").as_slice(),
        ];
        let input = directory.join("input");
        for data in fixtures {
            let case = data.len();
            std::fs::write(&input, data).unwrap();
            for (width, height, tab, breakat, wrap) in [
                (4, 3, 8, "", 0),
                (5, 4, 4, "", 0),
                (12, 4, 4, " ", 7),
                (1, 3, 8, "", 0),
                (8, 1, 8, "", 0),
            ] {
                for initial in [0, data.len() / 2, data.len()] {
                    // Native cursor_set can address a row outside its matrix
                    // for these degenerate geometries. Exercise Rust safety
                    // separately instead of treating a C abort as an oracle.
                    if width == 1 || height == 1 {
                        continue;
                    }
                    let output = Command::new(&binary)
                        .arg(&input)
                        .args([
                            width.to_string(),
                            height.to_string(),
                            tab.to_string(),
                            breakat.into(),
                            wrap.to_string(),
                            initial.to_string(),
                        ])
                        .args(commands)
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "C oracle failed bytes={case} w={width} h={height} tab={tab} initial={initial}: {:?} {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let expected: Vec<_> = String::from_utf8(output.stdout)
                        .unwrap()
                        .split_whitespace()
                        .map(|record| {
                            let mut n = record.split(':').map(|n| n.parse::<usize>().unwrap());
                            (n.next().unwrap(), n.next().unwrap(), n.next().unwrap())
                        })
                        .collect();
                    let mut view = NativeView::new(
                        data,
                        0,
                        ViewportMetrics {
                            width,
                            height,
                            top: 0,
                            wrapcolumn: wrap,
                        },
                        tab,
                        breakat,
                    );
                    let mut position = view.follow(initial);
                    let mut preferred = None;
                    for (command, expected) in commands.iter().zip(expected) {
                        match *command {
                            "up" | "down" | "begin" | "middle" | "end" | "top" | "bottom" => {
                                let movement = match *command {
                                    "up" => NativeMotion::ScreenLineUp,
                                    "down" => NativeMotion::ScreenLineDown,
                                    "begin" => NativeMotion::ScreenLineBegin,
                                    "middle" => NativeMotion::ScreenLineMiddle,
                                    "end" => NativeMotion::ScreenLineEnd,
                                    "top" => NativeMotion::WindowLineTop,
                                    _ => NativeMotion::WindowLineBottom,
                                };
                                (position, preferred) =
                                    view.motion(position, movement, 1, 0, preferred);
                                if matches!(*command, "top" | "bottom") {
                                    position = view.follow(position);
                                }
                            }
                            "zt" | "zz" | "zb" => view.window(
                                position,
                                match *command {
                                    "zt" => NativeWindow::Top,
                                    "zz" => NativeWindow::Center,
                                    _ => NativeWindow::Bottom,
                                },
                            ),
                            _ => {
                                position = view.scroll(
                                    position,
                                    match *command {
                                        "slideup" => NativeScroll::SlideUp,
                                        "slidedown" => NativeScroll::SlideDown,
                                        "pageup" => NativeScroll::PageUp,
                                        "pagedown" => NativeScroll::PageDown,
                                        "halfup" => NativeScroll::HalfPageUp,
                                        _ => NativeScroll::HalfPageDown,
                                    },
                                    1,
                                )
                            }
                        }
                        assert_eq!(
                            (position, view.start, view.end),
                            expected,
                            "bytes={case} w={width} h={height} tab={tab} breakat={breakat:?} wrap={wrap} initial={initial} command={command}"
                        );
                    }
                }
            }
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
