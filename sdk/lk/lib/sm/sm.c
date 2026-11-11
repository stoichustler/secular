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

#include <arch/mp.h>
#include <err.h>
#include <interface/arm_ffa/arm_ffa.h>
#include <kernel/event.h>
#include <kernel/mutex.h>
#include <kernel/thread.h>
#include <kernel/vm.h>
#include <lib/arm_ffa/arm_ffa.h>
#include <lib/binary_search_tree.h>
#include <lib/heap.h>
#include <lib/sm.h>
#include <lib/sm/sm_err.h>
#include <lib/sm/smcall.h>
#include <lk/init.h>
#include <platform.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>
#include <version.h>

#define LOCAL_TRACE 0

struct sm_std_call_state {
    spin_lock_t lock;
    event_t event;
    struct smc32_args args;
    long ret;
    bool done;
    int active_cpu;  /* cpu that expects stdcall result */
    int initial_cpu; /* Debug info: cpu that started stdcall */
    int last_cpu;    /* Debug info: most recent cpu expecting stdcall result */
    int restart_count;
};

extern unsigned long monitor_vector_table;
extern ulong lk_boot_args[4];

static void* boot_args;
static int boot_args_refcnt;
static mutex_t boot_args_lock = MUTEX_INITIAL_VALUE(boot_args_lock);
static atomic_uint_fast32_t sm_api_version;
static atomic_uint_fast32_t sm_api_version_min;
static atomic_uint_fast32_t sm_api_version_max = TRUSTY_API_VERSION_CURRENT;
static spin_lock_t sm_api_version_lock;
static atomic_bool platform_halted;

#if LIB_SM_WITH_FFA_LOOP
static bool sm_use_ffa = true;
static atomic_bool sm_ffa_valid_call;
#else
static bool sm_use_ffa = false;
#endif

enum sm_vm_state {
    SM_VM_STATE_FRESH,
    SM_VM_STATE_AVAILABLE,
    SM_VM_STATE_DESTROY_NOTIFYING,
    SM_VM_STATE_DESTROY_NOTIFIED,
    SM_VM_STATE_READY_TO_FREE
};

struct sm_vm {
    struct bst_node node;
    enum sm_vm_state state;
    ext_mem_obj_id_t client_id;
    struct list_node notifiers;
    struct obj refobj;
    struct obj_ref self_ref;
};

/*
 * VM ID to create; can be one of two values:
 * * Non-negative 16-bit VM ID, or
 * * -1 when no VM needs to be created
 */
static int32_t sm_vm_to_create = -1;
static struct bst_root sm_vm_tree = BST_ROOT_INITIAL_VALUE;
static struct bst_root sm_vm_free_tree = BST_ROOT_INITIAL_VALUE;
static spin_lock_t sm_vm_lock;
static event_t sm_vm_event =
        EVENT_INITIAL_VALUE(sm_vm_event, 0, EVENT_FLAG_AUTOUNSIGNAL);
static thread_t* sm_vm_notifier_thread;
static atomic_uintptr_t sm_vm_active_notifier;
static event_t sm_vm_notifier_done_event =
        EVENT_INITIAL_VALUE(sm_vm_notifier_done_event,
                            0,
                            EVENT_FLAG_AUTOUNSIGNAL);

/*
 * Placeholder compatibility VM for environments without hypervisors
 * and for the bootloader that may call Trusty before the hypervisor has
 * initialized. This pseudo-VM does not get creation or destruction messages
 * so we add and remove it from the tree manually.
 */
static struct sm_vm sm_vm_compat_vm = {
        .node = BST_NODE_INITIAL_VALUE,
        .state = SM_VM_STATE_FRESH,
        .client_id = 0,
        .notifiers = LIST_INITIAL_VALUE(sm_vm_compat_vm.notifiers),
        .self_ref = OBJ_REF_INITIAL_VALUE(sm_vm_compat_vm.self_ref),
};

static event_t nsirqevent[SMP_MAX_CPUS];
static thread_t* nsirqthreads[SMP_MAX_CPUS];
static thread_t* nsidlethreads[SMP_MAX_CPUS];
static thread_t* stdcallthread;
static bool irq_thread_ready[SMP_MAX_CPUS];
struct sm_std_call_state stdcallstate = {
        .event = EVENT_INITIAL_VALUE(stdcallstate.event, 0, 0),
        .active_cpu = -1,
        .initial_cpu = -1,
        .last_cpu = -1,
};

extern smc32_handler_t sm_stdcall_table[];
extern smc32_handler_t sm_nopcall_table[];
extern smc32_handler_t sm_fastcall_table[];

static long sm_get_stdcall_ret(ext_mem_obj_id_t);

long smc_sm_api_version(struct smc32_args* args) {
    uint32_t api_version = args->params[0];

    spin_lock(&sm_api_version_lock);
    LTRACEF("request api version %d\n", api_version);
    if (api_version > sm_api_version_max) {
        api_version = sm_api_version_max;
    }

    if (api_version < sm_api_version_min) {
        TRACEF("ERROR: Tried to select incompatible api version %d < %d, current version %d\n",
               api_version, sm_api_version_min, sm_api_version);
        api_version = sm_api_version;
    } else {
        /* Update and lock the version to prevent downgrade */
        sm_api_version = api_version;
        sm_api_version_min = api_version;
    }
    spin_unlock(&sm_api_version_lock);

    LTRACEF("return api version %d\n", api_version);
    return api_version;
}

long smc_get_smp_max_cpus(struct smc32_args* args) {
    return SMP_MAX_CPUS;
}

uint32_t sm_get_api_version(void) {
    return sm_api_version;
}

bool sm_check_and_lock_api_version(uint32_t version_wanted) {
    spin_lock_saved_state_t state;

    DEBUG_ASSERT(version_wanted > 0);

    if (sm_api_version_min >= version_wanted) {
        return true;
    }
    if (sm_api_version_max < version_wanted) {
        return false;
    }

    spin_lock_save(&sm_api_version_lock, &state, SPIN_LOCK_FLAG_IRQ_FIQ);
    if (sm_api_version < version_wanted) {
        sm_api_version_max = MIN(sm_api_version_max, version_wanted - 1);
        LTRACEF("max api version set: %d\n", sm_api_version_max);
    } else {
        sm_api_version_min = MAX(sm_api_version_min, version_wanted);
        LTRACEF("min api version set: %d\n", sm_api_version_min);
    }
    DEBUG_ASSERT(sm_api_version_min <= sm_api_version_max);
    DEBUG_ASSERT(sm_api_version >= sm_api_version_min);
    DEBUG_ASSERT(sm_api_version <= sm_api_version_max);

    spin_unlock_restore(&sm_api_version_lock, state, SPIN_LOCK_FLAG_IRQ_FIQ);

    return sm_api_version_min >= version_wanted;
}

