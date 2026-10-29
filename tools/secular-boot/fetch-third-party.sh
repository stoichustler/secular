#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Fetch pinned third_party sources from Fuchsia googlesource remotes.
# Never copy sources from smos or any other sibling tree.
#
# Usage: fetch-third-party.sh [project-path ...]
# Paths are relative to the repository root, e.g. third_party/re2/src

set -eu

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
root="$(cd "$script_dir/../.." && pwd)"
manifest="$root/products/manifests/third_party/all"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ -f "$manifest" ]] || die "missing manifest: $manifest"
command -v git >/dev/null || die "git is required to fetch third_party sources"

lookup() {
  local path="$1"
  python3 - "$manifest" "$path" <<'PY'
import re, sys
manifest, path = sys.argv[1], sys.argv[2]
text = open(manifest, encoding="utf-8").read()
blocks = re.findall(r"<project\b[^>]*>.*?</project>|<project\b[^>]*/>", text, re.S)
for block in blocks:
    attrs = dict(re.findall(r'(\w+)="([^"]*)"', block))
    if attrs.get("path") == path:
        remote = attrs.get("remote")
        revision = attrs.get("revision")
        if not remote or not revision:
            sys.exit(f"incomplete project for {path}")
        print(remote)
        print(revision)
        sys.exit(0)
sys.exit(f"no project with path={path}")
PY
}

fetch_one() {
  local path="$1"
  local dest remote revision
  dest="$root/$path"
  local info
  info="$(lookup "$path")" || die "manifest lookup failed for $path"
  remote="$(printf '%s\n' "$info" | sed -n '1p')"
  revision="$(printf '%s\n' "$info" | sed -n '2p')"
  [[ -n "$remote" && -n "$revision" ]] || die "incomplete lookup for $path"
  printf '[secular] fetch %s @ %s\n' "$path" "$revision"
  rm -rf "$dest"
  mkdir -p "$dest"
  # Prefer the googlesource snapshot tarball (no full clone). Fall back to a
  # shallow git fetch of the pinned revision.
  local archive="${remote}/+archive/${revision}.tar.gz"
  if curl -fsSL --connect-timeout 20 --max-time 120 -o "$dest/.archive.tar.gz" "$archive" &&
    tar -tzf "$dest/.archive.tar.gz" >/dev/null 2>&1; then
    tar -xzf "$dest/.archive.tar.gz" -C "$dest"
    rm -f "$dest/.archive.tar.gz"
  else
    rm -f "$dest/.archive.tar.gz"
    git init --quiet "$dest"
    git -C "$dest" remote add origin "$remote"
    git -C "$dest" fetch --depth 1 origin "$revision"
    git -C "$dest" checkout --quiet FETCH_HEAD
    rm -rf "$dest/.git"
  fi
}

if [[ "$#" -eq 0 ]]; then
  set -- \
    third_party/abseil-cpp/src \
    third_party/re2/src \
    third_party/zstd/src \
    third_party/lz4/src \
    third_party/pyyaml/src \
    third_party/boringssl/src \
    third_party/llvm-libc/src \
    third_party/scudo/src \
    third_party/scudo/gwp_asan \
    third_party/rapidjson
fi

for path in "$@"; do
  fetch_one "$path"
done
