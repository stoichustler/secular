// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Probe Trusty SPD and S-EL1, then print LK identity.
//!
//! GET_UUID is answered in EL3. Version, API, and SMP-max are Trusty
//! lib/sm fastcalls into S-EL1. NOP is a yielding call on every online
//! CPU. No shared memory, no TIPC, no TA.

use std::io::{self, Write};
use std::process::ExitCode;

const USAGE: &str = "Usage: lktee";

/// ATF Trusty SPD UUID: 40ee25f0-a2bc-304c-8c4c-a173c57d8af1
/// Packed as SMCCC UUID words (byte 0 in the low-order bits of each word).
const TRUSTY_UUID_WORDS: [u32; 4] = [0xf025ee40, 0x4c30bca2, 0x73a14c8c, 0xf18a7dc5];

/// SMCCC Trusted OS UID query (fast, SMC32, entity 63, function 0xFF01).
const SMC_FC_GET_UUID: u32 = 0xbf00ff01;
/// Yielding NOP into S-EL1 (entity 60, function 1).
const SMC_YC_NOP: u32 = 0x3c000001;
/// lib/sm SMC_FC_GET_VERSION_STR (entity 60, function 10).
const SMC_FC_GET_VERSION_STR: u32 = 0xbc00000a;
/// lib/sm SMC_FC_API_VERSION (entity 60, function 11).
const SMC_FC_API_VERSION: u32 = 0xbc00000b;
/// lib/sm SMC_FC_GET_SMP_MAX_CPUS (entity 60, function 13).
const SMC_FC_GET_SMP_MAX_CPUS: u32 = 0xbc00000d;

/// Request CURRENT (5). ATF SPD=trusty is not FF-A, so Trusty caps at 4.
const TRUSTY_API_VERSION_CURRENT: u64 = 5;
/// `smc_get_version_str(-1)` returns the buildid length.
const VERSION_STR_LEN_INDEX: u64 = 0xffff_ffff;
const VERSION_STR_MAX: u64 = 256;

/// lib/sm SMC_SC_LOCKED_NOP returns 0. The old trusty-tee stub returned 'TEE!'.
const TRUSTY_SEL1_ALIVE: u64 = 0;
const TRUSTY_SEL1_ALIVE_LEGACY: u64 = 0x5445_4521; // 'TEE!'
const SMC_UNK: u64 = 0xffff_ffff;

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
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

fn format_api_version(version: u64) -> String {
    let name = match version {
        1 => "restart-fiq",
        2 => "smp",
        3 => "smp-nop",
        4 => "phys-mem-obj",
        5 => "mem-obj",
        _ => return version.to_string(),
    };
    format!("{version} ({name})")
}

/// LK stores a compact stamp from `lib/version/buildid.sh` (five base-36
/// chars: year since 2011, month, day, hour, minute/2, optional `_LOCAL`).
/// GET_VERSION_STR returns that stamp; `lktee` prints `YYYY-MM-DD HH:MMZ`.
fn from_b36(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        'A'..='Z' => Some(10 + c as u32 - 'A' as u32),
        _ => None,
    }
}

fn unofficial_suffix(buildid: &str) -> (&str, bool) {
    match buildid.strip_suffix("_LOCAL") {
        Some(code) => (code, true),
        None => (buildid, false),
    }
}

fn with_local_tag(text: &str, unofficial: bool) -> String {
    if unofficial { format!("{text} local") } else { text.to_string() }
}

fn decode_compact_buildid(code: &str) -> Option<String> {
    let mut chars = code.chars();
    let year_ch = chars.next()?;
    let month_ch = chars.next()?;
    let day_ch = chars.next()?;
    let hour_ch = chars.next()?;
    let min_ch = chars.next()?;
    if chars.next().is_some() || !year_ch.is_ascii_uppercase() {
        return None;
    }
    let year = 2011 + (year_ch as u32 - 'A' as u32);
    let month = from_b36(month_ch)?;
    let day = from_b36(day_ch)?;
    let hour = from_b36(hour_ch)?;
    let minute = from_b36(min_ch)?.checked_mul(2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 58 {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}Z"))
}

fn format_lk_buildid(raw: &str) -> String {
    let (code, unofficial) = unofficial_suffix(raw);
    if let Some(decoded) = decode_compact_buildid(code) {
        return with_local_tag(&decoded, unofficial);
    }
    raw.to_string()
}

fn format_version_line(raw: &str) -> String {
    format!("TEE: version {}", format_lk_buildid(raw))
}

fn format_alive(live: u32, smp_max: Option<u64>) -> String {
    match smp_max {
        Some(max) => format!("TEE: alive ({live}/{max})"),
        None => format!("TEE: alive ({live}/?)"),
    }
}

fn smc_return_ok(arg0: u64) -> Option<u64> {
    if arg0 == SMC_UNK {
        return None;
    }
    if (arg0 as i64) < 0 {
        return None;
    }
    Some(arg0)
}

/// Parses operands after `argv[0]`. `lktee` takes no options or files.
fn parse_args(args: &[String]) -> Result<(), String> {
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            let flag = arg.chars().nth(1).unwrap_or('-');
            return Err(format!("lktee: invalid option -- '{flag}'\n{USAGE}"));
        }
        return Err(format!("lktee: extra operand '{arg}'\n{USAGE}"));
    }
    Ok(())
}

