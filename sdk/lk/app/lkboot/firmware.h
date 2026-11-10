/*
 * Copyright (c) 2026 The Fuchsia Authors
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#pragma once

#include <stddef.h>
#include <lk/compiler.h>
#include <sys/types.h>

__BEGIN_CDECLS

/*
 * Firmware payload source for the BL33 bootloader.
 *
 * On QEMU virt this is the firmware configuration interface (kernel +
 * ramdisk). Callers do not use fw_cfg directly.
 *
 *   firmware kernel  = linux-arm64-boot-shim  (ARM64 Linux Image)
 *   firmware ramdisk = zircon.zbi
 */
status_t firmware_kernel_size(size_t *out_size);
status_t firmware_ramdisk_size(size_t *out_size);
status_t firmware_load_kernel(void *dst, size_t len);
status_t firmware_load_ramdisk(void *dst, size_t len);

__END_CDECLS
