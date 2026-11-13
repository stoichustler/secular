# Trusty

Secular boots a Zircon kernel on `qemu-system-aarch64` (virt, arm64) with
TrustZone `secure=on`. ATF is EL3 firmware (`SPD=trusty`). LK Trusty is
BL32 at S-EL1. The NS image is still the console product. This note is
the map of the two BOOTFS probes that reach that TEE from `zxsh`:
**lktee** (SPD UUID, LK version/API/SMP, S-EL1 NOP) and **tipc** (queueless TIPC echo).

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read [`bootstrap.md`](../../../products/reference/zircon/bootstrap.md) for
ATF → shim → `zxsh`, [`ipc.md`](../../../products/reference/zircon/ipc.md)
for handles / VMOs, [`cpu.md`](../../../products/reference/zircon/cpu.md)
for affinity. LK Trusty / lkboot is [`sdk/lk/README.md`](../../../sdk/lk/README.md).
GlobalPlatform TAs hosted **inside** Zircon are [`tee/README.md`](tee/README.md);
they are a different TEE.

`lktee` and `tipc` are `/boot/bin` programs, not CML components. They are
started by `zxsh`. They talk to BL32 with `zx_smc_call`.

## Trusty

### Framework

Three worlds, one DRAM. EL3 answers a few fastcalls itself. Everything
else is a context switch into S-EL1.

```text
 QEMU virt  (secure=on)
   ATF  secular.bios          EL3  BL1 → BL2 → BL31 + Trusty SPD
   BL32 qemu-virt-arm64-trusty          S-EL1
   BL33 qemu-virt-arm64-lkboot          EL2, then shim
   Image linux-arm64-boot-shim.bin      x0 = DTB
   initrd secular.zbi                   kernel + BOOTFS
              │
              ▼
┌────────────────────────────────────────────────────────────────┐
│ EL3  ATF Trusty SPD                                            │
│  GET_UUID  →  40ee25f0-a2bc-304c-8c4c-a173c57d8af1             │
│  other SMC →  trusty_context_switch(NON_SECURE, fid, …)        │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ S-EL1  LK Trusty  (lib/sm)                                     │
│  sm-stdcall     LOWEST+2     yielding SMC                      │
│  idle-ns-switch LOWEST+1     return to NS when idle            │
│  ktipc echo     DEFAULT      com.android.trusty.kernel.echo    │
│  QL-TIPC        WITH_TRUSTY_QL_TIPC_DEV                        │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  SMC_YC_NS_RETURN
┌────────────────────────────────────────────────────────────────┐
│ NS  lkboot → boot-shim → vmzircon EL1 → userboot → zxsh EL0    │
│  /boot/bin/lktee                                                │
│  /boot/bin/tipc                                                │
│  /boot/config/secular.cfg                                      │
└────────────────────────────────────────────────────────────────┘
```

| Probe | Question | Worlds |
| --- | --- | --- |
| `lktee` | Is the Trusty SPD present, what is LK's identity, and does S-EL1 return a NOP? | EL3, then S-EL1 per CPU |
| `tipc` | Can an NS page carry a TIPC ping to the kernel echo port? | EL3 + S-EL1 + ktipc |

### Flow

Every probe call is the same NS path. Fastcalls stop in EL3 or in
Trusty lib/sm without blocking. Yielding calls enter `sm-stdcall` and
may return `SM_ERR_INTERRUPTED` / `SM_ERR_CPU_IDLE` until the work is
done.

```text
 zxsh  lktee | tipc
         │
         ▼
 fuchsia.kernel.SmcResource.Get     (console-launcher namespace)
         │
         ▼
 zx_smc_call(fid, arg1, arg2, arg3)
         │
         ▼
 Zircon  smc syscall  (must run on the pinned CPU)
         │
         ▼
 EL3  Trusty SPD
    ┌────┴────┐
    ▼         ▼
 GET_UUID   yielding / other fastcall
 (EL3)      trusty_context_switch
               │
               ▼
            S-EL1  sm_queue_stdcall / fastcall table
               │
               ▼
            handler  →  r0
               │
               ▼
            EL3  SMC_RET1  →  NS  zx_smc_result_t
```

