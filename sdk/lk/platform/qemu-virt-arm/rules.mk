LOCAL_DIR := $(GET_LOCAL_DIR)

MODULE := $(LOCAL_DIR)

ifeq ($(ARCH),)
ARCH := arm64
endif
ifeq ($(ARCH),arm64)
ARM_CPU ?= cortex-a53
RUST_TARGET := arm64-llvm
RUST_TARGET_PATH := $(abspath $(LKROOT))/arch/arm64/arm64-llvm.json
endif
ifeq ($(ARCH),arm)
ARM_CPU ?= cortex-a15
endif
WITH_SMP ?= 1

LK_HEAP_IMPLEMENTATION ?= dlmalloc

MODULE_SRCS += $(LOCAL_DIR)/debug.c
MODULE_SRCS += $(LOCAL_DIR)/platform.c

ifeq ($(call TOBOOL,$(LK_TRUSTY_TEE)),true)
# ATF QEMU secure DRAM (BL32): 0x0e100000, 15 MiB. Use 14 MiB so the
# initial MMU mapping is 2 MiB aligned and stays below BL32_LIMIT.
MEMBASE := 0x0e100000
MEMSIZE ?= 0x00e00000
KERNEL_LOAD_OFFSET := 0
else
MEMBASE := 0x40000000
MEMSIZE ?= 0x08000000   # 128MB
KERNEL_LOAD_OFFSET ?= 0x100000 # 1MB
endif

MODULE_DEPS += \
    dev/interrupt/arm_gic \
    dev/power/psci \
    dev/timer/arm_generic \
    lib/cbuf \
    lib/cmdline \
    lib/fdtwalk \

ifneq ($(call TOBOOL,$(LK_BOOTLOADER)),true)
ifneq ($(call TOBOOL,$(LK_TRUSTY_TEE)),true)
MODULE_DEPS += \
    dev/bus/pci \
    dev/bus/pci/drivers \
    dev/virtio/9p \
    dev/virtio/block \
    dev/virtio/gpu \
    dev/virtio/net \
    dev/virtio/rng \
    lib/fs/9p
endif
endif

ifeq ($(call TOBOOL,$(USE_RUST)),true)
MODULE_DEPS += lib/rust_support
MODULE_DEPS += rust/dev-pl011
else
MODULE_DEPS += dev/uart/pl011
endif

GLOBAL_DEFINES += \
    MEMBASE=$(MEMBASE) \
    MEMSIZE=$(MEMSIZE) \
    PLATFORM_SUPPORTS_PANIC_SHELL=1 \
    CONSOLE_HAS_INPUT_BUFFER=1 \
    TIMER_ARM_GENERIC_SELECTED=CNTV

ifeq ($(call TOBOOL,$(LK_BOOTLOADER)),true)
GLOBAL_DEFINES += LK_BOOTLOADER=1
endif

ifeq ($(call TOBOOL,$(LK_TRUSTY_TEE)),true)
GLOBAL_DEFINES += LK_TRUSTY_TEE=1
GLOBAL_INCLUDES += \
    include \
    include/uapi
GLOBAL_CFLAGS += -include trusty-lk-compat.h
GLOBAL_CPPFLAGS += -include trusty-lk-compat.h
endif

GLOBAL_DEFINES += MMU_WITH_TRAMPOLINE=1

LINKER_SCRIPT += \
    $(BUILDDIR)/system-onesegment.ld

include make/module.mk
