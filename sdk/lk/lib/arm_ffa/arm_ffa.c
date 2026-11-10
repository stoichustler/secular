/*
 * Copyright (c) 2019-2020 LK Trusty Authors. All Rights Reserved.
 * Copyright (c) 2022, Arm Limited. All rights reserved.
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

#define LOCAL_TRACE 0

#include <assert.h>
#include <endian.h>
#include <err.h>
#include <interface/arm_ffa/arm_ffa.h>
#include <inttypes.h>
#include <kernel/mutex.h>
#include <kernel/vm.h>
#include <lib/arm_ffa/arm_ffa.h>
#include <lib/smc/smc.h>
#include <lib/trusty/uuid.h>
#include <lk/init.h>
#include <lk/macros.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>

static enum arm_ffa_init_state ffa_init_state = ARM_FFA_INIT_UNINIT;
static uint32_t ffa_version;
static uint16_t ffa_local_id;
static size_t ffa_buf_size;
static void* ffa_tx;
static void* ffa_rx;
static bool supports_ns_bit = false;
static bool supports_rx_release = false;
static bool console_log_is_unsupported;
static mutex_t ffa_rxtx_buffer_lock = MUTEX_INITIAL_VALUE(ffa_rxtx_buffer_lock);
#if ARCH_ARM64
static bool send_direct_req2_is_unsupported;

static struct bst_root arm_ffa_direct_req2_handler_tree =
        BST_ROOT_INITIAL_VALUE;
static spin_lock_t arm_ffa_direct_req2_tree_lock = SPIN_LOCK_INITIAL_VALUE;

/**
 * struct arm_ffa_direct_req2_bst_obj - Binary search tree object for
 * ffa_direct_req2 handler
 * @bst_node: BST node
 * @uuid_lo_hi: Array that holds UUID as two 64 bit words
 *              uuid_lo_hi[0] is what the FFA spec labels "Lo" - bytes [0-7]
 *              uuid_lo_hi[1] is what the FFA spec labels "Hi" - bytes [8-15]
 * @handler: Pointer to FFA_DIRECT_REQ2 handler function
 */
struct arm_ffa_direct_req2_bst_obj {
    struct bst_node bst_node;
    uint64_t uuid_lo_hi[2];
    arm_ffa_direct_req2_handler_t handler;
};
static int arm_ffa_direct_req2_handler_compare(struct bst_node* a,
                                               struct bst_node* b);
#endif

/**
 * uuid_to_le64_pair() - convert uuid_t to (lo, hi)-pair per FFA spec.
 *
 * @uuid_lo_hi: Must be an array large enough to store a pair of 64-bit values.
 *      These output elements are little-endian encoded. Upon function return,
 *      uuid_lo_hi[0] contains what the FFA spec labels "Lo" - bytes [0-7], and
 *      uuid_lo_hi[1] contains what the FFA spec labels "Hi" - bytes [8-15].
 */
static inline void uuid_to_le64_pair(uuid_t uuid_obj,
                                     uint64_t uuid_lo_hi[static 2]) {
    uuid_lo_hi[0] = (((uint64_t)__bswap16(uuid_obj.time_hi_and_version) << 48) |
                     ((uint64_t)__bswap16(uuid_obj.time_mid) << 32) |
                     ((uint64_t)__bswap32(uuid_obj.time_low)));

    for (int i = 0; i < 8; i++) {
        uuid_lo_hi[1] |= ((uint64_t)uuid_obj.clock_seq_and_node[i]) << (i * 8);
    }
}

#if ARCH_ARM64
status_t arm_ffa_register_direct_req2_handler(
        uuid_t uuid,
        arm_ffa_direct_req2_handler_t handler) {
    struct arm_ffa_direct_req2_bst_obj* obj;

    obj = calloc(1, sizeof(*obj));
    if (!obj) {
        LTRACEF("ERROR: not enough memory for direct_req2 handler\n");
        return ERR_NO_MEMORY;
    }

    uuid_to_le64_pair(uuid, obj->uuid_lo_hi);
    obj->handler = handler;

    spin_lock(&arm_ffa_direct_req2_tree_lock);
    if (!bst_insert(&arm_ffa_direct_req2_handler_tree, &obj->bst_node,
                    arm_ffa_direct_req2_handler_compare)) {
        spin_unlock(&arm_ffa_direct_req2_tree_lock);
        free(obj);
        LTRACEF("ERROR: couldn't insert direct_req2 hander into BST\n");
        return ERR_ALREADY_EXISTS;
    } else {
        spin_unlock(&arm_ffa_direct_req2_tree_lock);
        return 0;
    }
}

static int arm_ffa_direct_req2_handler_compare(struct bst_node* a,
                                               struct bst_node* b) {
    struct arm_ffa_direct_req2_bst_obj* obj_a =
            containerof(a, struct arm_ffa_direct_req2_bst_obj, bst_node);
    struct arm_ffa_direct_req2_bst_obj* obj_b =
            containerof(b, struct arm_ffa_direct_req2_bst_obj, bst_node);

    return memcmp(obj_a->uuid_lo_hi, obj_b->uuid_lo_hi,
                  sizeof(obj_a->uuid_lo_hi));
}

status_t arm_ffa_handle_direct_req2(struct smc_ret18* regs) {
    struct arm_ffa_direct_req2_bst_obj search_obj;
    struct arm_ffa_direct_req2_bst_obj* found_obj;
    uint16_t sender_id = (regs->r1 >> 16) & 0xffff;
    search_obj.uuid_lo_hi[0] = regs->r2;
    search_obj.uuid_lo_hi[1] = regs->r3;

    spin_lock(&arm_ffa_direct_req2_tree_lock);
    found_obj = bst_search_type(&arm_ffa_direct_req2_handler_tree, &search_obj,
                                arm_ffa_direct_req2_handler_compare,
                                struct arm_ffa_direct_req2_bst_obj, bst_node);
    spin_unlock(&arm_ffa_direct_req2_tree_lock);

    if (found_obj) {
        return found_obj->handler(sender_id, &regs->r4);
    } else {
        LTRACEF("Error: No handler for UUID 0x%016lx 0x%016lx for sender %d\n",
                regs->r2, regs->r3, sender_id);
        return ERR_NOT_FOUND;
    }
}
#endif

enum arm_ffa_init_state arm_ffa_init_state(void) {
    return ffa_init_state;
}

