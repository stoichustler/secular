#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Resolve QEMU kernel and ZBI paths from GN outputs."""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys


def _gn_outputs(
    root: pathlib.Path, out_dir: pathlib.Path, gn: pathlib.Path, target: str
) -> list[pathlib.Path]:
    result = subprocess.run(
        [str(gn), "desc", str(out_dir), target, "outputs"],
        cwd=root,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    paths: list[pathlib.Path] = []
    for line in result.stdout.splitlines():
        line = line.strip()
        if line.startswith("//"):
            paths.append((root / line[2:]).resolve())
        elif line.startswith("/"):
            paths.append(pathlib.Path(line).resolve())
    return paths


def _inside(path: pathlib.Path, directory: pathlib.Path) -> bool:
    try:
        path.relative_to(directory)
        return True
    except ValueError:
        return False


def _pick(
    outputs: list[pathlib.Path], suffix: str, out_dir: pathlib.Path, description: str
) -> pathlib.Path:
    matches = [path for path in outputs if path.name.endswith(suffix)]
    if len(matches) != 1:
        raise ValueError(f"expected exactly one {description}, found {len(matches)}: {matches}")
    path = matches[0]
    if not _inside(path, out_dir):
        raise ValueError(f"{description} is outside output directory: {path}")
    if not path.is_file():
        raise FileNotFoundError(f"missing {description}: {path}")
    return path


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=pathlib.Path)
    parser.add_argument("--out-dir", required=True, type=pathlib.Path)
    parser.add_argument("--gn", required=True, type=pathlib.Path)
    parser.add_argument("--write", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)

    root = args.source_root.resolve()
    out_dir = args.out_dir.resolve()
    zbi = _pick(
        _gn_outputs(root, out_dir, args.gn, "//products/secular:secular"),
        ".zbi",
        out_dir,
        "ZBI",
    )
    shim_outputs = _gn_outputs(
        root,
        out_dir,
        args.gn,
        "//zircon/kernel/arch/arm64/phys/boot-shim:linux-arm64-boot-shim"
        "(//zircon/kernel/phys:kernel.phys_arm64)",
    )
    if not shim_outputs:
        shim_outputs = list(out_dir.glob("**/linux-*-boot-shim.bin"))
    shim = _pick(
        shim_outputs,
        ".bin",
        out_dir,
        "QEMU kernel",
    )
    args.write.write_text(f"QEMU_KERNEL={shim}\nZBI={zbi}\n")
    print(args.write)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
