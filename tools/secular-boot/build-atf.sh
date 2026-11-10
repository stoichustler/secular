#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Compile sdk/atf for PLAT=qemu with Trusty as BL32 and lkboot as BL33.

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

lk_bin="$out_dir/lk/build-qemu-virt-arm64-lkboot/lk.bin"
trusty_bin="$out_dir/lk/build-qemu-virt-arm64-trusty/lk.bin"
[[ -f "$lk_bin" ]] || die "missing LK BL33: $lk_bin (run make lkboot first)"
[[ -f "$trusty_bin" ]] || die "missing Trusty BL32: $trusty_bin (run make lkboot first)"

# ATF loads BL32 (Trusty) into secure DRAM and BL33 (lkboot) at
# NS_IMAGE_OFFSET (0x60000000). QEMU still supplies linux-arm64-boot-shim
# and secular.zbi via -kernel/-initrd (fw_cfg).
secular_stage "compile ATF qemu + BL32 trusty + BL33 lkboot (ARCH:$arch, OUT:$build_base)"
make -C "$atf_src" \
  -j"$jobs" \
  PLAT=qemu \
  ARCH=aarch64 \
  DEBUG=0 \
  LOG_LEVEL=40 \
  SPD=trusty \
  TRUSTY_SPD_WITH_GENERIC_SERVICES=1 \
  NEED_BL32=yes \
  ENABLE_SVE_FOR_NS=0 \
  ENABLE_SME_FOR_NS=0 \
  ENABLE_SME2_FOR_NS=0 \
  QEMU_USE_GIC_DRIVER=QEMU_GICV3 \
  ARM_LINUX_KERNEL_AS_BL33=1 \
  BL32="$trusty_bin" \
  BL33="$lk_bin" \
  BUILD_BASE="$build_base" \
  CC="$clang_bin" \
  OC="$objcopy_bin" \
  secular.bios \
  2>&1 | tee "$out_dir/logs/atf.log"
make_status=${PIPESTATUS[0]}
if [[ "$make_status" -ne 0 ]]; then
  exit "$make_status"
fi

bios="$build_base/qemu/release/secular.bios"
[[ -f "$bios" ]] || die "ATF did not produce secular.bios: $bios"
firmware_dir="$out_dir/firmware/arm-trusted-firmware-qemu-bios"
mkdir -p "$firmware_dir"
cp "$bios" "$firmware_dir/secular.bios"
rm -f "$firmware_dir/qemu_fw.bios"
secular_stage "complete (ARCH:$arch, BIOS:$firmware_dir/secular.bios)"