static int __NO_RETURN sm_stdcall_loop(void* arg) {
    long ret;
    spin_lock_saved_state_t state;

    while (true) {
        LTRACEF("cpu %d, wait for stdcall\n", arch_curr_cpu_num());
        event_wait(&stdcallstate.event);

        /* Dispatch 'standard call' handler */
        LTRACEF("cpu %d, got stdcall: 0x%x, 0x%x, 0x%x, 0x%x\n",
                arch_curr_cpu_num(), stdcallstate.args.smc_nr,
                stdcallstate.args.params[0], stdcallstate.args.params[1],
                stdcallstate.args.params[2]);
        ret = sm_stdcall_table[SMC_ENTITY(stdcallstate.args.smc_nr)](
                &stdcallstate.args);
        LTRACEF("cpu %d, stdcall(0x%x, 0x%x, 0x%x, 0x%x) returned 0x%lx (%ld)\n",
                arch_curr_cpu_num(), stdcallstate.args.smc_nr,
                stdcallstate.args.params[0], stdcallstate.args.params[1],
                stdcallstate.args.params[2], ret, ret);
        spin_lock_save(&stdcallstate.lock, &state, SPIN_LOCK_FLAG_IRQ);
        stdcallstate.ret = ret;
        stdcallstate.done = true;
        event_unsignal(&stdcallstate.event);
        spin_unlock_restore(&stdcallstate.lock, state, SPIN_LOCK_FLAG_IRQ);
    }
}

/* must be called with irqs disabled */
static long sm_queue_stdcall(struct smc32_args* args) {
    long ret;
    uint cpu = arch_curr_cpu_num();

    spin_lock(&stdcallstate.lock);

    if (stdcallstate.event.signaled || stdcallstate.done) {
        if (args->smc_nr == SMC_SC_RESTART_LAST &&
            stdcallstate.args.client_id != args->client_id) {
            dprintf(CRITICAL,
                    "%s: cpu %d, unexpected restart, "
                    "client %" PRIx64 " != %" PRIx64 "\n",
                    __func__, cpu, stdcallstate.args.client_id,
                    args->client_id);
            ret = SM_ERR_UNEXPECTED_RESTART;
            goto err;
        } else if (args->smc_nr == SMC_SC_RESTART_LAST &&
                   stdcallstate.active_cpu == -1) {
            stdcallstate.restart_count++;
            LTRACEF_LEVEL(3, "cpu %d, restart std call, restart_count %d\n",
                          cpu, stdcallstate.restart_count);
            goto restart_stdcall;
        }
        dprintf(CRITICAL, "%s: cpu %d, std call busy\n", __func__, cpu);
        ret = SM_ERR_BUSY;
        goto err;
    } else {
        if (args->smc_nr == SMC_SC_RESTART_LAST) {
            dprintf(CRITICAL,
                    "%s: cpu %d, unexpected restart, no std call active\n",
                    __func__, arch_curr_cpu_num());
            ret = SM_ERR_UNEXPECTED_RESTART;
            goto err;
        }
    }

    LTRACEF("cpu %d, queue std call 0x%x\n", cpu, args->smc_nr);
    stdcallstate.initial_cpu = cpu;
    stdcallstate.ret = SM_ERR_INTERNAL_FAILURE;
    stdcallstate.args = *args;
    stdcallstate.restart_count = 0;
    event_signal(&stdcallstate.event, false);

restart_stdcall:
    if (!sm_use_ffa) {
        /*
         * On FF-A, we do not keep track of the active CPU since
         * get_stdcall_ret is called by a separate direct message.
         */
        stdcallstate.active_cpu = cpu;
    }
    ret = 0;

err:
    spin_unlock(&stdcallstate.lock);

    return ret;
}

#if LIB_SM_WITH_FFA_LOOP
static long sm_ffa_handle_direct_req(long ret, struct smc_ret18* regs) {
    struct smc32_args args;
    uint16_t client_id = (regs->r1 >> 16) & 0xFFFFU;
    uint cpu = arch_curr_cpu_num();

    switch (regs->r3) {
    case TRUSTY_FFA_MSG_RUN_FASTCALL:
        if (SMC_IS_SMC64(regs->r4)) {
            return SM_ERR_NOT_SUPPORTED;
        }
        if (!SMC_IS_FASTCALL(regs->r4)) {
            dprintf(CRITICAL, "Synchronous message is not a fastcall: %lx\n",
                    regs->r4);
            return SM_ERR_INVALID_PARAMETERS;
        }

        args.smc_nr = regs->r4;
        args.params[0] = regs->r5;
        args.params[1] = regs->r6;
        args.params[2] = regs->r7;
        args.client_id = client_id;
        return sm_fastcall_table[SMC_ENTITY(args.smc_nr)](&args);

    case TRUSTY_FFA_MSG_QUEUE_STDCALL:
        if (SMC_IS_SMC64(regs->r4)) {
            return SM_ERR_NOT_SUPPORTED;
        }
        if (SMC_IS_FASTCALL(regs->r4)) {
            dprintf(CRITICAL, "Asynchronous message is a fastcall: %lx\n",
                    regs->r4);
            return SM_ERR_INVALID_PARAMETERS;
        }

        args.smc_nr = regs->r4;
        args.params[0] = regs->r5;
        args.params[1] = regs->r6;
        args.params[2] = regs->r7;
        args.client_id = client_id;

        ret = sm_queue_stdcall(&args);
        if (!ret) {
            /* Ring the doorbell on the host so it queues a Trusty NOP */
            sm_intc_raise_doorbell_irq();
        }
        return ret;

    case TRUSTY_FFA_MSG_GET_STDCALL_RET:
        return sm_get_stdcall_ret((ext_mem_obj_id_t)client_id);

    case TRUSTY_FFA_MSG_RUN_NOPCALL:
        args.smc_nr = SMC_SC_NOP;
        args.params[0] = regs->r4;
        args.params[1] = regs->r5;
        args.params[2] = regs->r6;
        args.client_id = client_id;

#if !ARM_MERGE_FIQ_IRQ
#error "FF-A libsm requires ARM_MERGE_FIQ_IRQ"
#endif
        ret = sm_nopcall_table[SMC_ENTITY(args.params[0])](&args);
        if (!ret) {
            /* Ring the doorbell on the host so it queues a Trusty NOP */
            sm_intc_raise_doorbell_irq();
        }
        return ret;

    case TRUSTY_FFA_MSG_IS_IDLE:
        return get_current_thread() == nsidlethreads[cpu];

    default:
        dprintf(CRITICAL,
                "Unsupported FF-A message from client %" PRIu16 ": %lx\n",
                client_id, regs->r3);
        return SM_ERR_NOT_SUPPORTED;
    }
}
#endif

