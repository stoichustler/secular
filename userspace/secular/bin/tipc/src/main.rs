// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Queueless TIPC client: NS VMO <-> Trusty S-EL1 kTIPC.
//!
//! ```text
//! tipc list
//! tipc kick [N]
//! ```

use std::process::ExitCode;

const DEFAULT_PORT: &str = "com.android.trusty.kernel.echo";
const PING: &[u8] = b"ping";

const QL_TIPC_DEV_RESP: u16 = 0x8000;
const QL_TIPC_DEV_CONNECT: u16 = 0x1;
const QL_TIPC_DEV_GET_EVENT: u16 = 0x2;
const QL_TIPC_DEV_SEND: u16 = 0x3;
const QL_TIPC_DEV_RECV: u16 = 0x4;
const QL_TIPC_DEV_DISCONNECT: u16 = 0x5;

const IPC_HANDLE_POLL_MSG: u32 = 0x8;
const IPC_HANDLE_POLL_HUP: u32 = 0x4;
const EVENT_WAIT_MS: u64 = 5000;

const SMC_ENTITY_TRUSTED_OS: u32 = 50;
const SMC_ENTITY_SECURE_MONITOR: u32 = 60;
const SMC_FC_API_VERSION: u32 = smc_fastcall(SMC_ENTITY_SECURE_MONITOR, 11);
const SMC_SC_RESTART_LAST: u32 = smc_stdcall(SMC_ENTITY_SECURE_MONITOR, 0);
const SMC_SC_CREATE_QL_TIPC_DEV: u32 = smc_stdcall(SMC_ENTITY_TRUSTED_OS, 30);
const SMC_SC_SHUTDOWN_QL_TIPC_DEV: u32 = smc_stdcall(SMC_ENTITY_TRUSTED_OS, 31);
const SMC_SC_HANDLE_QL_TIPC_DEV_CMD: u32 = smc_stdcall(SMC_ENTITY_TRUSTED_OS, 32);
const TRUSTY_API_VERSION_PHYS_MEM_OBJ: u32 = 4;

const SM_ERR_INTERRUPTED: i64 = -3;
const SM_ERR_BUSY: i64 = -5;
const SM_ERR_CPU_IDLE: i64 = -13;
const SM_ERR_NOP_INTERRUPTED: i64 = -14;

const NS_PTE_PHYSADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
const NS_MAIR_NORMAL_CACHED_WB_RWA: u64 = 0xff;
const NS_INNER_SHAREABLE: u64 = 0x3;

const fn smc_nr(entity: u32, fn_: u32, fastcall: u32) -> u32 {
    ((fastcall & 1) << 31) | ((entity & 0x3f) << 24) | (fn_ & 0xffff)
}
const fn smc_fastcall(entity: u32, fn_: u32) -> u32 {
    smc_nr(entity, fn_, 1)
}
const fn smc_stdcall(entity: u32, fn_: u32) -> u32 {
    smc_nr(entity, fn_, 0)
}

fn pack_ns_mem_id(paddr: u64) -> u64 {
    (paddr & NS_PTE_PHYSADDR_MASK)
        | (NS_MAIR_NORMAL_CACHED_WB_RWA << 48)
        | (NS_INNER_SHAREABLE << 8)
}

/// One NS<->Trusty TIPC window. `paddr == 0` means allocate a contiguous buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TipcWindow {
    name: String,
    paddr: u64,
    size: u64,
}

fn window_from_region(r: &secular_cfg::Region) -> TipcWindow {
    TipcWindow { name: r.name.clone(), paddr: r.paddr, size: r.size }
}

fn window_at(windows: &[TipcWindow], index: usize) -> Option<&TipcWindow> {
    windows.get(index)
}

fn default_window(page_size: u64) -> TipcWindow {
    TipcWindow { name: "tipc0".into(), paddr: 0, size: page_size }
}

fn read_windows() -> Vec<TipcWindow> {
    let text = std::fs::read_to_string(secular_cfg::CFG_PATH).unwrap_or_default();
    match secular_cfg::parse(&text) {
        Ok(cfg) => cfg.tipc_windows().map(window_from_region).collect(),
        Err(e) => {
            eprintln!("TIPC: {}: {e}", secular_cfg::CFG_PATH);
            Vec::new()
        }
    }
}

