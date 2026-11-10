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
TRUSTY_MARKERS = (
    "trusty: el3 spd ready",
    "TEE: s-el1 services started",
    "TEE: init complete, 8 cpus",
)
TRUSTY_ALIVE_MARKER = "TEE: alive"
# Kernel trusty-probe also prints TRUSTY_ALIVE_MARKER. Require the userspace
# tee-probe SMC path specifically so a blocked SmcResource cannot pass.
TEE_PROBE_MARKER = "tee: s-el1 nop"
TEE_PROBE_SMP_MARKER = "tee: alive (8 cpus)"
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
    probed_tee = False
    echoed_ok = False
    try:
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
                joined = b"".join(chunks)
                if not saw_prompt and PROMPT.search(joined):
                    saw_prompt = True
                if saw_prompt and not probed_tee:
                    os.write(master, b"/boot/bin/tee-probe\n")
                    probed_tee = True
            if proc.poll() is not None:
                break
            text = b"".join(chunks).decode("utf-8", "replace")
            if probed_tee and TEE_PROBE_MARKER in text.lower() and not echoed_ok:
                os.write(master, b"echo CONSOLE_OK\n")
                echoed_ok = True
            if "CONSOLE_OK" in text and TEE_PROBE_MARKER in text.lower() and saw_prompt:
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
    missing_trusty = [marker for marker in TRUSTY_MARKERS if marker not in lower]
    if missing_trusty:
        sys.stderr.write(text[-8000:])
        print(f"FAIL: missing Trusty TEE markers {missing_trusty}", file=sys.stderr)
        return 1
    if TRUSTY_ALIVE_MARKER not in lower:
        sys.stderr.write(text[-8000:])
        print("FAIL: kernel Trusty probe did not report Trusty TEE alive", file=sys.stderr)
        return 1
    if TEE_PROBE_MARKER not in lower:
        sys.stderr.write(text[-8000:])
        print("FAIL: tee-probe did not complete userspace SMC to S-EL1", file=sys.stderr)
        return 1
    if TEE_PROBE_SMP_MARKER not in lower:
        sys.stderr.write(text[-8000:])
        print("FAIL: tee-probe did not NOP S-EL1 on all 8 cpus", file=sys.stderr)
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
