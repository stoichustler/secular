#!/usr/bin/env python3
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import subprocess
import shlex
import os
import sys
import fnmatch

# Set PRINT_GIT_LS_TREE=1 in your environment to print
# the `git ls-tree` commands used during generation.
print_git_ls_tree = bool(os.environ.get("PRINT_GIT_LS_TREE") or os.environ.get("PRINT_GIT_LS_FILES"))

gn_in = open("BUILD.input.gn", "rb")
gn_file = gn_in.read()
gn_in.close()

fuchsia_dir = os.environ["FUCHSIA_DIR"]
assert fuchsia_dir, "FUCHSIA_DIR environment variable should be set before calling this script!"

protobuf_commit = sys.argv[1] if len(sys.argv) > 1 else "HEAD"

def get_files(paths, commit, exclude=[], append=[]):
    # Read files from the specific git commit using git ls-tree
    cmd = ["git", "ls-tree", "-r", "--name-only", commit]
    if print_git_ls_tree:
        print(' '.join(shlex.quote(c) for c in cmd))
    git_ls = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        cwd=os.path.join(fuchsia_dir, "third_party", "protobuf", "src"))
    stdout, _ = git_ls.communicate()
    files = stdout.decode().splitlines()

    # Match globs
    filtered_files = []
    for path_glob in paths:
        for f in files:
            if fnmatch.fnmatch(f, path_glob):
                excluded = False
                for ex in exclude:
                    if fnmatch.fnmatch(f, ex):
                        excluded = True
                        break
                if not excluded:
                    filtered_files.append(f)

    # Format strings
    result = b""
    for f in sorted(list(set(filtered_files))):
        result += f'    "src/{f}",\n'.encode()

    if append:
        for item in append:
            result += b'    "' + item.encode() + b'",\n'
    return result

# Replace public header place holders:
gn_file = gn_file.replace(
    b"PROTOBUF_LITE_PUBLIC",
    get_files(
        ["src/google/protobuf/*.h", "src/google/protobuf/*.inc"],
        exclude=["*/compiler/*", "*/testing/*", "*/util/*"],
        commit=protobuf_commit
    )
)
gn_file = gn_file.replace(
    b"PROTOBUF_FULL_PUBLIC",
    get_files(
        ["src/google/protobuf/*.h", "src/google/protobuf/*.inc"],
        exclude=["*/compiler/*", "*/testing/*"],
        commit=protobuf_commit
    )
)

# Replace UPB_LIB_SOURCES:
gn_file = gn_file.replace(
    b"UPB_LIB_SOURCES",
    get_files(
        [
            "upb/**/*.h",
            "upb/**/*.inc",
            "upb/**/*.c",
        ],
        exclude=[
            "*_test.cc",
            "*test*",
            "*benchmark*",
            "upb/conformance/*",
            "upb/cmake/*",
            "upb/reflection/stage0/*",
            "upb/reflection/stage1/*",
            "upb/reflection/cmake/*",
            "upb/json/*",
            "upb/util/*",
            "upb/text/*",
            "upb/io/*",
        ],
        append=[
            "//products/build/secondary/third_party/protobuf/upb_generator/common.cc",
            "src/upb/reflection/stage0/google/protobuf/descriptor.upb.c",
            "src/upb/reflection/stage0/google/protobuf/descriptor.upb.h",
        ],
        commit=protobuf_commit
    )
)

# Replace PROTOC_LIB_SOURCES:
gn_file = gn_file.replace(
    b"PROTOC_LIB_SOURCES",
    get_files(
        ["src/google/protobuf/compiler/*.cc"],
        exclude=["*/main.cc", "*test*", "*mock*", "src/google/protobuf/compiler/cpp/tools/*"],
        append=["src/src/google/protobuf/testing/file.cc"],
        commit=protobuf_commit
    )
)

gn_out = open("BUILD.gn", "wb")
gn_out.write(
    b"# THIS FILE IS GENERATED FROM BUILD.input.gn BY gen.py\n# EDIT BUILD.input.gn FIRST AND THEN RUN gen.py\n#\n#\n"
)
gn_out.write(gn_file)
gn_out.close()

try:
  cmd = [os.path.join(fuchsia_dir, "scripts", "fx"), "gn", "format", "BUILD.gn"]
  subprocess.check_output(cmd, text=True)
except subprocess.CalledProcessError as e:
  print(f"Error formatting BUILD.gn: {e}")