/// One subcommand. Add a row here, a `Command` variant, and a match arm
/// in `main` to extend the CLI.
struct CommandInfo {
    name: &'static str,
    args: &'static str,
    help: &'static str,
}

const COMMANDS: &[CommandInfo] = &[
    CommandInfo { name: "list", args: "", help: "list TIPC windows from secular.cfg" },
    CommandInfo { name: "kick", args: "[N]", help: "echo ping on window N (default 0)" },
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    List,
    Kick { index: usize },
}

fn usage() -> String {
    let mut s = String::from("usage: tipc <command> [args]\n");
    for c in COMMANDS {
        let spec = if c.args.is_empty() {
            c.name.to_string()
        } else {
            format!("{} {}", c.name, c.args)
        };
        s.push_str(&format!("  {spec:<12} {}\n", c.help));
    }
    s
}

fn parse_command(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter().map(String::as_str);
    let Some(cmd) = it.next() else {
        return Err(usage());
    };
    match cmd {
        "list" => {
            if it.next().is_some() {
                return Err(format!("tipc list takes no arguments\n{}", usage()));
            }
            Ok(Command::List)
        }
        "kick" => {
            let index = match it.next() {
                None => 0,
                Some(token) => token.parse::<usize>().map_err(|_| {
                    format!("tipc kick: bad window `{token}` (want a non-negative integer)\n{}", usage())
                })?,
            };
            if it.next().is_some() {
                return Err(format!("usage: tipc kick [N]\n{}", usage()));
            }
            Ok(Command::Kick { index })
        }
        "help" | "-h" | "--help" => Err(usage()),
        other => Err(format!("tipc: unknown command `{other}`\n{}", usage())),
    }
}

fn format_list(windows: &[TipcWindow]) -> String {
    if windows.is_empty() {
        return format!("TIPC: no windows in {}\n", secular_cfg::CFG_PATH);
    }
    let mut s = String::from("N   NAME    PADDR            SIZE\n");
    for (i, w) in windows.iter().enumerate() {
        s.push_str(&format!("{i:<3} {:<7} 0x{:<14x} 0x{:x}\n", w.name, w.paddr, w.size));
    }
    s
}

