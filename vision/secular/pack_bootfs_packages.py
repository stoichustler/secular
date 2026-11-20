#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Turn fuchsia_package manifests into a BOOTFS package index + blob map.

component_manager's fuchsia-boot resolver expects:

  data/bootfs_packages  - lines of "name/version=merkle"
  blob/<merkle>         - package blobs (meta.far and content)

This is the subset of product assembly needed to boot a bringup-style
console image without Bazel.
"""

from __future__ import annotations

import argparse
import json
import os
import sys


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fini", required=True)
    parser.add_argument("--index", required=True)
    parser.add_argument("--depfile")
    parser.add_argument("manifests", nargs="+")
    args = parser.parse_args(argv)

    index_lines: list[str] = []
    fini_lines: list[str] = []
    inputs: list[str] = list(args.manifests)
    seen_merkles: set[str] = set()

    for manifest_path in args.manifests:
        with open(manifest_path, encoding="utf-8") as handle:
            manifest = json.load(handle)
        package = manifest.get("package") or {}
        name = package.get("name")
        version = str(package.get("version", "0"))
        if not name:
            raise SystemExit(f"package manifest missing name: {manifest_path}")

        package_hash = None
        for blob in manifest.get("blobs") or []:
            merkle = blob["merkle"]
            source = blob["source_path"]
            inputs.append(source)
            if blob.get("path") == "meta/":
                package_hash = merkle
            if merkle not in seen_merkles:
                seen_merkles.add(merkle)
                fini_lines.append(f"blob/{merkle}={source}")
        if not package_hash:
            raise SystemExit(f"no meta.far blob in {manifest_path}")
        index_lines.append(f"{name}/{version}={package_hash}")

    index_lines.sort()
    os.makedirs(os.path.dirname(args.index) or ".", exist_ok=True)
    os.makedirs(os.path.dirname(args.fini) or ".", exist_ok=True)
    with open(args.index, "w", encoding="utf-8") as handle:
        handle.write("\n".join(index_lines))
        if index_lines:
            handle.write("\n")

    fini_lines.append(f"data/bootfs_packages={args.index}")
    with open(args.fini, "w", encoding="utf-8") as handle:
        handle.write("\n".join(fini_lines))
        handle.write("\n")

    if args.depfile:
        os.makedirs(os.path.dirname(args.depfile) or ".", exist_ok=True)
        with open(args.depfile, "w", encoding="utf-8") as handle:
            handle.write(f"{args.fini} {args.index}:")
            for path in inputs:
                handle.write(f" {path}")
            handle.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
