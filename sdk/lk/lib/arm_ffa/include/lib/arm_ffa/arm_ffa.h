/*
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

#pragma once

#include <arch/ops.h>
#include <interface/arm_ffa/arm_ffa.h>
#include <lib/smc/smc.h>
#include <lib/trusty/uuid.h>
#include <stdbool.h>

/**
 * enum arm_ffa_init_state - The current state of FF-A initialization.
 * @ARM_FFA_INIT_UNINIT: FF-A has not been initialized yet.
 * @ARM_FFA_INIT_SUCCESS: FF-A has been successfully initialized.
 * @ARM_FFA_INIT_FAILED: Failed to initialize FF-A.
 */
enum arm_ffa_init_state {
    ARM_FFA_INIT_UNINIT,
    ARM_FFA_INIT_SUCCESS,
    ARM_FFA_INIT_FAILED,
};

/**
 * arm_ffa_init_state() - Return the current state of FF-A initialization.
 *
 * Return: one of the &enum arm_ffa_init_state values.
 */
enum arm_ffa_init_state arm_ffa_init_state(void);

/**
 * arm_ffa_is_init() - Check whether this module initialized successfully.
 *
 * This should only be called once arm_ffa_init() is guaranteed to have
 * returned.
 *
 * Return: %true in case of success, %false otherwise.
 */
static inline bool arm_ffa_is_init(void) {
    return arm_ffa_init_state() == ARM_FFA_INIT_SUCCESS;
}

/**
 * arm_ffa_mem_relinquish() - Relinquish Trusty's access to a memory region.
 * @handle:        Handle of object to relinquish.
 *
 * Relinquish shared memory object id with SPM/Hypervisor. Allows the sender to
 * reclaim the memory (if it has not been retrieved by anyone else).
 */
status_t arm_ffa_mem_relinquish(uint64_t handle);

struct arm_ffa_cons_mrd;
struct ffa_part_info_desc;

/**
 * struct arm_ffa_mem_frag_info - A fragment of an FF-A shared memory object.
 * @received_len: Length of the fragment.
 * @start_index: Index of the address range array where to start reading.
 * @count: Number of elements in the address range buffer.
 * @address_ranges: The array of address ranges.
 */
struct arm_ffa_mem_frag_info {
    uint32_t received_len;
    size_t start_index;
    uint32_t count;
    struct ffa_cons_mrd* address_ranges;
};

/**
 * arm_ffa_mem_address_range_get() - Gets one address range from the buffer.
 * @buffer: Buffer that describes a part of an FF-A shared memory object.
 * @index: The index of the address range to retrieve.
 * @addr: [out] Start of the retrieved address range.
 * @size: [out] Size of the retrieved address range.
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_mem_address_range_get(struct arm_ffa_mem_frag_info* buffer,
                                       size_t index,
                                       paddr_t* addr,
                                       size_t* size);
/**
 * arm_ffa_mem_retrieve_start() - Retrieve a memory region from the
 *                                SPMC/hypervisor for access from Trusty.
 * @sender_id: Id of the memory owner.
 * @handle: The handle identifying the memory region in the transaction.
 * @tag: The tag identifying the transaction.
 * @address_range_count: [out] The number of address ranges retrieved.
 * @arch_mmu_flags: [out] The MMU flags of the received memory.
 * @frag_info: [out] The shared memory object fragment.
 *
 * Only expects one descriptor in the returned memory access descriptor array,
 * since we don't retrieve memory on behalf of anyone else.
 *
 * Grabs RXTX buffer lock. The lock must be subsequently released through
 * `arm_ffa_rx_release()`.
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_mem_retrieve_start(uint16_t sender_id,
                                    uint64_t handle,
                                    uint64_t tag,
                                    uint32_t* address_range_count,
                                    uint* arch_mmu_flags,
                                    struct arm_ffa_mem_frag_info* frag_info);
/**
 * arm_ffa_mem_retrieve_next_frag() - Performs an FF-A call to retrieve the
 *                                    next fragment of a shared memory object.
 * @handle: The handle of the FF-A memory object to retrieve.
 * @frag_info: [out] the retrieved fragment of the memory object.
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_mem_retrieve_next_frag(
        uint64_t handle,
        struct arm_ffa_mem_frag_info* frag_info);

/**
 * arm_ffa_mem_share_kernel_buffer() - Share kernel buffer over FFA.
 *
 * @receiver_id:    Id of the memory receiver.
 * @buffer:         The start of the buffer. The address must be aligned to
 *                  FFA_PAGE_SIZE.
 * @num_ffa_pages:  Number of pages in the buffer. This uses FFA_PAGE_SIZE which
 *                  may differ from Trusty's page size.
 * @arch_mmu_flags: MMU flags used when allocating the buffer. Buffers must have
 *                  the NO_EXECUTE bit.
 * @handle:         [out] The handle identifying the memory region in the
 *                  transaction.
 *
 * Grabs and releases the RXTX buffer lock.
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_mem_share_kernel_buffer(uint16_t receiver_id,
                                         paddr_t buffer,
                                         size_t num_ffa_pages,
                                         uint arch_mmu_flags,
                                         uint64_t* handle);

/**
 * arm_ffa_mem_reclaim() - Reclaim memory shared over FFA.
 *
 * @handle: The handle identifying the previously shared memory region. This
 *          must have been the result of a call to
 *          arm_ffa_mem_share_kernel_buffer().
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_mem_reclaim(uint64_t handle);

/**
 * arm_ffa_rx_release() - Relinquish ownership of the RX buffer.
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_rx_release(void);

/**
 * arm_ffa_call_error() - Report an error.
 *
 * @err: FF-A error code from &enum ffa_error.
 *
 * Return: the values of the CPU registers on return to Trusty.
 */