static status_t arm_ffa_call_id_get(uint16_t* id) {
    struct smc_ret8 smc_ret;

    smc_ret = smc8(SMC_FC_FFA_ID_GET, 0, 0, 0, 0, 0, 0, 0);

    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        if (smc_ret.r2 & ~0xFFFFUL) {
            TRACEF("Unexpected FFA_ID_GET result: %lx\n", smc_ret.r2);
            return ERR_NOT_VALID;
        }
        *id = (uint16_t)(smc_ret.r2 & 0xFFFF);
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        if ((int32_t)smc_ret.r2 == FFA_ERROR_NOT_SUPPORTED) {
            return ERR_NOT_SUPPORTED;
        } else {
            TRACEF("Unexpected FFA_ERROR: %lx\n", smc_ret.r2);
            return ERR_NOT_VALID;
        }

    default:
        TRACEF("Unexpected FFA SMC: %lx\n", smc_ret.r0);
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_call_version(uint16_t major,
                                     uint16_t minor,
                                     uint16_t* major_ret,
                                     uint16_t* minor_ret) {
    struct smc_ret8 smc_ret;

    uint32_t version = FFA_VERSION(major, minor);
    /* Bit 31 must be cleared. */
    ASSERT(!(version >> 31));
    smc_ret = smc8(SMC_FC_FFA_VERSION, version, 0, 0, 0, 0, 0, 0);
    if ((int32_t)smc_ret.r0 == FFA_ERROR_NOT_SUPPORTED) {
        return ERR_NOT_SUPPORTED;
    }
    *major_ret = FFA_VERSION_TO_MAJOR(smc_ret.r0);
    *minor_ret = FFA_VERSION_TO_MINOR(smc_ret.r0);

    return NO_ERROR;
}

/* TODO: When adding support for FFA version 1.1 feature ids should be added. */
static status_t arm_ffa_call_features(ulong id,
                                      bool* is_implemented,
                                      ffa_features2_t* features2,
                                      ffa_features3_t* features3) {
    struct smc_ret8 smc_ret;

    ASSERT(is_implemented);

    /*
     * According to the FF-A spec section "Discovery of NS bit usage",
     * NS_BIT is optionally set by a v1.0 SP such as Trusty, and must
     * be set by a v1.1+ SP. Here, we set it unconditionally for the
     * relevant feature.
     */
    bool request_ns_bit = (id == SMC_FC_FFA_MEM_RETRIEVE_REQ) ||
                          (id == SMC_FC64_FFA_MEM_RETRIEVE_REQ);
    smc_ret = smc8(SMC_FC_FFA_FEATURES, id,
                   request_ns_bit ? FFA_FEATURES2_MEM_RETRIEVE_REQ_NS_BIT : 0,
                   0, 0, 0, 0, 0);

    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        *is_implemented = true;
        if (features2) {
            *features2 = (ffa_features2_t)smc_ret.r2;
        }
        if (features3) {
            *features3 = (ffa_features3_t)smc_ret.r3;
        }
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        if ((int32_t)smc_ret.r2 == FFA_ERROR_NOT_SUPPORTED) {
            *is_implemented = false;
            return NO_ERROR;
        } else {
            TRACEF("Unexpected FFA_ERROR: %lx\n", smc_ret.r2);
            return ERR_NOT_VALID;
        }

    default:
        TRACEF("Unexpected FFA SMC: %lx\n", smc_ret.r0);
        return ERR_NOT_VALID;
    }
}

/*
 * Call with ffa_rxtx_buffer_lock acquired and the ffa_tx buffer already
 * populated with struct ffa_mtd_common. Transmit in a single fragment.
 */
static status_t arm_ffa_call_mem_retrieve_req(uint32_t* total_len,
                                              uint32_t* fragment_len) {
    struct smc_ret8 smc_ret;
    struct ffa_mtd_v1_0* req_v1_0 = ffa_tx;
    struct ffa_mtd_v1_1* req_v1_1 = ffa_tx;
    size_t len;

    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    if (ffa_version < FFA_VERSION(1, 1)) {
        len = offsetof(struct ffa_mtd_v1_0, emad[0]) +
              req_v1_0->emad_count * sizeof(struct ffa_emad);
    } else {
        len = req_v1_1->emad_offset +
              req_v1_1->emad_count * req_v1_1->emad_size;
    }

    smc_ret = smc8(SMC_FC_FFA_MEM_RETRIEVE_REQ, len, len, 0, 0, 0, 0, 0);

    int32_t error;
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_MEM_RETRIEVE_RESP:
        if (total_len) {
            *total_len = (uint32_t)smc_ret.r1;
        }
        if (fragment_len) {
            *fragment_len = (uint32_t)smc_ret.r2;
        }
        return NO_ERROR;
    case SMC_FC_FFA_ERROR:
        error = (int32_t)smc_ret.r2;
        switch (error) {
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_NO_MEMORY:
            return ERR_NO_MEMORY;
        case FFA_ERROR_DENIED:
            return ERR_BAD_STATE;
        case FFA_ERROR_ABORTED:
            return ERR_CANCELLED;
        default:
            TRACEF("Unknown error: 0x%x\n", error);
            return ERR_NOT_VALID;
        }
    default:
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_call_mem_frag_rx(uint64_t handle,
                                         uint32_t offset,
                                         uint32_t* fragment_len) {
    struct smc_ret8 smc_ret;

    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    smc_ret = smc8(SMC_FC_FFA_MEM_FRAG_RX, (uint32_t)handle, handle >> 32,
                   offset, 0, 0, 0, 0);

    /* FRAG_RX is followed by FRAG_TX on successful completion. */
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_MEM_FRAG_TX: {
        uint64_t handle_out =
                (uint32_t)smc_ret.r1 | ((uint64_t)(uint32_t)smc_ret.r2 << 32);
        if (handle != handle_out) {
            TRACEF("Handle for response doesn't match the request, %" PRId64
                   " != %" PRId64,
                   handle, handle_out);
            return ERR_NOT_VALID;
        }
        *fragment_len = smc_ret.r3;
        return NO_ERROR;
    }
    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_ABORTED:
            return ERR_CANCELLED;
        default:
            TRACEF("Unexpected error %d\n", (int32_t)smc_ret.r2);
            return ERR_NOT_VALID;
        }
    default:
        TRACEF("Unexpected function id returned 0x%08lx\n", smc_ret.r0);
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_call_mem_share(size_t num_comp_mrd,
                                       size_t num_cons_mrd,
                                       uint32_t* total_len,
                                       uint32_t* fragment_len,
                                       uint64_t* handle) {
    struct smc_ret8 smc_ret;
    struct ffa_mtd_v1_0* req_v1_0 = ffa_tx;
    struct ffa_mtd_v1_1* req_v1_1 = ffa_tx;
    size_t len;
    int32_t error;

    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    if (ffa_version < FFA_VERSION(1, 1)) {
        len = offsetof(struct ffa_mtd_v1_0, emad[0]) +
              (req_v1_0->emad_count * sizeof(struct ffa_emad)) +
              (num_comp_mrd * sizeof(struct ffa_comp_mrd)) +
              (num_cons_mrd * sizeof(struct ffa_cons_mrd));
    } else {
        len = req_v1_1->emad_offset +
              (req_v1_1->emad_count * req_v1_1->emad_size) +
              (num_comp_mrd * sizeof(struct ffa_comp_mrd)) +
              (num_cons_mrd * sizeof(struct ffa_cons_mrd));
    }

    /* w3 and w4 MBZ since tx buffer is used, the rest SBZ */
    smc_ret = smc8(SMC_FC64_FFA_MEM_SHARE, len, len, 0, 0, 0, 0, 0);
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
        if (total_len) {
            *total_len = (uint32_t)smc_ret.r1;
        }
        if (fragment_len) {
            *fragment_len = (uint32_t)smc_ret.r2;
        }
        if (handle) {
            *handle = (uint32_t)smc_ret.r2;
            *handle |= ((uint64_t)smc_ret.r3) << 32;
        }
        return NO_ERROR;
    case SMC_FC_FFA_ERROR:
        error = (int32_t)smc_ret.r2;
        switch (error) {
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_DENIED:
            return ERR_BAD_STATE;
        case FFA_ERROR_NO_MEMORY:
            return ERR_NO_MEMORY;
        case FFA_ERROR_BUSY:
            return ERR_BUSY;
        case FFA_ERROR_ABORTED:
            return ERR_CANCELLED;
        default:
            TRACEF("Unexpected error: 0x%x\n", error);
            return ERR_NOT_VALID;
        }
    default:
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_call_mem_relinquish(
        uint64_t handle,
        uint32_t flags,
        uint32_t endpoint_count,
        const ffa_endpoint_id16_t* endpoints) {
    struct smc_ret8 smc_ret;
    struct ffa_mem_relinquish_descriptor* req = ffa_tx;

    if (!req) {
        TRACEF("ERROR: no FF-A tx buffer\n");
        return ERR_NOT_CONFIGURED;
    }
    ASSERT(endpoint_count <=
           (ffa_buf_size - sizeof(struct ffa_mem_relinquish_descriptor)) /
                   sizeof(ffa_endpoint_id16_t));

    mutex_acquire(&ffa_rxtx_buffer_lock);

    req->handle = handle;
    req->flags = flags;
    req->endpoint_count = endpoint_count;

    memcpy(req->endpoint_array, endpoints,
           endpoint_count * sizeof(ffa_endpoint_id16_t));

    smc_ret = smc8(SMC_FC_FFA_MEM_RELINQUISH, 0, 0, 0, 0, 0, 0, 0);

    mutex_release(&ffa_rxtx_buffer_lock);

    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_NO_MEMORY:
            return ERR_NO_MEMORY;
        case FFA_ERROR_DENIED:
            return ERR_BAD_STATE;
        case FFA_ERROR_ABORTED:
            return ERR_CANCELLED;
        default:
            TRACEF("Unexpected FFA_ERROR: %lx\n", smc_ret.r2);
            return ERR_NOT_VALID;
        }
    default:
        TRACEF("Unexpected FFA SMC: %lx\n", smc_ret.r0);
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_call_rxtx_map(paddr_t tx_paddr,
                                      paddr_t rx_paddr,
                                      size_t page_count) {
    struct smc_ret8 smc_ret;

    /* Page count specified in bits [0:5] */
    ASSERT(page_count);
    ASSERT(page_count < (1 << 6));

#if ARCH_ARM64
    smc_ret = smc8(SMC_FC64_FFA_RXTX_MAP, tx_paddr, rx_paddr, page_count, 0, 0,
                   0, 0);
#else
    smc_ret = smc8(SMC_FC_FFA_RXTX_MAP, tx_paddr, rx_paddr, page_count, 0, 0, 0,
                   0);
#endif
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_NO_MEMORY:
            return ERR_NO_MEMORY;
        case FFA_ERROR_DENIED:
            return ERR_ALREADY_EXISTS;
        default:
            TRACEF("Unexpected FFA_ERROR: %lx\n", smc_ret.r2);
            return ERR_NOT_VALID;
        }
    default:
        TRACEF("Unexpected FFA SMC: %lx\n", smc_ret.r0);
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_call_rx_release(void) {
    struct smc_ret8 smc_ret;

    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    smc_ret = smc8(SMC_FC_FFA_RX_RELEASE, 0, 0, 0, 0, 0, 0, 0);
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_DENIED:
            return ERR_BAD_STATE;
        default:
            return ERR_NOT_VALID;
        }
    default:
        return ERR_NOT_VALID;
    }
}

