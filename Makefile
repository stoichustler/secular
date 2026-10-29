# Copyright 2026 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Put secular and secular-sdk in the same directory.
export SECULAR_SDK ?= $(realpath ../secular-sdk)

ARCH ?= arm64
TOOLCHAIN_PATH ?= tools/secular-boot
CTAGS ?= ctags

ifeq ($(V),1)
Q :=
else
Q := @
endif

.DEFAULT_GOAL := help

.PHONY: help configure build verify run qemu all clean tags

help: ## Show available build and verification targets.
	$(Q)awk 'BEGIN { FS = ":.*##" } /^[a-zA-Z][a-zA-Z0-9_-]*:.*##/ { printf "  %-16s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

configure: ## Generate the GN build directory for ARCH (default: arm64).
	$(Q)$(TOOLCHAIN_PATH)/configure.sh $(ARCH)

build: configure ## Build the console image for ARCH.
	$(Q)$(TOOLCHAIN_PATH)/build.sh $(ARCH)

verify: build ## Boot-test ARCH in QEMU (arm64 is automated).
	$(Q)$(TOOLCHAIN_PATH)/verify.sh $(ARCH)

run: build ## Build and start an interactive QEMU console; set QEMU_ARGS as needed.
	$(Q)$(TOOLCHAIN_PATH)/run-qemu.sh $(ARCH) -- $(QEMU_ARGS)

qemu:
	$(Q)$(TOOLCHAIN_PATH)/run-qemu.sh $(ARCH) -- $(QEMU_ARGS)

all: verify ## Configure, build, and verify the default architecture.

clean: ## Remove generated build output.
	$(Q)rm -rf out

tags: ## Generate C/C++/assembly tags for the retained source tree.
	$(Q)$(CTAGS) --languages=Asm,C,C++ -R zircon userspace sdk third_party
