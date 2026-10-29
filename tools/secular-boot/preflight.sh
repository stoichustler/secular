#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -u

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"

root=""
if [[ "${1:-}" == "--root" ]]; then
  [[ -n "${2:-}" ]] || die "--root requires a directory"
  root="$2"
  shift 2
fi

[[ "$#" -eq 1 ]] || die "usage: preflight.sh --root DIR ARCH"
arch="$1"
require_arch "$arch" || exit $?
[[ -d "$root" ]] || die "repository root is not a directory: $root"

tools=(gn ninja clang clang++ ld.lld llvm-ar rustc python3 buildidtool qemu-system-aarch64)

missing=0
for tool in "${tools[@]}"; do
  if ! resolve_tool "$root" "$arch" "$tool" >/dev/null; then
    printf 'missing: %s\n' "$tool" >&2
    missing=1
  fi
done

if [[ "$missing" -ne 0 ]]; then
  printf 'hint: export SECULAR_SDK to the standalone SDK (default ../secular-sdk)\n' >&2
fi

exit "$missing"
