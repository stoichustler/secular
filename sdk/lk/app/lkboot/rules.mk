LOCAL_DIR := $(GET_LOCAL_DIR)

MODULE := $(LOCAL_DIR)

MODULE_DEPS += \
	lib/fdt

MODULE_SRCS += \
	$(LOCAL_DIR)/firmware.c \
	$(LOCAL_DIR)/lkboot.c

include make/module.mk