#if WITH_SMP
static status_t ffa_call_secondary_ep_register(void) {
    struct smc_ret8 smc_ret;
    paddr_t secondary_ep_paddr;
    extern char _start[];

    secondary_ep_paddr = vaddr_to_paddr(_start);

    smc_ret = smc8(SMC_FC64_FFA_SECONDARY_EP_REGISTER, secondary_ep_paddr, 0, 0,
                   0, 0, 0, 0);
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        default:
            return ERR_NOT_VALID;
        }

    case SMC_UNKNOWN:
        return ERR_NOT_SUPPORTED;

    default:
        return ERR_NOT_VALID;
    }
}
#endif /* WITH_SMP */

struct smc_ret18 arm_ffa_call_error(enum ffa_error err) {
    long target = 0; /* Target must be zero (MBZ) at secure FF-A instances */
    return smc8_ret18(SMC_FC_FFA_ERROR, target, (ulong)err, 0, 0, 0, 0, 0);
}

struct smc_ret18 arm_ffa_call_msg_wait(void) {
    return smc8_ret18(SMC_FC_FFA_MSG_WAIT, 0, 0, 0, 0, 0, 0, 0);
}

struct smc_ret18 arm_ffa_msg_send_direct_resp(
        const struct smc_ret18* direct_req_regs,
        ulong a0,
        ulong a1,
        ulong a2,
        ulong a3,
        ulong a4) {
    uint32_t sender_receiver_id;
    uint32_t flags;

    DEBUG_ASSERT(direct_req_regs);

    /* Copy and flip the sender from the direct message request */
    sender_receiver_id = ((uint32_t)direct_req_regs->r1 >> 16) |
                         ((uint32_t)ffa_local_id << 16);
    /* Copy the flags as well */
    flags = direct_req_regs->r2;

    switch ((uint32_t)direct_req_regs->r0) {
    case SMC_FC_FFA_MSG_SEND_DIRECT_REQ:
        return smc8_ret18(SMC_FC_FFA_MSG_SEND_DIRECT_RESP, sender_receiver_id,
                          flags, a0, a1, a2, a3, a4);
    case SMC_FC64_FFA_MSG_SEND_DIRECT_REQ:
        return smc8_ret18(SMC_FC64_FFA_MSG_SEND_DIRECT_RESP, sender_receiver_id,
                          flags, a0, a1, a2, a3, a4);
    default:
        dprintf(CRITICAL, "Invalid direct request function id %lx\n",
                direct_req_regs->r0);
        return arm_ffa_call_error(FFA_ERROR_INVALID_PARAMETERS);
    }

    __UNREACHABLE;
}

struct smc_ret18 arm_ffa_msg_send_direct_resp2(
        const struct smc_ret18* direct_req_regs,
        uint64_t args[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT]) {
    uint32_t sender_receiver_id;

    DEBUG_ASSERT(direct_req_regs);
    DEBUG_ASSERT(args);
    if ((uint32_t)direct_req_regs->r0 != SMC_FC64_FFA_MSG_SEND_DIRECT_REQ2) {
        dprintf(CRITICAL, "Invalid direct request function id %x\n",
                (uint32_t)direct_req_regs->r0);
        return arm_ffa_call_error(FFA_ERROR_INVALID_PARAMETERS);
    }

    /* Copy and flip the sender from the direct message request */
    sender_receiver_id =
            (direct_req_regs->r1 >> 16) | ((uint32_t)ffa_local_id << 16);

    return smc18(SMC_FC64_FFA_MSG_SEND_DIRECT_RESP2, sender_receiver_id, 0, 0,
                 args[0], args[1], args[2], args[3], args[4], args[5], args[6],
                 args[7], args[8], args[9], args[10], args[11], args[12],
                 args[13]);
}

#if ARCH_ARM64
status_t arm_ffa_msg_send_direct_req2(
        uuid_t uuid,
        uint16_t receiver_id,
        const uint64_t args[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT],
        struct smc_ret18* resp) {
    struct smc_ret18 smc_ret;
    uint64_t uuid_lo_hi[2];
    uint32_t fid = SMC_FC64_FFA_MSG_SEND_DIRECT_REQ2;
    uint32_t sender_receiver_id = ((uint32_t)ffa_local_id << 16) | receiver_id;

    if (send_direct_req2_is_unsupported) {
        return FFA_ERROR_NOT_SUPPORTED;
    }

    if (!args || !resp) {
        return ERR_INVALID_ARGS;
    }

    uuid_to_le64_pair(uuid, uuid_lo_hi);

    smc_ret = smc18(fid, sender_receiver_id, uuid_lo_hi[0], uuid_lo_hi[1],
                    args[0], args[1], args[2], args[3], args[4], args[5],
                    args[6], args[7], args[8], args[9], args[10], args[11],
                    args[12], args[13]);

    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC64_FFA_MSG_SEND_DIRECT_RESP2:
        *resp = smc_ret;
        return NO_ERROR;

    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            send_direct_req2_is_unsupported = true;
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            dprintf(CRITICAL, "Invalid parameters for direct request2\n");
            return ERR_INVALID_ARGS;
        default:
            return ERR_NOT_VALID;
        }

    case SMC_UNKNOWN:
        send_direct_req2_is_unsupported = true;
        return ERR_NOT_SUPPORTED;

    case SMC_FC_FFA_INTERRUPT:
        /*
         * SMC_FC_FFA_INTERRUPT or SMC_FC_FFA_YIELD can be returned per the FF-A
         * spec but it shouldn't happen when Trusty is the receiver of requests.
         */
        panic("Received SMC_FC_FFA_INTERRUPT in response to direct request2");

    case SMC_FC_FFA_YIELD:
        /* See previous case */
        panic("Received SMC_FC_FFA_YIELD in response to direct request2");

    default:
        dprintf(CRITICAL, "Unexpected response (%x) to direct request2\n",
                (uint32_t)smc_ret.r0);
        return ERR_NOT_VALID;
    }
}
#endif

