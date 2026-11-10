/*
 * Copyright (c) 2019-2020 LK Trusty Authors. All Rights Reserved.
 *
 * Permission is hereby granted, free of charge, to any person obtaining
 * a copy of this software and associated documentation files
 * (the "Software"), to deal in the Software without restriction,
 * including without limitation the rights to use, copy, modify, merge,
 * publish, distribute, sublicense, and/or sell copies of the Software,
 * and to permit persons to whom the Software is furnished to do so,
 * subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be
 * included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
 * EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
 * IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
 * CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
 * TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 * SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

#include <compiler.h>
#include <debug.h>
#include <err.h>
#include <interface/arm_ffa/arm_ffa.h>
#include <inttypes.h>
#include <kernel/mutex.h>
#include <kernel/vm.h>
#include <lib/arm_ffa/arm_ffa.h>
#include <lib/extmem/extmem.h>
#include <lib/page_alloc.h>
#include <lib/sm.h>
#include <lib/smc/smc.h>
#include <lk/init.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>

#define LOCAL_TRACE 0

struct sm_mem_obj {
    struct sm_vm* vm;
    struct obj_ref vm_ref;
    struct ext_mem_obj ext_mem_obj;
};

static void sm_mem_obj_compat_destroy(struct vmm_obj* vmm_obj) {
    struct ext_mem_obj* obj = containerof(vmm_obj, struct ext_mem_obj, vmm_obj);
    free(obj);
}

static struct vmm_obj_ops sm_mem_obj_compat_ops = {
        .check_flags = ext_mem_obj_check_flags,
        .get_page = ext_mem_obj_get_page,
        .destroy = sm_mem_obj_compat_destroy,
};

/**
 * sm_mem_compat_get_vmm_obj - Create vmm_obj from id.
 * @client_id:  Id of external entity where the memory originated.
 * @mem_obj_id: Object id containing a packed address and attibutes.
 * @size:       Size of object.
 * @objp:       Pointer to return object in.
 * @obj_ref:    Reference to *@objp.
 *
 * The object paddr and attibutes are encoded in the id for now. Convert it to a
 * paddr and mmu-flags using the existing helper function.
 *
 * Return: 0 on success, negative error code if object could not be created.
 */
static status_t sm_mem_compat_get_vmm_obj(ext_mem_client_id_t client_id,
                                          ext_mem_obj_id_t mem_obj_id,
                                          size_t size,
                                          struct vmm_obj** objp,
                                          struct obj_ref* obj_ref) {
    int ret;
    struct ext_mem_obj* obj;
    struct ns_page_info pinf = {mem_obj_id};
    ns_addr_t ns_paddr;
    paddr_t paddr;
    uint arch_mmu_flags;

    ret = sm_decode_ns_memory_attr(&pinf, &ns_paddr, &arch_mmu_flags);
    if (ret) {
        return ret;
    }

    paddr = (paddr_t)ns_paddr;
    if (paddr != ns_paddr) {
        /*
         * If ns_addr_t is larger than paddr_t and we get an address that does
         * not fit, return an error as we cannot map that address.
         */
        TRACEF("unsupported paddr, 0x%0" PRIxNS_ADDR "\n", ns_paddr);
        return ERR_INVALID_ARGS;
    }

    obj = malloc(sizeof(*obj) + ext_mem_obj_page_runs_size(1));
    if (!obj) {
        return ERR_NO_MEMORY;
    }

    arch_mmu_flags |= ARCH_MMU_FLAG_NS | ARCH_MMU_FLAG_PERM_NO_EXECUTE;
    ext_mem_obj_initialize(obj, obj_ref, mem_obj_id, 0, &sm_mem_obj_compat_ops,
                           arch_mmu_flags, 1);
    obj->page_runs[0].paddr = paddr;
    obj->page_runs[0].size = size;
    *objp = &obj->vmm_obj;

    return 0;
}

/**
 * sm_mem_obj_destroy: Destroy memory object.
 * @vmm_obj:    VMM object to destroy.
 *
 * Called after the last reference to @vmm_obj has been released. Relinquish
 * shared memory object id with SPM/Hypervisor and free local tracking object.
 */
