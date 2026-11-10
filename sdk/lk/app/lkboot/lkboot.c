/*
 * Copyright (c) 2026 The Fuchsia Authors
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 *
 * BL33 bootloader, following lk2 app/mt_boot:
 *   boot_linux_from_firmware()  load kernel image + ramdisk
 *   boot_linux_fdt()            fill /chosen, lkbootloader
 *
 * Kernel image = linux-arm64-boot-shim (ARM64 Linux Image)
 * Ramdisk      = zircon.zbi
 *
 * DRAM (QEMU virt, MEMBASE 0x40000000):
 *
 *   0x40000000  DTB (ATF)
 *   0x40200000  kernel image  linux-arm64-boot-shim
 *   0x44000000  ramdisk       zircon.zbi
 */
#include "firmware.h"

#include <app.h>
#include <app/lkboot.h>
#include <arch.h>
#include <arch/arm64.h>
#include <arch/mmu.h>
#include <arch/ops.h>
#include <kernel/vm.h>
#include <libfdt.h>
#include <lk/debug.h>
#include <lk/err.h>
#include <lk/list.h>
#include <lk/trace.h>
#include <platform/qemu-virt.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define LOCAL_TRACE 0

#define ARM64_IMAGE_MAGIC 0x644d5241u /* "ARM\x64" little-endian */

#define DTB_PHYS             MEMORY_BASE_PHYS
#define DTB_MAX_SIZE         0x10000

#define KERNEL_IMAGE_PHYS    (MEMORY_BASE_PHYS + 0x00200000)
#define KERNEL_IMAGE_MAX     0x01000000 /* 16 MiB */

#define RAMDISK_PHYS         (MEMORY_BASE_PHYS + 0x04000000)
#define RAMDISK_MAX          0x04000000 /* 64 MiB */

const void *get_fdt(void);

static bool is_arm64_linux_image(const void *buf, size_t len) {
    uint32_t magic;

    if (len < 0x40) {
        return false;
    }
    memcpy(&magic, (const uint8_t *)buf + 0x38, sizeof(magic));
    return magic == ARM64_IMAGE_MAGIC;
}

/* Map a payload at a fixed physical address, lk2 vmm_alloc_physical style. */
static void *map_payload(const char *name, paddr_t pa, size_t size) {
    void *va = paddr_to_kvaddr(pa);
    status_t err = NO_ERROR;
    struct list_node list = LIST_INITIAL_VALUE(list);
    size_t pages = ROUNDUP(size, PAGE_SIZE) / PAGE_SIZE;
    size_t got;

    got = pmm_alloc_range(pa, pages, &list);
    if (got != pages) {
        dprintf(INFO, "lkboot: pmm_alloc_range %s 0x%lx x %zu got %zu\n",
                name, (unsigned long)pa, pages, got);
    }

    if (!va || !vaddr_to_paddr(va)) {
        va = (void *)(KERNEL_BASE + (pa - MEMORY_BASE_PHYS));
        err = vmm_alloc_physical(vmm_get_kernel_aspace(), name,
                                 ROUNDUP(size, PAGE_SIZE), &va, PAGE_SIZE_SHIFT, pa,
                                 VMM_FLAG_VALLOC_SPECIFIC,
                                 ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE |
                                     ARCH_MMU_FLAG_CACHED);
        if (err != NO_ERROR) {
            panic("lkboot: map %s pa 0x%lx failed: %d\n", name, (unsigned long)pa, err);
        }
    }
    return va;
}

static status_t patch_chosen_initrd(void *fdt, paddr_t initrd_pa, size_t initrd_sz) {
    int totalsize = fdt_totalsize(fdt);
    int bufsize = totalsize + 256;
    int offset;
    int err;

    if (bufsize > DTB_MAX_SIZE) {
        bufsize = DTB_MAX_SIZE;
    }
    err = fdt_open_into(fdt, fdt, bufsize);
    if (err < 0) {
        dprintf(CRITICAL, "lkboot: fdt_open_into failed: %d\n", err);
        return ERR_INVALID_ARGS;
    }

    offset = fdt_path_offset(fdt, "/chosen");
    if (offset < 0) {
        offset = fdt_add_subnode(fdt, 0, "chosen");
    }
    if (offset < 0) {
        dprintf(CRITICAL, "lkboot: no /chosen node\n");
        return ERR_NOT_FOUND;
    }

    err = fdt_setprop_u64(fdt, offset, "linux,initrd-start", (uint64_t)initrd_pa);
    if (err < 0) {
        return ERR_GENERIC;
    }
    err = fdt_setprop_u64(fdt, offset, "linux,initrd-end",
                          (uint64_t)initrd_pa + initrd_sz);
    if (err < 0) {
        return ERR_GENERIC;
    }
    err = fdt_pack(fdt);
    if (err < 0) {
        return ERR_GENERIC;
    }
    arch_clean_cache_range((addr_t)fdt, ROUNDUP((size_t)fdt_totalsize(fdt), CACHE_LINE));
    return NO_ERROR;
}

