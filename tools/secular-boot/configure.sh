#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -eu

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"
root="$(cd "$script_dir/../.." && pwd)"

[[ "$#" -eq 1 ]] || die "usage: configure.sh ARCH"
arch="$1"
require_arch "$arch" || exit $?
out_name="secular-boot-$arch"

ensure_jiri_generated_commit_info "$root"

sdk_root="$(resolve_sdk_root)"
[[ -n "$sdk_root" ]] || die "SECULAR_SDK is required (see README.md)"
[[ -d "$sdk_root" ]] || die "SDK root is not a directory: $sdk_root"
sdk_root="$(realpath "$sdk_root")"
export SECULAR_SDK="$sdk_root"
[[ "$sdk_root" != "$root" ]] || die "source and SDK roots must differ"
[[ ! -e "$root/prebuilt" && ! -L "$root/prebuilt" ]] ||
  die "source root must not contain prebuilt: $root/prebuilt"

python3="$script_dir/link_sdk.py"
host_python="$(command -v python3)" || die "missing host python3"
# Host clang/python/gn/ninja come from the SDK linux-amd64 tree. rustc still
# needs a merged host+target rustlib; that view is linux-amd64 too.
mapped="${SECULAR_PREBUILT:-$root/out/sdk-prebuilt}"
secular_stage "map SDK rust sysroot (ARCH:$arch)"
"$host_python" "$python3" --sdk "$sdk_root" --dest "$mapped"
mapped="$(realpath "$mapped")"
export SECULAR_PREBUILT="$mapped"
sdk_prebuilt="$sdk_root/prebuilt"
rustc_prefix="$mapped/third_party/rust/linux-amd64"

gn_escape_string() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//\$/\\\$}"
  printf '%s' "$value"
}

escaped_sdk_root="$(gn_escape_string "$sdk_root")"
escaped_prebuilt="$(gn_escape_string "$sdk_prebuilt")"
escaped_rustc_prefix="$(gn_escape_string "$rustc_prefix")"

out_dir="$root/out/$out_name"
mkdir -p "$out_dir"
args_tmp="$(mktemp "$out_dir/.args.gn.XXXXXX")"
trap 'rm -f "$args_tmp"' EXIT
cat >"$args_tmp" <<EOF
import("//products/boards/qemu-arm64.gni")
import("//products/secular.gni")

secular_sdk_root = "$escaped_sdk_root"
secular_prebuilt_root = "$escaped_prebuilt"
secular_host_platform = "linux-amd64"
rustc_prefix = "$escaped_rustc_prefix"
secular_boot_build = true
secular_without_icu = true
kernel_version_string = "secular"
compilation_mode = "debug"
rbe_mode = "off"
include_clippy = false
export_bazel_tests = false
build_only_labels = []
# SDK clang 21 does not accept this Fuchsia-local experimental flag.
call_graph_section = false
EOF

need_gen=0
if [[ ! -f "$out_dir/build.ninja" ]]; then
  need_gen=1
fi
if ! cmp -s "$args_tmp" "$out_dir/args.gn" 2>/dev/null; then
  mv "$args_tmp" "$out_dir/args.gn"
  need_gen=1
else
  rm -f "$args_tmp"
fi
trap - EXIT

if [[ "$need_gen" -eq 1 ]]; then
  secular_stage "generate GN graph (ARCH:$arch, OUT:$out_dir)"
  gn_bin="$(resolve_tool "$root" "$arch" gn)" || die "missing: gn"
  cd "$root"
  "$gn_bin" gen "out/$out_name" --fail-on-unused-args
else
  secular_stage "reuse GN graph (ARCH:$arch, OUT:$out_dir)"
fi
secular_stage "complete (ARCH:$arch)"