static int sm_vm_compare_key(const struct bst_node* a, const void* b) {
    const struct sm_vm* vm = containerof(a, struct sm_vm, node);
    ext_mem_obj_id_t key = *(ext_mem_obj_id_t*)b;

    if (key > vm->client_id) {
        return 1;
    } else if (key < vm->client_id) {
        return -1;
    } else {
        return 0;
    }
}

static int sm_vm_compare(struct bst_node* a, struct bst_node* b) {
    const struct sm_vm* vm_b = containerof(b, struct sm_vm, node);

    return sm_vm_compare_key(a, &vm_b->client_id);
}

static void sm_vm_add_compat_vm_locked(ext_mem_client_id_t client_id) {
    DEBUG_ASSERT(spin_lock_held(&sm_vm_lock));

    if (!bst_is_empty(&sm_vm_tree)) {
        /*
         * There is already a VM in the tree, so we don't need
         * to add the compatibility VM explicitly.
         */
        return;
    }
    if (sm_vm_to_create != -1) {
        /* The tree is empty but we have a pending VM queued up for creation */
        return;
    }

    DEBUG_ASSERT(sm_vm_compat_vm.state == SM_VM_STATE_FRESH ||
                 sm_vm_compat_vm.state == SM_VM_STATE_READY_TO_FREE);

    sm_vm_compat_vm.client_id = client_id;
    obj_init(&sm_vm_compat_vm.refobj, &sm_vm_compat_vm.self_ref);
    if (!bst_insert(&sm_vm_tree, &sm_vm_compat_vm.node, sm_vm_compare)) {
        panic("failed to insert compatibility VM\n");
    }
    sm_vm_compat_vm.state = SM_VM_STATE_AVAILABLE;
}

status_t sm_vm_notifier_register(struct sm_vm_notifier* notif) {
    spin_lock_saved_state_t state;
    struct sm_vm* vm;
    status_t ret;

    if (!notif) {
        return ERR_INVALID_ARGS;
    }
    if (!notif->destroy) {
        return ERR_INVALID_ARGS;
    }

    state = spin_lock_irqsave(&sm_vm_lock);
    sm_vm_add_compat_vm_locked(notif->client_id);

    vm = bst_search_key_type(&sm_vm_tree, &notif->client_id, sm_vm_compare_key,
                             struct sm_vm, node);
    if (!vm) {
        ret = ERR_NOT_FOUND;
    } else if (vm->state == SM_VM_STATE_AVAILABLE) {
        list_add_tail(&vm->notifiers, &notif->node);
        ret = NO_ERROR;
    } else {
        ret = ERR_BAD_STATE;
    }
    spin_unlock_irqrestore(&sm_vm_lock, state);

    return ret;
}

status_t sm_vm_notifier_unregister(struct sm_vm_notifier* notif) {
    spin_lock_saved_state_t state;
    status_t ret = NO_ERROR;
    struct sm_vm* vm;

    if (!notif) {
        return ERR_INVALID_ARGS;
    }

    state = spin_lock_irqsave(&sm_vm_lock);
    /*
     * Check the node with the lock held to avoid
     * it getting removed during the check
     */
    if (!list_in_list(&notif->node)) {
        ret = ERR_NOT_FOUND;
        goto err_notif_not_in_list;
    }
    if ((uintptr_t)notif == atomic_load(&sm_vm_active_notifier)) {
        spin_unlock_irqrestore(&sm_vm_lock, state);

        /* The callback is currently running, wait for it to finish */
        do {
            /*
             * If sm_vm_active_notifier is notif, that means that our
             * notifier is currently running; retry the event_wait
             * until the notifier actually completes in order to avoid
             * leftover wakeups. We use a global variable because the
             * notifier might have been destroyed by the handler by
             * the time it returns.
             */
            event_wait(&sm_vm_notifier_done_event);
        } while ((uintptr_t)notif == atomic_load(&sm_vm_active_notifier));

        /* Nothing else to do here, the notifier is already out of the list */
        return NO_ERROR;
    }

    vm = bst_search_key_type(&sm_vm_tree, &notif->client_id, sm_vm_compare_key,
                             struct sm_vm, node);
    if (!vm) {
        ret = ERR_NOT_FOUND;
        goto err_no_vm;
    }

    list_delete(&notif->node);

err_notif_not_in_list:
err_no_vm:
    spin_unlock_irqrestore(&sm_vm_lock, state);
    return ret;
}

status_t sm_vm_get(ext_mem_obj_id_t client_id,
                   struct obj_ref* ref,
                   struct sm_vm** out_vm) {
    spin_lock_saved_state_t state;
    struct sm_vm* vm;

    if (!ref) {
        return ERR_INVALID_ARGS;
    }
    if (obj_ref_active(ref)) {
        return ERR_INVALID_ARGS;
    }
    if (!out_vm) {
        return ERR_INVALID_ARGS;
    }

    state = spin_lock_irqsave(&sm_vm_lock);
    sm_vm_add_compat_vm_locked(client_id);

    vm = bst_search_key_type(&sm_vm_tree, &client_id, sm_vm_compare_key,
                             struct sm_vm, node);
    if (!vm) {
        spin_unlock_irqrestore(&sm_vm_lock, state);
        return ERR_NOT_FOUND;
    }

    obj_add_ref(&vm->refobj, ref);
    *out_vm = vm;
    spin_unlock_irqrestore(&sm_vm_lock, state);

    return NO_ERROR;
}

