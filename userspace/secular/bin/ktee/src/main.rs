// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Userspace Trusty probe: GET_UUID then a yielding NOP on every online CPU.

use std::process::ExitCode;

/// ATF Trusty SPD UUID: 40ee25f0-a2bc-304c-8c4c-a173c57d8af1
/// Packed as SMCCC UUID words (byte 0 in the low-order bits of each word).
const TRUSTY_UUID_WORDS: [u32; 4] = [0xf025ee40, 0x4c30bca2, 0x73a14c8c, 0xf18a7dc5];

fn format_smccc_uuid(w0: u32, w1: u32, w2: u32, w3: u32) -> String {
    let b = [
        w0 as u8,
        (w0 >> 8) as u8,
        (w0 >> 16) as u8,
        (w0 >> 24) as u8,
        w1 as u8,
        (w1 >> 8) as u8,
        (w1 >> 16) as u8,
        (w1 >> 24) as u8,
        w2 as u8,
        (w2 >> 8) as u8,
        (w2 >> 16) as u8,
        (w2 >> 24) as u8,
        w3 as u8,
        (w3 >> 8) as u8,
        (w3 >> 16) as u8,
        (w3 >> 24) as u8,
    ];
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

#[cfg(target_os = "fuchsia")]
mod fuchsia_probe {
    use super::{TRUSTY_UUID_WORDS, format_smccc_uuid};
    use anyhow::{Context as _, Error, format_err};
    use fidl_fuchsia_kernel as fkernel;
    use fuchsia_component::client::connect_to_protocol;

    /// SMCCC Trusted OS UID query (fast, SMC32, entity 63, function 0xFF01).
    const SMC_FC_GET_UUID: u32 = 0xbf00ff01;
    /// Yielding NOP into S-EL1 (entity 60, function 1).
    const SMC_YC_NOP: u32 = 0x3c000001;
    /// lib/sm SMC_SC_LOCKED_NOP returns 0. The old trusty-tee stub returned 'TEE!'.
    const TRUSTY_SEL1_ALIVE: u64 = 0;
    const TRUSTY_SEL1_ALIVE_LEGACY: u64 = 0x5445_4521; // 'TEE!'
    const SMC_UNK: u64 = 0xffff_ffff;

    fn smc_call(smc: &zx::Resource, func_id: u32) -> Result<zx::sys::zx_smc_result_t, zx::Status> {
        let mut params = zx::sys::zx_smc_parameters_t::default();
        params.func_id = func_id;
        let mut result = zx::sys::zx_smc_result_t::default();
        zx::ok(unsafe { zx::sys::zx_smc_call(smc.raw_handle(), &params, &mut result) })?;
        Ok(result)
    }

    fn pin_current_thread_to_cpu(
        profile_resource: &zx::Resource,
        cpu: u32,
    ) -> Result<(), zx::Status> {
        let mut info = unsafe { std::mem::zeroed::<zx::sys::zx_profile_info_t>() };
        info.flags = zx::sys::ZX_PROFILE_INFO_FLAG_CPU_MASK;
        let word = (cpu as usize) / zx::sys::ZX_CPU_SET_BITS_PER_WORD;
        let bit = (cpu as usize) % zx::sys::ZX_CPU_SET_BITS_PER_WORD;
        info.cpu_affinity_mask.mask[word] = 1u64 << bit;

        let mut out = zx::sys::ZX_HANDLE_INVALID;
        zx::ok(unsafe {
            zx::sys::zx_profile_create(profile_resource.raw_handle(), 0, &info, &mut out)
        })?;
        let profile = zx::Profile::from(unsafe { zx::Handle::from_raw_unchecked(out) });
        fuchsia_runtime::with_thread_self(|thread| thread.set_profile(&profile, 0))
    }

    async fn get_smc_resource() -> Result<zx::Resource, Error> {
        let proxy =
            connect_to_protocol::<fkernel::SmcResourceMarker>().context("connect SmcResource")?;
        proxy.get().await.context("SmcResource.Get")
    }

    async fn get_profile_resource() -> Result<zx::Resource, Error> {
        let proxy = connect_to_protocol::<fkernel::ProfileResourceMarker>()
            .context("connect ProfileResource")?;
        proxy.get().await.context("ProfileResource.Get")
    }

    pub async fn probe() -> Result<(), Error> {
        let smc = match get_smc_resource().await {
            Ok(resource) => resource,
            Err(error) => {
                eprintln!("TEE: connect SmcResource: {error:?}");
                println!("TEE: dead (no smc resource)");
                return Err(error);
            }
        };

        let uuid_res = match smc_call(&smc, SMC_FC_GET_UUID) {
            Ok(result) => result,
            Err(status) => {
                eprintln!("TEE: GET_UUID smc: {status}");
                println!("TEE: dead (spd)");
                return Err(format_err!("GET_UUID {status}"));
            }
        };
        if uuid_res.arg0 == SMC_UNK {
            println!("TEE: GET_UUID -> SMC_UNK");
            println!("TEE: dead (spd)");
            return Err(format_err!("GET_UUID SMC_UNK"));
        }

        let w0 = uuid_res.arg0 as u32;
        let w1 = uuid_res.arg1 as u32;
        let w2 = uuid_res.arg2 as u32;
        let w3 = uuid_res.arg3 as u32;
        println!("TEE: spd uuid {}", format_smccc_uuid(w0, w1, w2, w3));
        if [w0, w1, w2, w3] != TRUSTY_UUID_WORDS {
            println!("TEE: dead (unexpected uuid)");
            return Err(format_err!("unexpected uuid"));
        }

        let profile_resource = match get_profile_resource().await {
            Ok(resource) => resource,
            Err(error) => {
                eprintln!("TEE: connect ProfileResource: {error:?}");
                println!("TEE: dead (pin cpu 0)");
                return Err(error);
            }
        };

        let num_cpus = unsafe { zx::sys::zx_system_get_num_cpus() };
        let mut live = 0u32;
        for cpu in 0..num_cpus {
            if let Err(status) = pin_current_thread_to_cpu(&profile_resource, cpu) {
                eprintln!("TEE: pin cpu {cpu}: {status}");
                println!("TEE: dead (pin cpu {cpu})");
                return Err(format_err!("pin cpu {cpu}: {status}"));
            }

            let nop_res = match smc_call(&smc, SMC_YC_NOP) {
                Ok(result) => result,
                Err(status) => {
                    eprintln!("TEE: NOP smc cpu {cpu}: {status}");
                    println!("TEE: dead (S-EL1 cpu {cpu})");
                    return Err(format_err!("NOP cpu {cpu}: {status}"));
                }
            };
            println!("TEE: S-EL1 nop cpu {cpu} -> 0x{:x}", nop_res.arg0);
            if nop_res.arg0 != TRUSTY_SEL1_ALIVE && nop_res.arg0 != TRUSTY_SEL1_ALIVE_LEGACY {
                println!("TEE: dead (S-EL1 cpu {cpu})");
                return Err(format_err!("NOP cpu {cpu} returned 0x{:x}", nop_res.arg0));
            }
            live += 1;
        }

        println!("TEE: alive ({live} cpus)");
        Ok(())
    }
}

#[cfg(target_os = "fuchsia")]
fn main() -> ExitCode {
    let result = fuchsia_async::LocalExecutorBuilder::new()
        .build()
        .run_singlethreaded(fuchsia_probe::probe());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

#[cfg(not(target_os = "fuchsia"))]
fn main() -> ExitCode {
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusty_spd_uuid_string() {
        assert_eq!(
            format_smccc_uuid(
                TRUSTY_UUID_WORDS[0],
                TRUSTY_UUID_WORDS[1],
                TRUSTY_UUID_WORDS[2],
                TRUSTY_UUID_WORDS[3]
            ),
            "40ee25f0-a2bc-304c-8c4c-a173c57d8af1"
        );
    }
}
