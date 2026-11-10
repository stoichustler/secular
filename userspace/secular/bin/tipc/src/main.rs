// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Queueless TIPC client: NS VMO <-> Trusty S-EL1 kTIPC.

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
const TIPC_CONF_PATH: &str = "/boot/config/tipc.conf";
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

fn parse_u64(token: &str) -> Option<u64> {
    let token = token.trim();
    if let Some(hex) = token.strip_prefix("0x").or_else(|| token.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        token.parse().ok()
    }
}

/// Line format: `name paddr size` (`#` comments, extra regions later as `tipc1` …).
fn parse_tipc_conf(text: &str) -> Vec<TipcWindow> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(paddr) = parts.next().and_then(parse_u64) else { continue };
        let Some(size) = parts.next().and_then(parse_u64) else { continue };
        if size == 0 {
            continue;
        }
        out.push(TipcWindow { name: name.to_string(), paddr, size });
    }
    out
}

fn window_named<'a>(windows: &'a [TipcWindow], name: &str) -> Option<&'a TipcWindow> {
    windows.iter().find(|w| w.name == name).or_else(|| windows.first())
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

    fn load_window(page_size: u64) -> TipcWindow {
        let text = std::fs::read_to_string(TIPC_CONF_PATH).unwrap_or_default();
        window_named(&parse_tipc_conf(&text), "tipc0")
            .cloned()
            .unwrap_or(TipcWindow { name: "tipc0".into(), paddr: 0, size: page_size })
    }

    pub async fn run(port: &str) -> Result<(), Error> {
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
        let window = load_window(page_size);
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

#[cfg(target_os = "fuchsia")]
fn main() -> ExitCode {
    let port = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_PORT.to_string());
    let result = fuchsia_async::LocalExecutorBuilder::new()
        .build()
        .run_singlethreaded(fuchsia_tipc::run(&port));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("TIPC: {e:?}");
            ExitCode::from(1)
        }
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
        let windows = parse_tipc_conf(
            "# comment\n\
             tipc0 0x7fe00000 0x10000\n\
             tipc1 0 4096\n",
        );
        assert_eq!(
            windows,
            vec![
                TipcWindow { name: "tipc0".into(), paddr: 0x7fe00000, size: 0x10000 },
                TipcWindow { name: "tipc1".into(), paddr: 0, size: 4096 },
            ]
        );
        assert_eq!(window_named(&windows, "tipc0").unwrap().size, 0x10000);
    }
}