#[cfg(target_os = "fuchsia")]
mod fuchsia_probe {
    use super::{
        SMC_FC_API_VERSION, SMC_FC_GET_SMP_MAX_CPUS, SMC_FC_GET_UUID, SMC_FC_GET_VERSION_STR,
        SMC_UNK, SMC_YC_NOP, TRUSTY_API_VERSION_CURRENT, TRUSTY_SEL1_ALIVE,
        TRUSTY_SEL1_ALIVE_LEGACY, TRUSTY_UUID_WORDS, VERSION_STR_LEN_INDEX, VERSION_STR_MAX,
        format_alive, format_api_version, format_smccc_uuid, format_version_line, smc_return_ok,
    };
    use fidl_fuchsia_kernel as fkernel;
    use fuchsia_component::client::connect_to_protocol;

    fn smc_call(
        smc: &zx::Resource,
        func_id: u32,
        arg1: u64,
    ) -> Result<zx::sys::zx_smc_result_t, zx::Status> {
        let mut params = zx::sys::zx_smc_parameters_t::default();
        params.func_id = func_id;
        params.arg1 = arg1;
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

    async fn get_smc_resource() -> Result<zx::Resource, String> {
        let proxy = connect_to_protocol::<fkernel::SmcResourceMarker>()
            .map_err(|err| format!("TEE: connect SmcResource: {err:?}"))?;
        proxy.get().await.map_err(|err| format!("TEE: SmcResource.Get: {err:?}"))
    }

    async fn get_profile_resource() -> Result<zx::Resource, String> {
        let proxy = connect_to_protocol::<fkernel::ProfileResourceMarker>()
            .map_err(|err| format!("TEE: connect ProfileResource: {err:?}"))?;
        proxy.get().await.map_err(|err| format!("TEE: ProfileResource.Get: {err:?}"))
    }

    fn print_version_str(smc: &zx::Resource) {
        let len_res = match smc_call(smc, SMC_FC_GET_VERSION_STR, VERSION_STR_LEN_INDEX) {
            Ok(result) => result,
            Err(status) => {
                println!("TEE: version unavailable ({status})");
                return;
            }
        };
        let Some(len) = smc_return_ok(len_res.arg0) else {
            println!("TEE: version unavailable (0x{:x})", len_res.arg0);
            return;
        };
        if len > VERSION_STR_MAX {
            println!("TEE: version unavailable (len {len})");
            return;
        }
        let mut bytes = Vec::with_capacity(len as usize);
        for index in 0..len {
            let ch_res = match smc_call(smc, SMC_FC_GET_VERSION_STR, index) {
                Ok(result) => result,
                Err(status) => {
                    println!("TEE: version unavailable ({status})");
                    return;
                }
            };
            let Some(ch) = smc_return_ok(ch_res.arg0) else {
                println!("TEE: version unavailable (0x{:x})", ch_res.arg0);
                return;
            };
            if ch > 0xff {
                println!("TEE: version unavailable (byte 0x{ch:x})");
                return;
            }
            if ch == 0 {
                break;
            }
            bytes.push(ch as u8);
        }
        println!("{}", format_version_line(&String::from_utf8_lossy(&bytes)));
    }

    fn print_api_version(smc: &zx::Resource) {
        let res = match smc_call(smc, SMC_FC_API_VERSION, TRUSTY_API_VERSION_CURRENT) {
            Ok(result) => result,
            Err(status) => {
                println!("TEE: api unavailable ({status})");
                return;
            }
        };
        match smc_return_ok(res.arg0) {
            Some(version) => println!("TEE: api {}", format_api_version(version)),
            None => println!("TEE: api unavailable (0x{:x})", res.arg0),
        }
    }

    fn smp_max_cpus(smc: &zx::Resource) -> Option<u64> {
        let res = smc_call(smc, SMC_FC_GET_SMP_MAX_CPUS, 0).ok()?;
        smc_return_ok(res.arg0)
    }

    pub async fn probe() -> Result<(), String> {
        let smc = match get_smc_resource().await {
            Ok(resource) => resource,
            Err(error) => {
                eprintln!("{error}");
                println!("TEE: dead (no smc resource)");
                return Err(error);
            }
        };

        let uuid_res = match smc_call(&smc, SMC_FC_GET_UUID, 0) {
            Ok(result) => result,
            Err(status) => {
                eprintln!("TEE: GET_UUID smc: {status}");
                println!("TEE: dead (spd)");
                return Err(format!("GET_UUID {status}"));
            }
        };
        if uuid_res.arg0 == SMC_UNK {
            println!("TEE: GET_UUID -> SMC_UNK");
            println!("TEE: dead (spd)");
            return Err("GET_UUID SMC_UNK".into());
        }

        let w0 = uuid_res.arg0 as u32;
        let w1 = uuid_res.arg1 as u32;
        let w2 = uuid_res.arg2 as u32;
        let w3 = uuid_res.arg3 as u32;
        println!("TEE: spd uuid {}", format_smccc_uuid(w0, w1, w2, w3));
        if [w0, w1, w2, w3] != TRUSTY_UUID_WORDS {
            println!("TEE: dead (unexpected uuid)");
            return Err("unexpected uuid".into());
        }

        print_version_str(&smc);
        print_api_version(&smc);
        let smp_max = smp_max_cpus(&smc);

        let profile_resource = match get_profile_resource().await {
            Ok(resource) => resource,
            Err(error) => {
                eprintln!("{error}");
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
                return Err(format!("pin cpu {cpu}: {status}"));
            }

            let nop_res = match smc_call(&smc, SMC_YC_NOP, 0) {
                Ok(result) => result,
                Err(status) => {
                    eprintln!("TEE: NOP smc cpu {cpu}: {status}");
                    println!("TEE: dead (S-EL1 cpu {cpu})");
                    return Err(format!("NOP cpu {cpu}: {status}"));
                }
            };
            println!("TEE: S-EL1 nop cpu {cpu} -> 0x{:x}", nop_res.arg0);
            if nop_res.arg0 != TRUSTY_SEL1_ALIVE && nop_res.arg0 != TRUSTY_SEL1_ALIVE_LEGACY {
                println!("TEE: dead (S-EL1 cpu {cpu})");
                return Err(format!("NOP cpu {cpu} returned 0x{:x}", nop_res.arg0));
            }
            live += 1;
        }

        println!("{}", format_alive(live, smp_max));
        Ok(())
    }
}

fn run(args: &[String]) -> i32 {
    if let Err(message) = parse_args(args) {
        let _ = writeln!(io::stderr(), "{message}");
        return 1;
    }

    #[cfg(target_os = "fuchsia")]
    {
        let result = fuchsia_async::LocalExecutorBuilder::new()
            .build()
            .run_singlethreaded(fuchsia_probe::probe());
        match result {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }

    #[cfg(not(target_os = "fuchsia"))]
    {
        let _ = writeln!(io::stderr(), "lktee: Trusty SMC probe requires Fuchsia");
        1
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults() {
        let args: [String; 0] = [];
        assert!(parse_args(&args).is_ok());
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_args_rejects_operand() {
        let args = ["extra".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

    #[test]
    fn parse_args_double_dash_still_rejects_operand() {
        let args = ["--".to_string(), "-x".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("extra operand"));
    }

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

    #[test]
    fn smc_ids_match_libsm() {
        assert_eq!(SMC_FC_GET_UUID, 0xbf00ff01);
        assert_eq!(SMC_YC_NOP, 0x3c000001);
        assert_eq!(SMC_FC_GET_VERSION_STR, 0xbc00000a);
        assert_eq!(SMC_FC_API_VERSION, 0xbc00000b);
        assert_eq!(SMC_FC_GET_SMP_MAX_CPUS, 0xbc00000d);
        assert_eq!(VERSION_STR_LEN_INDEX, 0xffff_ffff);
        assert_eq!(VERSION_STR_MAX, 256);
        assert_eq!(TRUSTY_API_VERSION_CURRENT, 5);
        assert_eq!(TRUSTY_SEL1_ALIVE, 0);
        assert_eq!(TRUSTY_SEL1_ALIVE_LEGACY, 0x5445_4521);
    }

    #[test]
    fn format_api_version_names() {
        assert_eq!(format_api_version(4), "4 (phys-mem-obj)");
        assert_eq!(format_api_version(5), "5 (mem-obj)");
        assert_eq!(format_api_version(9), "9");
    }

    #[test]
    fn smc_return_ok_rejects_unk_and_errors() {
        assert_eq!(smc_return_ok(0), Some(0));
        assert_eq!(smc_return_ok(4), Some(4));
        assert_eq!(smc_return_ok(SMC_UNK), None);
        assert_eq!(smc_return_ok((-2i64) as u64), None);
    }

    #[test]
    fn format_lk_buildid_compact_and_iso() {
        assert_eq!(format_lk_buildid("PBDE1_LOCAL"), "2026-11-13 14:02Z local");
        assert_eq!(format_lk_buildid("PBDE1"), "2026-11-13 14:02Z");
        assert_eq!(
            format_lk_buildid("2026-11-13 14:38Z local"),
            "2026-11-13 14:38Z local"
        );
        assert_eq!(format_lk_buildid("custom"), "custom");
        assert_eq!(
            format_version_line("2026-11-13 14:38Z local"),
            "TEE: version 2026-11-13 14:38Z local"
        );
    }

    #[test]
    fn format_alive_merges_smp_max() {
        assert_eq!(format_alive(8, Some(8)), "TEE: alive (8/8)");
        assert_eq!(format_alive(4, Some(8)), "TEE: alive (4/8)");
        assert_eq!(format_alive(8, None), "TEE: alive (8/?)");
    }
}
