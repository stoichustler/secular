/*
 * Copyright (c) 2012-2015 Travis Geiselbrecht
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */
#include <platform/qemu-virt.h>

#include <arch.h>
#include <assert.h>
#include <lk/err.h>
#include <lk/debug.h>
#include <lk/trace.h>
#include <stdint.h>
#if WITH_DEV_BUS_PCI
#include <dev/bus/pci.h>
#endif
#include <dev/interrupt/arm_gic.h>
#include <dev/power/psci.h>
#include <dev/timer/arm_generic.h>
#if WITH_DEV_VIRTIO
#include <dev/virtio.h>
#include <dev/virtio/net.h>
#endif
#include <lib/fdtwalk.h>
#include <lk/init.h>
#include <kernel/vm.h>
#include <kernel/spinlock.h>
#include <platform.h>
#include <platform/gic.h>
#include <platform/interrupts.h>
#if LK_TRUSTY_TEE && WITH_SMP
#include <arch/arm64/mp.h>
#include <lk/main.h>
#endif

#if WITH_LIB_MINIP
#include <lib/minip.h>
#endif
#if WITH_DEV_UART_PL011
#include <dev/uart/pl011.h>
#endif
#if WITH_RUST_DEV_PL011
#include <rust/dev-pl011.h>
#endif

#define LOCAL_TRACE 0

#define DEFAULT_MEMORY_SIZE (MEMSIZE) /* try to fetch from the emulator via the fdt */

#if LK_TRUSTY_TEE
static pmm_arena_t trusty_ram_arena = {
    .name = "sdram",
    .base = MEMBASE,
    .size = MEMSIZE,
    .flags = PMM_ARENA_FLAG_KMAP,
};
#endif

/* initial memory mappings. parsed by start.S */
struct mmu_initial_mapping mmu_initial_mappings[] = {
    /* all of memory */
    {
#if LK_TRUSTY_TEE
        .phys = MEMBASE,
        .virt = KERNEL_BASE,
        .size = MEMSIZE,
#else
        .phys = MEMORY_BASE_PHYS,
        .virt = KERNEL_BASE,
        .size = MEMORY_APERTURE_SIZE,
#endif
        .flags = 0,
        .name = "memory"
    },

    /* 1GB of peripherals */
    {
        .phys = PERIPHERAL_BASE_PHYS,
        .virt = PERIPHERAL_BASE_VIRT,
        .size = PERIPHERAL_BASE_SIZE,
        .flags = MMU_INITIAL_MAPPING_FLAG_DEVICE,
        .name = "peripherals"
    },

    /* null entry to terminate the list */
    { 0 }
};

#if LK_TRUSTY_TEE
const void *fdt = NULL;
#else
const void *fdt = (void *)KERNEL_BASE;
#endif

const void *get_fdt(void) {
    return fdt;
}

void platform_early_init(void) {
    const struct pl011_config uart_config = {
        .base = UART_BASE,
        .irq = UART0_INT,
        .flag = PL011_FLAG_DEBUG_UART,
    };

    pl011_init_early(0, &uart_config);

#if !LK_TRUSTY_TEE
    /* Trusty must not program CNTV: NS Zircon owns the virtual timer PPIs. */
    arm_generic_timer_init(ARM_GENERIC_TIMER_VIRTUAL_INT, 0);
#endif

#if LK_TRUSTY_TEE
    /* Secure DRAM is not described in the NS DTB ATF hands BL33. */
    pmm_add_arena(&trusty_ram_arena);
#else
    if (LOCAL_TRACE) {
        LTRACEF("dumping FDT at %p\n", fdt);
        fdt_walk_dump(fdt);
    }

    // detect physical memory layout from the device tree
    fdtwalk_setup_memory(fdt, MEMORY_BASE_PHYS, MEMORY_BASE_PHYS, DEFAULT_MEMORY_SIZE);

    // initialize cmdline from device tree
    fdtwalk_setup_cmdline(fdt);
#endif
}

void platform_postvm_init(uint level) {
#if LK_TRUSTY_TEE
    /*
     * Leave the GIC to ATF. Re-initializing the distributor from S-EL1
     * would drop Group0 / Group1S configuration the SPD relies on.
     * Group-1 NS IRQs are FIQs here; S-EL1 keeps DAIF.F set so they
     * stay pending until we return to NS.
     */
    (void)level;
#else
    // Initialize the interrupt controller from the device tree.
    // Do it at this run level so the gic can map its registers.
    if (fdtwalk_setup_gic(fdt) != NO_ERROR) {
        panic("failed to initialize GIC from FDT\n");
    }
#endif
}

LK_INIT_HOOK(platform_postvm_init, platform_postvm_init, LK_INIT_LEVEL_VM);

