#!/usr/bin/env python3
"""Compare modal bytes and complete ordered selection state in Rust and C vis.

Build `cargo build --manifest-path rust/Cargo.toml --example replay` and the C
`vis` binary first. Default cases cover basic insertion/deletion/motion/history
and a small upstream fixture subset; --all exercises every .keys fixture.
Unknown Rust commands are reported UNSUPPORTED, never counted as passes. Use
--lua with the lua_replay example to execute real startup plugins, fixture Lua,
and file saves. Both editors run in separate temporary directories.
The mode, primary selection, all ranges/cursors and anchored orientation are
checked both before and after the automatic final Escape, which otherwise hides
visual selections or extra normal-mode cursors.

C configuration follows the upstream harness: test/vis fixtures load their real
visrc.lua and standard plugins; test/vim fixtures use their empty visrc.lua and
remain plugin-free. Built-in cases load lua/visrc.lua by default; select
--builtin-config core to exercise the empty core configuration instead. The
runner builds the original C vis-digraph helper in a temporary directory if the
repository helper is absent. --lua uses the built Rust vis-digraph-rs through a
temporary vis-digraph alias. No downloads or network access are used.
The three macro golf fixtures use native PTY input because Lua feedkeys disables
recording. F12 captures state before/after Escape outside the startup callback.
"""

import argparse
import contextlib
import errno
import fcntl
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]

BUILTINS = [
    ("insert-escape", b"one two\n", "iX<Escape>"),
    ("delete-word", b"one two three\n", "dw"),
    ("delete-lines", b"one\ntwo\nthree\n", "2dd"),
    ("motion-insert", b"one two\nthree\n", "wix<Escape>ji!<Escape>"),
    ("undo-redo", b"one two\n", "iX<Escape>u<C-r>"),
    ("change-word", b"one two\n", "cwnew<Escape>"),
    ("open-line", b"one\ntwo\n", "ohello<Escape>"),
    ("unicode-insert", "♪ test\n".encode(), "a♫<Escape>"),
    ("visual-forward", b"one two\n", "v2l"),
    ("visual-backward", b"one two\n", "v2lo"),
    ("structural-selections", b"a b a\n", ":,x/a/<Enter>"),
    ("secondary-cursor-escape", b"a b a\n", ":,x/a/<Enter><Escape>"),
    ("pending-find-repeat-delete", b"one two three\nfour five\nsix\n", "d;w"),
    ("counted-change-to-eof-linewise", b"one two three\nfour five\nsix\n", "2c3wX<Escape>"),
    ("counted-visual-put-dot-repeat", b"one two three\nfour five\nsix\n", "2VjP."),
    ("counted-visual-line-motion", b"one two three\nfour five\nsix\n", "2Vj"),
    ("counted-visual-char-escape", b"one two\n", "2v"),
    ("named-yank-does-not-replace-unnamed", b"one two three\nfour five\nsix\n", "\"ayyjVjP."),
    ("counted-goto-keeps-target-line", b"one\ntwo\nthree\nfour\n", "d3G"),
    ("file-end-linewise-inclusive", b"one\ntwo\nthree\n", "dG"),
    ("paragraph-change-inserts-newline", b"one two three\nfour five\nsix\n", "c{X<Escape>"),
    ("missing-textobject-still-changes", b"one two\n", "ci(X<Escape>"),
    ("percent-scans-forward", b"alpha.beta, gamma_delta (x) [y] {z}\n", "d%"),
]

DEFAULT_FIXTURES = [
    "test/vim/operators/insert/insert.keys",
    "test/vim/operators/insert/append.keys",
    "test/vim/operators/delete/delete.keys",
    "test/vim/operators/change/change.keys",
    "test/vim/motions/goto.keys",
]

FEEDKEYS_UNREPRESENTABLE = {"delete", "del", "insert", "ins"}


def unrepresentable_feedkeys_token(keys):
    """Return a named key token C `vis:feedkeys` cannot preserve distinctly."""
    for match in re.finditer(r"<([^<>]+)>", keys):
        if match.group(1).lower() in FEEDKEYS_UNREPRESENTABLE:
            return match.group(1)
    return None

# Lua vis:feedkeys intentionally disables macro recording. The upstream Vim
# harness uses native terminal input, so these fixtures require a PTY oracle.
NATIVE_MACRO_FIXTURES = {
    ROOT / f"test/vim/golf/{name}/1.keys" for name in (
        "4d1a34ccfa85f32065000004", "4ddbd92898957e0001000016",
        "4fbf8e303be58b0001000024",
    )
}