static void sm_mem_obj_destroy(struct vmm_obj* vmm_obj) {
    int ret;
    struct sm_mem_obj* obj =
            containerof(vmm_obj, struct sm_mem_obj, ext_mem_obj.vmm_obj);

    DEBUG_ASSERT(obj);
    DEBUG_ASSERT(obj->vm);

    ret = arm_ffa_mem_relinquish(obj->ext_mem_obj.id);
    if (ret != NO_ERROR) {
        TRACEF("Failed to relinquish the shared memory (%d)\n", ret);
    }

    /* Release the VM reference */
    sm_vm_del_ref(obj->vm, &obj->vm_ref);
    free(obj);
}

static struct vmm_obj_ops sm_mem_obj_ops = {
        .check_flags = ext_mem_obj_check_flags,
        .get_page = ext_mem_obj_get_page,
        .destroy = sm_mem_obj_destroy,
};

/**
 * sm_mem_alloc_obj - Allocate and initialize memory object.
 * @vm:             Pointer to VM object of sender.
 * @mem_id:         Id of object.
 * @tag:            Tag of the object
 * @page_run_count: Number of page runs to allocate for object.
 * @arch_mmu_flags: Memory type and permissions.
 * @obj_ref:        Reference to returned object.
 *
 * Return: Pointer to &struct sm_mem_obj, or %NULL if allocation fails.
 */
static struct sm_mem_obj* sm_mem_alloc_obj(struct sm_vm* vm,
                                           ext_mem_obj_id_t mem_id,
                                           uint64_t tag,
                                           size_t page_run_count,
                                           uint arch_mmu_flags,
                                           struct obj_ref* obj_ref) {
    struct sm_mem_obj* obj =
            malloc(sizeof(*obj) + ext_mem_obj_page_runs_size(page_run_count));
    if (!obj) {
        return NULL;
    }
    ext_mem_obj_initialize(&obj->ext_mem_obj, obj_ref, mem_id, tag,
                           &sm_mem_obj_ops, arch_mmu_flags, page_run_count);

    obj->vm = vm;
    obj_ref_init(&obj->vm_ref);
    sm_vm_add_ref(vm, &obj->vm_ref);

    return obj;
}

/* sm_mem_get_vmm_obj - Looks up a shared memory object using FF-A.
 * @vm:             Pointer to VM object of sender.
 * @mem_obj_id:     Id of shared memory object to lookup and return.
 * @tag:            Tag of the memory.
 * @size:           Size hint for object. Caller expects an object at least this
 *                  big.
 * @objp:           Pointer to return object in.
 * @obj_ref:        Reference to *@objp.
 *
 * Return: 0 on success. ERR_NOT_FOUND if @id does not exist.
 */
static status_t sm_mem_get_vmm_obj(struct sm_vm* vm,
                                   ext_mem_obj_id_t mem_obj_id,
                                   uint64_t tag,
                                   size_t size,
                                   struct vmm_obj** objp,
                                   struct obj_ref* obj_ref) {
    int ret;
    ext_mem_client_id_t client_id;
    struct arm_ffa_mem_frag_info frag_info;
    uint32_t address_range_count;
    uint arch_mmu_flags;
    struct sm_mem_obj* obj;
    struct obj_ref tmp_obj_ref = OBJ_REF_INITIAL_VALUE(tmp_obj_ref);

    DEBUG_ASSERT(vm);
    DEBUG_ASSERT(objp);
    DEBUG_ASSERT(obj_ref);

    client_id = sm_vm_get_id(vm);
    if ((client_id & 0xffff) != client_id) {
        TRACEF("Invalid client ID\n");
        return ERR_INVALID_ARGS;
    }

    ret = arm_ffa_mem_retrieve_start((uint16_t)client_id, mem_obj_id, tag,
                                     &address_range_count, &arch_mmu_flags,
                                     &frag_info);

    if (ret != NO_ERROR) {
        TRACEF("Failed to get FF-A memory buffer, err=%d\n", ret);
        goto err_mem_get_access;
    }
    obj = sm_mem_alloc_obj(vm, mem_obj_id, tag, address_range_count,
                           arch_mmu_flags, &tmp_obj_ref);
    if (!obj) {
        TRACEF("Failed to allocate a shared memory object\n");
        ret = ERR_NO_MEMORY;
        goto err_mem_alloc_obj;
    }

    for (uint32_t i = 0; i < address_range_count; i++) {
        if (frag_info.start_index + frag_info.count <= i) {
            arm_ffa_rx_release();
            ret = arm_ffa_mem_retrieve_next_frag(mem_obj_id, &frag_info);
            if (ret != NO_ERROR) {
                TRACEF("Failed to get next fragment, err=%d\n", ret);
                goto err_mem_next_frag;
            }
        }
        ret = arm_ffa_mem_address_range_get(
                &frag_info, i, &obj->ext_mem_obj.page_runs[i].paddr,
                &obj->ext_mem_obj.page_runs[i].size);
        if (ret != NO_ERROR) {
            TRACEF("Failed to get address range, err=%d\n", ret);
            goto err_mem_address_range;
        }
    }

    /* No lock needed as the object is not yet visible to anyone else */
    obj_ref_transfer(obj_ref, &tmp_obj_ref);
    *objp = &obj->ext_mem_obj.vmm_obj;

    arm_ffa_rx_release();

    return 0;

err_mem_address_range:
err_mem_next_frag:
    DEBUG_ASSERT(obj_ref_active(&tmp_obj_ref));
    vmm_obj_del_ref(&obj->ext_mem_obj.vmm_obj, &tmp_obj_ref);

err_mem_alloc_obj:
err_mem_get_access:
    arm_ffa_rx_release();
    return ret;
}

