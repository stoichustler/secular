# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# LINT.IfChange
# The new Rust bindings are currently in a phased rollout. This allowlist
# controls where in the tree generated bindings may be used from.
fidl_rust_next_allowlist = [
    "//examples:__subpackages__",
    "//examples/fidl/new/key_value_store/use_generic_values/rust_next:__subpackages__",
    "//examples/fidl/rust_next:__subpackages__",
    "//sdk/fidl:__subpackages__",
    "//sdk/lib/async:__subpackages__",
    "//sdk/lib/driver:__subpackages__",
    "//userspace/secular/bin/paver:__subpackages__",
    "//userspace/secular/bin/userboot:__subpackages__",
    "//userspace/secular/lib/userboot:__subpackages__",
    "//userspace/connectivity/bluetooth:__subpackages__",
    "//userspace/connectivity/overnet:__subpackages__",
    "//userspace/connectivity/wlan:__subpackages__",
    "//userspace/devices:__subpackages__",
    "//userspace/firmware/paver:__subpackages__",
    "//userspace/graphics:__subpackages__",
    "//userspace/lib/fdomain:__subpackages__",
    "//userspace/lib/fidl/rust_next:__subpackages__",
    "//userspace/lib/fuchsia-component:__subpackages__",
    "//userspace/power:__subpackages__",
    "//userspace/tests/fidl/conformance_suite:__subpackages__",
    "//userspace/ui/input:__subpackages__",
    "//userspace/ui/lib/input_pipeline:__subpackages__",
    "//userspace/ui/tools/print-input-report:__subpackages__",
    "//tools/fidl:__subpackages__",
    "//vendor/google:__subpackages__",
]
# LINT.ThenChange(fidl_rust_next.gni)