def lua_string(text):
    return '"' + "".join(f"\\{byte:03d}" for byte in text.encode()) + '"'


def normalize(fixture):
    source = fixture.read_text()
    if fixture.is_relative_to(ROOT / "test/vim"):
        result = subprocess.run(["cpp", "-P"], input=source, text=True,
                                capture_output=True, timeout=10)
        if result.returncode:
            raise RuntimeError(f"fixture preprocessor failed: {result.stderr.strip()}")
        source = result.stdout
    return "".join(line.rstrip() for line in source.splitlines())


def observer_source(requested):
    return (
        "local before_escape = nil\n"
        "local captured = false\n"
        "local observed = false\n"
        "local replaying = false\n"
        "local function snapshot(win)\n"
        "  local mode = 'UNKNOWN'\n"
        "  for name, value in pairs(vis.modes) do if vis.mode == value then mode = name end end\n"
        "  if vis.win ~= win then mode = 'COMMAND' end\n"
        "  mode = string.gsub(mode, '_', ' ')\n"
        "  if mode == 'OPERATOR PENDING' then mode = 'OPERATOR' end\n"
        "  local rows = {}\n"
        "  for selection in win:selections_iterator() do\n"
        "    local range = selection.range\n"
        "    local endpoint = not selection.anchored and 'point' or (range and selection.pos == range.start and 'start' or 'end')\n"
        "    rows[#rows + 1] = string.format('selection %s %s %s %d %s\\n', tostring(selection.pos), tostring(range and range.start), tostring(range and range.finish), selection.anchored and 1 or 0, endpoint)\n"
        "  end\n"
        "  return string.format('%s\\n%s\\nprimary %d\\nselections %d\\n', tostring(win.selection.pos), mode, win.selection.number - 1, #rows) .. table.concat(rows)\n"
        "end\n"
        "local function capture(win)\n"
        "  if captured then return end\n"
        "  assert(observed, 'key replay observer did not run')\n"
        "  assert(before_escape or replaying, 'replay aborted before its before-Escape observation')\n"
        "  local bytes = win.file:content(0, win.file.size)\n"
        "  local final = snapshot(win)\n"
        "  local out = assert(io.open('output', 'wb'))\n"
        "  out:write(bytes); out:close()\n"
        "  local state = assert(io.open('state', 'w'))\n"
        "  state:write(final, 'before_escape\\n', before_escape or final); state:close()\n"
        "  local marker = assert(io.open('observed', 'w'))\n"
        "  marker:write(before_escape and 'before_escape' or 'closed_during_replay'); marker:close()\n"
        "  captured = true\n"
        "end\n"
        "local function replay_input(win, input)\n"
        "  observed = true\n"
        "  replaying = true\n"
        "  local ok, err = pcall(function() vis:feedkeys(input) end)\n"
        "  replaying = false\n"
        "  if not ok then error(err) end\n"
        "  before_escape = snapshot(win)\n"
        "  vis:feedkeys('<Escape>')\n"
        "  pcall(capture, win)\n"
        "end\n"
        f"local function replay(win) replay_input(win, {lua_string(requested)}) end\n"
    )