void sm_vm_add_ref(struct sm_vm* vm, struct obj_ref* ref) {
    spin_lock_saved_state_t state;

    DEBUG_ASSERT(vm);
    DEBUG_ASSERT(ref);
    DEBUG_ASSERT(!obj_ref_active(ref));

    state = spin_lock_irqsave(&sm_vm_lock);
    obj_add_ref(&vm->refobj, ref);
    spin_unlock_irqrestore(&sm_vm_lock, state);
}

void sm_vm_del_ref(struct sm_vm* vm, struct obj_ref* ref) {
    spin_lock_saved_state_t state;

    DEBUG_ASSERT(vm);
    DEBUG_ASSERT(ref);
    DEBUG_ASSERT(obj_ref_active(ref));

    state = spin_lock_irqsave(&sm_vm_lock);
    obj_del_ref(&vm->refobj, ref, NULL);

    if (obj_has_only_ref(&vm->refobj, &vm->self_ref) &&
        vm->state == SM_VM_STATE_DESTROY_NOTIFYING) {
        /*
         * This is the last reference to the VM and it is getting destroyed,
         * so wake up the notifier thread so it updates the state
         */
        event_signal(&sm_vm_event, false);
    }
    spin_unlock_irqrestore(&sm_vm_lock, state);
}

ext_mem_client_id_t sm_vm_get_id(struct sm_vm* vm) {
    DEBUG_ASSERT(vm);

    return vm->client_id;
}

#if LIB_SM_WITH_FFA_LOOP
static long sm_ffa_handle_framework_msg(struct smc_ret18* regs) {
    uint32_t msg = regs->r2 & FFA_FRAMEWORK_MSG_MASK;
    ext_mem_obj_id_t client_id = regs->r5 & 0xffffU;
    struct sm_vm* vm;
    long ret;
    bool inserted;

    /* TODO: validate receiver */

    switch (msg) {
    case FFA_FRAMEWORK_MSG_VM_CREATED_REQ:
        LTRACEF_LEVEL(1, "Got VM creation message for %" PRIu64 "\n",
                      client_id);

        spin_lock(&sm_vm_lock);
        vm = bst_search_key_type(&sm_vm_tree, &client_id, sm_vm_compare_key,
                                 struct sm_vm, node);
        if (!vm) {
            if (sm_vm_to_create == -1) {
                sm_vm_to_create = client_id;
                event_signal(&sm_vm_event, false);
                sm_intc_raise_doorbell_irq();
            }
            ret = FFA_ERROR_RETRY;
        } else if (vm->state == SM_VM_STATE_FRESH) {
            vm->state = SM_VM_STATE_AVAILABLE;
            ret = 0;
        } else {
            dprintf(CRITICAL, "Duplicate VM creation for %" PRIu64 "\n",
                    client_id);
            ret = FFA_ERROR_INVALID_PARAMETERS;
        }
        spin_unlock(&sm_vm_lock);

        LTRACEF_LEVEL(2, "VM creation returning %ld\n", ret);
        regs->r2 = FFA_FRAMEWORK_MSG_VM_CREATED_RESP | FFA_FRAMEWORK_MSG_FLAG;
        return ret;

    case FFA_FRAMEWORK_MSG_VM_DESTROYED_REQ:
        LTRACEF_LEVEL(1, "Got VM destruction message for %" PRIu64 "\n",
                      client_id);

        spin_lock(&sm_vm_lock);
        vm = bst_search_key_type(&sm_vm_tree, &client_id, sm_vm_compare_key,
                                 struct sm_vm, node);
        if (!vm) {
            ret = FFA_ERROR_INVALID_PARAMETERS;
        } else {
            DEBUG_ASSERT(vm->state != SM_VM_STATE_READY_TO_FREE);

            switch (vm->state) {
            case SM_VM_STATE_FRESH:
                /*
                 * We got a creation request for this VM that we
                 * returned RETRY on, but the hypervisor never retried
                 * the request until we could report a success and now
                 * it's sending us a destruction request for that VM.
                 *
                 * We could start destroying the VM instead, but this
                 * is not correct hypervisor behavior so we are probably
                 * better off returning an error.
                 */
                dprintf(CRITICAL, "Got early VM destroy for %" PRIu64 "\n",
                        client_id);
                ret = FFA_ERROR_INVALID_PARAMETERS;
                break;

            case SM_VM_STATE_AVAILABLE:
                vm->state = SM_VM_STATE_DESTROY_NOTIFYING;
                /*
                 * Signal the thread so it destroys the VM and ring
                 * the doorbell on the host so it queues a Trusty NOP
                 */
                event_signal(&sm_vm_event, false);
                sm_intc_raise_doorbell_irq();
                __FALLTHROUGH;

            case SM_VM_STATE_DESTROY_NOTIFYING:
                ret = FFA_ERROR_RETRY;
                break;

            case SM_VM_STATE_DESTROY_NOTIFIED:
                /* Mark the VM for freeing since we're done with it */
                vm->state = SM_VM_STATE_READY_TO_FREE;
                bst_delete(&sm_vm_tree, &vm->node);
                inserted =
                        bst_insert(&sm_vm_free_tree, &vm->node, sm_vm_compare);
                DEBUG_ASSERT(inserted);
                ret = 0;
                /*
                 * Signal the event so the VM is freed later; we do not
                 * need to ring the doorbell because this is not urgent,
                 * so the freeing can happen whenever Trusty gets cycles next.
                 */
                event_signal(&sm_vm_event, false);
                break;

            default:
                panic("Invalid VM state: %d\n", vm->state);
            }
        }
        spin_unlock(&sm_vm_lock);

        LTRACEF_LEVEL(2, "VM destruction returning %ld\n", ret);
        regs->r2 = FFA_FRAMEWORK_MSG_VM_DESTROYED_RESP | FFA_FRAMEWORK_MSG_FLAG;
        return ret;

    default:
        dprintf(CRITICAL, "Unhandled FF-A framework message: %x\n", msg);
        return FFA_ERROR_NOT_SUPPORTED;
    }
}
#endif

