#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Compile sdk/atf for PLAT=qemu. Does not boot Secular; this only checks
# that BL1/BL2/BL31 and qemu_fw.bios pack with the SDK clang.

set -euo pipefail

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"
root="$(cd "$script_dir/../.." && pwd)"

[[ "$#" -eq 1 ]] || die "usage: build-atf.sh ARCH"
arch="$1"
require_arch "$arch" || exit $?

out_rel="out/secular-boot-$arch"
out_dir="$root/$out_rel"
atf_src="$root/sdk/atf"
[[ -f "$atf_src/Makefile" ]] || die "missing ATF tree: $atf_src"
[[ -f "$atf_src/plat/qemu/qemu/platform.mk" ]] ||
  die "missing ATF QEMU platform: $atf_src/plat/qemu/qemu"

sdk="$(resolve_sdk_root)"
[[ -n "$sdk" ]] || die "SECULAR_SDK is required (see README.md)"
export SECULAR_SDK="$sdk"

clang_bin="$(resolve_tool "$root" "$arch" clang)" || die "missing: clang"
objcopy_bin="$(resolve_tool "$root" "$arch" llvm-objcopy)" || die "missing: llvm-objcopy"
clang_dir="$(dirname "$clang_bin")"
export PATH="$clang_dir:$PATH"

build_base="$out_dir/atf"
mkdir -p "$out_dir/logs" "$build_base"

jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)"

# PRELOADED_BL33_BASE skips packing a BL33 into the FIP so this target does
# not need U-Boot. Secular still uses -kernel/-initrd at run time.
secular_stage "compile ATF qemu (ARCH:$arch, OUT:$build_base)"
make -C "$atf_src" \
  -j"$jobs" \
  PLAT=qemu \
  ARCH=aarch64 \
  DEBUG=0 \
  QEMU_USE_GIC_DRIVER=QEMU_GICV3 \
  PRELOADED_BL33_BASE=0x40200000 \
  ARM_LINUX_KERNEL_AS_BL33=1 \
  BUILD_BASE="$build_base" \
  CC="$clang_bin" \
  OC="$objcopy_bin" \
  qemu_fw.bios \
  2>&1 | tee "$out_dir/logs/atf.log"
make_status=${PIPESTATUS[0]}
if [[ "$make_status" -ne 0 ]]; then
  exit "$make_status"
fi

bios="$build_base/qemu/release/qemu_fw.bios"
[[ -f "$bios" ]] || die "ATF did not produce qemu_fw.bios: $bios"
ls -l \
  "$build_base/qemu/release/bl1.bin" \
  "$build_base/qemu/release/bl2.bin" \
  "$build_base/qemu/release/bl31.bin" \
  "$build_base/qemu/release/fip.bin" \
  "$bios"
secular_stage "complete (ARCH:$arch, BIOS:$bios)"
