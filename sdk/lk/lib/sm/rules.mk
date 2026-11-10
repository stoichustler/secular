#
# Copyright (c) 2013-2015, Google, Inc. All rights reserved
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
#

LOCAL_DIR := $(GET_LOCAL_DIR)

MODULE := $(LOCAL_DIR)

GLOBAL_DEFINES += \
	WITH_LIB_SM=1 \

ifeq (true,$(call TOBOOL,$(LIB_SM_WITH_FFA_LOOP)))
MODULE_DEFINES += LIB_SM_WITH_FFA_LOOP=1
endif

GLOBAL_INCLUDES += \
	$(LOCAL_DIR)/include \
	include \
	include/uapi

MODULE_SRCS += \
	$(LOCAL_DIR)/sm.c \
	$(LOCAL_DIR)/smcall.c \
	$(LOCAL_DIR)/ns_mem.c \
	$(LOCAL_DIR)/shared_mem.c \
        $(LOCAL_DIR)/trusty_sched_share.c \

MODULE_DEPS += \
	lib/arm_ffa \
	lib/binary_search_tree \
	lib/extmem \
	lib/version \
	lib/smc \

include $(LOCAL_DIR)/arch/$(ARCH)/rules.mk

include make/module.mk