`console-launcher` **use**s the kernel resource protocols and forwards
them into the console process. Without them the probes die before the
first SMC:

```text
 component_manager
         │
         ▼
 console-launcher
   fuchsia.kernel.SmcResource       lktee, tipc
   fuchsia.kernel.ProfileResource   CPU pin
   fuchsia.kernel.IommuResource     tipc BTI
         │
         ▼
 zxsh  →  lktee | tipc
```

| Line | Missing |
| --- | --- |
| `TEE: dead (no smc resource)` / `TIPC: dead (no smc resource)` | `SmcResource` |
| `TEE: dead (pin cpu N)` / `TIPC: dead (pin cpu 0)` | `ProfileResource` |
| `TIPC: dead (no iommu resource)` | `IommuResource` |

### SMC

SMCCC SMC32. Bit 31 is fast vs yielding. Bits 29:24 are the entity.
Bits 15:0 are the function.

```text
 31        24              0
 ┌─┬─┬──────┬──────────────┐
 │F│0│entity│   function   │
 └─┴─┴──────┴──────────────┘
  F=1 fast     F=0 yielding
```

| Name | ID | Entity | Fast | Where |
| --- | --- | --- | --- | --- |
| `SMC_FC_GET_UUID` | `0xbf00ff01` | 63 | yes | ATF SPD |
| `SMC_YC_NOP` / `SMC_SC_LOCKED_NOP` | `0x3c000001` | 60 | no | Trusty `smc_nop_stdcall` → `0` |
| `SMC_FC_GET_VERSION_STR` | `0xbc00000a` | 60 | yes | Trusty `lk_version.buildid` |
| `SMC_FC_API_VERSION` | `0xbc00000b` | 60 | yes | Trusty lib/sm |
| `SMC_FC_GET_SMP_MAX_CPUS` | `0xbc00000d` | 60 | yes | Trusty `SMP_MAX_CPUS` |
| `SMC_SC_RESTART_LAST` | `0x3c000000` | 60 | no | resume stdcall |
| `SMC_SC_CREATE_QL_TIPC_DEV` | `0x3200001e` | 50 | no | `ql_tipc_create_device` |
| `SMC_SC_SHUTDOWN_QL_TIPC_DEV` | `0x3200001f` | 50 | no | `ql_tipc_shutdown_device` |
| `SMC_SC_HANDLE_QL_TIPC_DEV_CMD` | `0x32000020` | 50 | no | `ql_tipc_handle_cmd` |

`tipc` retries a yielding call while `r0` is one of:

| `r0` | Meaning |
| --- | --- |
| `-3` `SM_ERR_INTERRUPTED` | stdcall still running |
| `-5` `SM_ERR_BUSY` | restart while busy |
| `-13` `SM_ERR_CPU_IDLE` | SMP idle; restart |
| `-14` `SM_ERR_NOP_INTERRUPTED` | NOP work pending |

Fastcalls (`GET_UUID`, `API_VERSION`) complete in one round trip.

Trusty CPU0 is the stdcall CPU. `tipc` pins to CPU 0. ktipc must sit
**above** the NS idle switcher, or accept/echo never run before Trusty
returns to NS:

```text
 higher  DEFAULT_PRIORITY     ktipc echo
         LOWEST+2             sm-stdcall
         LOWEST+1             idle-ns-switch-0   (return to NS)
 lower   LOWEST               (must not host ktipc)
```

## lktee

[`iokit/src/lktee.rs`](iokit/src/lktee.rs). EL3 SPD presence, LK identity
fastcalls, then S-EL1 liveness on every Zircon CPU. No shared memory, no
TIPC, no TA. `/boot/bin/lktee` is an IOKit command.

### Framework