static int __NO_RETURN sm_vm_notifier_loop(void* arg) {
    spin_lock_saved_state_t state;
    struct sm_vm* vm;
    struct sm_vm_notifier* notif;
    status_t ret;

    while (true) {
        event_wait(&sm_vm_event);

        /* Create the new VM if a message came in */
        while (true) {
            int32_t vm_id;
            bool inserted;

            state = spin_lock_irqsave(&sm_vm_lock);
            if (sm_vm_to_create != -1 &&
                sm_vm_compat_vm.state == SM_VM_STATE_AVAILABLE) {
                /* We got an actual VM, tear down the compatibility one */
                sm_vm_compat_vm.state = SM_VM_STATE_DESTROY_NOTIFYING;
                /*
                 * Signal the event so we continue the outer loop because
                 * the remainder of the current iteration will handle the
                 * new NOTIFYING state for the compatibility VM. The event
                 * will be used at the start of the next iteration to get
                 * back here and create the new VM.
                 */
                event_signal(&sm_vm_event, false);
                /* Defer creation of the new VM until compat_vm is gone */
                vm_id = -1;
            } else {
                vm_id = sm_vm_to_create;
            }
            spin_unlock_irqrestore(&sm_vm_lock, state);

            LTRACEF_LEVEL(2, "Creating fresh VM %d\n", vm_id);
            if (vm_id == -1) {
                break;
            }

            vm = calloc(1, sizeof(struct sm_vm));
            if (!vm) {
                dprintf(CRITICAL, "Out of memory for VMs\n");
                continue;
            }

            vm->state = SM_VM_STATE_FRESH;
            vm->client_id = vm_id;
            list_initialize(&vm->notifiers);
            obj_init(&vm->refobj, &vm->self_ref);

            state = spin_lock_irqsave(&sm_vm_lock);
            sm_vm_to_create = -1;
            inserted = bst_insert(&sm_vm_tree, &vm->node, sm_vm_compare);
            spin_unlock_irqrestore(&sm_vm_lock, state);
            DEBUG_ASSERT(inserted);
        }

        /* Destroy all VMs on the free list */
        while (true) {
            state = spin_lock_irqsave(&sm_vm_lock);
            vm = bst_next_type(&sm_vm_free_tree, NULL, struct sm_vm, node);
            if (vm) {
                bst_delete(&sm_vm_free_tree, &vm->node);
            }
            spin_unlock_irqrestore(&sm_vm_lock, state);

            if (!vm) {
                break;
            }

            LTRACEF_LEVEL(2, "Freeing VM %" PRIu64 "\n", vm->client_id);
            DEBUG_ASSERT(vm->state == SM_VM_STATE_READY_TO_FREE);
            obj_del_ref(&vm->refobj, &vm->self_ref, NULL);
            free(vm);
        }

        /* Call the next notifier */
        while (true) {
            state = spin_lock_irqsave(&sm_vm_lock);
            notif = NULL;
            bst_for_every_entry(&sm_vm_tree, vm, struct sm_vm, node) {
                if (vm->state == SM_VM_STATE_DESTROY_NOTIFYING) {
                    if (!list_is_empty(&vm->notifiers)) {
                        notif = list_remove_head_type(
                                &vm->notifiers, struct sm_vm_notifier, node);
                        atomic_store(&sm_vm_active_notifier, (uintptr_t)notif);
                        break;
                    }

                    if (!obj_has_only_ref(&vm->refobj, &vm->self_ref)) {
                        /* There are active references to this VM */
                        continue;
                    }

                    /*
                     * No more notifiers or references, we can mark the VM
                     * as "destroy-notified" and move on to the next one.
                     *
                     * This is thread-safe because only the current thread
                     * runs the notifiers, and no new nodes can be added
                     * while in the SM_VM_STATE_DESTROY_NOTIFYING.
                     */
                    vm->state = SM_VM_STATE_DESTROY_NOTIFIED;

                    if (vm == &sm_vm_compat_vm) {
                        /*
                         * We are done with the compatibility VM,
                         * remove it from the tree permanently.
                         */
                        vm->state = SM_VM_STATE_READY_TO_FREE;
                        bst_delete(&sm_vm_tree, &vm->node);
                        DEBUG_ASSERT(sm_vm_event.signaled);
                    }
                }
            }
            spin_unlock_irqrestore(&sm_vm_lock, state);

            if (!notif) {
                break;
            }

            LTRACEF_LEVEL(2, "Calling VM destroy handler for %" PRIu64 "\n",
                          notif->client_id);
            DEBUG_ASSERT(notif->destroy);
            ret = notif->destroy(notif);
            if (ret) {
                TRACEF("VM destroy handler returned error (%d)\n", ret);
            }
            atomic_store(&sm_vm_active_notifier, 0);
            event_signal(&sm_vm_notifier_done_event, true);
        }
    }
}

