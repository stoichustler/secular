/*
 * Copyright (c) 2013-2016 Google Inc. All rights reserved
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
#ifndef __SM_H
#define __SM_H

#include <lib/extmem/extmem.h>
#include <lib/sm/smcall.h>
#include <lk/list.h>
#include <lk/reflist.h>
#include <stdbool.h>
#include <stddef.h>
#include <sys/types.h>
#include <uapi/err.h>

#define PRIxNS_ADDR PRIx64

typedef uint64_t ns_addr_t;
typedef uint32_t ns_size_t;

struct ns_page_info {
    uint64_t attr;
};

struct smc32_args {
    uint32_t smc_nr;
    uint32_t params[SMC_NUM_PARAMS];
    ext_mem_obj_id_t client_id;
};

#define SMC32_ARGS_INITIAL_VALUE(args) \
    { 0, {0}, 0 }

typedef long (*smc32_handler_t)(struct smc32_args* args);

struct smc32_entity {
    smc32_handler_t fastcall_handler;
    smc32_handler_t nopcall_handler;
    smc32_handler_t stdcall_handler;
};

/* Get selected api version. */
uint32_t sm_get_api_version(void);

/**
 * sm_check_and_lock_api_version - Check and lock api version
 * @api_version_wanted: Version wanted.
 *
 * Check if the currently selected api version is greater or equal to
 * @api_version_wanted and prevent changing the selected api version to a
 * a version that would change that answer.
 *
 * Return: true if currently connected client support @api_version_wanted.
 */
bool sm_check_and_lock_api_version(uint32_t api_version_wanted);

/* Schedule Secure OS */
long sm_sched_secure(struct smc32_args* args);

/* Schedule Non-secure OS */
void sm_sched_nonsecure(long retval, struct smc32_args* args);

/* Handle an interrupt */
enum handler_return sm_handle_irq(void);
void sm_handle_fiq(void);

/* Version */
long smc_sm_api_version(struct smc32_args* args);

/* SMP mode */
long smc_get_smp_max_cpus(struct smc32_args* args);

/* Interrupt controller irq/fiq support */
long smc_intc_get_next_irq(struct smc32_args* args);
/* return 0 to enter ns-fiq handler, return non-0 to return */
status_t sm_intc_fiq_enter(void);
enum handler_return sm_intc_enable_interrupts(void);

/*
 * Ring the doorbell or equivalent interrupt on the primary scheduler
 * so that it enqueues a NOP for Trusty.
 */
void sm_intc_raise_doorbell_irq(void);

/* Get the argument block passed in by the bootloader */
status_t sm_get_boot_args(void** boot_argsp, size_t* args_sizep);

/* Release bootloader arg block */
void sm_put_boot_args(void);

/* Register handler(s) for an entity */
status_t sm_register_entity(uint entity_nr, struct smc32_entity* entity);

status_t sm_decode_ns_memory_attr(struct ns_page_info* pinf,
                                  ns_addr_t* ppa,
                                  uint* pmmu);

/**
 * struct sm_vm_notifier - VM notifier to call on VM events.
 * @node:           List node in notifiers list.
 * @client_id:      VM identifier.
 * @destroy:        Destruction event callback.
 *
 * The &struct sm_vm_notifier object must exist at least until both the
 * @destroy callback is called or sm_vm_notifier_unregister() has returned.
 * If sm_vm_notifier_unregister() has returned, the callback will not be called
 * and it is safe to free the object.
 *
 * If the client intends to call sm_vm_notifier_unregister() at any point
 * after the callback returns, it should keep the notifier object alive
 * until after the last call to sm_vm_notifier_unregister().
 * Note that all such invocations will just return %ERR_NOT_FOUND.
 */
struct sm_vm_notifier {
    struct list_node node;
    ext_mem_obj_id_t client_id;
    status_t (*destroy)(struct sm_vm_notifier*);
};

/**
 * sm_vm_notifier_init() - Initialize a notifier.
 * @notif: Pointer to notifier
 * @client_id: VM identifier to set in notifier.
 * @destroy: Destruction callback.
 */
static inline status_t sm_vm_notifier_init(
        struct sm_vm_notifier* notif,
        ext_mem_obj_id_t client_id,
        status_t (*destroy)(struct sm_vm_notifier*)) {
    if (!notif) {
        return ERR_INVALID_ARGS;
    }
    if (!destroy) {
        return ERR_INVALID_ARGS;
    }

    list_clear_node(&notif->node);
    notif->client_id = client_id;
    notif->destroy = destroy;

    return NO_ERROR;
}

/**
 * sm_vm_notifier_register() - Register a notifier for VM events.
 * @notif: Pointer to notifier.
 *
 * Return:
 * * %0 in case of success
 * * %ERR_INVALID_ARGS if @notif is invalid
 * * %ERR_NOT_FOUND if the VM has not been created
 * * %ERR_BAD_STATE if the VM is already present and in an invalid state
 *
 * The contents of @notif should be initialized using
 * sm_vm_notifier_init().
 *
 * The function does not take ownership of @notif.
 */
status_t sm_vm_notifier_register(struct sm_vm_notifier* notif);

/**
 * sm_vm_notifier_unregister() - Unregister a notifier for VM events.
 * @notif: Pointer to notifier.
 *
 * Return:
 * * %0 in case of success
 * * %ERR_INVALID_ARGS if @notif is invalid
 * * %ERR_NOT_FOUND if the notifier has not been previously registered
 *
 * The function will block until the destruction callback finishes
 * if it is already running on another thread. If the destruction callback
 * is not already running, it will not be called.
 */
status_t sm_vm_notifier_unregister(struct sm_vm_notifier* notif);

struct sm_vm;

/**
 * sm_vm_get() - Get a &struct sm_vm for a given client id.
 * @client_id: Id of the referenced VM.
 * @ref:       Pointer to uninitialized &struct obj_ref object.
 *             Must not be %NULL.
 * @out_vm:    Output pointer to receive a pointer to the VM object.
 *
 * Return:
 * * %0 in case of success
 * * %ERR_INVALID_ARGS if @ref or @out_vm are invalid.
 * * %ERR_NOT_FOUND if the VM does not exist
 */
status_t sm_vm_get(ext_mem_obj_id_t client_id,
                   struct obj_ref* ref,
                   struct sm_vm** out_vm);

/**
 * sm_vm_add_ref() - Add a reference to a VM.
 * @vm:  Pointer to referenced VM object. Must not be %NULL.
 * @ref: Pointer to uninitialized &struct obj_ref object.
 *       Must not be %NULL.
 */
void sm_vm_add_ref(struct sm_vm* vm, struct obj_ref* ref);

/**
 * sm_vm_del_ref() - Delete a reference to a VM.
 * @vm:  Pointer to referenced VM object. Must not be %NULL.
 * @ref: Pointer to valid &struct obj_ref object. Must not be %NULL.
 */
void sm_vm_del_ref(struct sm_vm* vm, struct obj_ref* ref);

/**
 * sm_vm_get_id() - Get the client ID of a VM.
 * @vm: Pointer to VM object. Must not be %NULL.
 *
 * Return: the VM ID.
 */
ext_mem_client_id_t sm_vm_get_id(struct sm_vm* vm);

#endif /* __SM_H */
