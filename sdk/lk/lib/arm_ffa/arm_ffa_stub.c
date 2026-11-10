/*
 * ATF SPD=trusty is not an FF-A SPMC. Report init-failed so lib/sm uses
 * the legacy yielding-SMC path.
 */
#include <err.h>
#include <lib/arm_ffa/arm_ffa.h>
#include <string.h>

enum arm_ffa_init_state arm_ffa_init_state(void) {
    return ARM_FFA_INIT_FAILED;
}

status_t arm_ffa_mem_relinquish(uint64_t handle) {
    (void)handle;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_mem_address_range_get(struct arm_ffa_mem_frag_info *buffer,
                                       size_t index, paddr_t *addr, size_t *size) {
    (void)buffer;
    (void)index;
    (void)addr;
    (void)size;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_mem_retrieve_start(uint16_t sender_id, uint64_t handle, uint64_t tag,
                                    uint32_t *address_range_count, uint *arch_mmu_flags,
                                    struct arm_ffa_mem_frag_info *frag_info) {
    (void)sender_id;
    (void)handle;
    (void)tag;
    (void)address_range_count;
    (void)arch_mmu_flags;
    (void)frag_info;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_mem_retrieve_next_frag(uint64_t handle,
                                        struct arm_ffa_mem_frag_info *frag_info) {
    (void)handle;
    (void)frag_info;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_mem_share_kernel_buffer(uint16_t receiver_id, paddr_t buffer,
                                         size_t num_ffa_pages, uint arch_mmu_flags,
                                         uint64_t *handle) {
    (void)receiver_id;
    (void)buffer;
    (void)num_ffa_pages;
    (void)arch_mmu_flags;
    (void)handle;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_mem_reclaim(uint64_t handle) {
    (void)handle;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_rx_release(void) { return ERR_NOT_SUPPORTED; }

struct smc_ret18 arm_ffa_call_error(enum ffa_error err) {
    struct smc_ret18 regs;
    memset(&regs, 0, sizeof(regs));
    (void)err;
    return regs;
}

struct smc_ret18 arm_ffa_call_msg_wait(void) {
    struct smc_ret18 regs;
    memset(&regs, 0, sizeof(regs));
    return regs;
}

struct smc_ret18 arm_ffa_msg_send_direct_resp(const struct smc_ret18 *direct_req_args,
                                              ulong a0, ulong a1, ulong a2, ulong a3,
                                              ulong a4) {
    (void)direct_req_args;
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    return arm_ffa_call_msg_wait();
}

struct smc_ret18 arm_ffa_msg_send_direct_resp2(
        const struct smc_ret18 *direct_req_regs,
        uint64_t args[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT]) {
    (void)direct_req_regs;
    (void)args;
    return arm_ffa_call_msg_wait();
}

status_t arm_ffa_msg_send_direct_req2(uuid_t uuid, uint16_t receiver_id,
                                      const uint64_t args[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT],
                                      struct smc_ret18 *resp) {
    (void)uuid;
    (void)receiver_id;
    (void)args;
    (void)resp;
    return ERR_NOT_SUPPORTED;
}

ssize_t arm_ffa_console_log(const char *buf, size_t len) {
    (void)buf;
    return (ssize_t)len;
}

#if ARCH_ARM64
status_t arm_ffa_handle_direct_req2(struct smc_ret18 *regs) {
    (void)regs;
    return ERR_NOT_SUPPORTED;
}

status_t arm_ffa_direct_req2_register_handler(uuid_t uuid,
                                              arm_ffa_direct_req2_handler_t handler) {
    (void)uuid;
    (void)handler;
    return ERR_NOT_SUPPORTED;
}
#endif