def prepare_reference(data, keys, directory, profile="standard"):
    source = directory / "input.in"
    source.write_bytes(data)
    requested = keys.replace("<Space>", " ")
    observer = observer_source(requested)
    # Absolute paths keep Lua module/plugin loading independent of the temporary
    # replay working directory, without consulting the user's visrc.lua.
    configuration = f"package.path = {lua_string(str(ROOT / 'lua/?.lua') + ';')} .. package.path\n"
    if profile == "vis":
        # Keep the authoritative fixture normalization, events and save behavior.
        # Replace its single replay call with an explicit observer hook: native
        # userdata methods such as vis.feedkeys cannot be overridden reliably by
        # assigning an instance field.
        (directory / "input.keys").write_text(keys)
        fixture_config = (ROOT / "test/vis/visrc.lua").read_text().replace(
            "../../lua/", str(ROOT / "lua") + "/"
        )
        replay_call = "vis:feedkeys(keys..'<Escape>')"
        if fixture_config.count(replay_call) != 1:
            raise RuntimeError("upstream test/vis replay call changed; observer insertion requires review")
        fixture_config = fixture_config.replace(
            replay_call, "__vis_rust_oracle.replay(win, keys)"
        )
        (directory / "fixture-visrc.lua").write_text(fixture_config)
        configuration += observer + (
            "__vis_rust_oracle = { replay = replay_input }\n"
            f"dofile({lua_string(str(directory / 'fixture-visrc.lua'))})\n"
            "vis.events.subscribe(vis.events.WIN_CLOSE, capture)\n"
        )
    elif profile == "standard":
        configuration += (
            f"dofile({lua_string(str(ROOT / 'lua/visrc.lua'))})\n"
            + observer
            + "vis.events.subscribe(vis.events.WIN_CLOSE, capture)\n"
            + "vis.events.subscribe(vis.events.WIN_OPEN, replay)\n"
        )
    elif profile == "core":
        # test/vim/visrc.lua is deliberately empty. Loading vis.lua here would
        # add plugins absent from the upstream Vim comparison. These callbacks
        # are observation/replay only, using the C Lua event entry points.
        configuration += (
            f"dofile({lua_string(str(ROOT / 'test/vim/visrc.lua'))})\n"
            + observer
            + "vis.events = vis.events or {}\n"
            + "vis.events.win_close = capture\n"
            + "vis.events.win_open = replay\n"
        )
    else:
        raise ValueError(f"unknown C configuration profile: {profile}")
    (directory / "visrc.lua").write_text(configuration)
    return source


def reference(binary, data, keys, directory, profile="standard", helper_directory=None):
    source = prepare_reference(data, keys, directory, profile)
    helpers = helper_directory or ROOT
    env = {**os.environ, "VIS_PATH": str(directory), "TERM": "xterm", "LC_ALL": "C.UTF-8",
           "PATH": str(helpers) + os.pathsep + os.environ.get("PATH", "")}
    process = subprocess.run([str(binary), "+qall!", str(source)], cwd=directory,
                             env=env, input=b"", capture_output=True, timeout=15)
    (directory / "stderr").write_bytes(process.stderr)
    if process.returncode or not (directory / "output").exists():
        raise RuntimeError(f"C oracle failed ({process.returncode}): {process.stderr[:400]!r}")
    if not (directory / "observed").is_file():
        raise RuntimeError("C oracle did not verify before-Escape observer execution")
    if (directory / "observed").read_text() not in ("before_escape", "closed_during_replay"):
        raise RuntimeError("C oracle reported an invalid observer phase")
    return (directory / "output").read_bytes(), parse_state((directory / "state").read_text())


def native_key_chunks(keys):
    """Encode the native keys used by macro fixtures, delaying each lone Escape.

    This follows test/util/keys.c for the fixture tokens involved. Unknown angle
    text stays literal; recognized but unsupported terminal key tokens fail the
    oracle rather than silently disappearing or becoming document text.
    """
    sequences = {
        "enter": b"\r", "return": b"\r", "cr": b"\r", "space": b" ",
        "tab": b"\t", "s-tab": b"\x1b[Z", "backtab": b"\x1b[Z",
        "backspace": b"\x08", "bs": b"\x08", "lt": b"<",
        "up": b"\x1bOA", "down": b"\x1bOB", "right": b"\x1bOC", "left": b"\x1bOD",
    }
    pending = bytearray()
    remaining = keys
    while remaining:
        value = None
        if remaining.startswith("<") and ">" in remaining:
            end = remaining.index(">")
            token = remaining[1:end].lower()
            if token in ("escape", "esc"):
                pending.append(27)
                yield bytes(pending), 0.15
                pending.clear()
                remaining = remaining[end + 1:]
                continue
            if token in sequences:
                value = sequences[token]
            elif token.startswith("c-") and len(token) == 3 and ord(token[-1]) < 128:
                value = bytes([ord(token[-1]) & 0x1f])
            elif token.startswith(("a-", "m-", "s-", "page")) or (
                token.startswith("f") and token[1:].isdigit()
            ):
                raise RuntimeError(f"native C oracle cannot encode <{remaining[1:end]}>")
            if value is not None:
                pending.extend(value)
                remaining = remaining[end + 1:]
                continue
        pending.extend(remaining[0].encode())
        remaining = remaining[1:]
    if pending:
        yield bytes(pending), 0


