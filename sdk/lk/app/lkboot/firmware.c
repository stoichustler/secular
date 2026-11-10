/*
 * Copyright (c) 2026 The Fuchsia Authors
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 *
 * QEMU virt firmware payload backend. The hardware interface is the
 * firmware configuration device at 0x09020000; this file is the only
 * place that talks to it.
 */
#include "firmware.h"

#include <lk/debug.h>
#include <lk/err.h>
#include <lk/reg.h>
#include <platform/qemu-virt.h>
#include <stdint.h>
#include <string.h>

#define FIRMWARE_SELECTOR_OFF 0x08
#define FIRMWARE_KERNEL_SIZE  0x08
#define FIRMWARE_INITRD_SIZE  0x0b
#define FIRMWARE_KERNEL_DATA  0x11
#define FIRMWARE_INITRD_DATA  0x12

static uint16_t bswap16(uint16_t v) {
    return (uint16_t)((v << 8) | (v >> 8));
}

static void firmware_select(uint16_t key) {
    mmio_write16((volatile uint16_t *)(FW_CFG_BASE + FIRMWARE_SELECTOR_OFF), bswap16(key));
}

static void firmware_read_bytes(void *dst, size_t len) {
    uint8_t *out = dst;
    volatile uint64_t *data64 = (volatile uint64_t *)FW_CFG_BASE;
    volatile uint8_t *data8 = (volatile uint8_t *)FW_CFG_BASE;

    while (len >= sizeof(uint64_t) && (((uintptr_t)out) & 7) == 0) {
        *(uint64_t *)out = mmio_read64(data64);
        out += sizeof(uint64_t);
        len -= sizeof(uint64_t);
    }
    while (len > 0) {
        *out++ = mmio_read8(data8);
        len--;
    }
}

static uint32_t firmware_read_le32(uint16_t key) {
    uint32_t value = 0;
    firmware_select(key);
    firmware_read_bytes(&value, sizeof(value));
    return value;
}

static status_t firmware_payload_size(uint16_t size_key, size_t *out_size) {
    uint32_t size;

    if (!out_size) {
        return ERR_INVALID_ARGS;
    }
    size = firmware_read_le32(size_key);
    if (size == 0) {
        return ERR_NOT_FOUND;
    }
    *out_size = size;
    return NO_ERROR;
}

static status_t firmware_load_payload(uint16_t data_key, void *dst, size_t len) {
    if (!dst || len == 0) {
        return ERR_INVALID_ARGS;
    }
    firmware_select(data_key);
    firmware_read_bytes(dst, len);
    return NO_ERROR;
}

status_t firmware_kernel_size(size_t *out_size) {
    return firmware_payload_size(FIRMWARE_KERNEL_SIZE, out_size);
}

status_t firmware_ramdisk_size(size_t *out_size) {
    return firmware_payload_size(FIRMWARE_INITRD_SIZE, out_size);
}

status_t firmware_load_kernel(void *dst, size_t len) {
    return firmware_load_payload(FIRMWARE_KERNEL_DATA, dst, len);
}

status_t firmware_load_ramdisk(void *dst, size_t len) {
    return firmware_load_payload(FIRMWARE_INITRD_DATA, dst, len);
}
