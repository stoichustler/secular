#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fill userspace/secular/meta/secular.txt placeholders for BOOTFS."""

from __future__ import annotations

import argparse
import datetime
import os
import re
import subprocess
import sys
from pathlib import Path


PLACEHOLDER = re.compile(r"\{\{([A-Z0-9_]+)\}\}")


def first_line(command: list[str]) -> str:
    try:
        completed = subprocess.run(
            command,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except OSError:
        return "unavailable"
    text = (completed.stdout or "").strip()
    if not text:
        return "unavailable"
    return text.splitlines()[0].strip()


def iso_utc_from_unix(stamp: str) -> str:
    if not stamp:
        return "unknown"
    try:
        seconds = int(stamp)
    except ValueError:
        return stamp
    return datetime.datetime.fromtimestamp(seconds, datetime.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--template", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    parser.add_argument("--clang")
    parser.add_argument("--rustc")
    parser.add_argument("--gn")
    parser.add_argument("--ninja")
    parser.add_argument("--qemu")
    parser.add_argument("--commit-stamp")
    args = parser.parse_args(argv)

    values: dict[str, str] = {}
    for item in args.set:
        if "=" not in item:
            raise SystemExit(f"invalid --set (expected KEY=VALUE): {item}")
        key, value = item.split("=", 1)
        values[key] = value

    if args.clang and "CLANG_VERSION" not in values:
        values["CLANG_VERSION"] = first_line([args.clang, "--version"])
    if args.rustc and "RUSTC_VERSION" not in values:
        values["RUSTC_VERSION"] = first_line([args.rustc, "--version"])
    if args.gn and "GN_VERSION" not in values:
        values["GN_VERSION"] = first_line([args.gn, "--version"])
    if args.ninja and "NINJA_VERSION" not in values:
        values["NINJA_VERSION"] = first_line([args.ninja, "--version"])
    if args.qemu and "QEMU_VERSION" not in values:
        values["QEMU_VERSION"] = first_line([args.qemu, "--version"])
    if args.commit_stamp and "COMMIT_DATE" not in values:
        values["COMMIT_DATE"] = iso_utc_from_unix(args.commit_stamp)

    template = Path(args.template).read_text(encoding="utf-8")

    def replace(match: re.Match[str]) -> str:
        key = match.group(1)
        if key not in values:
            raise SystemExit("unsubstituted placeholder {{" + key + "}}")
        return values[key]

    text = PLACEHOLDER.sub(replace, template)
    leftover = PLACEHOLDER.findall(text)
    if leftover:
        raise SystemExit(f"unsubstituted placeholders: {', '.join(sorted(set(leftover)))}")

    output = Path(args.output)
    os.makedirs(output.parent, exist_ok=True)
    if not text.endswith("\n"):
        text += "\n"
    output.write_text(text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
