#!/usr/bin/env fuchsia-vendored-python
# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Runs the zbi tool, taking care of unwrapping response files."""

import argparse
import os
import subprocess


def _pad_file(path: str, alignment: int) -> None:
    size = os.path.getsize(path)
    pad = (alignment - (size % alignment)) % alignment
    if pad:
        with open(path, "ab") as f:
            f.write(b"\0" * pad)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--zbi", help="Path to the zbi tool", required=True)
    parser.add_argument(
        "--depfile", help="Path to the depfile generated for GN", required=True
    )
    parser.add_argument(
        "--rspfile",
        help="Path to the response file for the zbi tool",
        required=True,
    )
    parser.add_argument(
        "--pad-to",
        type=int,
        default=0,
        help="Zero-pad --output to this alignment after the zbi tool runs",
    )
    args, zbi_args = parser.parse_known_args()

    intermediate_depfile = args.depfile + ".intermediate"

    # Run the zbi tool.
    command = [
        args.zbi,
        "--depfile",
        intermediate_depfile,
    ]
    with open(args.rspfile, "r") as rspfile:
        command.extend([l.strip() for l in rspfile.readlines()])
    subprocess.check_call(command + zbi_args)

    if args.pad_to:
        output = None
        for i, arg in enumerate(zbi_args):
            if arg == "--output" and i + 1 < len(zbi_args):
                output = zbi_args[i + 1]
                break
        if output is None:
            raise SystemExit("run_zbi.py: --pad-to requires --output")
        _pad_file(output, args.pad_to)

    # Write the final depfile.
    with open(intermediate_depfile, "r") as depfile:
        contents = depfile.read().strip()
    with open(args.depfile, "w") as final_depfile:
        final_depfile.write(contents + " " + args.rspfile)

    # Remove the intermediate depfile.
    os.remove(intermediate_depfile)


if __name__ == "__main__":
    main()