ssize_t arm_ffa_console_log(const char* buf, size_t len) {
    struct smc_ret8 smc_ret;

    if (console_log_is_unsupported) {
        return ERR_NOT_SUPPORTED;
    }
    if (!len) {
        /* Nothing to print, just return */
        return 0;
    }
    if (len != 1) {
        /* TODO: support more than one character */
        len = 1;
    }

    smc_ret = smc8(SMC_FC_FFA_CONSOLE_LOG, len, (ulong)buf[0], 0, 0, 0, 0, 0);
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
    case SMC_FC64_FFA_SUCCESS:
        return len;

    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_NOT_SUPPORTED:
            console_log_is_unsupported = true;
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_RETRY:
            /* FFA_ERROR_RETRY returns how many characters were printed */
            return (uint32_t)smc_ret.r3;
        default:
            return ERR_NOT_VALID;
        }

    case SMC_UNKNOWN:
        console_log_is_unsupported = true;
        return ERR_NOT_SUPPORTED;

    default:
        return ERR_NOT_VALID;
    }
}

static status_t arm_ffa_rx_release_is_implemented(bool* is_implemented) {
    bool is_implemented_val;
    status_t res = arm_ffa_call_features(SMC_FC_FFA_RX_RELEASE,
                                         &is_implemented_val, NULL, NULL);
    if (res != NO_ERROR) {
        TRACEF("Failed to query for feature FFA_RX_RELEASE, err = %d\n", res);
        return res;
    }
    if (is_implemented) {
        *is_implemented = is_implemented_val;
    }
    return NO_ERROR;
}

static status_t arm_ffa_rxtx_map_is_implemented(bool* is_implemented,
                                                size_t* buf_size_log2) {
    ffa_features2_t features2;
    bool is_implemented_val = false;
    status_t res;

    ASSERT(is_implemented);
#if ARCH_ARM64
    res = arm_ffa_call_features(SMC_FC64_FFA_RXTX_MAP, &is_implemented_val,
                                &features2, NULL);
#else
    res = arm_ffa_call_features(SMC_FC_FFA_RXTX_MAP, &is_implemented_val,
                                &features2, NULL);
#endif
    if (res != NO_ERROR) {
        TRACEF("Failed to query for feature FFA_RXTX_MAP, err = %d\n", res);
        return res;
    }
    if (!is_implemented_val) {
        *is_implemented = false;
        return NO_ERROR;
    }
    if (buf_size_log2) {
        ulong buf_size_id = features2 & FFA_FEATURES2_RXTX_MAP_BUF_SIZE_MASK;
        switch (buf_size_id) {
        case FFA_FEATURES2_RXTX_MAP_BUF_SIZE_4K:
            *buf_size_log2 = 12;
            break;
        case FFA_FEATURES2_RXTX_MAP_BUF_SIZE_16K:
            *buf_size_log2 = 14;
            break;
        case FFA_FEATURES2_RXTX_MAP_BUF_SIZE_64K:
            *buf_size_log2 = 16;
            break;
        default:
            TRACEF("Unexpected rxtx buffer size identifier: %lx\n",
                   buf_size_id);
            return ERR_NOT_VALID;
        }
    }

    *is_implemented = true;
    return NO_ERROR;
}

static status_t arm_ffa_mem_retrieve_req_is_implemented(
        bool* is_implemented,
        bool* dyn_alloc_supp,
        bool* has_ns_bit,
        size_t* ref_count_num_bits) {
    ffa_features2_t features2;
    ffa_features3_t features3;
    bool is_implemented_val = false;
    status_t res;

    ASSERT(is_implemented);

    res = arm_ffa_call_features(SMC_FC_FFA_MEM_RETRIEVE_REQ,
                                &is_implemented_val, &features2, &features3);
    if (res != NO_ERROR) {
        TRACEF("Failed to query for feature FFA_MEM_RETRIEVE_REQ, err = %d\n",
               res);
        return res;
    }
    if (!is_implemented_val) {
        *is_implemented = false;
        return NO_ERROR;
    }
    if (dyn_alloc_supp) {
        *dyn_alloc_supp = !!(features2 & FFA_FEATURES2_MEM_DYNAMIC_BUFFER);
    }
    if (has_ns_bit) {
        *has_ns_bit = !!(features2 & FFA_FEATURES2_MEM_RETRIEVE_REQ_NS_BIT);
    }
    if (ref_count_num_bits) {
        *ref_count_num_bits =
                (features3 & FFA_FEATURES3_MEM_RETRIEVE_REQ_REFCOUNT_MASK) + 1;
    }
    *is_implemented = true;
    return NO_ERROR;
}

/* Helper function to set up the tx buffer with standard values
   before calling FFA_MEM_RETRIEVE_REQ. */
static void arm_ffa_populate_receive_req_tx_buffer(uint16_t sender_id,
                                                   uint64_t handle,
                                                   uint64_t tag) {
    struct ffa_mtd_v1_0* req_v1_0 = ffa_tx;
    struct ffa_mtd_v1_1* req_v1_1 = ffa_tx;
    struct ffa_mtd_common* req = ffa_tx;
    struct ffa_emad* emad;
    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    if (ffa_version < FFA_VERSION(1, 1)) {
        memset(req_v1_0, 0, sizeof(struct ffa_mtd_v1_0));
    } else {
        memset(req_v1_1, 0, sizeof(struct ffa_mtd_v1_1));
    }

    req->sender_id = sender_id;
    req->handle = handle;
    /* We must use the same tag as the one used by the sender to retrieve. */
    req->tag = tag;

    if (ffa_version < FFA_VERSION(1, 1)) {
        /*
         * We only support retrieving memory for ourselves for now.
         * TODO: Also support stream endpoints. Possibly more than one.
         */
        req_v1_0->emad_count = 1;
        emad = req_v1_0->emad;
    } else {
        req_v1_1->emad_count = 1;
        req_v1_1->emad_size = sizeof(struct ffa_emad);
        req_v1_1->emad_offset = sizeof(struct ffa_mtd_v1_1);
        emad = (struct ffa_emad*)((uint8_t*)req_v1_1 + req_v1_1->emad_offset);
    }

    memset(emad, 0, sizeof(struct ffa_emad));
    emad[0].mapd.endpoint_id = ffa_local_id;
}

