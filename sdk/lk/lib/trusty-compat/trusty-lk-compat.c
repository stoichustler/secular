#include <err.h>
#include <kernel/thread.h>
#include <kernel/vm.h>
#include <platform/time.h>
#include <trusty-lk-compat.h>

status_t vmm_get_obj(const vmm_aspace_t *aspace, vaddr_t vaddr, size_t size,
                     struct vmm_obj_slice *slice) {
    (void)aspace;
    (void)vaddr;
    (void)size;
    (void)slice;
    return ERR_NOT_SUPPORTED;
}

status_t vmm_free_region_etc(vmm_aspace_t *aspace, vaddr_t va, size_t size, uint32_t flags) {
    (void)size;
    (void)flags;
    return vmm_free_region(aspace, va);
}

status_t vmm_alloc_no_physical(vmm_aspace_t *aspace, const char *name, size_t size, void **ptr,
                               uint8_t align_log2, uint vmm_flags, uint arch_mmu_flags) {
    (void)vmm_flags;
    (void)arch_mmu_flags;
    vaddr_t va = 0;
    status_t err = vmm_reserve_range(aspace, name, size, align_log2, &va);
    if (err == NO_ERROR && ptr) {
        *ptr = (void *)va;
    }
    return err;
}

status_t vmm_alloc_physical_etc(vmm_aspace_t *aspace, const char *name, size_t size, void **ptr,
                                uint8_t align_log2, const paddr_t *paddr, uint paddr_count,
                                uint vmm_flags, uint arch_mmu_flags) {
    if (!paddr || paddr_count == 0) {
        return ERR_INVALID_ARGS;
    }
    (void)paddr_count;
    return vmm_alloc_physical(aspace, name, size, ptr, align_log2, paddr[0], vmm_flags,
                              arch_mmu_flags);
}

status_t vmm_create_aspace_with_quota(vmm_aspace_t **aspace, const char *name, size_t quota,
                                      uint flags) {
    (void)quota;
    return vmm_create_aspace(aspace, name, flags);
}



void thread_sleep_ns(lk_time_ns_t delay) {
    lk_time_t ms = (lk_time_t)(delay / 1000000ULL);
    if (ms == 0 && delay != 0) {
        ms = 1;
    }
    thread_sleep(ms);
}

bool arch_tagging_enabled(void) { return false; }
bool arch_bti_supported(void) { return false; }
void arch_set_user_tls(user_addr_t tls) { (void)tls; }
void *fd_io_handle(uint32_t fd) {
    (void)fd;
    return NULL;
}
void io_lock(void *h) { (void)h; }
void io_unlock(void *h) { (void)h; }
void io_write_commit(void *h) { (void)h; }

#include <lib/sm.h>

long smc_intc_get_next_irq(struct smc32_args *args) {
    (void)args;
    return -1;
}

status_t sm_intc_fiq_enter(void) { return 1; }

enum handler_return sm_intc_enable_interrupts(void) { return INT_NO_RESCHEDULE; }

void sm_intc_raise_doorbell_irq(void) {}
