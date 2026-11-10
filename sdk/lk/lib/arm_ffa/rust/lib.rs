/*
 * Copyright (c) 2025 Google Inc. All rights reserved
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

#![no_std]

use crate::sys::arm_ffa_mem_reclaim;
use crate::sys::arm_ffa_mem_share_kernel_buffer;
use crate::sys::arm_ffa_msg_send_direct_req2;
use crate::sys::arm_ffa_register_direct_req2_handler;
use crate::sys::smc_ret18;

use log::error;
use rust_support::paddr_t;
use rust_support::status_t;
use rust_support::uuid::Uuid;
use rust_support::Error as LkError;
use zerocopy::FromZeros;

pub use crate::sys::arm_ffa_partition_info_get_count;
pub use crate::sys::arm_ffa_partition_info_get_desc;
pub use crate::sys::ffa_part_info_desc as FFAPartInfoDesc;

// bindgen'ed as u32; exposing this as a usize avoids casts in many places
pub const ARM_FFA_MSG_EXTENDED_ARGS_COUNT: usize =
    crate::sys::ARM_FFA_MSG_EXTENDED_ARGS_COUNT as usize;

pub use crate::sys::FFA_PAGE_SIZE;

mod sys {
    #![allow(unused)]
    #![allow(non_camel_case_types)]
    use rust_support::uuid_t;
    include!(env!("BINDGEN_INC_FILE"));
}

pub type EndpointID = u16;

/// An FFA memory handle that has been shared with an endpoint.
///
/// The caller must call `mem_reclaim` on memory handles to avoid leaking memory to other FFA
/// endpoints.
#[derive(Debug)]
pub struct MemoryHandle(u64);

impl MemoryHandle {
    /// Get the value of the FFA memory handle.
    pub fn get(&self) -> u64 {
        // This can be safe because mem_reclaim accepts `Self` instead of `u64` and freeing through
        // C's arm_ffa_mem_reclaim will require unsafe anyway.
        self.0
    }
}

pub struct DirectResp2 {
    pub sender_id: EndpointID,
    pub params: [u64; ARM_FFA_MSG_EXTENDED_ARGS_COUNT],
}

/// Register a callback for a given FFA endpoint service.
///
/// The handler is called every time an FFA_MSG_SEND_DIRECT_REQ2 with the given UUID is received.
/// The handler takes the sender VM ID and the DIRECT_REQ2 params as arguments. The pointee is then
/// returned to the FFA endpoint as the response parameters in FFA_MSG_SEND_DIRECT_RESP2.
///
/// # Safety
///
/// The callback passed to this function is allowed to treat its params pointer argument as a
/// mutable reference to a 14-element u64 array, but must not have any other safety requirements.
pub unsafe fn register_direct_req2_handler(
    endpoint_service: Uuid,
    handler: unsafe extern "C" fn(u16, *mut u64) -> status_t,
) -> Result<(), LkError> {
    // SAFETY: This schedules calling the unsafe handler function at a later point. Registering
    // the handler itself is thread-safe since arm_ffa_register_direct_req2_handler grabs a spinlock
    // before modifying any internal static variables. The safety requirements of calling the
    // callback are delegated to the safety requirements of this function.
    let rc = unsafe { arm_ffa_register_direct_req2_handler(endpoint_service.0, Some(handler)) };
    LkError::from_lk(rc)?;
    Ok(())
}

/// Send an FFA_MSG_SEND_DIRECT_REQ2 request with a given UUID to an FFA endpoint with a given ID.
pub fn msg_send_direct_req2(
    endpoint_uuid: Uuid,
    endpoint_id: EndpointID,
    msg: &[u64; ARM_FFA_MSG_EXTENDED_ARGS_COUNT],
) -> Result<DirectResp2, LkError> {
    let mut resp = smc_ret18::new_zeroed();
    let msg_ptr = msg.as_ptr();
    // SAFETY: Resp points to a mutable smc_ret18. This function blocks and resp is
    // on the stack so it remains valid for the duration of the function call.
    let rc = unsafe {
        arm_ffa_msg_send_direct_req2(endpoint_uuid.0, endpoint_id, msg_ptr, &raw mut resp)
    };
    LkError::from_lk(rc)?;
    // FFA_MSG_SEND_DIRECT_RESP2 puts the endpoint ID for the source (of the response) in the top 16
    // bits of r1. It should match the endpoint_id of the request.
    let sender_id = (resp.r1 >> 16) as u16;
    if endpoint_id != sender_id {
        error!(
            "endpoint IDs mismatch between request ({:?}) and response ({:?})",
            endpoint_id, sender_id
        );
        return Err(LkError::ERR_NOT_VALID);
    }

    // SAFETY: It's valid to access the union in resp as req2_params to create a copy since it was
    // just populated by the call to arm_ffa_msg_send_direct_req2.
    Ok(DirectResp2 { sender_id, params: unsafe { resp.__bindgen_anon_1.req2_params } })
}

/// Share memory with an FFA endpoint specified by receiver id and returns an FFA memory handle.
///
/// # Safety
///
/// The callee must ensure the buffer described by paddr and num_ffa_pages may be shared with other
/// FFA endpoints. Any accesses to that memory after it is shared must be synchronized using some
/// well-defined protocol. The caller must also ensure that flags matches the arch_mmu_flags passed
/// to vmm_alloc_obj when the shared memory was allocated.
pub unsafe fn mem_share_kernel_buffer(
    receiver_id: u16,
    paddr: paddr_t,
    num_ffa_pages: usize,
    flags: u32,
) -> Result<MemoryHandle, LkError> {
    let mut handle = 0;
    // SAFETY: Safety delegated to the caller. See function documentation for details.
    let rc = unsafe {
        arm_ffa_mem_share_kernel_buffer(receiver_id, paddr, num_ffa_pages, flags, &raw mut handle)
    };
    LkError::from_lk(rc)?;
    Ok(MemoryHandle(handle))
}

/// Reclaim memory shared with another FFA endpoint using the FFA memory handle.
///
/// The other FFA endpoints must have relinquished the memory for this to succeed. If this fails it
/// returns the memory handle to allow retrying the reclaim.
pub fn mem_reclaim(handle: MemoryHandle) -> Result<(), (MemoryHandle, LkError)> {
    // SAFETY: Safety delegated to the caller. See function documentation for details.
    let rc = unsafe { arm_ffa_mem_reclaim(handle.get()) };
    LkError::from_lk(rc).map_err(|e| (handle, e))?;
    Ok(())
}