static void arm_ffa_populate_share_tx_buffer(uint16_t receiver_id,
                                             paddr_t buffer,
                                             size_t num_ffa_pages,
                                             uint arch_mmu_flags,
                                             uint64_t tag) {
    struct ffa_mtd_v1_0* req_v1_0 = ffa_tx;
    struct ffa_mtd_v1_1* req_v1_1 = ffa_tx;
    struct ffa_mtd_common* req = ffa_tx;
    struct ffa_emad* emad;
    ffa_mem_attr8_t attributes = 0;
    ffa_mem_perm8_t permissions = 0;
    uint32_t comp_mrd_offset = 0;
    struct ffa_comp_mrd* comp_mrd;

    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    if (ffa_version < FFA_VERSION(1, 1)) {
        memset(req_v1_0, 0, sizeof(struct ffa_mtd_v1_0));
    } else {
        memset(req_v1_1, 0, sizeof(struct ffa_mtd_v1_1));
    }

    req->sender_id = ffa_local_id;

    switch (arch_mmu_flags & ARCH_MMU_FLAG_CACHE_MASK) {
    case ARCH_MMU_FLAG_UNCACHED_DEVICE:
        attributes |= FFA_MEM_ATTR_DEVICE_NGNRE;
        break;
    case ARCH_MMU_FLAG_UNCACHED:
        attributes |= FFA_MEM_ATTR_NORMAL_MEMORY_UNCACHED;
        break;
    case ARCH_MMU_FLAG_CACHED:
        attributes |= FFA_MEM_ATTR_NORMAL_MEMORY_CACHED_WB |
                      FFA_MEM_ATTR_INNER_SHAREABLE;
        break;
    }

    req->memory_region_attributes = attributes;
    req->flags = FFA_MTD_FLAG_TYPE_SHARE_MEMORY;
    /* We must use the same tag as the one used by the receiver to share . */
    req->tag = tag;
    /* MBZ for MEM_SHARE */
    req->handle = 0;

    if (ffa_version < FFA_VERSION(1, 1)) {
        /*
         * We only support retrieving memory for ourselves for now.
         * TODO: Also support stream endpoints. Possibly more than one.
         */
        req_v1_0->emad_count = 1;
        emad = req_v1_0->emad;
    } else {
        req_v1_1->emad_count = 1;
        req_v1_1->emad_size = sizeof(struct ffa_emad);
        req_v1_1->emad_offset = sizeof(struct ffa_mtd_v1_1);
        emad = (struct ffa_emad*)((uint8_t*)req_v1_1 + req_v1_1->emad_offset);
    }

    memset(emad, 0, sizeof(struct ffa_emad));
    emad[0].mapd.endpoint_id = receiver_id;
    permissions = FFA_MEM_PERM_NX;
    if (arch_mmu_flags & ARCH_MMU_FLAG_PERM_RO) {
        permissions |= FFA_MEM_PERM_RO;
    } else {
        permissions |= FFA_MEM_PERM_RW;
    }
    emad[0].mapd.memory_access_permissions = permissions;
    if (ffa_version < FFA_VERSION(1, 1)) {
        /* We only support one emad */
        comp_mrd_offset = sizeof(struct ffa_mtd_v1_0) + sizeof(struct ffa_emad);
    } else {
        comp_mrd_offset = sizeof(struct ffa_mtd_v1_1) + sizeof(struct ffa_emad);
    }
    emad[0].comp_mrd_offset = comp_mrd_offset;

    comp_mrd = (struct ffa_comp_mrd*)((uint8_t*)emad + sizeof(struct ffa_emad));
    comp_mrd->total_page_count = num_ffa_pages;
    comp_mrd->address_range_count = 1;
    comp_mrd->address_range_array[0].address = buffer;
    comp_mrd->address_range_array[0].page_count = num_ffa_pages;
}

/* *desc_buffer is malloc'd and on success passes responsibility to free to
   the caller. Populate the tx buffer before calling. */
static status_t arm_ffa_mem_retrieve(uint16_t sender_id,
                                     uint64_t handle,
                                     uint32_t* len,
                                     uint32_t* fragment_len) {
    status_t res = NO_ERROR;

    DEBUG_ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));
    DEBUG_ASSERT(len);

    uint32_t len_out, fragment_len_out;
    res = arm_ffa_call_mem_retrieve_req(&len_out, &fragment_len_out);
    LTRACEF("total_len: %u, fragment_len: %u\n", len_out, fragment_len_out);
    if (res != NO_ERROR) {
        TRACEF("FF-A memory retrieve request failed, err = %d\n", res);
        return res;
    }
    if (fragment_len_out > len_out) {
        TRACEF("Fragment length larger than total length %u > %u\n",
               fragment_len_out, len_out);
        return ERR_IO;
    }

    /* Check that the first fragment fits in our buffer */
    if (fragment_len_out > ffa_buf_size) {
        TRACEF("Fragment length %u larger than buffer size\n",
               fragment_len_out);
        return ERR_IO;
    }

    if (fragment_len) {
        *fragment_len = fragment_len_out;
    }
    if (len) {
        *len = len_out;
    }

    return NO_ERROR;
}

status_t arm_ffa_mem_address_range_get(struct arm_ffa_mem_frag_info* frag_info,
                                       size_t index,
                                       paddr_t* addr,
                                       size_t* size) {
    uint32_t page_count;
    size_t frag_idx;

    DEBUG_ASSERT(frag_info);

    if (index < frag_info->start_index ||
        index >= frag_info->start_index + frag_info->count) {
        return ERR_OUT_OF_RANGE;
    }

    frag_idx = index - frag_info->start_index;

    page_count = frag_info->address_ranges[frag_idx].page_count;
    LTRACEF("address %p, page_count 0x%x\n",
            (void*)frag_info->address_ranges[frag_idx].address,
            frag_info->address_ranges[frag_idx].page_count);
    if (page_count < 1 || ((size_t)page_count > (SIZE_MAX / FFA_PAGE_SIZE))) {
        TRACEF("bad page count 0x%x at %zu\n", page_count, index);
        return ERR_IO;
    }

    if (addr) {
        *addr = (paddr_t)frag_info->address_ranges[frag_idx].address;
    }
    if (size) {
        *size = page_count * FFA_PAGE_SIZE;
    }

    return NO_ERROR;
}