/**
 * ext_mem_get_vm_vmm_obj - Lookup or create shared memory object for a VM.
 * @vm:         VM where the memory originated.
 * @mem_obj_id: Id of shared memory object to lookup and return.
 * @tag:        Value to identify the transaction.
 * @size:       Size hint for object.
 * @objp:       Pointer to return object in.
 * @obj_ref:    Reference to *@objp.
 *
 * Call SPM/Hypervisor to retrieve memory region or extract address and
 * attributes from id for old clients.
 */
status_t ext_mem_get_vm_vmm_obj(struct sm_vm* vm,
                                ext_mem_obj_id_t mem_obj_id,
                                uint64_t tag,
                                size_t size,
                                struct vmm_obj** objp,
                                struct obj_ref* obj_ref) {
    ext_mem_client_id_t client_id = sm_vm_get_id(vm);

    if (sm_get_api_version() >= TRUSTY_API_VERSION_MEM_OBJ) {
        return sm_mem_get_vmm_obj(vm, mem_obj_id, tag, size, objp, obj_ref);
    } else if (!client_id && !tag) {
        /* If client is not running under a hypervisor allow using
           old api. */
        return sm_mem_compat_get_vmm_obj(client_id, mem_obj_id, size, objp,
                                         obj_ref);
    } else {
        return ERR_NOT_SUPPORTED;
    }
}

/*
 * ext_mem_get_vmm_obj - Lookup or create shared memory object.
 * @client_id:  Id of external entity where the memory originated.
 * @mem_obj_id: Id of shared memory object to lookup and return.
 * @tag:        Value to identify the transaction.
 * @size:       Size hint for object.
 * @objp:       Pointer to return object in.
 * @obj_ref:    Reference to *@objp.
 *
 * Call SPM/Hypervisor to retrieve memory region or extract address and
 * attributes from id for old clients.
 */
status_t ext_mem_get_vmm_obj(ext_mem_client_id_t client_id,
                             ext_mem_obj_id_t mem_obj_id,
                             uint64_t tag,
                             size_t size,
                             struct vmm_obj** objp,
                             struct obj_ref* obj_ref) {
    struct sm_vm* vm = NULL;
    struct obj_ref vm_ref = OBJ_REF_INITIAL_VALUE(vm_ref);
    status_t ret;

    /*
     * Get the VM for the given client ID.
     * This should work even for the compatibility path because
     * we have a default compatibility VM that the callee should return here.
     */
    ret = sm_vm_get(client_id, &vm_ref, &vm);
    if (ret != NO_ERROR) {
        TRACEF("Failed to get VM %" PRIu64 " reference (%d)\n", client_id, ret);
        return ret;
    }

    ret = ext_mem_get_vm_vmm_obj(vm, mem_obj_id, tag, size, objp, obj_ref);
    sm_vm_del_ref(vm, &vm_ref);
    return ret;
}

/**
 * shared_mem_init - Connect to SPM/Hypervisor.
 * @level:  Unused.
 *
 */
static void shared_mem_init(uint level) {
    /* Check the FF-A module initialized successfully */
    if (!arm_ffa_is_init()) {
        TRACEF("arm_ffa module is not initialized\n");
        if (sm_check_and_lock_api_version(TRUSTY_API_VERSION_MEM_OBJ)) {
            panic("shared_mem_init failed after mem_obj version selected\n");
        }
    }
}

LK_INIT_HOOK(shared_mem, shared_mem_init, LK_INIT_LEVEL_APPS);