def prepare_native_reference(data, directory):
    source = directory / "input.in"
    source.write_bytes(data)
    configuration = (
        f"dofile({lua_string(str(ROOT / 'test/vim/visrc.lua'))})\n"
        + observer_source("")
        + "local original_window = nil\n"
        + "local function finish()\n"
        + "  assert(original_window, 'native oracle has no input window')\n"
        + "  observed = true\n"
        + "  before_escape = snapshot(original_window)\n"
        + "  vis:feedkeys('<Escape>')\n"
        + "  capture(original_window)\n"
        + "  vis:command('qall!')\n"
        + "end\n"
        + "for _, mode in pairs(vis.modes) do assert(vis:map(mode, '<F12>', finish)) end\n"
        + "vis.events = vis.events or {}\n"
        + "vis.events.win_open = function(win) if not original_window then original_window = win end end\n"
        + "vis.events.start = function()\n"
        + "  local ready = assert(io.open('ready', 'w')); ready:write('ready'); ready:close()\n"
        + "end\n"
    )
    (directory / "visrc.lua").write_text(configuration)
    return source


def native_reference(binary, data, keys, directory, helper_directory=None):
    """Native core replay so macro recording receives record=true key events."""
    source = prepare_native_reference(data, directory)
    helpers = helper_directory or ROOT
    env = {**os.environ, "VIS_PATH": str(directory), "TERM": "xterm", "LC_ALL": "C.UTF-8",
           "PATH": str(helpers) + os.pathsep + os.environ.get("PATH", "")}
    master, slave = os.openpty()
    process = None
    transcript = bytearray()
    deadline = time.monotonic() + 20

    def drain(timeout):
        if select.select([master], [], [], timeout)[0]:
            try:
                transcript.extend(os.read(master, 65536))
                if len(transcript) > 262144:
                    del transcript[:-262144]
            except OSError as error:
                if error.errno not in (errno.EIO, errno.EAGAIN):
                    raise

    def check_live():
        if process.poll() is not None:
            raise RuntimeError(f"native C oracle exited early ({process.returncode}): {bytes(transcript[-1500:])!r}")
        if time.monotonic() >= deadline:
            raise RuntimeError(f"native C oracle timed out: {bytes(transcript[-1500:])!r}")

    def send(value):
        offset = 0
        while offset < len(value):
            check_live()
            readable, writable, _ = select.select([master], [master], [], 0.05)
            if readable:
                drain(0)
            if writable:
                try:
                    offset += os.write(master, value[offset:])
                except BlockingIOError:
                    continue

    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        process = subprocess.Popen([str(binary), str(source)], cwd=directory, env=env,
                                   stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.set_blocking(master, False)
        while not (directory / "ready").is_file() or termios.tcgetattr(slave)[3] & termios.ICANON:
            check_live()
            drain(0.05)
        for value, delay in native_key_chunks(keys):
            send(value)
            until = time.monotonic() + delay
            while time.monotonic() < until:
                check_live()
                drain(min(0.05, until - time.monotonic()))
        send(b"\x1b[24~")  # Unique F12 observer, after all real recorded input.
        while process.poll() is None:
            if time.monotonic() >= deadline:
                raise RuntimeError(f"native C oracle did not capture/exit: {bytes(transcript[-1500:])!r}")
            drain(0.05)
        drain(0)
        if process.returncode or not (directory / "output").is_file():
            raise RuntimeError(f"native C oracle failed ({process.returncode}): {bytes(transcript[-1500:])!r}")
        if (directory / "observed").read_text() != "before_escape":
            raise RuntimeError("native C oracle did not observe state before Escape")
        return (directory / "output").read_bytes(), parse_state((directory / "state").read_text())
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        os.close(master)
        os.close(slave)


def parse_state(source):
    def integer(value):
        return None if value == "nil" else int(value)

    def snapshot(source):
        lines = source.splitlines()
        if len(lines) < 4 or not lines[2].startswith("primary ") or not lines[3].startswith("selections "):
            raise ValueError(f"malformed replay state: {source[:200]!r}")
        count = int(lines[3].split()[1])
        if len(lines) != count + 4:
            raise ValueError("selection count does not match state rows")
        selections = []
        for line in lines[4:]:
            kind, cursor, start, end, anchored, endpoint = line.split()
            if kind != "selection" or anchored not in ("0", "1") or endpoint not in ("point", "start", "end"):
                raise ValueError(f"malformed selection: {line!r}")
            selections.append({"cursor": integer(cursor), "start": integer(start), "end": integer(end),
                               "anchored": anchored == "1", "endpoint": endpoint})
        return {"cursor": integer(lines[0]), "mode": lines[1], "primary": int(lines[2].split()[1]),
                "selections": selections}

    final, separator, before = source.partition("before_escape\n")
    if not separator:
        raise ValueError("missing before-Escape selection snapshot")
    return {"final": snapshot(final), "before_escape": snapshot(before)}


def config_profile(fixture, builtin):
    if fixture is not None and fixture.is_relative_to(ROOT / "test/vis"):
        return "vis"
    if fixture is not None and fixture.is_relative_to(ROOT / "test/vim"):
        return "core"
    return builtin


def lua_reference(binary, data, keys, directory, profile, helper_directory, native=False):
    key_file = directory / "keys"
    key_file.write_text(keys)
    if native:
        source = prepare_native_reference(data, directory)
        command = [str(binary), str(source), "--config-dir", str(directory), "--native-keys", str(key_file)]
    else:
        source = prepare_reference(data, keys, directory, profile)
        command = [str(binary), str(source), "--config-dir", str(directory)]
    env = {**os.environ, "VIS_PATH": str(directory), "TERM": "xterm", "LC_ALL": "C.UTF-8",
           "PATH": str(helper_directory) + os.pathsep + os.environ.get("PATH", "")}
    return subprocess.run(command, cwd=directory, env=env, capture_output=True, timeout=20)


@contextlib.contextmanager
def case_directory(args, name):
    with tempfile.TemporaryDirectory(prefix="vis-key-diff-") as temporary:
        directory = Path(temporary)
        try:
            yield directory
        finally:
            if args.artifacts and not (directory / ".passed").is_file():
                args.artifacts.mkdir(parents=True, exist_ok=True)
                target = Path(tempfile.mkdtemp(prefix="failure-", dir=args.artifacts))
                shutil.copytree(directory, target, dirs_exist_ok=True)
                (target / "case.txt").write_text(str(name) + "\n")
                print(f"  artifacts: {target}")


def compare(args, name, data, keys, fixture=None):
    with case_directory(args, name) as directory:
        c_dir, rust_dir = directory / "c", directory / "rust"
        c_dir.mkdir()
        rust_dir.mkdir()
        profile = config_profile(fixture, args.builtin_config)
        if args.lua and fixture is not None and fixture.with_suffix(".lua").is_file():
            for working in (c_dir, rust_dir):
                (working / "input.lua").write_bytes(fixture.with_suffix(".lua").read_bytes())
        if fixture in NATIVE_MACRO_FIXTURES:
            expected, expected_state = native_reference(
                args.c_vis.resolve(), data, keys, c_dir, args.helper_directory,
            )
        else:
            expected, expected_state = reference(
                args.c_vis.resolve(), data, keys, c_dir,
                profile, args.helper_directory,
            )
        source, key_file, state = rust_dir / "input", rust_dir / "keys", rust_dir / "state"
        key_file.write_text(keys)
        if args.lua:
            actual = lua_reference(args.replay.resolve(), data, keys, rust_dir, profile,
                                   args.lua_helper_directory, fixture in NATIVE_MACRO_FIXTURES)
        else:
            source.write_bytes(data)
            command = [str(args.replay.resolve()), str(source), str(key_file), "--state", str(state)]
            actual = subprocess.run(command, cwd=rust_dir, capture_output=True, timeout=15)
        (rust_dir / "stderr").write_bytes(actual.stderr)
        if actual.returncode:
            status = "UNSUPPORTED" if actual.returncode == 2 else "ERROR"
            print(f"{status} {name}: {actual.stderr.decode(errors='replace').strip()}")
            return status
        actual_bytes = actual.stdout
        if args.lua:
            if (rust_dir / "observed").read_text() not in ("before_escape", "closed_during_replay"):
                raise RuntimeError("Rust Lua replay did not execute its before-Escape observer")
            if profile == "vis":
                # The original vis fixture saves after replay. Compare the bytes
                # actually saved, including FILE_SAVE_PRE edits, while retaining
                # the independently observed before/after-Escape selection state.
                if not (c_dir / "input.out").is_file():
                    raise RuntimeError("C fixture callback did not complete its original save")
                if not (rust_dir / "input.out").is_file():
                    raise RuntimeError("Rust fixture callback did not complete its original save: "
                                       + actual.stderr.decode(errors="replace").strip())
                expected = (c_dir / "input.out").read_bytes()
                actual_bytes = (rust_dir / "input.out").read_bytes()
                if fixture is not None and fixture.with_suffix(".lua").is_file():
                    upstream = fixture.with_suffix(".ref").read_bytes()
                    if expected != upstream:
                        raise RuntimeError("C fixture save did not match the authoritative .ref output")
        actual_state = parse_state(state.read_text())
        if actual_bytes == expected and actual_state == expected_state:
            (directory / ".passed").touch()
            print(f"PASS {name}")
            return "PASS"
        print(f"FAIL {name}")
        for phase in ("before_escape", "final"):
            if actual_state[phase] != expected_state[phase]:
                print(f"  {phase} C:    {expected_state[phase]}")
                print(f"  {phase} Rust: {actual_state[phase]}")
        if actual_bytes != expected:
            mismatch = next((i for i, (a, b) in enumerate(zip(actual_bytes, expected)) if a != b),
                            min(len(actual_bytes), len(expected)))
            start, end = max(0, mismatch - 60), mismatch + 100
            print(f"  first different byte {mismatch}; C length={len(expected)}, Rust length={len(actual_bytes)}")
            print(f"  expected {expected[start:end]!r}\n  received {actual_bytes[start:end]!r}")
        return "FAIL"


def verify_observers(args):
    """Reject a broken state oracle before reporting any editor comparisons."""
    expected_before = {
        "cursor": 2, "mode": "VISUAL", "primary": 0,
        "selections": [{"cursor": 2, "start": 0, "end": 3,
                        "anchored": True, "endpoint": "end"}],
    }
    expected_final = {
        "cursor": 2, "mode": "NORMAL", "primary": 0,
        "selections": [{"cursor": 2, "start": 2, "end": 3,
                        "anchored": False, "endpoint": "point"}],
    }
    with tempfile.TemporaryDirectory(prefix="vis-observer-check-") as name:
        directory = Path(name)
        for profile in ("standard", "core", "vis", "native"):
            working = directory / profile
            working.mkdir()
            if profile == "native":
                output, state = native_reference(args.c_vis.resolve(), b"one two\n", "v2l",
                                                 working, args.helper_directory)
            else:
                output, state = reference(args.c_vis.resolve(), b"one two\n", "v2l", working,
                                          profile, args.helper_directory)
            if output != b"one two\n" or state != {
                "before_escape": expected_before, "final": expected_final,
            }:
                raise RuntimeError(f"{profile} C observer lost before-Escape anchored state: {state!r}")
            if args.lua:
                rust_working = directory / f"rust-{profile}"
                rust_working.mkdir()
                result = lua_reference(args.replay.resolve(), b"one two\n", "v2l", rust_working,
                                       profile, args.lua_helper_directory, profile == "native")
                if result.returncode:
                    raise RuntimeError(f"{profile} Rust Lua observer failed: {result.stderr.decode(errors='replace')}")
                rust_state = parse_state((rust_working / "state").read_text())
                if result.stdout != output or rust_state != state:
                    raise RuntimeError(f"{profile} Rust Lua observer lost before-Escape anchored state: {rust_state!r}")
        working = directory / "recording"
        working.mkdir()
        output, _ = native_reference(args.c_vis.resolve(), b"\n", "qaiX<Escape>q@a",
                                     working, args.helper_directory)
        if output != b"XX\n":
            raise RuntimeError(f"native C macro recording self-check failed: {output!r}")
        if args.lua:
            working = directory / "rust-recording"
            working.mkdir()
            result = lua_reference(args.replay.resolve(), b"\n", "qaiX<Escape>q@a", working,
                                   "core", args.lua_helper_directory, True)
            if result.returncode or result.stdout != b"XX\n":
                raise RuntimeError(f"native Rust Lua recording self-check failed: {result.stdout!r}, {result.stderr!r}")
    print("PASS C oracle self-check: distinct anchored before/after-Escape states in all profiles; native macro recording")
    if args.lua:
        print("PASS Rust Lua oracle self-check: same distinct phases in all profiles; native host-key macro recording")


def run_cases(args):
    print(f"C configurations: built-ins={args.builtin_config}; test/vim=core (empty visrc); "
          "test/vis=upstream visrc and standard plugins")
    verify_observers(args)
    fixtures = args.fixtures or ([*sorted((ROOT / "test/vim").rglob("*.keys")),
                                 *sorted((ROOT / "test/vis").rglob("*.keys"))] if args.all
                                else [ROOT / name for name in DEFAULT_FIXTURES])
    counts = dict.fromkeys(["PASS", "FAIL", "UNSUPPORTED", "ERROR"], 0)
    if not args.fixtures:
        for name, data, keys in BUILTINS:
            token = unrepresentable_feedkeys_token(keys)
            if token:
                print(f"UNSUPPORTED {name}: C vis:feedkeys cannot express <{token}> as a key")
                counts["UNSUPPORTED"] += 1
                continue
            try:
                counts[compare(args, name, data, keys)] += 1
            except (RuntimeError, OSError, subprocess.TimeoutExpired, ValueError) as error:
                print(f"ERROR {name}: {error}")
                counts["ERROR"] += 1
    for fixture in fixtures:
        fixture = fixture.resolve()
        name = fixture.relative_to(ROOT) if fixture.is_relative_to(ROOT) else fixture
        if fixture.with_suffix(".lua").exists() and not args.lua:
            print(f"UNSUPPORTED {name}: fixture requires Lua configuration")
            counts["UNSUPPORTED"] += 1
            continue
        if fixture.with_suffix(".in.disabled").exists() and not fixture.with_suffix(".in").exists():
            print(f"UNSUPPORTED {name}: input fixture is disabled upstream")
            counts["UNSUPPORTED"] += 1
            continue
        keys = normalize(fixture)
        token = unrepresentable_feedkeys_token(keys)
        if token:
            print(f"UNSUPPORTED {name}: C vis:feedkeys cannot express <{token}> as a key")
            counts["UNSUPPORTED"] += 1
            continue
        try:
            counts[compare(args, name, fixture.with_suffix(".in").read_bytes(), keys, fixture)] += 1
        except (RuntimeError, OSError, subprocess.TimeoutExpired, ValueError) as error:
            print(f"ERROR {name}: {error}")
            counts["ERROR"] += 1
    print(", ".join(f"{value} {key.lower()}" for key, value in counts.items()))
    return int(any(counts[key] for key in ["FAIL", "UNSUPPORTED", "ERROR"]))


def prepare_helper(directory):
    helper = ROOT / "vis-digraph"
    if helper.is_file() and os.access(helper, os.X_OK):
        return ROOT
    helper = directory / "vis-digraph"
    command = shlex.split(os.environ.get("CC", "cc")) + [
        "-std=c99", str(ROOT / "vis-digraph.c"), "-o", str(helper),
    ]
    result = subprocess.run(command, text=True, capture_output=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"C digraph helper build failed: {result.stderr.strip()}")
    return directory


def prepare_lua_helper(directory, binary):
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError("Rust digraph helper is absent; run cargo build --manifest-path rust/Cargo.toml "
                           "--bin vis-digraph-rs or set --rust-digraph")
    directory.mkdir()
    (directory / "vis-digraph").symlink_to(binary.resolve())
    return directory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--replay", type=Path)
    parser.add_argument("--lua", action="store_true", help="run real Lua configuration and saves with lua_replay")
    parser.add_argument("--rust-digraph", type=Path, default=ROOT / "rust/target/debug/vis-digraph-rs",
                        help="Rust helper used by Lua plugins (never the C oracle helper)")
    parser.add_argument("--artifacts", type=Path, help="retain failed case inputs, configuration, state, output and stderr")
    parser.add_argument("--all", action="store_true", help="exercise all upstream key fixtures")
    parser.add_argument("--builtin-config", choices=("standard", "core"), default="standard",
                        help="C configuration for built-in cases (fixtures keep their upstream configuration)")
    parser.add_argument("fixtures", nargs="*", type=Path)
    args = parser.parse_args()
    if args.replay is None:
        args.replay = ROOT / "rust/target/debug/examples" / ("lua_replay" if args.lua else "replay")
    if args.artifacts is not None:
        args.artifacts = args.artifacts.resolve()
    with tempfile.TemporaryDirectory(prefix="vis-key-helpers-") as temporary:
        try:
            args.helper_directory = prepare_helper(Path(temporary))
            if args.lua:
                args.lua_helper_directory = prepare_lua_helper(Path(temporary) / "rust", args.rust_digraph)
            return run_cases(args)
        except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
            print(f"ERROR C oracle setup: {error}")
            return 1


if __name__ == "__main__":
    raise SystemExit(main())
