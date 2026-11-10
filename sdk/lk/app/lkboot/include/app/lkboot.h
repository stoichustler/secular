/*
 * Copyright (c) 2026 The Fuchsia Authors
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#pragma once

#include <sys/types.h>

void notify_boot_linux(void);
int boot_linux_from_firmware(void);
int boot_linux_fdt(void *kernel, paddr_t kernel_pa, void *ramdisk,
                   paddr_t ramdisk_pa, unsigned int ramdisk_sz);
