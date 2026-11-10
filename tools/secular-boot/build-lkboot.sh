#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Compile sdk/lk as the QEMU virt ARM64 BL33 bootloader (app/lkboot)
# and as the Trusty TEE BL32 payload (app/trusty).

set -euo pipefail

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"
root="$(cd "$script_dir/../.." && pwd)"

[[ "$#" -eq 1 ]] || die "usage: build-lkboot.sh ARCH"
arch="$1"
require_arch "$arch" || exit $?

out_rel="out/secular-boot-$arch"
out_dir="$root/$out_rel"
lk_src="$root/sdk/lk"
bl33_project="qemu-virt-arm64-lkboot"
bl32_project="qemu-virt-arm64-trusty"
[[ -f "$lk_src/makefile" ]] || die "missing LK tree: $lk_src"
[[ -f "$lk_src/project/${bl33_project}.mk" ]] ||
  die "missing LK QEMU project: $lk_src/project/${bl33_project}.mk"
[[ -f "$lk_src/project/${bl32_project}.mk" ]] ||
  die "missing LK Trusty project: $lk_src/project/${bl32_project}.mk"
[[ -f "$lk_src/platform/qemu-virt-arm/rules.mk" ]] ||
  die "missing LK QEMU platform: $lk_src/platform/qemu-virt-arm"

sdk="$(resolve_sdk_root)"
[[ -n "$sdk" ]] || die "SECULAR_SDK is required (see README.md)"
export SECULAR_SDK="$sdk"

clang_bin="$(resolve_tool "$root" "$arch" clang)" || die "missing: clang"
objcopy_bin="$(resolve_tool "$root" "$arch" llvm-objcopy)" || die "missing: llvm-objcopy"
python_bin="$(resolve_tool "$root" "$arch" python3)" || die "missing: python3"
clang_dir="$(dirname "$clang_bin")"
python_dir="$(dirname "$python_bin")"
export PATH="$clang_dir:$python_dir:$PATH"

build_base="$out_dir/lk"
mkdir -p "$out_dir/logs" "$build_base"

jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)"

build_lk_project() {
  local project="$1"
  local log_name="$2"
  secular_stage "compile LK $project (ARCH:$arch, OUT:$build_base)"
  make -C "$lk_src" \
    -j"$jobs" \
    "$project" \
    TOOLCHAIN=clang \
    LD=ld.lld \
    CLANG_BINDIR="$clang_dir" \
    BUILDROOT="$build_base" \
    IGNORE_LOCAL_MK=1 \
    2>&1 | tee "$out_dir/logs/${log_name}.log"
  local make_status
  make_status=${PIPESTATUS[0]}
  if [[ "$make_status" -ne 0 ]]; then
    exit "$make_status"
  fi
  local elf="$build_base/build-${project}/lk.elf"
  local bin="$build_base/build-${project}/lk.bin"
  [[ -f "$elf" ]] || die "LK did not produce lk.elf: $elf"
  [[ -f "$bin" ]] || die "LK did not produce lk.bin: $bin"
}

build_lk_project "$bl33_project" "lkboot"
build_lk_project "$bl32_project" "trusty"
secular_stage "complete (ARCH:$arch, BL33:$build_base/build-${bl33_project}/lk.bin, BL32:$build_base/build-${bl32_project}/lk.bin)"