UUID is answered in EL3 (the SPD is the object). Version, API, and
SMP-max are Trusty lib/sm fastcalls. NOP is a yielding call into S-EL1
(`smc_nop_stdcall` returns `0`; a legacy stub returned `'TEE!'`
`0x54454521`).

```text
 lktee
   │
   ├─ GET_UUID           0xbf00ff01     EL3 only
   │     words  40ee25f0-a2bc-304c-8c4c-a173c57d8af1
   ├─ GET_VERSION_STR    0xbc00000a     S-EL1  compact b36 buildid
   ├─ API_VERSION        0xbc00000b     S-EL1  negotiate CURRENT (5)
   ├─ GET_SMP_MAX_CPUS   0xbc00000d     S-EL1  SMP_MAX_CPUS
   └─ for cpu in 0 .. zx_system_get_num_cpus
         pin thread to cpu
         SMC_YC_NOP  0x3c000001     EL3 → S-EL1 → 0
```

### Flow

```text
 zxsh: lktee
         │
         ▼  (1) SmcResource.Get
         │
         ▼  (2) GET_UUID
 EL3 SPD, no S-EL1 entry
         │
    SMC_UNK ──────────────► TEE: dead (spd)
    other UUID ───────────► TEE: dead (unexpected uuid)
         │
         ▼  (3) GET_VERSION_STR / API_VERSION / GET_SMP_MAX_CPUS
 S-EL1 fastcall; missing info prints `unavailable`, not `dead`
         │
         ▼  (4) ProfileResource.Get
         │
         ▼  (5) each online CPU
 pin → SMC_YC_NOP
         │
    arg0 not 0 / TEE! ────► TEE: dead (S-EL1 cpu N)
         │
         ▼
 TEE: alive (N/M)
```

| Node | EL | Who |
| --- | --- | --- |
| (1) | EL0 | FIDL |
| (2) | EL0 → EL1 → EL3 | ATF SPD |
| (3) | EL0 → EL1 → EL3 → S-EL1 | Trusty lib/sm fastcalls |
| (4) | EL0 | FIDL |
| (5) | EL0 → EL1 → EL3 → S-EL1 | Trusty `smc_nop_stdcall` |

### Serial

```text
TEE: spd uuid 40ee25f0-a2bc-304c-8c4c-a173c57d8af1
TEE: version 2026-11-13 14:38Z local
TEE: api 4 (phys-mem-obj)
TEE: S-EL1 nop cpu 0 -> 0x0
TEE: S-EL1 nop cpu 1 -> 0x0
…
TEE: alive (8/8)
```

`GET_VERSION_STR` returns the compact `buildid.sh` stamp (`PBDEJ_LOCAL`).
`lktee` decodes it to `YYYY-MM-DD HH:MMZ` (`local` means unofficial).
LK serial `print_version` uses `lk_buildid_string()` for the same UTC
text. `alive (N/M)` is NOP-live CPUs over Trusty `SMP_MAX_CPUS`.

A BIOS without `SPD=trusty` fails at (2). `lktee` should pass before
debugging `tipc`.

## tipc

[`tipc/src/main.rs`](tipc/src/main.rs). NS contiguous page as a
queueless TIPC device. Connect to `com.android.trusty.kernel.echo`,
send `ping`, compare the echo.

```text
tipc list          list TIPC windows from /boot/config/secular.cfg
tipc kick [N]      echo ping on window N (default 0)
```

### Framework

One NS window, one Trusty device, one ktipc port. The window is a
contiguous VMO, pinned with a stub IOMMU BTI, mapped RW in `tipc`.
Trusty maps the same PA `PERM_READ|PERM_WRITE|PERM_NO_EXECUTE`.

