#!/bin/sh
# Add a trace of the native editor's view to the C sources.
#
# The trace writes the cell data of every draw to the file named by
# VIS_DEBUG_TRACE, so the rows the native editor holds can be compared with
# the rows the port draws. The original editor writes only the cells it marks
# dirty, so its screen keeps the rows a scroll replaced, while its view has
# moved; the trace shows the view.
#
# Revert with: git checkout view.c
set -e
cd "$(dirname "$0")/../.."

python3 - <<'PYEOF'
from pathlib import Path

p = Path("view.c")
s = p.read_text()

helper = '''static void view_trace(const char *fmt, ...) {
\tconst char *path = getenv("VIS_DEBUG_TRACE");
\tif (!path)
\t\treturn;
\tFILE *f = fopen(path, "a");
\tif (!f)
\t\treturn;
\tva_list args;
\tva_start(args, fmt);
\tvfprintf(f, fmt, args);
\tva_end(args);
\tfclose(f);
}

'''

s = s.replace('#include "vis-core.h"', '#include "vis-core.h"\n#include <stdarg.h>')
s = s.replace("/* reset internal view data structures (cell matrix, line offsets etc.) */",
              helper + "/* reset internal view data structures (cell matrix, line offsets etc.) */")

draw = '''\t/* set end of viewing region */
\tview->end = pos;
\tview_trace("DRAW start=%zu end=%zu w=%d h=%d\\n", view->start, view->end, view->width, view->height);
\tfor (s32 i = 0; i < view->height; i++) {
\t\tview_trace("ROW %d ", i);
\t\tfor (s32 j = 0; j < view->width; j++) {
\t\t\tVisCellData d = view->cell_data[i * view->width + j];
\t\t\tif (d.width == 0)
\t\t\t\tcontinue;
\t\t\tif (d.data_length == 0) {
\t\t\t\tview_trace(" ");
\t\t\t} else {
\t\t\t\tchar buffer[8] = {0};
\t\t\t\tmemcpy(buffer, d.data, MIN((size_t)d.data_length, sizeof(buffer) - 1));
\t\t\t\tview_trace("%s", buffer);
\t\t\t}
\t\t}
\t\tview_trace("\\n");
\t}'''
s = s.replace("""\t/* set end of viewing region */
\tview->end = pos;""", draw)
p.write_text(s)
print("the trace is added")
PYEOF

make vis >/dev/null
echo "the C editor is rebuilt with the trace"
