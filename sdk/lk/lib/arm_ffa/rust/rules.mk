# Copyright (c) 2025, Google Inc. All rights reserved
#
# Permission is hereby granted, free of charge, to any person obtaining
# a copy of this software and associated documentation files
# (the "Software"), to deal in the Software without restriction,
# including without limitation the rights to use, copy, modify, merge,
# publish, distribute, sublicense, and/or sell copies of the Software,
# and to permit persons to whom the Software is furnished to do so,
# subject to the following conditions:
#
# The above copyright notice and this permission notice shall be
# included in all copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
# EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
# MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
# IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
# CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
# TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
# SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

LOCAL_DIR := $(GET_LOCAL_DIR)

MODULE := $(LOCAL_DIR)

MODULE_CRATE_NAME := arm_ffa

MODULE_SRCS += \
	$(LOCAL_DIR)/lib.rs \

MODULE_ADD_IMPLICIT_DEPS := false

MODULE_DEPS := \
	external/lk/lib/rust_support \
	lib/arm_ffa \
	lib/smc \
	trusty/user/base/lib/liballoc-rust \
	trusty/user/base/lib/libcompiler_builtins-rust \
	trusty/user/base/lib/libcore-rust \
	$(call FIND_CRATE,lazy_static) \
	$(call FIND_CRATE,log) \
	$(call FIND_CRATE,zerocopy) \

MODULE_BINDGEN_ALLOW_FUNCTIONS := \
	arm_ffa_mem_reclaim \
	arm_ffa_mem_share_kernel_buffer \
	arm_ffa_msg_send_direct_req2 \
	arm_ffa_partition_info_get_count \
	arm_ffa_partition_info_get_desc \
	arm_ffa_register_direct_req2_handler \

MODULE_BINDGEN_ALLOW_TYPES := \
	ffa_part_info_desc \

MODULE_BINDGEN_ALLOW_VARS := \
	ARM_FFA_MSG_EXTENDED_ARGS_COUNT \
	FFA_PAGE_SIZE \

MODULE_BINDGEN_SRC_HEADER := $(LOCAL_DIR)/bindings.h

ZEROCOPY_DERIVES := \
	zerocopy::FromBytes,zerocopy::Immutable,zerocopy::KnownLayout \

MODULE_BINDGEN_FLAGS += \
	--with-derive-custom="ffa_part_info_desc=$(ZEROCOPY_DERIVES)" \
	--with-derive-custom="ffa_part_info_desc__bindgen_ty_1=$(ZEROCOPY_DERIVES)" \
	--with-derive-custom="smc_ret18=$(ZEROCOPY_DERIVES)" \
	--with-derive-custom="smc_ret18__bindgen_ty_1=$(ZEROCOPY_DERIVES)" \
	--with-derive-custom="smc_ret18__bindgen_ty_1__bindgen_ty_1=$(ZEROCOPY_DERIVES)" \

MODULE_BINDGEN_BLOCK_TYPES := \
	uuid_t \

MODULE_RUST_USE_CLIPPY := true

include make/module.mk