int boot_linux_fdt(void *kernel, paddr_t kernel_pa, void *ramdisk,
                   paddr_t ramdisk_pa, unsigned int ramdisk_sz) {
    void *fdt = (void *)get_fdt();
    paddr_t dtb_pa;

    if (!fdt || fdt_check_header(fdt) != 0) {
        panic("lkboot: no FDT at %p\n", fdt);
    }
    if (!is_arm64_linux_image(kernel, 0x40)) {
        panic("lkboot: kernel image at pa 0x%lx is not linux-arm64-boot-shim\n",
              (unsigned long)kernel_pa);
    }

    if (ramdisk && ramdisk_sz) {
        if (patch_chosen_initrd(fdt, ramdisk_pa, ramdisk_sz) != NO_ERROR) {
            panic("lkboot: failed to set linux,initrd-* for zircon.zbi\n");
        }
    }

    dtb_pa = vaddr_to_paddr(fdt);
    if (dtb_pa == 0) {
        dtb_pa = DTB_PHYS;
    }

    dprintf(ALWAYS, "lkboot: kernel  linux-arm64-boot-shim pa 0x%lx\n",
            (unsigned long)kernel_pa);
    dprintf(ALWAYS, "lkboot: ramdisk zircon.zbi            pa 0x%lx size 0x%x\n",
            (unsigned long)ramdisk_pa, ramdisk_sz);
    dprintf(ALWAYS, "lkboot: dtb                           pa 0x%lx\n",
            (unsigned long)dtb_pa);
    {
        unsigned long el = ARM64_READ_SYSREG(CURRENTEL) >> 2;

        dprintf(ALWAYS, "lkboot: jumping to secular [EL%lu]\n\n", el);
        if (el != 2) {
            panic("lkboot: Secular must be entered at EL2 (VHE), not EL%lu\n",
                  el);
        }
    }

    /* Linux arm64 boot protocol: x0 = DTB, x1-x3 reserved. */
    lkbootloader(kernel, dtb_pa, 0, 0, 0);
    return 0;
}

int boot_linux_from_firmware(void) {
    size_t kernel_sz = 0;
    size_t ramdisk_sz = 0;
    void *kernel_va;
    void *ramdisk_va;
    status_t err;

    dprintf(ALWAYS, "lkboot: firmware kernel=linux-arm64-boot-shim ramdisk=zircon.zbi\n");

    err = firmware_kernel_size(&kernel_sz);
    if (err != NO_ERROR || kernel_sz == 0 || kernel_sz > KERNEL_IMAGE_MAX) {
        panic("lkboot: firmware has no kernel image (linux-arm64-boot-shim)\n");
    }
    err = firmware_ramdisk_size(&ramdisk_sz);
    if (err != NO_ERROR || ramdisk_sz == 0 || ramdisk_sz > RAMDISK_MAX) {
        panic("lkboot: firmware has no ramdisk (zircon.zbi)\n");
    }

    kernel_va = map_payload("vm_kernel", KERNEL_IMAGE_PHYS, kernel_sz);
    ramdisk_va = map_payload("vm_ramdisk", RAMDISK_PHYS, ramdisk_sz);

    if (firmware_load_kernel(kernel_va, kernel_sz) != NO_ERROR) {
        panic("lkboot: firmware_load_kernel failed\n");
    }
    arch_clean_cache_range((addr_t)kernel_va, ROUNDUP(kernel_sz, CACHE_LINE));
    dprintf(ALWAYS, "lkboot: loaded linux-arm64-boot-shim %zu bytes at pa 0x%lx\n",
            kernel_sz, (unsigned long)KERNEL_IMAGE_PHYS);

    if (firmware_load_ramdisk(ramdisk_va, ramdisk_sz) != NO_ERROR) {
        panic("lkboot: firmware_load_ramdisk failed\n");
    }
    arch_clean_cache_range((addr_t)ramdisk_va, ROUNDUP(ramdisk_sz, CACHE_LINE));
    dprintf(ALWAYS, "lkboot: loaded zircon.zbi %zu bytes at pa 0x%lx\n",
            ramdisk_sz, (unsigned long)RAMDISK_PHYS);

    return boot_linux_fdt(kernel_va, KERNEL_IMAGE_PHYS, ramdisk_va, RAMDISK_PHYS,
                          (unsigned int)ramdisk_sz);
}

void notify_boot_linux(void) {
    boot_linux_from_firmware();
}

void lkboot_init(const struct app_descriptor *app) {
    (void)app;
    notify_boot_linux();
}

APP_START(lkboot)
    .init = lkboot_init,
    .entry = NULL,
APP_END