status_t arm_ffa_mem_retrieve_start(uint16_t sender_id,
                                    uint64_t handle,
                                    uint64_t tag,
                                    uint32_t* address_range_count,
                                    uint* arch_mmu_flags,
                                    struct arm_ffa_mem_frag_info* frag_info) {
    status_t res;
    struct ffa_mtd_v1_0* mtd_v1_0;
    struct ffa_mtd_v1_1* mtd_v1_1;
    struct ffa_mtd_common* mtd;
    struct ffa_emad* emad;
    struct ffa_comp_mrd* comp_mrd;
    uint32_t computed_len;
    uint32_t header_size;

    uint32_t total_len;
    uint32_t fragment_len;

    DEBUG_ASSERT(frag_info);

    mutex_acquire(&ffa_rxtx_buffer_lock);
    arm_ffa_populate_receive_req_tx_buffer(sender_id, handle, tag);
    res = arm_ffa_mem_retrieve(sender_id, handle, &total_len, &fragment_len);

    if (res != NO_ERROR) {
        TRACEF("FF-A memory retrieve failed err=%d\n", res);
        return res;
    }

    mtd = ffa_rx;
    if (ffa_version < FFA_VERSION(1, 1)) {
        if (fragment_len < sizeof(struct ffa_mtd_v1_0)) {
            TRACEF("Fragment too short for memory transaction descriptor\n");
            return ERR_IO;
        }

        mtd_v1_0 = ffa_rx;
        if (fragment_len <
            offsetof(struct ffa_mtd_v1_0, emad) + sizeof(struct ffa_emad)) {
            TRACEF("Fragment too short for endpoint memory access descriptor\n");
            return ERR_IO;
        }
        emad = mtd_v1_0->emad;

        /*
         * We don't retrieve the memory on behalf of anyone else, so we only
         * expect one receiver address range descriptor.
         */
        if (mtd_v1_0->emad_count != 1) {
            TRACEF("unexpected response count %d != 1\n", mtd_v1_0->emad_count);
            return ERR_IO;
        }
    } else {
        if (fragment_len < sizeof(struct ffa_mtd_v1_1)) {
            TRACEF("Fragment too short for memory transaction descriptor\n");
            return ERR_IO;
        }

        mtd_v1_1 = ffa_rx;
        /*
         * We know from the check above that
         *   fragment_len >= sizeof(ffa_mtd_v1) >= sizeof(ffa_emad)
         * so we can rewrite the following
         *   fragment_len < emad_offset + sizeof(ffa_emad)
         * into
         *   fragment_len - sizeof(ffa_emad) < emad_offset
         * to avoid a potential overflow.
         */
        if (fragment_len - sizeof(struct ffa_emad) < mtd_v1_1->emad_offset) {
            TRACEF("Fragment too short for endpoint memory access descriptor\n");
            return ERR_IO;
        }
        if (mtd_v1_1->emad_offset < sizeof(struct ffa_mtd_v1_1)) {
            TRACEF("Endpoint memory access descriptor offset too short\n");
            return ERR_IO;
        }
        if (!IS_ALIGNED(mtd_v1_1->emad_offset, 16)) {
            TRACEF("Endpoint memory access descriptor not aligned to 16 bytes\n");
            return ERR_IO;
        }
        emad = (struct ffa_emad*)((uint8_t*)mtd_v1_1 + mtd_v1_1->emad_offset);

        if (mtd_v1_1->emad_count != 1) {
            TRACEF("unexpected response count %d != 1\n", mtd_v1_1->emad_count);
            return ERR_IO;
        }
    }

    LTRACEF("comp_mrd_offset: %u\n", emad->comp_mrd_offset);
    if (emad->comp_mrd_offset + sizeof(*comp_mrd) > fragment_len) {
        TRACEF("Fragment length %u too short for comp_mrd_offset %u\n",
               fragment_len, emad->comp_mrd_offset);
        return ERR_IO;
    }

    comp_mrd = ffa_rx + emad->comp_mrd_offset;

    uint32_t address_range_count_out = comp_mrd->address_range_count;
    frag_info->address_ranges = comp_mrd->address_range_array;
    LTRACEF("address_range_count: %u\n", address_range_count_out);

    computed_len = emad->comp_mrd_offset +
                   offsetof(struct ffa_comp_mrd, address_range_array) +
                   sizeof(struct ffa_cons_mrd) * comp_mrd->address_range_count;
    if (total_len != computed_len) {
        TRACEF("Reported length %u != computed length %u\n", total_len,
               computed_len);
        return ERR_IO;
    }

    header_size = emad->comp_mrd_offset +
                  offsetof(struct ffa_comp_mrd, address_range_array);
    frag_info->count =
            (fragment_len - header_size) / sizeof(struct ffa_cons_mrd);
    LTRACEF("Descriptors in fragment %u\n", frag_info->count);

    if (frag_info->count * sizeof(struct ffa_cons_mrd) + header_size !=
        fragment_len) {
        TRACEF("fragment length %u, contains partial descriptor\n",
               fragment_len);
        return ERR_IO;
    }

    frag_info->received_len = fragment_len;
    frag_info->start_index = 0;

    uint arch_mmu_flags_out = 0;

    switch (mtd->flags & FFA_MTD_FLAG_TYPE_MASK) {
    case FFA_MTD_FLAG_TYPE_SHARE_MEMORY:
        /*
         * If memory is shared, assume it is not safe to execute out of. This
         * specifically indicates that another party may have access to the
         * memory.
         */
        arch_mmu_flags_out |= ARCH_MMU_FLAG_PERM_NO_EXECUTE;
        break;
    case FFA_MTD_FLAG_TYPE_LEND_MEMORY:
        break;
    case FFA_MTD_FLAG_TYPE_DONATE_MEMORY:
        TRACEF("Unexpected donate memory transaction type is not supported\n");
        return ERR_NOT_IMPLEMENTED;
    default:
        TRACEF("Unknown memory transaction type: 0x%x\n", mtd->flags);
        return ERR_NOT_VALID;
    }

    switch (mtd->memory_region_attributes & ~FFA_MEM_ATTR_NONSECURE) {
    case FFA_MEM_ATTR_DEVICE_NGNRE:
        arch_mmu_flags_out |= ARCH_MMU_FLAG_UNCACHED_DEVICE;
        break;
    case FFA_MEM_ATTR_NORMAL_MEMORY_UNCACHED:
        arch_mmu_flags_out |= ARCH_MMU_FLAG_UNCACHED;
        break;
    case (FFA_MEM_ATTR_NORMAL_MEMORY_CACHED_WB | FFA_MEM_ATTR_INNER_SHAREABLE):
        arch_mmu_flags_out |= ARCH_MMU_FLAG_CACHED;
        break;
    default:
        TRACEF("Invalid memory attributes, 0x%x\n",
               mtd->memory_region_attributes);
        return ERR_NOT_VALID;
    }

    if (!(emad->mapd.memory_access_permissions & FFA_MEM_PERM_RW)) {
        arch_mmu_flags_out |= ARCH_MMU_FLAG_PERM_RO;
    }
    if (emad->mapd.memory_access_permissions & FFA_MEM_PERM_NX) {
        /*
         * Don't allow executable mappings if the stage 2 page tables don't
         * allow it. The hardware allows the stage 2 NX bit to only apply to
         * EL1, not EL0, but neither FF-A nor LK can currently express this, so
         * disallow both if FFA_MEM_PERM_NX is set.
         */
        arch_mmu_flags_out |= ARCH_MMU_FLAG_PERM_NO_EXECUTE;
    }

    if (!supports_ns_bit ||
        (mtd->memory_region_attributes & FFA_MEM_ATTR_NONSECURE)) {
        arch_mmu_flags_out |= ARCH_MMU_FLAG_NS;
        /* Regardless of origin, we don't want to execute out of NS memory. */
        arch_mmu_flags_out |= ARCH_MMU_FLAG_PERM_NO_EXECUTE;
    }

    if (arch_mmu_flags) {
        *arch_mmu_flags = arch_mmu_flags_out;
    }
    if (address_range_count) {
        *address_range_count = address_range_count_out;
    }

    return res;
}

/* This assumes that the fragment is completely composed of memory
   region descriptors (struct ffa_cons_mrd) */
status_t arm_ffa_mem_retrieve_next_frag(
        uint64_t handle,
        struct arm_ffa_mem_frag_info* frag_info) {
    status_t res;
    uint32_t fragment_len;

    mutex_acquire(&ffa_rxtx_buffer_lock);

    res = arm_ffa_call_mem_frag_rx(handle, frag_info->received_len,
                                   &fragment_len);

    if (res != NO_ERROR) {
        TRACEF("Failed to get memory retrieve fragment, err = %d\n", res);
        return res;
    }

    frag_info->received_len += fragment_len;
    frag_info->start_index += frag_info->count;

    frag_info->count = fragment_len / sizeof(struct ffa_cons_mrd);
    if (frag_info->count * sizeof(struct ffa_cons_mrd) != fragment_len) {
        TRACEF("fragment length %u, contains partial descriptor\n",
               fragment_len);
        return ERR_IO;
    }

    frag_info->address_ranges = ffa_rx;

    return NO_ERROR;
}