```text
 /boot/config/secular.cfg        windows[]  tipc0 paddr=0 size=4K
        │
        ▼
 NS  contiguous VMO  size 0x1000
        │  BTI pin  CONTIGUOUS | READ | WRITE
        ▼
 paddr  packed ns_mem_id
   bits 47:12  PA
   bits  9:8   inner shareable 0x3
   bits 55:48  MAIR WB RWA 0xff
        │
        ▼
┌───────────────────────────────┐     ┌──────────────────────────────┐
│ NS window                     │     │ S-EL1  ql_tipc_dev           │
│  TipcCmdHdr                   │◀───▶│  CREATE / HANDLE_CMD /       │
│  payload                      │ PA  │  SHUTDOWN                    │
│  CACHE_CLEAN before SMC       │     │  local handle base 0x20      │
│  CACHE_INVALIDATE after       │     └──────────────┬───────────────┘
└───────────────────────────────┘                    │
                                                     ▼
                                      ┌──────────────────────────────┐
                                      │ ktipc  ns_echo               │
                                      │  com.android.trusty.kernel   │
                                      │         .echo                │
                                      └──────────────────────────────┘
```

Command header at offset 0 of the window (little-endian):

```text
 0   u16  opcode
 2   u16  flags
 4   u32  status          0 = OK, 1 = error
 8   u32  handle          local TIPC addr (base 0x20)
12   u32  payload_len
16        payload
```

Response opcode = request `| 0x8000`.

| Opcode | Name | Payload |
| --- | --- | --- |
| `0x1` | CONNECT | `{cookie u64, reserved u64}` + port + NUL |
| `0x2` | GET_EVENT | `{timeout_ms u64}` (`UINT64_MAX` = infinite) |
| `0x3` | SEND | message bytes |
| `0x4` | RECV | empty; reply written after the header |
| `0x5` | DISCONNECT | empty |

GET_EVENT returns `{event u32, handle u32, cookie u64}`:

| Bit | Name | Meaning |
| --- | --- | --- |
| `0x1` | READY | peer accepted (**one-shot**) |
| `0x4` | HUP | peer closed |
| `0x8` | MSG | message queued |

`paddr == 0` in `secular.cfg` means allocate. A non-zero `paddr` is a
reserved PA; the client still allocates a contiguous buffer unless that
range is carved out of PMM.

### Flow

`ipc_port_connect_async` only **queues** the client. ktipc must
`ipc_port_accept` before SEND is legal. `chan_poll` treats READY as
one-shot: Trusty CONNECT `handle_wait`s (5 s) and **consumes** READY.
NS must not poll `0x1` again; it SEND and waits for MSG.

```text
 zxsh: tipc kick
         │
         ▼  (1) SmcResource + ProfileResource, pin CPU 0
         │
         ▼  (2) API_VERSION  want 4 PHYS_MEM_OBJ
    version < 4 ──────────► TIPC: dead (api version)
         │
         ▼  (3) IommuResource → stub IOMMU → BTI
         │
         ▼  (4) contiguous VMO, pin, map RW
         │
         ▼  (5) CREATE_QL_TIPC_DEV(ns_mem_id, size)
    ret != 0 ─────────────► TIPC: dead (create N)
         │
         ▼  (6) HANDLE_CMD CONNECT  echo port + NUL
 Trusty: connect_async, handle_wait(READY, 5s)
 ktipc:  accept  →  CONNECTED, READY consumed
    fail ─────────────────► TIPC: dead (connect)
         │
         ▼  (7) HANDLE_CMD SEND  "ping"
    fail ─────────────────► TIPC: dead (send)
         │
         ▼  (8) GET_EVENT(handle, 5000 ms)  wait MSG
    timeout / HUP ────────► TIPC: dead (msg)
         │
         ▼  (9) RECV
    payload != ping ──────► TIPC: dead (echo mismatch)
         │
         ▼  (10) DISCONNECT + SHUTDOWN + unpin
         │
         ▼
 TIPC: alive (echo 4 bytes)
```