fn resolve_kick_window(windows: &[TipcWindow], index: usize, page_size: u64) -> Result<TipcWindow, String> {
    if windows.is_empty() {
        if index == 0 {
            return Ok(default_window(page_size));
        }
        return Err(format!("TIPC: no window {index} (none configured)"));
    }
    window_at(windows, index).cloned().ok_or_else(|| {
        format!("TIPC: no window {index} (have {})", windows.len())
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TipcCmdHdr {
    opcode: u16,
    flags: u16,
    status: u32,
    handle: u32,
    payload_len: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TipcConnectReq {
    cookie: u64,
    reserved: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TipcEvent {
    event: u32,
    handle: u32,
    cookie: u64,
}

#[cfg(target_os = "fuchsia")]
mod fuchsia_tipc {
    use super::*;
    use anyhow::{Context as _, Error, format_err};
    use fidl_fuchsia_kernel as fkernel;
    use fuchsia_component::client::connect_to_protocol;
    use std::ptr;
    use zx::{Bti, BtiOptions, Iommu, IommuDescStub, VmarFlags, Vmo, VmoOp};

    fn smc_call(
        smc: &zx::Resource,
        func_id: u32,
        arg1: u64,
        arg2: u64,
        arg3: u64,
    ) -> Result<zx::sys::zx_smc_result_t, zx::Status> {
        let mut params = zx::sys::zx_smc_parameters_t::default();
        params.func_id = func_id;
        params.arg1 = arg1;
        params.arg2 = arg2;
        params.arg3 = arg3;
        let mut result = zx::sys::zx_smc_result_t::default();
        zx::ok(unsafe { zx::sys::zx_smc_call(smc.raw_handle(), &params, &mut result) })?;
        Ok(result)
    }

    fn as_i64(result: &zx::sys::zx_smc_result_t) -> i64 {
        result.arg0 as i64
    }

    fn stdcall(
        smc: &zx::Resource,
        func_id: u32,
        arg1: u64,
        arg2: u64,
        arg3: u64,
    ) -> Result<i64, Error> {
        let mut result = smc_call(smc, func_id, arg1, arg2, arg3).context("stdcall")?;
        for _ in 0..64 {
            match as_i64(&result) {
                SM_ERR_INTERRUPTED | SM_ERR_BUSY | SM_ERR_CPU_IDLE | SM_ERR_NOP_INTERRUPTED => {
                    result = smc_call(smc, SMC_SC_RESTART_LAST, 0, 0, 0).context("restart")?;
                }
                other => return Ok(other),
            }
        }
        Err(format_err!("stdcall 0x{func_id:x} did not complete"))
    }

    fn pin_cpu0(profile_resource: &zx::Resource) -> Result<(), zx::Status> {
        let mut info = unsafe { std::mem::zeroed::<zx::sys::zx_profile_info_t>() };
        info.flags = zx::sys::ZX_PROFILE_INFO_FLAG_CPU_MASK;
        info.cpu_affinity_mask.mask[0] = 1;
        let mut out = zx::sys::ZX_HANDLE_INVALID;
        zx::ok(unsafe {
            zx::sys::zx_profile_create(profile_resource.raw_handle(), 0, &info, &mut out)
        })?;
        let profile = zx::Profile::from(unsafe { zx::Handle::from_raw_unchecked(out) });
        fuchsia_runtime::with_thread_self(|thread| thread.set_profile(&profile, 0))
    }

    async fn get_smc() -> Result<zx::Resource, Error> {
        connect_to_protocol::<fkernel::SmcResourceMarker>()
            .context("connect SmcResource")?
            .get()
            .await
            .context("SmcResource.Get")
    }

    async fn get_profile() -> Result<zx::Resource, Error> {
        connect_to_protocol::<fkernel::ProfileResourceMarker>()
            .context("connect ProfileResource")?
            .get()
            .await
            .context("ProfileResource.Get")
    }

    async fn get_iommu() -> Result<zx::Resource, Error> {
        connect_to_protocol::<fkernel::IommuResourceMarker>()
            .context("connect IommuResource")?
            .get()
            .await
            .context("IommuResource.Get")
    }

    unsafe fn hdr_at(base: usize) -> *mut TipcCmdHdr {
        base as *mut TipcCmdHdr
    }

    fn payload_at(base: usize) -> *mut u8 {
        (base + std::mem::size_of::<TipcCmdHdr>()) as *mut u8
    }

    fn write_hdr(base: usize, opcode: u16, handle: u32, payload_len: u32) {
        unsafe {
            *hdr_at(base) = TipcCmdHdr { opcode, flags: 0, status: 0, handle, payload_len };
        }
    }

    fn cache_clean(vmo: &Vmo, size: u64) -> Result<(), zx::Status> {
        vmo.op_range(VmoOp::CACHE_CLEAN, 0, size)
    }

    fn cache_invalidate(vmo: &Vmo, size: u64) -> Result<(), zx::Status> {
        vmo.op_range(VmoOp::CACHE_INVALIDATE, 0, size)
    }

    fn handle_cmd(
        smc: &zx::Resource,
        vmo: &Vmo,
        buf_id: u64,
        page_size: u64,
        mapped: usize,
        opcode: u16,
        handle: u32,
        payload: &[u8],
    ) -> Result<(u32, u32, Vec<u8>), Error> {
        write_hdr(mapped, opcode, handle, payload.len() as u32);
        if !payload.is_empty() {
            unsafe {
                ptr::copy_nonoverlapping(payload.as_ptr(), payload_at(mapped), payload.len());
            }
        }
        cache_clean(vmo, page_size).context("cache clean")?;
        let cmd_sz = (std::mem::size_of::<TipcCmdHdr>() + payload.len()) as u64;
        let ret = stdcall(smc, SMC_SC_HANDLE_QL_TIPC_DEV_CMD, buf_id & 0xffff_ffff, buf_id >> 32, cmd_sz)?;
        if ret < 0 {
            return Err(format_err!("HANDLE_CMD 0x{opcode:x} smc {ret}"));
        }
        cache_invalidate(vmo, page_size).context("cache invalidate")?;
        let hdr = unsafe { &*hdr_at(mapped) };
        if hdr.opcode != (opcode | QL_TIPC_DEV_RESP) {
            return Err(format_err!("bad resp opcode 0x{:x}", hdr.opcode));
        }
        if hdr.status != 0 {
            return Err(format_err!("tipc status {} opcode 0x{opcode:x}", hdr.status));
        }
        let mut out = vec![0u8; hdr.payload_len as usize];
        if !out.is_empty() {
            unsafe {
                ptr::copy_nonoverlapping(payload_at(mapped), out.as_mut_ptr(), out.len());
            }
        }
        Ok((hdr.handle, hdr.payload_len, out))
    }

    fn wait_event(
        smc: &zx::Resource,
        vmo: &Vmo,
        buf_id: u64,
        page_size: u64,
        mapped: usize,
        handle: u32,
        want: u32,
    ) -> Result<TipcEvent, Error> {
        let wait = EVENT_WAIT_MS.to_le_bytes();
        let (_, _, payload) = handle_cmd(
            smc,
            vmo,
            buf_id,
            page_size,
            mapped,
            QL_TIPC_DEV_GET_EVENT,
            handle,
            &wait,
        )?;
        if payload.len() >= std::mem::size_of::<TipcEvent>() {
            let ev = unsafe { ptr::read_unaligned(payload.as_ptr() as *const TipcEvent) };
            if ev.handle != 0 && (ev.event & want) != 0 {
                return Ok(ev);
            }
            if ev.handle != 0 && (ev.event & IPC_HANDLE_POLL_HUP) != 0 {
                return Err(format_err!("channel hup event 0x{:x}", ev.event));
            }
            if ev.handle != 0 {
                return Err(format_err!(
                    "tipc event 0x{:x} handle 0x{:x}, want 0x{want:x}",
                    ev.event,
                    ev.handle
                ));
            }
        }
        Err(format_err!("timeout waiting for tipc event 0x{want:x}"))
    }

    pub async fn kick(window: &TipcWindow, port: &str) -> Result<(), Error> {
        let smc = get_smc().await.map_err(|e| {
            eprintln!("TIPC: {e:?}");
            println!("TIPC: dead (no smc resource)");
            e
        })?;
        let profile = get_profile().await.map_err(|e| {
            eprintln!("TIPC: {e:?}");
            println!("TIPC: dead (pin cpu 0)");
            e
        })?;
        pin_cpu0(&profile).context("pin cpu 0")?;

        let ver = smc_call(&smc, SMC_FC_API_VERSION, TRUSTY_API_VERSION_PHYS_MEM_OBJ as u64, 0, 0)
            .context("API_VERSION")?;
        println!("TIPC: api version {}", ver.arg0);
        if ver.arg0 < TRUSTY_API_VERSION_PHYS_MEM_OBJ as u64 {
            println!("TIPC: dead (api version)");
            return Err(format_err!("api version {}", ver.arg0));
        }

        let iommu_resource = get_iommu().await.map_err(|e| {
            eprintln!("TIPC: {e:?}");
            println!("TIPC: dead (no iommu resource)");
            e
        })?;
        let iommu = Iommu::create_stub(&iommu_resource, IommuDescStub::default())
            .context("iommu create")?;
        let bti = Bti::create(&iommu, 0).context("bti create")?;
        let page_size = zx::system_get_page_size() as u64;
        let buf_size = window.size.max(page_size);
        if buf_size % page_size != 0 {
            println!("TIPC: dead ({} size 0x{buf_size:x} not page aligned)", window.name);
            return Err(format_err!("unaligned tipc window"));
        }
        if window.paddr != 0 {
            println!(
                "TIPC: {} config paddr 0x{:x} size 0x{buf_size:x} (allocating contiguous NS buffer)",
                window.name, window.paddr
            );
        }
        let vmo = Vmo::create_contiguous(&bti, buf_size as usize, 0).context("contiguous vmo")?;
        let mut paddrs = [0usize; 1];
        let pmt = bti
            .pin(
                BtiOptions::PERM_READ | BtiOptions::PERM_WRITE | BtiOptions::CONTIGUOUS,
                &vmo,
                0,
                buf_size,
                &mut paddrs,
            )
            .context("bti pin")?;
        let paddr = paddrs[0] as u64;
        let buf_id = pack_ns_mem_id(paddr);
        let mapped = fuchsia_runtime::vmar_root_self()
            .map(0, &vmo, 0, buf_size as usize, VmarFlags::PERM_READ | VmarFlags::PERM_WRITE)
            .context("map shared buffer")?;

        let create = stdcall(
            &smc,
            SMC_SC_CREATE_QL_TIPC_DEV,
            buf_id & 0xffff_ffff,
            buf_id >> 32,
            buf_size,
        );
        let mut created = matches!(create, Ok(0));
        let mut pmt = Some(pmt);
        fn shutdown(
            smc: &zx::Resource,
            buf_id: u64,
            created: &mut bool,
            pmt: &mut Option<zx::Pmt>,
        ) {
            if *created {
                let _ = stdcall(
                    smc,
                    SMC_SC_SHUTDOWN_QL_TIPC_DEV,
                    buf_id & 0xffff_ffff,
                    buf_id >> 32,
                    0,
                );
                *created = false;
            }
            if let Some(token) = pmt.take() {
                let _ = unsafe { token.unpin() };
            }
        }
        if !created {
            shutdown(&smc, buf_id, &mut created, &mut pmt);
            let create = create.unwrap_or(-1);
            println!("TIPC: dead (create {create})");
            return Err(format_err!("CREATE_QL_TIPC_DEV {create}"));
        }
        println!("TIPC: {} ql device @ paddr 0x{paddr:x} size 0x{buf_size:x}", window.name);

        let mut connect = Vec::new();
        connect.extend_from_slice(&TipcConnectReq { cookie: 1, reserved: 0 }.as_bytes());
        connect.extend_from_slice(port.as_bytes());
        connect.push(0);
        let (handle, _, _) = match handle_cmd(
            &smc,
            &vmo,
            buf_id,
            buf_size,
            mapped,
            QL_TIPC_DEV_CONNECT,
            0,
            &connect,
        ) {
            Ok(v) => v,
            Err(e) => {
                shutdown(&smc, buf_id, &mut created, &mut pmt);
                println!("TIPC: dead (connect {port})");
                return Err(e);
            }
        };
        println!("TIPC: connected {port} handle 0x{handle:x}");

        if let Err(e) =
            handle_cmd(&smc, &vmo, buf_id, buf_size, mapped, QL_TIPC_DEV_SEND, handle, PING)
        {
            shutdown(&smc, buf_id, &mut created, &mut pmt);
            println!("TIPC: dead (send)");
            return Err(e);
        }

        let ev = match wait_event(
            &smc,
            &vmo,
            buf_id,
            buf_size,
            mapped,
            handle,
            IPC_HANDLE_POLL_MSG,
        ) {
            Ok(ev) => ev,
            Err(e) => {
                shutdown(&smc, buf_id, &mut created, &mut pmt);
                println!("TIPC: dead (msg)");
                return Err(e);
            }
        };
        let recv_handle = if ev.handle != 0 { ev.handle } else { handle };
        let (_, _, reply) = match handle_cmd(
            &smc,
            &vmo,
            buf_id,
            buf_size,
            mapped,
            QL_TIPC_DEV_RECV,
            recv_handle,
            &[],
        ) {
            Ok(v) => v,
            Err(e) => {
                shutdown(&smc, buf_id, &mut created, &mut pmt);
                println!("TIPC: dead (recv)");
                return Err(e);
            }
        };
        let _ = handle_cmd(
            &smc,
            &vmo,
            buf_id,
            buf_size,
            mapped,
            QL_TIPC_DEV_DISCONNECT,
            handle,
            &[],
        );
        shutdown(&smc, buf_id, &mut created, &mut pmt);

        if reply != PING {
            println!("TIPC: dead (echo mismatch {:?})", reply);
            return Err(format_err!("echo mismatch"));
        }
        println!("TIPC: alive (echo {} bytes)", reply.len());
        Ok(())
    }

    trait AsBytes {
        fn as_bytes(&self) -> &[u8];
    }
    impl AsBytes for TipcConnectReq {
        fn as_bytes(&self) -> &[u8] {
            unsafe {
                std::slice::from_raw_parts(
                    ptr::from_ref(self).cast::<u8>(),
                    std::mem::size_of::<Self>(),
                )
            }
        }
    }
}

fn print_parse_error(msg: &str) -> ExitCode {
    eprint!("{msg}");
    if !msg.ends_with('\n') {
        eprintln!();
    }
    ExitCode::from(2)
}

#[cfg(target_os = "fuchsia")]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse_command(&args) {
        Ok(c) => c,
        Err(msg) => return print_parse_error(&msg),
    };
    match command {
        Command::List => {
            print!("{}", format_list(&read_windows()));
            ExitCode::SUCCESS
        }
        Command::Kick { index } => {
            let page_size = zx::system_get_page_size() as u64;
            let window = match resolve_kick_window(&read_windows(), index, page_size) {
                Ok(w) => w,
                Err(msg) => {
                    println!("{msg}");
                    return ExitCode::from(1);
                }
            };
            println!("TIPC: kick {index} ({})", window.name);
            let result = fuchsia_async::LocalExecutorBuilder::new()
                .build()
                .run_singlethreaded(fuchsia_tipc::kick(&window, DEFAULT_PORT));
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("TIPC: {e:?}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

#[cfg(not(target_os = "fuchsia"))]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_command(&args) {
        Ok(Command::List) => {
            print!("{}", format_list(&read_windows()));
            ExitCode::SUCCESS
        }
        Ok(Command::Kick { .. }) => {
            println!("TIPC: kick requires a Fuchsia target");
            ExitCode::from(1)
        }
        Err(msg) => print_parse_error(&msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_page_aligned_paddr() {
        let id = pack_ns_mem_id(0x4000_0000);
        assert_eq!(id & NS_PTE_PHYSADDR_MASK, 0x4000_0000);
        assert_eq!(id >> 48, NS_MAIR_NORMAL_CACHED_WB_RWA);
    }

    #[test]
    fn smc_ids() {
        assert_eq!(SMC_SC_CREATE_QL_TIPC_DEV, 0x3200_001e);
        assert_eq!(SMC_FC_API_VERSION, 0xbc00_000b);
    }

    #[test]
    fn parse_tipc0_and_tipc1() {
        let cfg = secular_cfg::parse(
            r#"{
              windows: [
                { name: "tipc0", paddr: "0x7fe00000", size: "0x10000", role: "tipc" },
                { name: "tipc1", paddr: 0, size: 4096 },
              ],
            }"#,
        )
        .unwrap();
        let windows: Vec<TipcWindow> = cfg.tipc_windows().map(window_from_region).collect();
        assert_eq!(
            windows,
            vec![
                TipcWindow { name: "tipc0".into(), paddr: 0x7fe00000, size: 0x10000 },
                TipcWindow { name: "tipc1".into(), paddr: 0, size: 4096 },
            ]
        );
        assert_eq!(window_at(&windows, 0).unwrap().name, "tipc0");
        assert_eq!(window_at(&windows, 1).unwrap().size, 4096);
        assert!(window_at(&windows, 2).is_none());
    }

    #[test]
    fn parse_list_and_kick() {
        assert_eq!(parse_command(&[]).unwrap_err().lines().next(), Some("usage: tipc <command> [args]"));
        assert_eq!(parse_command(&[String::from("list")]).unwrap(), Command::List);
        assert_eq!(
            parse_command(&[String::from("kick")]).unwrap(),
            Command::Kick { index: 0 }
        );
        assert_eq!(
            parse_command(&[String::from("kick"), String::from("3")]).unwrap(),
            Command::Kick { index: 3 }
        );
        assert!(parse_command(&[String::from("kick"), String::from("x")]).is_err());
        assert!(parse_command(&[String::from("ping")]).is_err());
        assert!(COMMANDS.iter().any(|c| c.name == "list"));
        assert!(COMMANDS.iter().any(|c| c.name == "kick"));
    }

    #[test]
    fn list_and_resolve() {
        let cfg = secular_cfg::parse(
            r#"{
              windows: [
                { name: "tipc0", paddr: 0, size: "0x1000", role: "tipc" },
                { name: "tipc1", paddr: "0x10", size: "0x2000", role: "tipc" },
              ],
            }"#,
        )
        .unwrap();
        let windows: Vec<TipcWindow> = cfg.tipc_windows().map(window_from_region).collect();
        let table = format_list(&windows);
        assert!(table.contains("tipc0"));
        assert!(table.contains("tipc1"));
        assert_eq!(resolve_kick_window(&windows, 1, 0x1000).unwrap().name, "tipc1");
        assert!(resolve_kick_window(&windows, 2, 0x1000).is_err());
        assert_eq!(resolve_kick_window(&[], 0, 0x1000).unwrap(), default_window(0x1000));
        assert!(resolve_kick_window(&[], 1, 0x1000).is_err());
        assert!(format_list(&[]).contains(secular_cfg::CFG_PATH));
    }
}
