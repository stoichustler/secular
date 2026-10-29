#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Boot Secular on a PTY and verify serial console output."""

from __future__ import annotations

import argparse
import os
import pathlib
import pty
import re
import selectors
import signal
import subprocess
import sys
import time


BOOT_MARKERS = (
    "physboot",
    "userboot",
)
CONSOLE_MARKERS = (
    "component_manager",
    "zxsh",
)
FATAL_MARKERS = (
    "<== CRASH:",
    "ZIRCON KERNEL OOPS",
    "KERNEL PANIC",
    "StorageBootFs item not found",
)
# Zircon logs look like "00000:01025> ..."; do not treat those as a shell.
PROMPT = re.compile(rb"(?:^|[\r\n])(?:secular |\$ |zxsh[>#] )")


def _run_qemu(root: pathlib.Path, arch: str, timeout_s: float) -> tuple[int, str, bool]:
    master, slave = pty.openpty()
    script = root / "tools" / "secular-boot" / "run-qemu.sh"
    proc = subprocess.Popen(
        [str(script), arch],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        cwd=root,
        preexec_fn=os.setsid,
    )
    os.close(slave)
    selector = selectors.DefaultSelector()
    selector.register(master, selectors.EVENT_READ)
    chunks: list[bytes] = []
    deadline = time.monotonic() + timeout_s
    saw_prompt = False
    try:
        os.write(master, b"echo CONSOLE_OK\n")
        while time.monotonic() < deadline:
            events = selector.select(timeout=0.5)
            for key, _ in events:
                try:
                    data = os.read(key.fd, 4096)
                except OSError:
                    data = b""
                if not data:
                    proc.poll()
                    break
                chunks.append(data)
                if PROMPT.search(b"".join(chunks[-8:])):
                    saw_prompt = True
                    os.write(master, b"echo CONSOLE_OK\n")
            if proc.poll() is not None:
                break
            text = b"".join(chunks).decode("utf-8", "replace")
            if "CONSOLE_OK" in text and saw_prompt:
                break
            if any(marker in text for marker in FATAL_MARKERS):
                break
    finally:
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
        os.close(master)
    output = b"".join(chunks).decode("utf-8", "replace")
    return proc.returncode or 0, output, saw_prompt


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("arch")
    parser.add_argument("--timeout", type=float, default=90.0)
    args = parser.parse_args(argv)
    root = pathlib.Path(__file__).resolve().parents[2]
    status, text, saw_prompt = _run_qemu(root, args.arch, args.timeout)
    lower = text.lower()
    for marker in FATAL_MARKERS:
        if marker in text:
            sys.stderr.write(text[-8000:])
            print(f"FAIL: fatal boot marker {marker}", file=sys.stderr)
            return 1
    missing = [marker for marker in BOOT_MARKERS if marker not in lower]
    if missing:
        sys.stderr.write(text[-8000:])
        print(f"FAIL: missing boot markers {missing}", file=sys.stderr)
        return 1
    if "CONSOLE_OK" not in text and not any(marker in lower for marker in CONSOLE_MARKERS):
        sys.stderr.write(text[-8000:])
        print("FAIL: serial console did not become usable", file=sys.stderr)
        return 1
    if not saw_prompt:
        sys.stderr.write(text[-8000:])
        print("FAIL: no interactive prompt on serial", file=sys.stderr)
        return 1
    print("PASS: qemu-system-aarch64 console boot")
    return 0 if status in (0, -signal.SIGTERM) else status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