void platform_init(void) {
    pl011_init(0);

#if LK_TRUSTY_TEE && WITH_SMP
    /* NS CPU_ON via ATF SPD brings up these cores; do not PSCI them from S-EL1. */
    arm64_set_secondary_cpu_count(SMP_MAX_CPUS - 1);
    lk_init_secondary_cpus(SMP_MAX_CPUS - 1);
    dprintf(ALWAYS, "TEE: %d cpus, secondaries join on NS CPU_ON\n",
            SMP_MAX_CPUS);
#endif

#if !LK_TRUSTY_TEE
    // start secondary cores
    fdtwalk_setup_cpus_arm(fdt);

#if WITH_DEV_BUS_PCI
    /* configure and start pci from device tree */
    status_t err = fdtwalk_setup_pci(fdt);
    if (err >= NO_ERROR) {
        // start the bus manager
        pci_bus_mgr_init();

        // assign resources to all devices in case they need it
        pci_bus_mgr_assign_resources();

        // scan for an initialize any virtio devices
        virtio_pci_init();
    }
#endif

#if WITH_DEV_VIRTIO
    /* detect any virtio devices */
    uint virtio_irqs[NUM_VIRTIO_TRANSPORTS];
    for (int i = 0; i < NUM_VIRTIO_TRANSPORTS; i++) {
        virtio_irqs[i] = VIRTIO0_INT_BASE + i;
    }

    virtio_mmio_detect((void *)VIRTIO_BASE, NUM_VIRTIO_TRANSPORTS, virtio_irqs, 0x200);
#endif
#endif /* !LK_TRUSTY_TEE */
}

#if WITH_DEV_BUS_PCI
status_t platform_pci_int_line_to_vector(unsigned int pci_int_line, pci_location_t loc,
        unsigned int *vector) {
    (void)pci_int_line;
    (void)loc;
    (void)vector;

    // QEMU virt arm uses INTx pin swizzling for legacy routing.
    return ERR_NOT_SUPPORTED;
}

status_t platform_pci_int_pin_to_vector(unsigned int pci_int_pin, pci_location_t loc,
    unsigned int *vector) {

    // QEMU arm virt machine uses standard PCI swizzle on 4 legacy IRQs:
    // irq = first_irq + ((pin - 1 + slot) % 4), where pin is 1..4.
    // first_irq here is PCIE_INT_BASE.
    if (pci_int_pin < 1 || pci_int_pin > 4) {
        return ERR_OUT_OF_RANGE;
    }

    *vector = PCIE_INT_BASE + ((pci_int_pin - 1 + loc.dev) % 4);
    return NO_ERROR;
}
#endif

status_t platform_allocate_interrupts(size_t count, uint align_log2, bool msi, unsigned int *vector) {
    TRACEF("count %zu align %u msi %d\n", count, align_log2, msi);

    // TODO: handle nonzero alignment, count > 0, and add locking

    // list of allocated msi interrupts
    static uint64_t msi_bitmap = 0;

    // cannot handle allocating for anything but MSI interrupts
    if (!msi) {
        return ERR_NOT_SUPPORTED;
    }

    // cannot deal with alignment yet
    DEBUG_ASSERT(align_log2 == 0);
    DEBUG_ASSERT(count == 1);

    // make a copy of the bitmap
    int allocated = -1;
    for (size_t i = 0; i < sizeof(msi_bitmap) * 8; i++) {
        if ((msi_bitmap & (1UL << i)) == 0) {
            msi_bitmap |= (1UL << i);
            allocated = i;
            break;
        }
    }
    if (allocated < 0) {
        return ERR_NOT_FOUND;
    }

    allocated += MSI_INT_BASE;

    TRACEF("allocated msi at %u\n", allocated);
    *vector = allocated;
    return NO_ERROR;
}

status_t platform_compute_msi_values(unsigned int vector, unsigned int cpu, bool edge,
                                     uint64_t *msi_address_out, uint16_t *msi_data_out) {

    // only handle edge triggered at the moment
    DEBUG_ASSERT(edge);
    // only handle cpu 0
    DEBUG_ASSERT(cpu == 0);

    // TODO: call through to the appropriate gic driver to deal with GICv2 vs v3

    *msi_data_out = vector;
    *msi_address_out = 0x08020040; // address of the GICv2 MSI port

    return NO_ERROR;
}

void platform_halt(platform_halt_action suggested_action, platform_halt_reason reason) {
#if LK_TRUSTY_TEE
    /*
     * Do not PSCI-reset the whole machine from S-EL1. Return to ATF with
     * SM_ERR_PANIC so the NS bootloader can keep running (or hang visibly).
     */
    (void)suggested_action;
    dprintf(ALWAYS, "TEE: HALT reason=%d\n", reason);
    for (;;) {
        register uint64_t x0 asm("x0") = 0x3C000000u; /* SMC_YC_NS_RETURN */
        register uint64_t x1 asm("x1") = (uint64_t)(long)-11; /* SM_ERR_PANIC */
        __asm__ volatile("smc #0" : "+r"(x0), "+r"(x1) : : "x2", "x3", "memory");
    }
#else
    // Use the default halt implementation using psci as the reset and shutdown implementation.
    platform_halt_default(suggested_action, reason, &psci_system_reset, &psci_system_off);
#endif
}
