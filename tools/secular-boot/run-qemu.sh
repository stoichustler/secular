#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -eu

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"
root="$(cd "$script_dir/../.." && pwd)"

[[ "$#" -ge 1 ]] || die "usage: run-qemu.sh ARCH [run-zircon args...] [-- QEMU_ARGS...]"
arch="$1"
shift
require_arch "$arch" || exit $?
out_name="secular-boot-$arch"
secular_stage "validate invocation (ARCH:$arch)"

# Flags before `--` go to zircon/scripts/run-zircon (e.g. -s, -m, -c).
# Everything after `--` is passed through to QEMU.
run_zircon_args=()
while [[ "$#" -gt 0 ]]; do
  if [[ "$1" == "--" ]]; then
    shift
    break
  fi
  run_zircon_args+=("$1")
  shift
done

secular_stage "validate host tools (ARCH:$arch)"
"$script_dir/preflight.sh" --root "$root" "$arch"
artifacts="$root/out/$out_name/artifacts.env"
[[ -f "$artifacts" ]] || die "missing artifacts: $artifacts (run build.sh $arch)"
unset QEMU_KERNEL ZBI
source "$artifacts"
: "${QEMU_KERNEL:?missing QEMU_KERNEL in $artifacts}"
: "${ZBI:?missing ZBI in $artifacts}"

canonical_in_tree() {
  local path
  path="$(realpath "$1")" || die "invalid artifact path: $1"
  case "$path" in
    "$root"/*) printf '%s\n' "$path" ;;
    *) die "artifact is outside source tree: $path" ;;
  esac
}

QEMU_KERNEL="$(canonical_in_tree "$QEMU_KERNEL")"
ZBI="$(canonical_in_tree "$ZBI")"
[[ -f "$QEMU_KERNEL" ]] || die "missing QEMU kernel: $QEMU_KERNEL"
[[ -f "$ZBI" ]] || die "missing ZBI: $ZBI"

qemu_bin="$(resolve_tool "$root" "$arch" qemu-system-aarch64)" ||
  die "missing: qemu-system-aarch64"
qemu_dir="$(dirname "$qemu_bin")"
runner="${SECULAR_RUN_ZIRCON:-$root/zircon/scripts/run-zircon}"
[[ -x "$runner" ]] || die "missing executable runner: $runner"

sdk="$(resolve_sdk_root)"
[[ -n "$sdk" ]] || die "SECULAR_SDK is not set"
# run-zircon looks up ATF secular.bios under PREBUILT_DIR/firmware/.
# Prefer the in-tree BIOS (ATF + LK BL33); fall back to the SDK copy.
in_tree_prebuilt="$root/out/$out_name"
in_tree_bios="$in_tree_prebuilt/firmware/arm-trusted-firmware-qemu-bios/secular.bios"
if [[ -f "$in_tree_bios" ]]; then
  export PREBUILT_DIR="$in_tree_prebuilt"
else
  export PREBUILT_DIR="$sdk/prebuilt"
fi
bios="$PREBUILT_DIR/firmware/arm-trusted-firmware-qemu-bios/secular.bios"
[[ -f "$bios" ]] || die "missing ATF QEMU BIOS: $bios (run make atf)"

# -kernel = linux-arm64-boot-shim (Linux Image); -initrd = zircon.zbi (ramdisk).
# ATF EL3 Trusty SPD starts S-EL1 BL32, then lkboot BL33 loads both from firmware.
secular_stage "launch QEMU (ARCH:$arch, linux:$QEMU_KERNEL, ramdisk:$ZBI)"
exec "$runner" \
  -a "$arch" \
  -t "$QEMU_KERNEL" \
  -z "$ZBI" \
  -m 1024 \
  -s 8 \
  --no-kvm \
  --no-virtio \
  --gic=3 \
  -q "$qemu_dir" \
  "${run_zircon_args[@]}" \
  -- "$@"
