#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Create an include root so legacy #include spellings find moved trees.

C++ keeps Fuchsia include spelling. The source tree is:

  userspace/                 (was src/)
  userspace/secular/         (was src/bringup/)
  products/build|boards|...  (was top-level build/, boards/, ...)

This script builds:

  <alias>/src/<dir>      -> <userspace>/<dir>
  <alias>/src/bringup    -> <userspace>/secular
  <alias>/build          -> <products>/build
  <alias>/boards         -> <products>/boards
  <alias>/bundles        -> <products>/bundles
  <alias>/manifests      -> <products>/manifests

GN adds <alias> to default include_dirs. cwd is the build directory.
"""

from __future__ import annotations

import os
import sys


def _link_dir(link: str, dest: str) -> None:
    if os.path.islink(link) or os.path.exists(link):
        if os.path.islink(link) and os.readlink(link) == dest:
            return
        os.unlink(link)
    os.symlink(dest, link)


def main(argv: list[str]) -> int:
    if len(argv) not in (3, 4):
        sys.stderr.write(
            "usage: write_userspace_include_alias.py USERSPACE PRODUCTS ALIAS_ROOT [OBJ_DIR]\n"
        )
        return 2

    userspace = os.path.abspath(argv[0])
    products = os.path.abspath(argv[1])
    alias_root = os.path.abspath(argv[2])
    obj_dir = os.path.abspath(argv[3]) if len(argv) > 3 else ""
    src_dir = os.path.join(alias_root, "src")
    os.makedirs(src_dir, exist_ok=True)
    os.makedirs(alias_root, exist_ok=True)

    wanted: dict[str, str] = {}
    for name in sorted(os.listdir(userspace)):
        if name == "BUILD.gn":
            continue
        dest = os.path.join(userspace, name)
        if os.path.islink(dest):
            continue
        if not os.path.isdir(dest):
            continue
        wanted[name] = dest

    secular = os.path.join(userspace, "secular")
    if os.path.isdir(secular):
        wanted["bringup"] = secular
        wanted["secular"] = secular

    existing = set(os.listdir(src_dir))
    for name in existing - set(wanted):
        os.unlink(os.path.join(src_dir, name))

    for name, dest in wanted.items():
        _link_dir(os.path.join(src_dir, name), dest)

    # Generated headers land under obj/userspace/... while C++ still
    # includes "src/bringup/..." / "src/<dir>/...". Mirror that layout
    # under alias/obj so -I alias/obj finds them.
    if obj_dir:
        gen_src = os.path.join(alias_root, "obj", "src")
        os.makedirs(gen_src, exist_ok=True)
        gen_wanted: dict[str, str] = {}
        for name in wanted:
            if name == "bringup":
                gen_wanted[name] = os.path.join(obj_dir, "userspace", "secular")
            else:
                gen_wanted[name] = os.path.join(obj_dir, "userspace", name)
        existing_gen = set(os.listdir(gen_src))
        for name in existing_gen - set(gen_wanted):
            os.unlink(os.path.join(gen_src, name))
        for name, dest in gen_wanted.items():
            _link_dir(os.path.join(gen_src, name), dest)

    for name in ("build", "boards", "bundles", "manifests"):
        dest = os.path.join(products, name)
        link = os.path.join(alias_root, name)
        if os.path.isdir(dest):
            _link_dir(link, dest)
        elif os.path.islink(link) or os.path.exists(link):
            os.unlink(link)

    sys.stdout.write(alias_root + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
