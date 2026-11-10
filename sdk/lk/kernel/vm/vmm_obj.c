#include <err.h>
#include <kernel/vm.h>
#include <kernel/vm_obj.h>
#include <lk/debug.h>
#include <lk/err.h>
#include <stdlib.h>
#include <string.h>

void vmm_obj_add_ref(struct vmm_obj *obj, struct obj_ref *ref) {
    obj_add_ref(&obj->obj, ref);
}

static void vmm_obj_destroy_obj(struct obj *o) {
    struct vmm_obj *v = containerof(o, struct vmm_obj, obj);
    v->ops->destroy(v);
}

void vmm_obj_del_ref(struct vmm_obj *obj, struct obj_ref *ref) {
    obj_del_ref(&obj->obj, ref, vmm_obj_destroy_obj);
}

bool vmm_obj_has_only_ref(struct vmm_obj *obj, struct obj_ref *ref) {
    return obj_has_only_ref(&obj->obj, ref);
}

status_t vmm_alloc_obj(vmm_aspace_t *aspace, const char *name, struct vmm_obj *obj,
                       size_t offset, size_t size, void **ptr, uint8_t align_log2,
                       uint vmm_flags, uint arch_mmu_flags) {
    status_t err;
    vaddr_t va = 0;
    size_t mapped = 0;

    DEBUG_ASSERT(aspace);
    DEBUG_ASSERT(obj);
    DEBUG_ASSERT(obj->ops);

    size = ROUNDUP(size, PAGE_SIZE);
    if (size == 0) {
        return ERR_INVALID_ARGS;
    }
    if (!name) {
        name = "";
    }

    err = obj->ops->check_flags(obj, &arch_mmu_flags);
    if (err) {
        return err;
    }
    if (arch_mmu_flags & ARCH_MMU_FLAG_PERM_NO_EXECUTE) {
        arch_mmu_flags &= ~ARCH_MMU_FLAG_PERM_EXECUTE;
        arch_mmu_flags &= ~ARCH_MMU_FLAG_PERM_NO_EXECUTE;
    }
    if (arch_mmu_flags & ARCH_MMU_FLAG_PERM_RO) {
        arch_mmu_flags &= ~ARCH_MMU_FLAG_PERM_WRITE;
        arch_mmu_flags &= ~ARCH_MMU_FLAG_PERM_RO;
    }
    if (!(arch_mmu_flags & ARCH_MMU_FLAG_PERM_READ)) {
        arch_mmu_flags |= ARCH_MMU_FLAG_PERM_READ;
    }

    err = vmm_reserve_range(aspace, name, size, align_log2, &va);
    if (err) {
        return err;
    }

    vmm_aspace_lock(aspace);
    while (mapped < size) {
        paddr_t paddr;
        size_t paddr_size = 0;
        err = obj->ops->get_page(obj, offset + mapped, &paddr, &paddr_size);
        if (err) {
            break;
        }
        if (paddr_size < PAGE_SIZE) {
            err = ERR_INVALID_ARGS;
            break;
        }
        paddr_size = MIN(paddr_size, size - mapped);
        paddr_size = ROUNDDOWN(paddr_size, PAGE_SIZE);
        err = arch_mmu_map(&aspace->arch_aspace, va + mapped, paddr, paddr_size / PAGE_SIZE,
                           arch_mmu_flags);
        if (err) {
            break;
        }
        mapped += paddr_size;
    }
    vmm_aspace_unlock(aspace);

    if (err) {
        vmm_free_region(aspace, va);
        return err;
    }

    if (ptr) {
        *ptr = (void *)va;
    }
    return NO_ERROR;
}

void vmm_obj_slice_init(struct vmm_obj_slice *slice) {
    slice->obj = NULL;
    obj_ref_init(&slice->obj_ref);
    slice->offset = 0;
    slice->size = 0;
}

void vmm_obj_slice_bind(struct vmm_obj_slice *slice, struct vmm_obj *obj, size_t offset,
                        size_t size) {
    DEBUG_ASSERT(!slice->obj);
    slice->obj = obj;
    obj_add_ref(&slice->obj->obj, &slice->obj_ref);
    slice->offset = offset;
    slice->size = size;
}

void vmm_obj_slice_release(struct vmm_obj_slice *slice) {
    if (slice->obj) {
        vmm_obj_del_ref(slice->obj, &slice->obj_ref);
        slice->obj = NULL;
    }
}