| Node | What runs in S-EL1 |
| --- | --- |
| (2) | lib/sm fastcall |
| (5) | `ql_tipc_create_device` maps the NS page |
| (6) | `dev_connect` + ktipc accept |
| (7) | `ipc_send_msg` → `echo_on_message` → `ktipc_send` |
| (8) | `handle_wait` MSG (stdcall blocks; ktipc can run) |
| (9) | `ipc_get_msg` / `ipc_read_msg` into the NS window |

Yielding HANDLE_CMD may return interrupted while CONNECT or GET_EVENT
blocks. NS `stdcall()` issues `SMC_SC_RESTART_LAST` until `r0` is a
real status.

### Shared-memory round trip

```text
 tipc  write TipcCmdHdr + payload
         │
         ▼
 VMO  CACHE_CLEAN
         │
         ▼
 zx_smc_call  HANDLE_QL_TIPC_DEV_CMD
   arg1/arg2 = ns_mem_id lo/hi
   arg3      = hdr + payload size
         │
         ▼
 Trusty  memcpy hdr, opcode switch
         │
         ▼
 fill hdr.opcode = REQ|RESP, status, payload
         │
         ▼
 SMC return
         │
         ▼
 VMO  CACHE_INVALIDATE
         │
         ▼
 tipc  read hdr / payload
```

### Serial

```text
TIPC: api version 4
TIPC: tipc0 ql device @ paddr 0x........ size 0x1000
TIPC: connected com.android.trusty.kernel.echo handle 0x20
TIPC: alive (echo 4 bytes)
```

A client that polled READY after CONNECT printed `dead (ready)` /
`timeout waiting for tipc event 0x1`. READY was already consumed in
(6). That wait is gone.

Trusty-side files:

| Path | Role |
| --- | --- |
| [`sdk/lk/lib/trusty/smcall_ql.c`](../../../sdk/lk/lib/trusty/smcall_ql.c) | entity 50 CREATE / SHUTDOWN / HANDLE_CMD |
| [`sdk/lk/lib/trusty/tipc_dev_ql.c`](../../../sdk/lk/lib/trusty/tipc_dev_ql.c) | NS window, CONNECT wait, GET_EVENT timeout |
| [`sdk/lk/lib/ktipc/ktipc.c`](../../../sdk/lk/lib/ktipc/ktipc.c) | server loop at `DEFAULT_PRIORITY` |
| [`sdk/lk/services/ns_echo/`](../../../sdk/lk/services/ns_echo/) | echo port |
| [`sdk/lk/project/qemu-virt-arm64-trusty.mk`](../../../sdk/lk/project/qemu-virt-arm64-trusty.mk) | `WITH_TRUSTY_QL_TIPC_DEV := true` |

## How they differ

```text
 lktee                         tipc
  GET_UUID (EL3)               API_VERSION 4
  version / api                NS page as QL-TIPC device
  NOP per CPU (S-EL1)          CONNECT / SEND / RECV echo
  TEE: alive (N/M)             TIPC: alive (echo 4 bytes)
```

| | `lktee` | `tipc` |
| --- | --- | --- |
| Goal | SPD + S-EL1 alive | IPC payload path |
| Shared memory | none | 4 KiB contiguous NS window |
| CPU pin | every online CPU | CPU 0 only |
| Trusty work | `smc_nop_stdcall` | QL-TIPC + `ns_echo` |

`lktee` passing means ATF and BL32 answer SMCs. `tipc` passing means NS
memory, stdcall restart, ktipc scheduling, and the echo port all work.

From the serial `zxsh` prompt after `make run` / `make kick`:

```text
lktee
tipc list
tipc kick
tipc kick 0
```

Trusty changes need a new BIOS, not only a new ZBI:

```text
make lkboot && make atf && make build
```

| Image | Path |
| --- | --- |
| BIOS | `out/secular-boot-arm64/firmware/arm-trusted-firmware-qemu-bios/secular.bios` |
| ZBI | `out/secular-boot-arm64/secular.zbi` (`/boot/bin/lktee`, `/boot/bin/tipc`, `/boot/config/secular.cfg`) |