struct smc_ret18 arm_ffa_call_error(enum ffa_error err);

/**
 * arm_ffa_call_msg_wait() - Invoke FFA_MSG_WAIT.
 *
 * Return: the values of the CPU registers on return to Trusty.
 */
struct smc_ret18 arm_ffa_call_msg_wait(void);

/**
 * arm_ffa_msg_send_direct_resp() - Send a direct message response.
 *
 * @direct_req_args: The registers passed to the correspoding direct request
 *                   message. Must not be %NULL.
 * @a0: The 1st argument returned as the response.
 * @a1: The 2nd argument returned as the response.
 * @a2: The 3rd argument returned as the response.
 * @a3: The 4th argument returned as the response.
 * @a4: The 5th argument returned as the response.
 *
 * Return: the values of the CPU registers on return to Trusty.
 */
struct smc_ret18 arm_ffa_msg_send_direct_resp(
        const struct smc_ret18* direct_req_regs,
        ulong a0,
        ulong a1,
        ulong a2,
        ulong a3,
        ulong a4);

/**
 * arm_ffa_msg_send_direct_req2() - Send a direct message request.
 *
 * @uuid: Handler UUID.
 * @receiver_id: Receiver ID.
 * @args: Contents of message request - x4-x17. Must not be %NULL.
 * @resp: The registers passed back in response to the direct message iff
 *        the request was successful. Must not be %NULL.
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_msg_send_direct_req2(
        uuid_t uuid,
        uint16_t receiver_id,
        const uint64_t args[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT],
        struct smc_ret18* resp);

/**
 * arm_ffa_msg_send_direct_resp2() - Send a direct message response.
 *
 * @direct_req_regs: The registers passed back in response to the direct message
 *        iff the request was successful. Must not be %NULL.
 * @args: Contents of message response - x4-x17. Must not be %NULL.
 *
 * Return: the values of the CPU registers on return to Trusty.
 */
struct smc_ret18 arm_ffa_msg_send_direct_resp2(
        const struct smc_ret18* direct_req_regs,
        uint64_t args[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT]);

/**
 * arm_ffa_console_log() - Output a buffer using %FFA_CONSOLE_LOG.
 *
 * @buf: The buffer to print.
 * @len: The length of the buffer to print.
 *
 * Return: the number of characters successfully printed, or an error code.
 */
ssize_t arm_ffa_console_log(const char* buf, size_t len);

#if ARCH_ARM64
/**
 * arm_ffa_direct_req2_handler_t - Handler function for DIRECT_REQ2 calls
 *
 * @sender_id: Sender's endpoint ID
 * @regs: Contents of message - x4-x17
 *
 * Return: 0 on success, LK error code on failure.
 */
typedef status_t (*arm_ffa_direct_req2_handler_t)(
        uint16_t sender_id,
        uint64_t regs[static ARM_FFA_MSG_EXTENDED_ARGS_COUNT]);

/**
 * arm_ffa_handle_direct_req2() - Handle DIRECT_REQ2 call
 *
 * @regs: CPU registers for FFA call
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_handle_direct_req2(struct smc_ret18* regs);

/**
 * arm_ffa_register_direct_req2_handler() - Register DIRECT_REQ2 call handler
 *
 * @uuid: UUID of handler to register
 * @handler: pointer to handler function
 *
 * Return: 0 on success, LK error code on failure.
 */
status_t arm_ffa_register_direct_req2_handler(
        uuid_t uuid,
        arm_ffa_direct_req2_handler_t handler);

/**
 * arm_ffa_partition_info_get_count() - Get number of partitions for UUID.
 *
 * It is necessary to know how many partitions to allocate buffer for before
 * calling arm_ffa_partition_info_get_desc(). This function helps with that.
 *
 * @uuid_obj: The UUID specifying the partition.
 * @count: [out] A pointer to return the number of partition info descriptors.
 *         Must not be %NULL.
 *
 * Return: 0 on success, a negative LK error code on failure.
 */
status_t arm_ffa_partition_info_get_count(uuid_t uuid_obj, size_t* count_out);

/**
 * arm_ffa_partition_info_get_desc() - Get partition descriptors for UUID.
 *
 * @uuid_obj: The UUID specifying the partition.
 * @num_desc: The maximal number of descriptors that @desc can hold.
 * @desc: [out] A pointer to a buffer large enough to hold *@num_desc
 *        partition info descriptors. Must not be %NULL.
 * @count_out: [out] A pointer to return the number of partition info
 *             descriptors copied into @desc. Must not be %NULL. Upon
 *             success, *@count_out will be between zero and @num_desc.
 *
 * Callers should first call arm_ffa_partition_info_get_count() to obtain the
 * number of descriptors to allocate space for in @desc. The descriptor capacity
 * should then be passed in @num_desc.
 *
 * Return:
 * * %0                       - on success, a negative LK error code on failure.
 * * %ERR_NOT_ENOUGH_BUFFER   - if @num_desc is too small to store the response
 * * %ERR_INVALID_ARGS        - if @desc or @count_out are %NULL
 * * a negative LK error code - on any other failure.
 */
status_t arm_ffa_partition_info_get_desc(uuid_t uuid_obj,
                                         size_t num_desc,
                                         struct ffa_part_info_desc* desc,
                                         size_t* count_out);

#endif