#if LIB_SM_WITH_FFA_LOOP
static void sm_ffa_loop(long ret, struct smc32_args* args) {
    struct smc_ret18 regs = {0};
    uint64_t extended_args[ARM_FFA_MSG_EXTENDED_ARGS_COUNT];
    STATIC_ASSERT(sizeof extended_args == sizeof regs.req2_params);
    enum arm_ffa_init_state ffa_init_state = arm_ffa_init_state();

    if (atomic_load(&platform_halted)) {
        regs = arm_ffa_call_error(FFA_ERROR_ABORTED);
    } else if (ffa_init_state == ARM_FFA_INIT_UNINIT) {
        panic("FF-A not initialized before main loop\n");
    } else if (ffa_init_state == ARM_FFA_INIT_FAILED) {
        TRACEF("FF-A failed to initialize, "
               "falling back to legacy SPD SMCs\n");
        sm_use_ffa = false;
        return;
    } else {
        /*
         * Linux will check the shadow priority next and
         * give us more cycles if it's anything other than IDLE
         */
        LTRACEF_LEVEL(5, "Calling FFA_MSG_WAIT (%ld)\n", ret);
        regs = arm_ffa_call_msg_wait();
    }

    while (true) {
        LTRACEF_LEVEL(5, "Incoming FF-A SMC (%lx)\n", regs.r0);
        switch ((uint32_t)regs.r0) {
        case SMC_FC_FFA_MSG_SEND_DIRECT_REQ:
        case SMC_FC64_FFA_MSG_SEND_DIRECT_REQ:
            if (atomic_load(&platform_halted)) {
                /* Return to NS since we have nothing to do */
                regs = arm_ffa_call_error(FFA_ERROR_ABORTED);
                break;
            }
            atomic_store(&sm_ffa_valid_call, true);

            if (regs.r2 & FFA_FRAMEWORK_MSG_FLAG) {
                ret = sm_ffa_handle_framework_msg(&regs);
            } else {
                ret = sm_ffa_handle_direct_req(ret, &regs);
            }

            LTRACEF_LEVEL(5, "Calling FFA_MSG_SEND_DIRECT_RESP (%ld)\n", ret);
            regs = arm_ffa_msg_send_direct_resp(&regs, (ulong)ret, 0, 0, 0, 0);
            break;

        case SMC_FC64_FFA_MSG_SEND_DIRECT_REQ2:
            if (atomic_load(&platform_halted)) {
                regs = arm_ffa_call_error(FFA_ERROR_ABORTED);
                break;
            }

            ret = arm_ffa_handle_direct_req2(&regs);
            /*
             * Whereas sm_ffa_handle_direct_req returns secure monitor error
             * codes, arm_ffa_handle_direct_req2 can fail with Trusty error
             * codes not understood by the caller, e.g., if no handler is found.
             */
            if (ret) {
                dprintf(CRITICAL,
                        "Failed to handle FFA_MSG_SEND_DIRECT_REQ2: %lx\n",
                        ret);
                regs = arm_ffa_call_error(FFA_ERROR_ABORTED);
                break;
            }

            LTRACEF_LEVEL(5, "Calling FFA_MSG_SEND_DIRECT_RESP2 (%ld)\n", ret);
            /*
             * copy req2_params into a fresh buffer `args` since the former can
             * be overwritten at any time by the callee.
             */
            memcpy(extended_args, regs.req2_params, sizeof extended_args);
            regs = arm_ffa_msg_send_direct_resp2(&regs, extended_args);
            break;

        case SMC_FC_FFA_RUN:
            if (atomic_load(&platform_halted)) {
                /* Return to NS since we have nothing to do */
                regs = arm_ffa_call_error(FFA_ERROR_ABORTED);
                break;
            }
            atomic_store(&sm_ffa_valid_call, true);

            args->smc_nr = SMC_SC_NOP;
            args->params[0] = args->params[1] = args->params[2] = 0;
            return;

        case SMC_FC_FFA_INTERRUPT:
            atomic_store(&sm_ffa_valid_call, true);
            sm_intc_fiq_enter();
            /*
             * sm_intc_fiq_enter rings the doorbell,
             * so we do not need to do it again here.
             */
            regs = arm_ffa_call_msg_wait();
            break;

        case SMC_FC_FFA_ERROR:
            if (atomic_load(&platform_halted)) {
                /*
                 * Loop forever if we halted and
                 * got back here from FFA_ERROR_ABORTED,
                 * there is not much else we can do
                 */
                break;
            }
            if ((int32_t)regs.r2 == FFA_ERROR_NOT_SUPPORTED &&
                !atomic_load(&sm_ffa_valid_call)) {
                TRACEF("Using legacy SPD SMCs\n");
                sm_use_ffa = false;
                return;
            }
            panic("Received FFA_ERROR from SPMC: (%lx, %lx)\n", regs.r1,
                  regs.r2);

        case SMC_UNKNOWN:
            if (atomic_load(&sm_ffa_valid_call)) {
                /* We already got a valid FF-A call earlier */
                panic("Received SMC_UNKNOWN from SPMC\n");
            }
            TRACEF("Using legacy SPD SMCs\n");
            sm_use_ffa = false;
            return;

        default:
            dprintf(CRITICAL, "Unhandled FF-A SMC: %lx\n", regs.r0);
            regs = arm_ffa_call_error(FFA_ERROR_NOT_SUPPORTED);
        }
    }
}
#endif

static void sm_sched_nonsecure_fiq_loop(long ret, struct smc32_args* args) {
#if LIB_SM_WITH_FFA_LOOP
    if (sm_use_ffa) {
        sm_ffa_loop(ret, args);
        /* Check again in case we switched to the legacy SPD SMCs */
        if (sm_use_ffa) {
            return;
        }
    }
#endif

    while (true) {
        if (atomic_load(&platform_halted)) {
            ret = SM_ERR_PANIC;
        }
        sm_sched_nonsecure(ret, args);
        if (atomic_load(&platform_halted) &&
            args->smc_nr != (uint32_t)SMC_FC_FIQ_ENTER) {
            continue;
        }
        if (SMC_IS_SMC64(args->smc_nr)) {
            ret = SM_ERR_NOT_SUPPORTED;
            continue;
        }
        if (!SMC_IS_FASTCALL(args->smc_nr)) {
            break;
        }
        ret = sm_fastcall_table[SMC_ENTITY(args->smc_nr)](args);
    }
}

/* must be called with irqs disabled */
static enum handler_return sm_return_and_wait_for_next_stdcall(long ret,
                                                               int cpu) {
    struct smc32_args args = SMC32_ARGS_INITIAL_VALUE(args);

    do {
#if ARCH_HAS_FIQ
        arch_disable_fiqs();
#endif
        sm_sched_nonsecure_fiq_loop(ret, &args);
#if ARCH_HAS_FIQ
        arch_enable_fiqs();
#endif

        /* Allow concurrent SMC_SC_NOP calls on multiple cpus */
        if (args.smc_nr == SMC_SC_NOP) {
            LTRACEF_LEVEL(3, "cpu %d, got nop\n", cpu);
            ret = sm_nopcall_table[SMC_ENTITY(args.params[0])](&args);
        } else {
            DEBUG_ASSERT(!sm_use_ffa);
            ret = sm_queue_stdcall(&args);
        }
    } while (ret);

    return sm_intc_enable_interrupts();
}

static void sm_irq_return_ns(void) {
    long ret;
    int cpu;

    cpu = arch_curr_cpu_num();

    spin_lock(&stdcallstate.lock); /* TODO: remove? */
    LTRACEF_LEVEL(2, "got irq on cpu %d, stdcallcpu %d\n", cpu,
                  stdcallstate.active_cpu);
    if (stdcallstate.active_cpu == cpu) {
        stdcallstate.last_cpu = stdcallstate.active_cpu;
        stdcallstate.active_cpu = -1;
        ret = SM_ERR_INTERRUPTED;
    } else {
        ret = SM_ERR_NOP_INTERRUPTED;
    }
    LTRACEF_LEVEL(2, "got irq on cpu %d, return %ld\n", cpu, ret);
    spin_unlock(&stdcallstate.lock);
    sm_return_and_wait_for_next_stdcall(ret, cpu);
}

