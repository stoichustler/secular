/* Compatibility layer between Trusty overlay sources and this LK. */
#pragma once

#include <lk/macros.h>

#ifndef SPIN_LOCK_FLAG_IRQ
#define SPIN_LOCK_FLAG_IRQ 0
#endif

#ifndef READ_ONCE
#define READ_ONCE(x) (*(volatile typeof(x) *)&(x))
#endif

#ifndef WRITE_ONCE
#define WRITE_ONCE(x, val) do { *(volatile typeof(x) *)&(x) = (val); } while (0)
#endif

#ifndef IPC_MAX_HANDLES
#define IPC_MAX_HANDLES 64
#endif

#ifndef LK_LOGLEVEL
#define LK_LOGLEVEL LK_DEBUGLEVEL
#endif

#ifndef VMM_FLAG_QUOTA
#define VMM_FLAG_QUOTA 0
#endif

#ifndef VMM_FLAG_NO_PHYSICAL
#define VMM_FLAG_NO_PHYSICAL 0
#endif

#ifndef VMM_FLAG_NO_START_GUARD
#define VMM_FLAG_NO_START_GUARD 0
#endif

#ifndef VMM_FLAG_NO_END_GUARD
#define VMM_FLAG_NO_END_GUARD 0
#endif

#ifndef VMM_ASPACE_FLAG_BTI
#define VMM_ASPACE_FLAG_BTI 0
#endif

#ifndef PRIxPADDR
#define PRIxPADDR "lx"
#endif
#ifndef PRIxVADDR
#define PRIxVADDR "lx"
#endif
#ifndef PRIxPTR_USER
#define PRIxPTR_USER "lx"
#endif
#ifndef PRIxSIZE_USER
#define PRIxSIZE_USER "zx"
#endif
#ifndef PRIuSIZE_USER
#define PRIuSIZE_USER "zu"
#endif

#ifndef ENTER_USPACE_FLAGS
#define ENTER_USPACE_FLAGS 0
#endif

#ifndef ASSEMBLY
#include <err.h>
#include <kernel/thread.h>
#include <kernel/usercopy.h>
#include <kernel/vm.h>
#include <sys/types.h>

status_t vmm_get_obj(const vmm_aspace_t *aspace, vaddr_t vaddr, size_t size,
                     struct vmm_obj_slice *slice);
status_t vmm_free_region_etc(vmm_aspace_t *aspace, vaddr_t va, size_t size, uint32_t flags);
status_t vmm_alloc_no_physical(vmm_aspace_t *aspace, const char *name, size_t size, void **ptr,
                               uint8_t align_log2, uint vmm_flags, uint arch_mmu_flags);
status_t vmm_alloc_physical_etc(vmm_aspace_t *aspace, const char *name, size_t size, void **ptr,
                                uint8_t align_log2, const paddr_t *paddr, uint paddr_count,
                                uint vmm_flags, uint arch_mmu_flags);
status_t vmm_create_aspace_with_quota(vmm_aspace_t **aspace, const char *name, size_t quota,
                                      uint flags);

void thread_sleep_ns(lk_time_ns_t delay);
bool arch_tagging_enabled(void);
bool arch_bti_supported(void);
void arch_set_user_tls(user_addr_t tls);
void *fd_io_handle(uint32_t fd);
void io_lock(void *h);
void io_unlock(void *h);
void io_write_commit(void *h);
#endif
