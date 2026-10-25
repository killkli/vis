#!/bin/bash
set -euo pipefail
VIS_QUICK_ROOT=$(cd -- "$(dirname -- "$0")" && pwd -P)
export VIS_QUICK_ROOT
export VIS_PATH="$VIS_QUICK_ROOT/runtime"
export PATH="$VIS_QUICK_ROOT/tools/bin:$VIS_QUICK_ROOT/bin:$PATH"
exec "$VIS_QUICK_ROOT/bin/vis" "$@"