static int __NO_RETURN sm_irq_loop(void* arg) {
    int cpu;
    /* cpu that requested this thread, the current cpu could be different */
    int eventcpu = (uintptr_t)arg;

    /*
     * Run this thread with interrupts masked, so we don't reenter the
     * interrupt handler. The interrupt handler for non-secure interrupts
     * returns to this thread with the interrupt still pending.
     */
    arch_disable_ints();
    irq_thread_ready[eventcpu] = true;

    cpu = arch_curr_cpu_num();
    LTRACEF("wait for irqs for cpu %d, on cpu %d\n", eventcpu, cpu);
    while (true) {
        event_wait(&nsirqevent[eventcpu]);
        sm_irq_return_ns();
    }
}

/* must be called with irqs disabled */
static long sm_get_stdcall_ret(ext_mem_obj_id_t client_id) {
    long ret;
    uint cpu = arch_curr_cpu_num();

    spin_lock(&stdcallstate.lock);

    if (!sm_use_ffa && stdcallstate.active_cpu != (int)cpu) {
        dprintf(CRITICAL, "%s: stdcallcpu, a%d != curr-cpu %d, l%d, i%d\n",
                __func__, stdcallstate.active_cpu, cpu, stdcallstate.last_cpu,
                stdcallstate.initial_cpu);
        ret = SM_ERR_INTERNAL_FAILURE;
        goto err;
    }
    if (stdcallstate.args.client_id != client_id) {
        dprintf(CRITICAL, "%s: stdcallcpu, client %" PRIx64 " != %" PRIx64 "\n",
                __func__, stdcallstate.args.client_id, client_id);
        ret = SM_ERR_NOT_ALLOWED;
        goto err;
    }
    stdcallstate.last_cpu = (int)cpu;
    stdcallstate.active_cpu = -1;

    if (stdcallstate.done) {
        stdcallstate.done = false;
        ret = stdcallstate.ret;
        LTRACEF("cpu %d, return stdcall result, %ld, initial cpu %d\n", cpu,
                stdcallstate.ret, stdcallstate.initial_cpu);
    } else {
        if (sm_check_and_lock_api_version(TRUSTY_API_VERSION_SMP))
            ret = SM_ERR_CPU_IDLE; /* ns using smp api */
        else if (stdcallstate.restart_count)
            ret = SM_ERR_BUSY;
        else
            ret = SM_ERR_INTERRUPTED;
        LTRACEF("cpu %d, initial cpu %d, restart_count %d, std call not finished, return %ld\n",
                cpu, stdcallstate.initial_cpu, stdcallstate.restart_count, ret);
    }
err:
    spin_unlock(&stdcallstate.lock);

    return ret;
}

static uint enter_smcall_critical_section(void) {
    /*
     * Disable interrupts so stdcallstate.active_cpu does not
     * change to or from this cpu after checking it in the critical
     * section.
     */
    arch_disable_ints();

    /* Switch to sm-stdcall if sm_queue_stdcall woke it up */
    thread_yield();

    return arch_curr_cpu_num();
}

static void exit_smcall_critical_section(long ret, uint cpu) {
    enum handler_return resched;

    resched = sm_return_and_wait_for_next_stdcall(ret, cpu);
    if (resched == INT_RESCHEDULE)
        thread_preempt();

    /* Re-enable interrupts (needed for SMC_SC_NOP) */
    arch_enable_ints();
}

static int sm_wait_for_smcall(void* arg) {
    int cpu;
    long ret = 0;

    dprintf(ALWAYS, "TEE: cpu %d idle, yield to HEE\n",
            arch_curr_cpu_num());
    LTRACEF("wait for stdcalls, on cpu %d\n", arch_curr_cpu_num());

    while (true) {
        cpu = enter_smcall_critical_section();

        if (cpu == stdcallstate.active_cpu)
            ret = sm_get_stdcall_ret(stdcallstate.args.client_id);
        else
            ret = SM_ERR_NOP_DONE;

        exit_smcall_critical_section(ret, cpu);
    }
}

#if WITH_LIB_SM_MONITOR
/* per-cpu secure monitor initialization */
static void sm_mon_percpu_init(uint level) {
    /* let normal world enable SMP, lock TLB, access CP10/11 */
    __asm__ volatile(
            "mrc	p15, 0, r1, c1, c1, 2	\n"
            "orr	r1, r1, #0xC00		\n"
            "orr	r1, r1, #0x60000	\n"
            "mcr	p15, 0, r1, c1, c1, 2	@ NSACR	\n"
            :
            :
            : "r1");

    __asm__ volatile("mcr	p15, 0, %0, c12, c0, 1	\n"
                     :
                     : "r"(&monitor_vector_table));
}
LK_INIT_HOOK_FLAGS(libsm_mon_perrcpu,
                   sm_mon_percpu_init,
                   LK_INIT_LEVEL_PLATFORM - 3,
                   LK_INIT_FLAG_ALL_CPUS);
#endif

static void sm_init(uint level) {
    status_t err;
    char name[32];

    mutex_acquire(&boot_args_lock);

    /* Map the boot arguments if supplied by the bootloader */
    if (lk_boot_args[1] && lk_boot_args[2]) {
        ulong offset = lk_boot_args[1] & (PAGE_SIZE - 1);
        paddr_t paddr = round_down(lk_boot_args[1], PAGE_SIZE);
        size_t size = round_up(lk_boot_args[2] + offset, PAGE_SIZE);
        void* vptr;

        err = vmm_alloc_physical(vmm_get_kernel_aspace(), "sm", size, &vptr,
                                 PAGE_SIZE_SHIFT, paddr, 0,
                                 ARCH_MMU_FLAG_NS |
                                         ARCH_MMU_FLAG_PERM_NO_EXECUTE |
                                         ARCH_MMU_FLAG_CACHED);
        if (!err) {
            boot_args = (uint8_t*)vptr + offset;
            boot_args_refcnt++;
        } else {
            boot_args = NULL;
            TRACEF("Error mapping boot parameter block: %d\n", err);
        }
    }

    mutex_release(&boot_args_lock);

    for (int cpu = 0; cpu < SMP_MAX_CPUS; cpu++) {
        event_init(&nsirqevent[cpu], false, EVENT_FLAG_AUTOUNSIGNAL);

        snprintf(name, sizeof(name), "irq-ns-switch-%d", cpu);
        nsirqthreads[cpu] =
                thread_create(name, sm_irq_loop, (void*)(uintptr_t)cpu,
                              HIGHEST_PRIORITY, DEFAULT_STACK_SIZE);
        if (!nsirqthreads[cpu]) {
            panic("failed to create irq NS switcher thread for cpu %d!\n", cpu);
        }
        thread_set_pinned_cpu(nsirqthreads[cpu], cpu);
        thread_set_real_time(nsirqthreads[cpu]);
        thread_resume(nsirqthreads[cpu]);

        snprintf(name, sizeof(name), "idle-ns-switch-%d", cpu);
        nsidlethreads[cpu] =
                thread_create(name, sm_wait_for_smcall, NULL,
                              LOWEST_PRIORITY + 1, DEFAULT_STACK_SIZE);
        if (!nsidlethreads[cpu]) {
            panic("failed to create idle NS switcher thread for cpu %d!\n",
                  cpu);
        }
        thread_set_pinned_cpu(nsidlethreads[cpu], cpu);
        thread_set_real_time(nsidlethreads[cpu]);
        /* CPU 0 nsidle is resumed from LAST (libsm_bootargs) after init. */
        if (cpu != 0) {
            thread_resume(nsidlethreads[cpu]);
        }
    }

    stdcallthread = thread_create("sm-stdcall", sm_stdcall_loop, NULL,
                                  LOWEST_PRIORITY + 2, DEFAULT_STACK_SIZE);
    if (!stdcallthread) {
        panic("failed to create sm-stdcall thread!\n");
    }
    thread_set_real_time(stdcallthread);
    thread_resume(stdcallthread);

    sm_vm_notifier_thread =
            thread_create("sm-vm-notifier", sm_vm_notifier_loop, NULL,
                          HIGH_PRIORITY, DEFAULT_STACK_SIZE);
    if (!sm_vm_notifier_thread) {
        panic("failed to create sm-vm-notifier thread!\n");
    }
    thread_resume(sm_vm_notifier_thread);
}

