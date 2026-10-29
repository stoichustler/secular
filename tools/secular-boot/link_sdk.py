#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Merge SDK rust host + target rustlib into a linux-amd64 sysroot view.

Clang, python, gn, ninja, and other host tools are used directly from
${SECULAR_SDK}/prebuilt/.../linux-amd64. rustc still needs fuchsia
target libs next to the host sysroot, so configure.sh points rustc_prefix
at this merged view.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import shutil
import sys

RUST_HOST_REL = pathlib.Path("third_party/rust/host/linux-amd64")
RUST_TARGET_ROOT = pathlib.Path("third_party/rust/target")
RUST_VIEW_REL = pathlib.Path("third_party/rust/linux-amd64")


def _rel_symlink(link: pathlib.Path, target: pathlib.Path) -> None:
    link.parent.mkdir(parents=True, exist_ok=True)
    relative = os.path.relpath(target, start=link.parent)
    if link.is_symlink() and os.readlink(link) == relative:
        return
    if link.is_symlink() or link.exists():
        if link.is_dir() and not link.is_symlink():
            shutil.rmtree(link)
        else:
            link.unlink()
    link.symlink_to(relative)


def _require(path: pathlib.Path, description: str) -> pathlib.Path:
    if not path.exists():
        raise FileNotFoundError(f"SDK is missing {description}: {path}")
    return path


def link_sdk(sdk_root: pathlib.Path, dest: pathlib.Path) -> None:
    sdk = sdk_root.resolve()
    prebuilt = _require(sdk / "prebuilt", "prebuilt/")
    dest = dest.resolve()
    dest.mkdir(parents=True, exist_ok=True)

    rust_host = _require(prebuilt / RUST_HOST_REL, str(RUST_HOST_REL))
    rust_view = dest / RUST_VIEW_REL
    rust_view.mkdir(parents=True, exist_ok=True)
    for name in ("etc", "libexec", "share", ".versions"):
        host_path = rust_host / name
        if host_path.exists():
            _rel_symlink(rust_view / name, host_path)

    # rustc resolves its sysroot from realpath(argv0). A symlink to the
    # host rustc would ignore this merged rustlib, so wrap the drivers.
    host_bin = rust_host / "bin"
    view_bin = rust_view / "bin"
    view_bin.mkdir(parents=True, exist_ok=True)
    wrap = {"rustc", "rustdoc"}
    for entry in host_bin.iterdir():
        dest_bin = view_bin / entry.name
        if entry.name in wrap:
            text = (
                "#!/bin/bash\n"
                f'REAL="{entry}"\n'
                f'SYSROOT="{rust_view}"\n'
                'for arg in "$@"; do\n'
                '  case "$arg" in\n'
                '    --sysroot|--sysroot=*) exec "$REAL" "$@" ;;\n'
                '  esac\n'
                'done\n'
                'exec "$REAL" --sysroot="$SYSROOT" "$@"\n'
            )
            if not dest_bin.is_file() or dest_bin.read_text() != text:
                dest_bin.write_text(text)
                dest_bin.chmod(0o755)
        else:
            _rel_symlink(dest_bin, entry)

    host_lib = rust_host / "lib"
    rust_lib = rust_view / "lib"
    rust_lib.mkdir(parents=True, exist_ok=True)
    for entry in host_lib.iterdir():
        if entry.name == "rustlib":
            continue
        _rel_symlink(rust_lib / entry.name, entry)

    fuchsia_lib = prebuilt / RUST_TARGET_ROOT / "fuchsia" / "lib"
    if fuchsia_lib.is_dir():
        for name in ("runtime.json", "debug", "dist"):
            src = fuchsia_lib / name
            if src.exists():
                _rel_symlink(rust_lib / name, src)

    rustlib = rust_lib / "rustlib"
    rustlib.mkdir(exist_ok=True)
    host_rustlib = host_lib / "rustlib"
    for entry in host_rustlib.iterdir():
        _rel_symlink(rustlib / entry.name, entry)

    target_root = prebuilt / RUST_TARGET_ROOT
    if target_root.is_dir():
        for rustlib_dir in target_root.glob("*/lib/rustlib"):
            for entry in rustlib_dir.iterdir():
                _rel_symlink(rustlib / entry.name, entry)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", required=True, type=pathlib.Path)
    parser.add_argument("--dest", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    link_sdk(args.sdk, args.dest)
    print(args.dest)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
