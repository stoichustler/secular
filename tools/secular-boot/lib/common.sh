#!/bin/bash

# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

secular_stage() {
  printf '[secular] %s\n' "$*"
}

require_arch() {
  case "$1" in
    arm64) ;;
    *)
      printf 'unsupported architecture: %s\n' "$1" >&2
      return 2
      ;;
  esac
}

# Ninja lists these as source inputs for latest-commit-date.txt (kernel UTC
# backstop). Fuchsia fills them with a jiri hook; Secular has no jiri, so
# synthesize from this repo's HEAD.
ensure_jiri_generated_commit_info() {
  local root="$1"
  local dir="$root/products/build/info/jiri_generated"
  local hash stamp path contents
  mkdir -p "$dir"
  export GIT_CONFIG_GLOBAL=/dev/null
  export GIT_OPTIONAL_LOCKS=0
  export GIT_CONFIG_NOSYSTEM=1
  if git -C "$root" rev-parse --verify HEAD >/dev/null 2>&1; then
    hash="$(git -C "$root" rev-parse HEAD)"
    stamp="$(git -C "$root" log -n1 --date=unix --format=%cd)"
  else
    hash="0000000000000000000000000000000000000000"
    stamp="$(date -u +%s)"
  fi
  for path in \
    "$dir/integration_commit_hash.txt" \
    "$dir/integration_daily_commit_hash.txt"; do
    contents="$hash"
    if [[ ! -r "$path" ]] || [[ "$(cat "$path")" != "$contents" ]]; then
      printf '%s\n' "$contents" >"$path"
    fi
  done
  for path in \
    "$dir/integration_commit_stamp.txt" \
    "$dir/integration_daily_commit_stamp.txt"; do
    contents="$stamp"
    if [[ ! -r "$path" ]] || [[ "$(cat "$path")" != "$contents" ]]; then
      printf '%s\n' "$contents" >"$path"
    fi
  done
}

resolve_sdk_root() {
  local sdk=""
  if [[ -n "${SECULAR_SDK:-}" ]]; then
    sdk="$(/usr/bin/realpath -m "$SECULAR_SDK")"
  elif [[ -n "${SMOS_SDK:-}" ]]; then
    sdk="$(/usr/bin/realpath -m "$SMOS_SDK")"
  elif [[ -n "${SMOS_SDK_ROOT:-}" ]]; then
    sdk="$(/usr/bin/realpath -m "$SMOS_SDK_ROOT")"
  fi
  printf '%s\n' "$sdk"
}

resolve_tool() {
  local root="$1"
  local name="$3"
  local candidate base sdk=""
  local -a candidates=()
  local -a roots=("$root")

  sdk="$(resolve_sdk_root)"
  if [[ -n "$sdk" ]]; then
    roots+=("$sdk")
  fi

  if [[ -n "${SECULAR_PREBUILT:-}" ]]; then
    roots+=("$(/usr/bin/realpath -m "$SECULAR_PREBUILT")")
  fi

  for base in "${roots[@]}"; do
    case "$name" in
      gn)
        candidates+=(
          "$base/prebuilt/third_party/gn/linux-amd64/gn"
          "$base/prebuilt/third_party/gn/linux-x64/gn"
          "$base/third_party/gn/linux-x64/gn"
          "$base/third_party/gn/linux-amd64/gn"
        )
        ;;
      ninja)
        candidates+=(
          "$base/prebuilt/third_party/ninja/linux-amd64/ninja"
          "$base/prebuilt/third_party/ninja/linux-amd64/bin/ninja"
          "$base/prebuilt/third_party/ninja/linux-x64/ninja"
          "$base/prebuilt/third_party/ninja/linux-x64/bin/ninja"
          "$base/third_party/ninja/linux-x64/ninja"
          "$base/third_party/ninja/linux-x64/bin/ninja"
        )
        ;;
      clang | clang++ | ld.lld | llvm-ar)
        candidates+=(
          "$base/prebuilt/third_party/clang/linux-amd64/bin/$name"
          "$base/prebuilt/third_party/clang/linux-x64/bin/$name"
          "$base/third_party/clang/linux-x64/bin/$name"
        )
        ;;
      rustc)
        candidates+=(
          "$base/prebuilt/third_party/rust/host/linux-amd64/bin/rustc"
          "$base/prebuilt/third_party/rust/linux-amd64/bin/rustc"
          "$base/prebuilt/third_party/rust/linux-x64/bin/rustc"
          "$base/third_party/rust/linux-x64/bin/rustc"
        )
        ;;
      python3)
        candidates+=(
          "$base/prebuilt/third_party/python3/linux-amd64/bin/python3"
          "$base/prebuilt/third_party/python3/linux-amd64/bin/python3.11"
          "$base/prebuilt/third_party/python3/linux-x64/bin/python3"
          "$base/prebuilt/third_party/python3/linux-x64/bin/python3.11"
          "$base/third_party/python3/linux-x64/bin/python3"
        )
        ;;
      buildidtool)
        candidates+=(
          "$base/prebuilt/tools/buildidtool/linux-amd64/buildidtool"
          "$base/prebuilt/tools/buildidtool/linux-x64/buildidtool"
          "$base/tools/buildidtool/linux-x64/buildidtool"
        )
        ;;
      qemu-system-aarch64)
        candidates+=(
          "$base/prebuilt/third_party/qemu/linux-amd64/bin/$name"
          "$base/prebuilt/third_party/qemu/linux-x64/bin/$name"
          "$base/third_party/qemu/linux-x64/bin/$name"
        )
        ;;
    esac
  done

  for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate" && -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  command -v "$name" 2>/dev/null
}