LK_INIT_HOOK(libsm, sm_init, LK_INIT_LEVEL_PLATFORM - 1);

static void sm_init_complete(uint level) {
    (void)level;
    dprintf(ALWAYS, "TEE: sm initialized, %d cpu%s, yield to HEE\n",
            SMP_MAX_CPUS, SMP_MAX_CPUS == 1 ? "" : "s");
}
LK_INIT_HOOK(libsm_complete, sm_init_complete, LK_INIT_LEVEL_LAST);

enum handler_return sm_handle_irq(void) {
    int cpu = arch_curr_cpu_num();
    if (irq_thread_ready[cpu]) {
        event_signal(&nsirqevent[cpu], false);
    } else {
        TRACEF("warning: got ns irq before irq thread is ready\n");
        sm_irq_return_ns();
    }

    return INT_RESCHEDULE;
}

void sm_handle_fiq(void) {
    uint32_t expected_return;
    struct smc32_args args = SMC32_ARGS_INITIAL_VALUE(args);
    DEBUG_ASSERT(!sm_use_ffa);
    if (sm_check_and_lock_api_version(TRUSTY_API_VERSION_RESTART_FIQ)) {
        sm_sched_nonsecure_fiq_loop(SM_ERR_FIQ_INTERRUPTED, &args);
        expected_return = SMC_SC_RESTART_FIQ;
    } else {
        sm_sched_nonsecure_fiq_loop(SM_ERR_INTERRUPTED, &args);
        expected_return = SMC_SC_RESTART_LAST;
    }
    if (args.smc_nr != expected_return) {
        TRACEF("got bad restart smc %x, expected %x\n", args.smc_nr,
               expected_return);
        while (args.smc_nr != expected_return)
            sm_sched_nonsecure_fiq_loop(SM_ERR_INTERLEAVED_SMC, &args);
    }
}

void platform_halt(platform_halt_action suggested_action,
                   platform_halt_reason reason) {
    bool already_halted;
    struct smc32_args args = SMC32_ARGS_INITIAL_VALUE(args);

    arch_disable_ints();
    already_halted = atomic_exchange(&platform_halted, true);
    if (!already_halted) {
        for (int cpu = 0; cpu < SMP_MAX_CPUS; cpu++) {
            if (nsirqthreads[cpu]) {
                event_signal(&nsirqevent[cpu], false);
            }
        }
        dprintf(ALWAYS, "%s %s %s %s %s\n", lk_version.arch, lk_version.platform,
                lk_version.target, lk_version.project, lk_version.buildid);
        dprintf(ALWAYS, "HALT: (reason = %d)\n", reason);
    }

#if ARCH_HAS_FIQ
    arch_disable_fiqs();
#endif
    while (true)
        sm_sched_nonsecure_fiq_loop(SM_ERR_PANIC, &args);
}

status_t sm_get_boot_args(void** boot_argsp, size_t* args_sizep) {
    status_t err = NO_ERROR;

    if (!boot_argsp || !args_sizep)
        return ERR_INVALID_ARGS;

    mutex_acquire(&boot_args_lock);

    if (!boot_args) {
        err = ERR_NOT_CONFIGURED;
        goto unlock;
    }

    boot_args_refcnt++;
    *boot_argsp = boot_args;
    *args_sizep = lk_boot_args[2];
unlock:
    mutex_release(&boot_args_lock);
    return err;
}

static void resume_nsthreads(void) {
    int i;

    for (i = 0; i < SMP_MAX_CPUS; i++) {
        DEBUG_ASSERT(nsirqthreads[i]);
        DEBUG_ASSERT(nsidlethreads[i]);

        thread_resume(nsirqthreads[i]);
        thread_resume(nsidlethreads[i]);
    }
}

void sm_put_boot_args(void) {
    mutex_acquire(&boot_args_lock);

    if (!boot_args) {
        TRACEF("WARNING: caller does not own "
               "a reference to boot parameters\n");
        goto unlock;
    }

    boot_args_refcnt--;
    if (boot_args_refcnt == 0) {
        vmm_free_region(vmm_get_kernel_aspace(), (vaddr_t)boot_args);
        boot_args = NULL;
        resume_nsthreads();
    }
unlock:
    mutex_release(&boot_args_lock);
}

static void sm_release_boot_args(uint level) {
    if (boot_args) {
        sm_put_boot_args();
    } else {
        /* we need to resume the ns-switcher here if
         * the boot loader didn't pass bootargs
         */
        resume_nsthreads();
    }

    if (boot_args)
        TRACEF("WARNING: outstanding reference to boot args"
               "at the end of initialzation!\n");
}

LK_INIT_HOOK(libsm_bootargs, sm_release_boot_args, LK_INIT_LEVEL_LAST);
