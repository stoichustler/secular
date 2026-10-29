#!/bin/bash
# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

set -eu

script_dir="$(cd "${BASH_SOURCE[0]%/*}" && pwd)"
source "$script_dir/lib/common.sh"
root="$(cd "$script_dir/../.." && pwd)"
[[ "$#" -eq 1 ]] || die "usage: verify.sh ARCH"

arch="$1"
require_arch "$arch" || exit $?
python_bin="$(resolve_tool "$root" "$arch" python3)" || python_bin="$(command -v python3)" ||
  die "missing: python3"
"$python_bin" "$script_dir/verify.py" "$arch"