status_t arm_ffa_mem_share_kernel_buffer(uint16_t receiver_id,
                                         paddr_t buffer,
                                         size_t num_ffa_pages,
                                         uint arch_mmu_flags,
                                         uint64_t* handle) {
    status_t res;
    uint32_t len_out, fragment_len_out;

    DEBUG_ASSERT(handle);

    if (buffer % FFA_PAGE_SIZE) {
        LTRACEF("Buffer address must be page-aligned\n");
        return ERR_INVALID_ARGS;
    }
    if (!(arch_mmu_flags & ARCH_MMU_FLAG_PERM_NO_EXECUTE)) {
        LTRACEF("Only non-executable buffers may be shared over FFA\n");
        return ERR_INVALID_ARGS;
    }

    mutex_acquire(&ffa_rxtx_buffer_lock);

    /* Populate the tx buffer with 1 composite mrd and 1 constituent mrd */
    arm_ffa_populate_share_tx_buffer(receiver_id, buffer, num_ffa_pages,
                                     arch_mmu_flags, 0);
    res = arm_ffa_call_mem_share(1, 1, &len_out, &fragment_len_out, handle);
    LTRACEF("total_len: %u, fragment_len: %u, handle: %" PRIx64 "\n", len_out,
            fragment_len_out, *handle);
    if (res != NO_ERROR) {
        TRACEF("FF-A memory share failed, err= %d\n", res);
    }

    mutex_release(&ffa_rxtx_buffer_lock);
    return res;
}

status_t arm_ffa_mem_reclaim(uint64_t handle) {
    struct smc_ret8 smc_ret;
    uint32_t handle_lo = (uint32_t)handle;
    uint32_t handle_hi = (uint32_t)(handle >> 32);
    uint32_t flags = 0;

    smc_ret = smc8(SMC_FC_FFA_MEM_RECLAIM, handle_lo, handle_hi, flags, 0, 0, 0,
                   0);

    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
        return NO_ERROR;
    case SMC_FC_FFA_ERROR:
        switch ((int32_t)smc_ret.r2) {
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_NO_MEMORY:
            return ERR_NO_MEMORY;
        case FFA_ERROR_DENIED:
            return ERR_BAD_STATE;
        case FFA_ERROR_ABORTED:
            return ERR_CANCELLED;
        default:
            TRACEF("Unexpected FFA_ERROR: %lx\n", smc_ret.r2);
            return ERR_NOT_VALID;
        }
    default:
        TRACEF("Unexpected FFA SMC: %lx\n", smc_ret.r0);
        return ERR_NOT_VALID;
    }
}

status_t arm_ffa_rx_release(void) {
    status_t res;
    ASSERT(is_mutex_held(&ffa_rxtx_buffer_lock));

    if (!supports_rx_release) {
        res = NO_ERROR;
    } else {
        res = arm_ffa_call_rx_release();
    }

    mutex_release(&ffa_rxtx_buffer_lock);

    if (res == ERR_NOT_SUPPORTED) {
        TRACEF("Tried to release rx buffer when the operation is not supported!\n");
    } else if (res != NO_ERROR) {
        TRACEF("Failed to release rx buffer, err = %d\n", res);
        return res;
    }
    return NO_ERROR;
}

status_t arm_ffa_mem_relinquish(uint64_t handle) {
    status_t res;

    /* As flags are set to 0 no request to zero the memory is made */
    res = arm_ffa_call_mem_relinquish(handle, 0, 1, &ffa_local_id);
    if (res != NO_ERROR) {
        TRACEF("Failed to relinquish memory region, err = %d\n", res);
    }

    return res;
}

static status_t arm_ffa_setup(void) {
    status_t res;
    uint16_t ver_major_ret;
    uint16_t ver_minor_ret;
    bool is_implemented;
    size_t buf_size_log2;
    size_t ref_count_num_bits;
    size_t arch_page_count;
    size_t ffa_page_count;
    size_t count;
    paddr_t tx_paddr;
    paddr_t rx_paddr;
    void* tx_vaddr;
    void* rx_vaddr;
    struct list_node page_list = LIST_INITIAL_VALUE(page_list);

    res = arm_ffa_call_version(FFA_CURRENT_VERSION_MAJOR,
                               FFA_CURRENT_VERSION_MINOR, &ver_major_ret,
                               &ver_minor_ret);
    if (res != NO_ERROR) {
        TRACEF("No compatible FF-A version found\n");
        return res;
    } else if (FFA_CURRENT_VERSION_MAJOR != ver_major_ret) {
        /* Allow downgrade within the same major version */
        TRACEF("Incompatible FF-A interface version, %" PRIu16 ".%" PRIu16 "\n",
               ver_major_ret, ver_minor_ret);
        return ERR_NOT_SUPPORTED;
    }

    ffa_version = FFA_VERSION(ver_major_ret, ver_minor_ret);
    if (ffa_version > FFA_CURRENT_VERSION) {
        /* The SPMC supports a newer version, downgrade us */
        ffa_version = FFA_CURRENT_VERSION;
    }
    LTRACEF("Negotiated FF-A version %" PRIu16 ".%" PRIu16 "\n",
            FFA_VERSION_TO_MAJOR(ffa_version),
            FFA_VERSION_TO_MINOR(ffa_version));

    res = arm_ffa_call_id_get(&ffa_local_id);
    if (res != NO_ERROR) {
        TRACEF("Failed to get FF-A partition id (err=%d)\n", res);
        return res;
    }

    res = arm_ffa_rx_release_is_implemented(&is_implemented);
    if (res != NO_ERROR) {
        TRACEF("Error checking if FFA_RX_RELEASE is implemented (err=%d)\n",
               res);
        return res;
    }
    if (is_implemented) {
        supports_rx_release = true;
    } else {
        LTRACEF("FFA_RX_RELEASE is not implemented\n");
    }

    res = arm_ffa_rxtx_map_is_implemented(&is_implemented, &buf_size_log2);
    if (res != NO_ERROR) {
        TRACEF("Error checking if FFA_RXTX_MAP is implemented (err=%d)\n", res);
        return res;
    }
    if (!is_implemented) {
        TRACEF("FFA_RXTX_MAP is not implemented\n");
        return ERR_NOT_SUPPORTED;
    }

    res = arm_ffa_mem_retrieve_req_is_implemented(
            &is_implemented, NULL, &supports_ns_bit, &ref_count_num_bits);
    if (res != NO_ERROR) {
        TRACEF("Error checking if FFA_MEM_RETRIEVE_REQ is implemented (err=%d)\n",
               res);
        return res;
    }
    if (!is_implemented) {
        TRACEF("FFA_MEM_RETRIEVE_REQ is not implemented\n");
    } else if (ref_count_num_bits < 64) {
        /*
         * Expect 64 bit reference count. If we don't have it, future calls to
         * SMC_FC_FFA_MEM_RETRIEVE_REQ can fail if we receive the same handle
         * multiple times. Warn about this, but don't return an error as we only
         * receive each handle once in the typical case.
         */
        TRACEF("Warning FFA_MEM_RETRIEVE_REQ does not have 64 bit reference count (%zu)\n",
               ref_count_num_bits);
    }

    ffa_buf_size = 1U << buf_size_log2;
    ASSERT((ffa_buf_size % FFA_PAGE_SIZE) == 0);

    arch_page_count = DIV_ROUND_UP(ffa_buf_size, PAGE_SIZE);
    ffa_page_count = ffa_buf_size / FFA_PAGE_SIZE;
    count = pmm_alloc_contiguous(arch_page_count, buf_size_log2, &tx_paddr,
                                 &page_list);
    if (count != arch_page_count) {
        TRACEF("Failed to allocate tx buffer %zx!=%zx\n", count,
               arch_page_count);
        res = ERR_NO_MEMORY;
        goto err_alloc_tx;
    }
    tx_vaddr = paddr_to_kvaddr(tx_paddr);
    ASSERT(tx_vaddr);

    count = pmm_alloc_contiguous(arch_page_count, buf_size_log2, &rx_paddr,
                                 &page_list);
    if (count != arch_page_count) {
        TRACEF("Failed to allocate rx buffer %zx!=%zx\n", count,
               arch_page_count);
        res = ERR_NO_MEMORY;
        goto err_alloc_rx;
    }
    rx_vaddr = paddr_to_kvaddr(rx_paddr);
    ASSERT(rx_vaddr);

    res = arm_ffa_call_rxtx_map(tx_paddr, rx_paddr, ffa_page_count);
    if (res != NO_ERROR) {
        TRACEF("Failed to map tx @ %p, rx @ %p, page count 0x%zx (err=%d)\n",
               (void*)tx_paddr, (void*)rx_paddr, ffa_page_count, res);
        goto err_rxtx_map;
    }

    ffa_tx = tx_vaddr;
    ffa_rx = rx_vaddr;

    return res;

err_rxtx_map:
err_alloc_rx:
    pmm_free(&page_list);
err_alloc_tx:
    /* pmm_alloc_contiguous leaves the page list unchanged on failure */

    return res;
}

