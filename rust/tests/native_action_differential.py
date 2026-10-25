#!/usr/bin/env python3
"""Exercise named native actions through real Rust/C Lua runtimes.

Recognition is checked separately by editor::actions::tests against every entry
in main.c. These temporary-file cases compare bytes, mode, primary selection,
ordered cursor/range/anchor state both before and after final Escape. Unsupported
operations are reported separately and never counted as passes. No suspension,
interactive shell, or destructive host action is executed.
"""
import argparse
from pathlib import Path
import tempfile
import key_differential as oracle

ROOT = Path(__file__).resolve().parents[2]
TEXT = b"one two three\nfour five\nsix\n"
CASES = [
    ("character-count", TEXT, "2<vis-motion-char-next>iX<Escape>"),
    ("character-cross-line", b"ab\ncd\n", "2<vis-motion-char-next><vis-motion-char-next>iX<Escape>"),
    ("line-char-boundary", b"ab\ncd\n", "9<vis-motion-line-char-next>iX<Escape>"),
    ("codepoint", "a\u0301♫b\n".encode(), "<vis-motion-codepoint-next>iX<Escape>"),
    ("word-start", TEXT, "2<vis-motion-word-start-next>iX<Escape>"),
    ("word-end", TEXT, "2<vis-motion-word-end-next>iX<Escape>"),
    ("bigword", b"one-two three\n", "<vis-motion-bigword-start-next>iX<Escape>"),
    ("line-begin-idempotent", TEXT, "w99<vis-motion-line-begin>iX<Escape>"),
    ("line-start", b"\t  text\n", "<vis-motion-line-start>iX<Escape>"),
    ("line-end", TEXT, "<vis-motion-line-end>iX<Escape>"),
    ("line-finish", b"text  \n", "<vis-motion-line-finish>iX<Escape>"),
    ("line-down", TEXT, "2<vis-motion-line-down>iX<Escape>"),
    ("line-last", TEXT, "<vis-motion-line-last>iX<Escape>"),
    ("line-number", TEXT, "2<vis-motion-line-first>iX<Escape>"),
    ("byte", TEXT, "5<vis-motion-byte>iX<Escape>"),
    ("byte-utf8", "♫abc\n".encode(), "1<vis-motion-byte>iX<Escape>"),
    ("change-word-motion", TEXT, "<vis-operator-change><vis-motion-word-start-next>X<Escape>"),
    ("byte-relative", TEXT, "5<vis-motion-byte-right>2<vis-motion-byte-left>iX<Escape>"),
    ("column", TEXT, "4<vis-motion-column>iX<Escape>"),
    ("percent", TEXT, "50<vis-motion-percent>iX<Escape>"),
    ("block", b"{ one { two } three }\n", "<vis-motion-block-end>iX<Escape>"),
    ("parenthesis", b"(one (two) three)\n", "<vis-motion-parenthesis-end>iX<Escape>"),
    ("delete-motion", TEXT, "<vis-operator-delete><vis-motion-word-start-next>"),
    ("delete-count", TEXT, "2<vis-operator-delete>2<vis-motion-word-start-next>"),
    ("delete-doubled", TEXT, "<vis-operator-delete><vis-operator-delete>"),
    ("change-object", TEXT, "<vis-operator-change><vis-textobject-word-inner>X<Escape>"),
    ("yank-put", TEXT, "<vis-operator-yank><vis-textobject-word-inner><vis-put-after>"),
    ("register", TEXT, "<vis-register>a<vis-operator-delete><vis-textobject-word-inner><vis-register>a<vis-put-before>"),
    ("shift", TEXT, "<vis-operator-shift-right><vis-operator-shift-right>"),
    ("shift-multi-shared", b"one two\nthree four\nfive six\n", ":x/[a-z]+/<Enter><vis-operator-shift-right>"),
    ("shift-multi-reversed", b"one two\nthree four\nfive six\n", ":x/[a-z]+/<Enter><vis-selection-flip><vis-operator-shift-right>"),
    ("shift-left-multi-shared", b"\t\tone two\n\t\tthree four\n", ":x/[a-z]+/<Enter><vis-operator-shift-left>"),
    ("shift-multi-count-overlap", b"one\ntwo\nthree\nfour\nfive\n", "<C-j><C-j>2>>"),
    ("shift-multi-expandtab", b"one two\nthree four\n", ":set tabwidth 3<Enter>:set expandtab<Enter>:x/[a-z]+/<Enter>>"),
    ("shift-multi-spaces", b"       one two\n       three four\n", ":set tabwidth 3<Enter>:x/[a-z]+/<Enter><"),
    ("shift-blank-lines", b"one\n\ntwo\nthree\n", "V2j>"),
    ("shift-multi-dot", b"one\ntwo\nthree\nfour\nfive\n", "<C-j>2>>j."),
    ("shift-multi-undo", b"one two\nthree four\n", ":x/[a-z]+/<Enter><vis-operator-shift-right><vis-undo>"),
    ("shift-registers", b"one\ntwo\nthree\nfour\n", "yy<C-j><C-j>>><Escape>P"),
    ("shift-invalid-bytes", b"\xffone\n\x80two\n\xfethree\n", "<C-j>>><C-k><<"),

    ("delete-action", TEXT, "<vis-delete-char-next>"),
    ("delete-backward", TEXT, "w<vis-delete-char-prev>"),
    ("mode-insert", TEXT, "<vis-mode-insert>X<vis-mode-normal>"),
    ("append", TEXT, "<vis-append-char-next>X<vis-mode-normal>"),
    ("append-line", TEXT, "<vis-append-line-end>X<vis-mode-normal>"),
    ("insert-start", b"  text\n", "<vis-insert-line-start>X<vis-mode-normal>"),
    ("replace-mode", TEXT, "<vis-mode-replace>X<vis-mode-normal>"),
    ("open-above", TEXT, "<vis-open-line-above>X<vis-mode-normal>"),
    ("open-below", TEXT, "<vis-open-line-below>X<vis-mode-normal>"),
    ("newline", TEXT, "i<vis-insert-newline><Escape>"),
    ("tab", TEXT, "i<vis-insert-tab><Escape>"),
    ("verbatim", TEXT, "i<vis-insert-verbatim>u2200<Escape>"),
    ("replace-char", TEXT, "<vis-replace-char>X"),
    ("visual", TEXT, "<vis-mode-visual-charwise>2<vis-motion-char-next>"),
    ("visual-line", TEXT, "<vis-mode-visual-linewise>"),
    ("visual-line-motion", TEXT, "<vis-mode-visual-linewise><vis-motion-line-down>"),
    ("physical-line-motion", TEXT, "Vj"),
    ("visual-line-reverse", TEXT, "j<vis-mode-visual-linewise><vis-motion-line-up>"),
    ("visual-line-object", TEXT, "<vis-mode-visual-linewise><vis-textobject-word-inner>"),
    ("visual-flip", TEXT, "v2l<vis-selection-flip>"),
    ("visual-escape", TEXT, "v2l<vis-mode-visual-escape>"),
    ("visual-count-escape", TEXT, "v2l5<vis-mode-visual-escape>"),
    ("object-outer-word", TEXT, "<vis-operator-delete><vis-textobject-word-outer>"),
    ("object-bigword", b"one-two three\n", "<vis-operator-delete><vis-textobject-bigword-inner>"),
    ("object-quote", b'"one two" rest\n', "l<vis-operator-delete><vis-textobject-quote-inner>"),
    ("object-brackets", b"(one [two]) rest\n", "l<vis-operator-delete><vis-textobject-parenthesis-outer>"),
    ("object-line", TEXT, "<vis-operator-delete><vis-textobject-line-outer>"),
    ("selections-new", TEXT, "<vis-selection-new-lines-below><vis-selection-new-lines-below>iX<Escape>"),
    ("selections-remove", TEXT, "<vis-selection-new-lines-below><vis-selections-remove-last>"),
    ("selections-navigate", TEXT, "<vis-selection-new-lines-below><vis-selection-prev>"),
    ("selections-clear", TEXT, "<vis-selection-new-lines-below><vis-selections-remove-all>"),
    ("selections-save-restore", TEXT, "v2l<vis-selections-save><Escape>w<vis-selections-restore>"),
    ("selections-match", b"one one one\n", "viw<vis-selection-new-match-next>"),
    ("selections-match-all", b"one one one\n", "viw<vis-selection-new-match-all>"),
    ("selections-trim", b" one \n", "v3l<vis-selections-trim>"),
    ("search-word-forward", b"one two one\n", "<vis-motion-search-word-forward>iX<Escape>"),
    ("search-word-backward", b"one two one\n", "8l<vis-motion-search-word-backward>iX<Escape>"),
    ("search-word-wrap", b"one two one\n", "8l<vis-motion-search-word-forward><vis-mode-insert>X<vis-mode-normal>"),
    ("find-to-right", b"abc\ndef\n", "<vis-motion-to-right>d<vis-mode-insert>X<vis-mode-normal>"),
    ("find-to-left", b"abc\ndef\n", "<vis-motion-line-down><vis-motion-to-left>b<vis-mode-insert>X<vis-mode-normal>"),
    ("find-till-right", b"abc\ndef\n", "<vis-motion-till-right>e<vis-mode-insert>X<vis-mode-normal>"),
    ("find-till-left", b"abc\ndef\n", "<vis-motion-line-down><vis-motion-till-left>b<vis-mode-insert>X<vis-mode-normal>"),
    ("find-to-right-repeat", b"a b a c a\n", "<vis-motion-to-right>a;<vis-mode-insert>X<vis-mode-normal>"),
    ("case-upper-char", b"hello\n", "<vis-selections-case-toupper>"),
    ("case-lower-visual", b"HELLO\n", "v4l<vis-selections-case-tolower>"),
    ("case-toggle-visual", b"hELLo\n", "v4l<vis-selections-case-toggle>"),
    ("case-upper-utf8", "éß\n".encode(), "v1l<vis-selections-case-toupper>"),
    ("case-upper-multi", b"one two\nthree\n", ":x/[a-z]+/<Enter><vis-selections-case-toupper>"),
    ("case-invalid-utf8", b"\xffone\n", "<vis-selections-case-toupper>"),
    ("selections-union", TEXT, "v2l<vis-selections-save><Escape>wv2l<vis-selections-union>"),
    ("selections-complement", TEXT, "v2l<vis-selections-complement>"),
    ("find", TEXT, "<vis-motion-to-line-right>t"),
    ("till", TEXT, "<vis-motion-till-line-right>t"),
    ("find-repeat", TEXT, "<vis-motion-to-line-right>o<vis-motion-totill-repeat>"),
    ("join", TEXT, "<vis-join-lines>"),
    ("join-trim", TEXT, "<vis-join-lines-trim>"),
    ("join-multi", b"one\ntwo\nthree\nfour\nfive\n", "<C-j><C-j><vis-join-lines>"),
    ("join-multi-trim", b"one\n  two\n\tthree\n four\nfive\n", "<C-j><C-j><vis-join-lines-trim>"),
    ("join-multi-shared", b"one two\nthree four\nfive six\nseven eight\n", ":x/[a-z]+/<Enter><Escape><vis-join-lines>"),
    ("join-multi-count", b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n", "<C-j>3<vis-join-lines>"),
    ("join-multi-dot", b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n", "<C-j>J."),
    ("join-first-line-visual", b"one\ntwo\nthree\n", "VJ"),
    ("join-later-line-visual", b"one\ntwo\nthree\nfour\n", "jVJ"),
    ("join-visual-reverse", b"one\ntwo\nthree\nfour\n", "V2jkJ"),
    ("join-multi-visual", b"one two\nthree four\nfive six\n", ":x/[a-z]+/<Enter><vis-selection-flip>J"),
    ("join-invalid-bytes", b"\xffone\n\x80two\n\xfethree\nfour\n", "<C-j>J"),
    ("join-empty-lines", b"one\n\n two\n\t\nthree\n", "<C-j>3J"),
    ("join-undo", b"one\ntwo\nthree\nfour\n", "<C-j>Ju"),
    ("join-registers", b"one\ntwo\nthree\nfour\n", "yy<C-j>J<Escape>P"),
    ("undo-redo", TEXT, "iX<Escape><vis-undo><vis-redo>"),
    ("repeat", TEXT, "iX<Escape>w<vis-repeat>"),
    ("macro-replay", b"iX<Escape>\n", "v9l<vis-register>a<vis-operator-yank><vis-macro-replay>a"),
    ("unicode-info", TEXT, "<vis-unicode-info>"),
    ("utf8-info", TEXT, "<vis-utf8-info>"),
    ("prompt-command", TEXT, "<vis-prompt-show>1<Enter>"),
    ("prompt-search", TEXT, "<vis-search-forward>two<Enter>iX<Escape>"),
    ("no-op", TEXT, "<vis-nop>"),
    ("named-count-no-digit", TEXT, "<vis-count>"),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-vis", type=Path, default=ROOT / "vis")
    parser.add_argument("--replay", type=Path, default=ROOT / "rust/target/debug/examples/lua_replay")
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("cases", nargs="*")
    args = parser.parse_args()
    unknown = set(args.cases) - {name for name, _, _ in CASES}
    if unknown:
        parser.error("unknown native action cases: " + ", ".join(sorted(unknown)))
    passed = failed = unsupported = 0
    for name, data, keys in CASES:
        if args.cases and name not in args.cases:
            continue
        with tempfile.TemporaryDirectory(prefix="vis-native-action-") as temporary:
            directory = Path(temporary)
            cdir, rdir = directory / "c", directory / "rust"
            cdir.mkdir(); rdir.mkdir()
            expected, state = oracle.reference(args.c_vis.resolve(), data, keys, cdir, "core")
            result = oracle.lua_reference(args.replay.resolve(), data, keys, rdir, "core", ROOT)
            if b"unsupported native action" in result.stderr:
                unsupported += 1
                print("UNSUPPORTED", name, result.stderr.decode(errors="replace").strip())
                continue
            actual_state = oracle.parse_state((rdir / "state").read_text()) if (rdir / "state").exists() else None
            # Informational actions deliberately emit a message. Unexpected
            # diagnostics must not be hidden by matching final bytes/state.
            diagnostics_ok = not result.stderr or name in ("unicode-info", "utf8-info")
            if result.returncode == 0 and diagnostics_ok and result.stdout == expected and actual_state == state:
                passed += 1
                print("PASS", name)
            else:
                failed += 1
                print("FAIL", name, "keys", keys)
                print("  bytes", repr(expected), repr(result.stdout))
                print("  C", state)
                print("  Rust", actual_state, result.stderr.decode(errors="replace"))
                if args.artifacts:
                    import shutil
                    target = args.artifacts / name
                    shutil.copytree(directory, target, dirs_exist_ok=True)
    print(f"{passed} pass, {failed} fail, {unsupported} explicitly unsupported (not passes)")
    return bool(failed or unsupported)

if __name__ == "__main__":
    raise SystemExit(main())
