#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -uo pipefail

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"
root="$(cd "$script_dir/../.." && pwd)"

[[ "$#" -eq 1 ]] || die "usage: build.sh ARCH"

build_arch() {
  local arch="$1"
  local out_rel="out/secular-boot-$arch"
  local out_dir="$root/$out_rel"
  local -a targets=(
    "products/secular:secular"
    "products/secular:boot_shim"
  )

  ensure_jiri_generated_commit_info "$root"

  secular_stage "BLD: preflight (ARCH:$arch)"
  "$script_dir/preflight.sh" --root "$root" "$arch" || return $?
  if [[ ! -f "$out_dir/build.ninja" ]]; then
    "$script_dir/configure.sh" "$arch" || return $?
  fi

  mkdir -p "$out_dir/logs"
  local ninja_bin
  ninja_bin="$(resolve_tool "$root" "$arch" ninja)" || {
    printf 'error: missing: ninja\n' >&2
    return 1
  }

  cd "$root"
  secular_stage "BLD: compile image (ARCH:$arch)"
  "$ninja_bin" -C "$out_rel" -k 0 \
    "${targets[@]}" 2>&1 |
    tee "$out_rel/logs/build.log"
  local ninja_status=${PIPESTATUS[0]}
  if [[ "$ninja_status" -ne 0 ]]; then
    return "$ninja_status"
  fi

  local gn_bin python_bin
  gn_bin="$(resolve_tool "$root" "$arch" gn)" || {
    printf 'error: missing: gn\n' >&2
    return 1
  }
  python_bin="$(resolve_tool "$root" "$arch" python3)" || python_bin="$(command -v python3)" || {
    printf 'error: missing: python3\n' >&2
    return 1
  }
  "$python_bin" "$script_dir/artifacts.py" \
    --source-root "$root" \
    --out-dir "$out_dir" \
    --gn "$gn_bin" \
    --write "$out_dir/artifacts.env" || return $?
  secular_stage "BLD: complete (ARCH:$arch, ARTIFACTS:$out_dir/artifacts.env)"
}

case "$1" in
  arm64) build_arch "$1" ;;
  *)
    require_arch "$1"
    exit $?
    ;;
esac