#if TRUSTY_VM_GUEST
/*
 * If num_desc is zero, return number of descriptors in count_out.
 * If num_desc is non-zero, retrieve descriptors in ffa_rx and return the number
 * of descriptors in count_out.
 *
 * If FF-A version is 1.0, the function must be called with
 * ffa_rxtx_buffer_lock held. For later FF-A versions, the ffa_rxtx_buffer_lock
 * must be held when num_desc is non-zero.
 *
 * If the function returns NO_ERROR, the caller must release the ffa_rx buffer
 * unless num_desc was non-zero and FF-A version is later than 1.0.
 * If an error is returned, the caller must release the ffa_rxtx_buffer_lock if
 * it was acquired prior to the call.
 */
static status_t arm_ffa_call_partition_info_get(uuid_t uuid_obj,
                                                size_t num_desc,
                                                uint32_t flags,
                                                size_t* count_out) {
    int32_t error;
    struct smc_ret18 smc_ret;
    uint64_t uuid[2];

    DEBUG_ASSERT(count_out);
    /* FF-A adds a flag to request count only so the rx buffer isn't acquired */
    DEBUG_ASSERT((flags & FFA_PARTITION_INFO_GET_FLAG_RETURN_COUNT_ONLY) ||
                 is_mutex_held(&ffa_rxtx_buffer_lock));

    uuid_to_le64_pair(uuid_obj, uuid);

    smc_ret = smc18(SMC_FC_FFA_PARTITION_INFO_GET, uuid[0] & 0xFFFFFFFFU,
                    uuid[0] >> 32, uuid[1] & 0xFFFFFFFFU, uuid[1] >> 32, flags,
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    switch ((uint32_t)smc_ret.r0) {
    case SMC_FC_FFA_SUCCESS:
        /* FF-A 1.0 does not include descriptor size in the response */
        if (num_desc > 0 && ffa_version > FFA_VERSION(1, 0)) {
            /* If Bit[0] in flags is 0, r3 holds the size of each descriptor */
            if ((uint32_t)smc_ret.r3 != sizeof(struct ffa_part_info_desc)) {
                panic("Expected descriptor size (%zu) != size in response (%u)\n",
                      sizeof(struct ffa_part_info_desc), (uint32_t)smc_ret.r3);
            }
        }
        *count_out = (uint32_t)smc_ret.r2;
        return NO_ERROR;
    case SMC_FC_FFA_ERROR:
        error = (int32_t)smc_ret.r2;
        switch (error) {
        case FFA_ERROR_BUSY:
            return ERR_BUSY;
        case FFA_ERROR_INVALID_PARAMETERS:
            return ERR_INVALID_ARGS;
        case FFA_ERROR_NO_MEMORY:
            return ERR_NO_MEMORY;
        case FFA_ERROR_DENIED:
            return ERR_BAD_STATE;
        case FFA_ERROR_NOT_SUPPORTED:
            return ERR_NOT_SUPPORTED;
        case FFA_ERROR_NOT_READY:
            return ERR_NOT_READY;
        default:
            TRACEF("Unexpected FFA_ERROR: %x\n", error);
            return ERR_NOT_VALID;
        }
    default:
        return ERR_NOT_VALID;
    }
}

status_t arm_ffa_partition_info_get_count(uuid_t uuid_obj, size_t* count_out) {
    status_t res;
    size_t num_desc = 0;
    uint32_t flags = 0;
    if (ffa_version == FFA_VERSION(1, 0)) {
        /* FF-A version 1.0 acquires the rx buffer */
        mutex_acquire(&ffa_rxtx_buffer_lock);
    } else {
        /* FF-A version 1.1 and later does not need rx buffer to get count */
        flags |= FFA_PARTITION_INFO_GET_FLAG_RETURN_COUNT_ONLY;
    }

    res = arm_ffa_call_partition_info_get(uuid_obj, num_desc, flags, count_out);

    if (ffa_version == FFA_VERSION(1, 0)) {
        if (res != NO_ERROR) {
            TRACEF("Call to PARTITION_INFO_GET failed, err = %d\n", res);
            mutex_release(&ffa_rxtx_buffer_lock);
            return res;
        }
        /* This also releases the rxtx buffer lock */
        arm_ffa_rx_release();
    }

    return res;
}

status_t arm_ffa_partition_info_get_desc(uuid_t uuid_obj,
                                         size_t num_desc,
                                         struct ffa_part_info_desc* desc,
                                         size_t* count_out) {
    status_t res = NO_ERROR;
    size_t count;
    uint32_t flags = 0;

    if (num_desc * sizeof(struct ffa_part_info_desc) > ffa_buf_size) {
        return ERR_TOO_BIG;
    }

    if (!count_out || !desc) {
        return ERR_INVALID_ARGS;
    }

    mutex_acquire(&ffa_rxtx_buffer_lock);

    /* Clear the entire rx buffer */
    memset(ffa_rx, 0, ffa_buf_size);

    res = arm_ffa_call_partition_info_get(uuid_obj, num_desc, flags, &count);
    /* On failure, only release rxtx lock; release buffer and lock otherwise */
    if (res != NO_ERROR) {
        TRACEF("Call to PARTITION_INFO_GET failed, err = %d\n", res);
        mutex_release(&ffa_rxtx_buffer_lock);
        return res;
    }

    /* Do we have enough space to store all descriptors returned by FFA? */
    if (count > num_desc) {
        res = ERR_NOT_ENOUGH_BUFFER;
        goto err_not_enough_buffer;
    }

    *count_out = count; /* count is <= num_desc */
    memcpy(desc, ffa_rx, count * sizeof(struct ffa_part_info_desc));

err_not_enough_buffer:
    /* This also releases the rxtx buffer lock */
    arm_ffa_rx_release();
    return res;
}
#endif

static void arm_ffa_init(uint level) {
    status_t res;

    res = arm_ffa_setup();

    if (res == NO_ERROR) {
        ffa_init_state = ARM_FFA_INIT_SUCCESS;

#if WITH_SMP
        res = ffa_call_secondary_ep_register();
        if (res == ERR_NOT_SUPPORTED) {
            LTRACEF("FFA_SECONDARY_EP_REGISTER is not supported\n");
        } else if (res != NO_ERROR) {
            TRACEF("Failed to register secondary core entry point (err=%d)\n",
                   res);
        }
#endif
    } else {
        TRACEF("Failed to initialize FF-A (err=%d)\n", res);
        ffa_init_state = ARM_FFA_INIT_FAILED;
    }
}

LK_INIT_HOOK(arm_ffa_init, arm_ffa_init, LK_INIT_LEVEL_PLATFORM - 2);
