#!/usr/bin/env python3
"""Measure interactive latency of C vis versus the Rust port.

The editor redraws after every key. A PTY buffers output, so timing the write
alone measures nothing, and waiting for a fixed number of reads can block when
the editor is faster than the reader.

This harness therefore measures the time from sending a burst of keys until
the terminal stops producing output: the editor processed every key and
emitted the final frame. The time is measured to the last byte the editor
produced, not to the end of the quiet period, so the measurement carries no
fixed overhead. Dividing by the key count gives the per-key cost.

Run from the repository root:

    python3 rust/tests/latency_probe.py --binary ./vis
    python3 rust/tests/latency_probe.py --binary rust/target/release/vis-rs
"""
import argparse
from pathlib import Path
import statistics
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "rust/tests"))
from terminal_smoke import Session  # noqa: E402


def build_file(path: Path, size: int) -> None:
    line = b"the quick brown fox jumps over the lazy dog 0123456789\n"
    body = (line * (size // len(line) + 1))[:size]
    path.write_bytes(body)


def burst_time(session: Session, keys: bytes, idle: float = 0.05) -> float:
    """Send keys and return the seconds until the editor stopped producing.

    The returned time ends with the last byte the editor wrote, so the
    trailing idle period is not part of it.
    """
    start = time.monotonic()
    session.send(keys)
    last_change = start
    deadline = start + 120.0
    while time.monotonic() < deadline:
        before = len(session.output)
        session.read(0.002)
        if len(session.output) != before:
            last_change = time.monotonic()
        elif time.monotonic() - last_change >= idle:
            break
    return last_change - start


def measure(binary: Path, size: int, rounds: int, keys: int) -> None:
    with tempfile.TemporaryDirectory(prefix="vis-latency-") as folder:
        path = Path(folder) / "input"
        build_file(path, size)
        session = Session(binary, path)
        try:
            session.ready()
            # Let the editor settle before the first measurement.
            burst_time(session, b"", idle=0.3)

            per_round = []
            for _ in range(rounds):
                elapsed = burst_time(session, b"j" * keys)
                per_round.append(elapsed * 1000.0 / keys)
            session.send(b"\x1b")
            burst_time(session, b"", idle=0.05)
        finally:
            session.close()

    per_round.sort()
    print(
        f"size={size // 1024:>5}KiB  "
        f"ms/key: median={statistics.median(per_round):8.3f} "
        f"best={per_round[0]:8.3f} worst={per_round[-1]:8.3f}"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--sizes", type=int, nargs="*", default=[64, 1024, 4096])
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--keys", type=int, default=100)
    args = parser.parse_args()
    binary = args.binary.resolve()
    print(f"binary: {binary}")
    for size in args.sizes:
        measure(binary, size * 1024, args.rounds, args.keys)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
