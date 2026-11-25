# TEE

**After this:** you can say which Savior↔TEE wire a hang is on
(forward, vtee, HEE TIPC, SOS_STATE, smc_irq, RPMB), and draw
F14. **First read:** Interaction map, diagrams 1–5 and 11–14,
then RPMB 8–10. **Next:** [`guest.md`](guest.md).

Guests do not enter S-EL1. A guest `SMC` is trapped (`HCR.TSC`)
and Savior either consumes it (PSCI, VHM, GPU) or **forwards** it
to EL3 / TEE with the guest's VMID in `x7`. TIPC and vtee are the
virtio-shaped control plane for Trusty on AHYP. Trusty **storage**
reaches the UFS RPMB LU through a REE proxy (virtio-rpmb **or**
native UFS) — Savior does not emulate the MAC.

Read [`cpu.md`](cpu.md) for the SMC intercept, [`bootstrap.md`](bootstrap.md)
for SPD → BL32 before Savior, [`vmm.md`](vmm.md) for
`TeeVmCreate` on guest start, [`devices.md`](devices.md) for
notifier IRQs and virtio PCI.

This note is every Savior↔TEE wire: probe, VM create/destroy
notify, SMC forward (Savior / OP-TEE / Trustonic / FF-A), guest
TIPC / vtee, SOS GPR window, HEE Trusty leftover, `smc_irq`
(VM notify, **not** TEE complete), GIC dump translate, and the
RPMB pipe Trusty storage uses.

## Principle

1. **TEE is not a Savior VM.** It is BL32 at S-EL1, started by the
   SPD before HEE. Savior talks to it with `arm_smccc_smc` from
   EL1/EL2. Guests never hold a secure TTBR.

2. **Every forwarded SMC is tagged with kernel VMID.**
   `TeeSmcHandler` writes `x7 = vmid` (unless FF-A Direct is
   already carrying the VM). Secure world uses that as client id.
   Guests pick `x0`/`x1`…; they must **not** pick the client id.
   Overwrite is the isolation: SOS and UOS cannot impersonate
   each other by forging `x7`. Leave guest `x7` alone and TEE
   would trust whoever the guest claimed to be.

3. **Probe once at `LK_INIT_LEVEL_PLATFORM`, AHYP only.** Order:
   FF-A ≥ 1.1 → Savior `VM_DESTROYED` → OP-TEE `VM_DESTROYED` →
   Trustonic `MC_FC_INFO`. Failure → `ZX_TEE_VENDOR_NONE` (SMC
   still runs a default passthrough). `kernel.tee.probe=false`
   skips the probe.

4. **VM lifecycle is a notify, not a partition spawn.**
   `zx_guest_tee_vm_create` / `Guest::TeeVmCreate` →
   `NotifyVmCreated`. SOS also calls it on `HC_START_VM` (before
   UOS `eret`). Bitmap `vmid_bitmap_` prevents double-create.
   Destroy retries Savior `SM_ERR_BUSY` up to 100 × 100 ms.

5. **Two secure-looking worlds.** HEE Trusty TAs live inside
   Savior (`sm_handle_smc_call`, leftover HYP stdcall). BL32 is
   S-EL1. Guest vtee uses `SMC_TEE_SECURE_ID` and **does** go
   through `TeeSmcHandler`. Do not mix the kick IRQs.

6. **RPMB is the chip, virtio is a pipe.** Trusty storage RPC
   `STORAGE_RPMB_SEND` is TIPC to a REE proxy, not a Savior
   hypercall. The proxy sends JEDEC frames via `-t virt`
   (virtio-rpmb SG_IO) or `-t ufs` (native UFS RPMB). Savior
   does not hold the write counter or re-MAC virtio frames.

## Framework

```text
 Guest EL1h  smc
    HCR.TSC  →  handle_smc_instruction
       Savior HYP / PSCI / GPU     handled here, not forwarded
       else TeeVendor::TeeSmcHandler
              │
              ▼
        FF-A?  ffa_smc_handler  (RXTX, direct msg, mem share)
        else   x7 = vmid
               Savior / OP-TEE / Trustonic / default  arm_smccc_smc
              │
              ▼
 EL3  BL31  SPD  →  S-EL1 TEE
```

Host TAs (HEE Trusty apps) use Zircon TIPC syscalls
(`zx_tipc_port_create` / `connect` / `accept`). Guest Linux uses
virtio-TIPC + `vtee` with `SMC_TEE_SECURE_ID`, which **does**
go through `TeeSmcHandler`.

### Interaction map

| Wire | Direction | Transport | Notes |
| --- | --- | --- | --- |
| Probe | host kernel → EL3 | `arm_smccc_smc` at `LK_INIT_LEVEL_PLATFORM` | FF-A then Nebula / OP-TEE / Trustonic |
| VM created / destroyed | host → TEE | vendor SMC or FF-A `VM_CREATED` | before UOS `eret` |
| Guest TEE SMC | guest EL1 → BL32 | `HCR.TSC` → `TeeSmcHandler` → EL3 SPD | `x7` overwritten with kernel VMID |
| Guest HEE TIPC | guest → HEE Trusty | `SMC_ENTITY_NEBULA` + `SMC_HYP_SECURE_ID` leftover → `sm_handle_smc_call` | virtio-TIPC id 13 |
| Guest vtee | guest → BL32 | `SMC_TEE_SECURE_ID` (0) in `x7` | **not** HYP; goes to `TeeSmcHandler` |
| TIPC kick | HEE → guest VCPU | `TipcVqueueNotifier` → AHYP `trusty::vcpu_kick` / TEE bootmode NS IRQ | lua `setTipcVqNotifierIrq` |
| vtee notifier | host GIC → guest | SPI on VGIC passthrough list + DTB `/tee` | lua `setVteeNotifierIrq`; not the TIPC notifier |
| SOS GPR window | host → TEE | `SMC_FC_NBL_SOS_STATE` at SOS `Vcpu::Create` | PA of `El2State.guest_state`; no UOS_STATE |
| `smc_irq` | VM → other VM | ringbuf / notifybuf + VIRTUAL inject | **not** TEE SMC complete |
| GIC dump SIP | guest → ATF via TEE path | `MTK_SIP_KERNEL_GIC_DUMP` | translate vCPU↔pCPU then `TeeSmcHandler` |
| RPMB | Trusty TA → UFS LU | TIPC `STORAGE_RPMB_SEND` → REE proxy | not a Savior SMC |
| Boot order | ATF SPD | BL32 `ERET` S-EL1 **before** BL33 Savior | TEE is not a Guest |

`x7` packing: `SMC_SECURE_ID` is bits 31:16 (`(x7 >> 16) & 0xffff`).
`SMC_TEE_SECURE_ID = 0`, `SMC_HYP_SECURE_ID = 1`.
`validate_hypervcall` IABTs **only** `SMC_SC_NBL_VHM_REQ`,
`SMC_FC_NBL_VHM_REQ`, and `SMC_SC_NBL_VIRTGPU_DBG_MEM` when
`secure_id != 1`. Other FIDs with a wrong id are not IABT’d.
HYP id with a non-NEBULA entity only logs a warning and continues.

Swimlane: [`flows.md`](flows.md) F3 (SMC class), F14 (TEE/FF-A),
F19 (RPMB).

## Diagrams

### 1. SMC classes

```text
 guest  SMC  x0 = FID

 ENTITY_NEBULA + HYP_SECURE_ID     hypervisor  (VHM, GPU, IRQ, vmlog)
 PSCI64 / HYPERVISER_GUEST_*       PSCI / PM   (cpu.md)
 everything else                   TEE
```

Illegal **VHM / virtgpu-dbg** FIDs with `secure_id != 1` inject a
guest IABT (`VBAR_EL1+0x200`). They are not forwarded. Other
mismatches are not IABT.

### 2. Probe (once, AHYP)

```text
 LK_INIT_LEVEL_PLATFORM  probe_tee_vendor
    bootmode != AHYP?                 return
    kernel.tee.probe == false?        return
    TeeVendor::Probe
       │
       ├─ ffa_probe
       │     VERSION **exactly** 1.1 (`FFA_MIN_SPMC_VERSION` == `FFA_HYP_VERSION`)
       │     map host RX/TX (1 page each)
       │     PARTITION_INFO_GET → UUID
       │     OP-TEE or Trustonic UUID → SetVendorId(..., ffa=true), stop
       │     no match / version miss → fall through (not Nebula-on-FFA)
       │
       ├─ NEBULA_SMC_FC_VM_DESTROYED  →  ZX_TEE_VENDOR_NEBULA
       ├─ OPTEE_SMC_VM_DESTROYED      →  ZX_TEE_VENDOR_OPTEE
       ├─ MC_FC_INFO                  →  ZX_TEE_VENDOR_TRUSTONIC
       └─ else                        →  ZX_TEE_VENDOR_NONE
                                          (default passthrough still runs)
```

NONE is not “TEE is dead”. Forward still happens; there is no
vendor-specific rewrite of caps / SHM.

### 3. VM create / destroy notify

```text
 Vmm::Initialize  (SOS / tbox)  and  SOS HC_START_VM  (UOS)
    zx_guest_tee_vm_create(user vmid)
       UserVmidtoTeeVmid   −1→1  0→2  1→3
       NotifyVmCreated(tee_vmid)
          already in vmid_bitmap_?  ZX_ERR_INVALID_ARGS
          FF-A  FFA_MSG_SEND_VM_CREATED  (DIRECT_REQ to SP)
          Savior  NEBULA_SMC_FC_VM_CREATED   (x0 forced ZX_OK)
          OP-TEE  OPTEE_SMC_VM_CREATED
          Trustonic / NONE  no-op, ZX_OK
          SetVmidBitmap
    then UOS/SOS  eret

 Guest teardown / SOS HC_DESTROY_VM
    NotifyVmDestroyed
       not in bitmap?  log, ZX_OK
       Savior  VM_DESTROYED, retry SM_ERR_BUSY  100 × 100 ms
       OP-TEE  VM_DESTROYED
       Trustonic  MC_FC_VM_DESTROY + SIQ + YIELD until NOT_INITIALIZED
       FF-A  FFA_MSG_SEND_VM_DESTROYED, destory_ffa_ctx
       ClearVmidBitmap
```

SOS `HC_START_VM` notifies TEE **before** `file://uos` `eret`.
A second `TeeVmCreate` for the same kernel VMID logs
`tee vm already exists`.

### 4. Forwarded SMC (legacy)

```text
 TeeSmcHandler(guest_state, vmid)
    FF-A enabled?  →  diagram 5
    else
       x7 = vmid                         overwrite guest x7
       switch vendor
          NEBULA     arm_smccc_smc, copy x0–x3,x6 back
          OP-TEE     strip reserved-SHM cap
                     require DYNAMIC_SHM else ENOTAVAIL
          Trustonic  swallow VM_DESTROY / REGISTER_SHM / RECLAIM_SHM
                     VM_CREATE success → SetVmidBitmap
          NONE       passthrough
```

There is **no** `/features/tee-smc-timeout` in this tree. A hung
TEE SMC blocks that VCPU thread until EL3 returns.

### 5. FF-A Direct (when `ffa_probe` succeeds)

Per-VM `ffa_ctx`: RX/TX IPA, refcnt, `dying`. Host keeps one
global RX/TX page pair for EL3. Direct VM→SP only, under
`ffa_direct_msg_lock`.

```text
 guest  smc  x0 = FFA_*
    is_ffa_call?
       no   x7 = vmid, guest_smc_call            (ordinary TEE FID)
       yes  guest_ffa_smc_call
              FFA_ID_GET              x1 = vmid, kernel success
              FFA_RXTX_MAP            map guest pages, cap page count
              FFA_RXTX_UNMAP          wait refcnt 0 if dying
              FFA_PARTITION_INFO_GET  copy host-probed SP into guest RX
              FFA_RX_RELEASE          clear busy
              FFA_MEM_SHARE           VM→one SP, ≤ 1 MiB, ≤ 32 regions
              FFA_MEM_FRAG_TX         fragment of a share
              FFA_MSG_SEND_DIRECT_REQ
                 lock ffa_direct_msg_lock
                 guest_smc_call                  EL3 → SP
              FFA_FEATURES / VERSION / MEM_RECLAIM
                 passthrough
              else  FFA_RET_NOT_SUPPORTED
```

Notifications / `FFA_RUN` stay in the header comment list; they
are not silently success.

**Limits** (`ffa.h` + `ffa.cpp`):

| Call | Cap |
| --- | --- |
| `RXTX_MAP` | `page_count ≤ FFA_MAX_RXTX_PAGE_COUNT` (**1** 4 KiB page each). Maps guest IPA into host VMAR as `ctx->rx`/`tx` |
| `MEM_SHARE` | VM → **one** SP; descriptor in TX; `addr` must be 0 (no separate descriptor GPA); `frag_len ≤ page_count×4K`; copy into host `ffa_tx` then `guest_smc_call`; ≤ `FFA_MAX_SHM_COUNT` (**32**) live shares; ≤ `FFA_MAX_SHM_PAGE_COUNT` (**256** pages ≈ 1 MiB) |
| `MEM_FRAG_TX` | next fragment, same TX copy |
| `MEM_RECLAIM` | passthrough to EL3 |
| DONATE / LEND / RETRIEVE / SEND2 / RUN | **not implemented** (`FFA_RET_NOT_SUPPORTED`) |

`MEM_SHARE` takes `ffa_tx_buffer_lock` around the copy+SMC (one
VM can stall another). `dying` or `rxtx_busy` → `FFA_RET_DENIED`.

### 6. Guest TIPC / vtee kick (AHYP)

Two stacks, two IRQs. Do not swap them.

```text
 virtio-TIPC  (HEE)
    lua  setTipcVqNotifierIrq     QEMU SOS 245 / SoC SOS 286
    TipcVqueueNotifier(irq, vmid)
       zx_vqueue_notifier_create / bind_vcpu
    guest kicks VQ  →  AHYP  trusty::vcpu_kick
                       TEE bootmode  NS IRQ

 vtee  (BL32)
    lua  setVteeNotifierIrq       QEMU SOS 513 / SoC SOS 174
                                  SoC UOS 179 / tbox 173
    added to VGIC passthrough list + DTB /tee interrupts
    not the TIPC vqueue_notifier
```

Wrong bootmode or swapping the two IRQs → hung IPC.

### 7. SOS GPR window (`SOS_STATE`)

On `Vcpu::Create`, SOS SMC EL3 (host kernel, not a guest SMC):

```text
 SMC_FC_NBL_SOS_STATE
    x1 = vpid - 1
    x2 = &El2State.guest_state     (PA)
         │
         ▼
 TEE can read SOS GPRs
```

There is no `SMC_FC_NBL_UOS_STATE` in this tree. tbox does not
register.

### 11. Two worlds, two `x7` ids

```text
 Guest  smc
    x7.secure_id
       1  SMC_HYP_SECURE_ID
          ENTITY_NEBULA  →  VHM / GPU / vmlog  or  sm_handle_smc_call
                            (HEE Trusty: virtio-TIPC, SHARED_LOG, NOP, …)
       0  SMC_TEE_SECURE_ID
          not (NEBULA && HYP), not PSCI  →  TeeSmcHandler  →  EL3  →  BL32
          vtee / OP-TEE / Trustonic / SIP (GIC dump) FIDs
```

Do not put TIPC (HEE) on the vtee IRQ, or vtee (BL32) on the TIPC
notifier. [`system.md`](system.md) IRQ table.

### 12. Guest vtee → BL32

```text
 Guest  vtee_probe
    smc_secure_id = 0
    nebula_init_api_version     smc  (TEE FIDs, x7 = 0)
         │  HCR.TSC
         ▼
 handle_smc_instruction
    not (ENTITY_NEBULA && HYP_SECURE_ID)
    not PSCI
    TeeSmcHandler(guest_state, tee_vmid)
       x7 = kernel VMID          overwrite 0
       nebula_tee_smc_handler
          arm_smccc_smc(x0..x6, x7=vmid)
         │
         ▼
 EL3  SPD  S-EL1 TEE
    copy x0–x3, x6 back into GuestState
    eret guest                   same VCPU thread
```

Yielding Trusty calls (`SMC_SC_NOP`, `RESTART_LAST`, virtio
descr) that use **HYP** secure_id stay in HEE (`sm_handle_smc_call`).
vtee’s API-version / std calls with secure_id 0 are BL32.

### 13. HEE Trusty leftover (not BL32)

```text
 Guest  virtio-TIPC  (VIRTIO_ID_TRUSTY_IPC = 13)
    SMC_ENTITY_NEBULA  x7.secure_id = 1
    SMC_SC_NBL_VIRTIO_* / SHARED_LOG / NOP / …
         │
         ▼
 handle_smc_instruction
    HYP path, not VHM_REQ, not FC_NBL_VHM_REQ
    sm_handle_smc_call(x0,x1,x2,x3, x7)
       fastcall  sm_fastcall_table[entity]
       stdcall   sm_stdcall_table[entity]
         │
         ▼
 HEE Trusty TAs     (Savior EL0, zx_tipc_*)
    kick guest:  AHYP  trusty::vcpu_kick
                 TEE bootmode  NS IRQ
```

Host TAs never SMC from a guest. They are Zircon processes.

### 14. `smc_irq` is a VM doorbell, not TEE-complete

`zx_guest_set_smc_irq` / lua `setSmcIrq` stores a virtual SPI on
`Guest`. Inject happens in `Guest::NotifyVmMessage` and the
`HYPERVISER_GUEST_OS_WRITE` path: write 32 B into the **other**
VM’s ringbuf/notifybuf, then `Vcpu::Interrupt(smc_irq)` on that
VM’s VCPU 0.

```text
 VM A  SMC  OS_WRITE  target_vmid  payload
    WriteToRingBuffer / NotifyBuffer
    Interrupt(target.smc_irq_)     VIRTUAL
 VM B  handler reads ringbuf
```

`TeeSmcHandler` does **not** inject `smc_irq`. A forwarded TEE
SMC returns in `x0–x3,x6` on the **same** `zx_vcpu_resume`.
SoC SOS `setSmcIrq(984)` (see [`README.md`](README.md) Two configs).

### 15. GIC dump SIP (guest → ATF, via TEE handler)

```text
 Guest  SMC  MTK_SIP_KERNEL_GIC_DUMP
    FIND_ACTIVE_IRQ
       x2 vCPU  →  translate to pCPU
       TeeSmcHandler
    GIC_CPU_DUMP
       TeeSmcHandler
       ATF result bits 14..29 = pCPU mask
       translate back to vCPU mask in x0
```

This is still `TeeSmcHandler` because the FID is not HYP/PSCI.
It is ATF SIP, not BL32 storage.

### 16. Boot: SPD then Savior

```text
 BL31
    SPD  ERET S-EL1  BL32 TEE     (before HEE)
    SMC back
    ERET EL2h  boot-shim → Savior
 Savior  probe_tee_vendor         LK_INIT_LEVEL_PLATFORM
 Guest   TeeVmCreate              notify TEE the VM exists
 SOS VCPU0  SMC_FC_NBL_SOS_STATE  GPR window
```

TEE is never `zx_vcpu_create`’d.

## RPMB

Replay Protected Memory Block is a **UFS well-known LU**. Savior
does not emulate the MAC or the write counter. It **forwards
guest frames** to the host UFS driver (SCSI SECURITY PROTOCOL).
The controller checks HMAC-SHA256 and the monotonic counter.

QEMU skips virtio-rpmb (`is_qemu_platform()`). SoC `Vmm::Initialize`
(SOS **and** tbox) calls `InitializeVirtioRpmb`. UOS uses
`platform_init_rpmb` (PCI connect only — see pitfall).

### Why it sits in this note

Trusty keymaster / storage keep keys in RPMB. The TA talks TIPC
`STORAGE_RPMB_SEND` to a REE proxy (`storageproxyd`). That is
the secure-storage path; the SMC forward path above never sees
the frames. The proxy’s `-t` selects **virt** (virtio-rpmb) or
**ufs** (direct LU). Default in `mtk-ufs-proxy.c` is `MMC_RPMB`.

### RPMB principle

1. **The chip is the authority.** A 512-byte JEDEC frame carries
   data, nonce, write counter, address, and a 32-byte MAC. The
   programmed authentication key lives in the RPMB LU. A forged
   write without the key fails `Authentication failure`.

2. **Virtio is a pipe, not a second RPMB.** Guest
   `virtio-rpmb` (`VIRTIO_ID_UFS_RPMB` = 56) kicks one virtqueue.
   The Vmm copies frames into a shared packet and wakes the host
   UFS `vrpmb` worker. That worker calls `ufs_rpmb_request`.

3. **HMAC is built by the client, verified by the device.**
   Guest userspace / Trusty proxy fills `key_mac`. Host
   `ufs_rpmb_request` **does not re-MAC** those frames. A
   separate host helper (`rpmb_write_block_locked`) *does* MAC
   for Savior’s own reads/writes; virtio does not use it.

4. **Region is in the request, not a VM sandbox.** Frames name
   region 0–3. The backend session is keyed by
   `rpmb_vmid = user_vmid + 1` (SOS −1 → 0, UOS 0 → 1, tbox 1 →
   2). That isolates **sessions**, not RPMB regions.

5. **One in-flight request per session.** IPC virtqueue depth is
   1. The Vmm holds `ipc_lock_` across kick → wait → copy-back.
   The UFS host lock serializes SECURITY PROTOCOL against other
   host RPMB users.

### RPMB framework

```text
 Trusty TA  STORAGE_RPMB_SEND     (TIPC, not a Savior SMC)
    │
    ▼
 SOS storageproxyd  /  Guest  /dev/virtio-rpmb
    ioctl SG_IO  rpmb_request { req_frm, rsp_frm, nfrm }
    virtqueue  sgs[req, rsp]  kick
         │  PCI virtio  BAR1 notify  (VIRTUAL IRQ back)
         ▼
 Vmm  VirtioRpmb                 (machina, EL0)
    HandleCommand
    copy frames → shm packet_
    virtq_frontend_to_avail + eventpair
         │  /dev/sys/platform/64:14:1/sdc/vrpmb
         ▼
 Host UFS driver  vrpmb session   (garnet/drivers/block/ufs)
    worker pCPU 5, prio 17
    ufs_rpmb_request(region, frames)
         │  SCSI SECURITY PROTOCOL 0xEC / RPMB id 1
         ▼
 UFS RPMB LU                     (device)
    HMAC + write counter
    response frames
```

Backend path: `/dev/sys/platform/64:14:1/sdc/vrpmb`. Missing node
→ SOS `InitializeVirtioRpmb` logs warning and **skips** the PCI
device (`ZX_OK`).

### 8. RPMB objects

```text
 Guest
   virtio-rpmb FE     PCI  vendor 1af4  virtio id 56 on the Vmm
                      (ALPS driver binds VIRTIO_ID_RPMB=28 unless
                       VIRTIO_ID_UFS_RPMB is predefined — see pitfall)
   chardev            SG_IO / SG_SET_FORCE_PACK_ID (region)
        │
 Vmm  VirtioRpmb
   queue[0] size 128     guest virtqueue
   shm VMO + eventpair   IPC to host driver
   packet_               ufs_virtio_rpmb_packet_t
        │ ioctl START / GET_HANDLE / STOP
 Host  g_ufs_virtio_rpmb_backend
   sessions[4]           one per rpmb_vmid
      shm_vmo, event[2], worker thread
   ufs_rpmb_request      hba lock
        │
 UFS   RPMB WLUN 0xC4
```

`rpmb_vmid = guest_.vmid() + 1`. User vmid −1/0/1 → session 0/1/2.
`UFS_VIRTIO_RPMB_MAX_GUESTS` is 4.

### 9. Authenticated write

```text
 Guest  build frames
    data[256], wr_cnt, addr, blk_cnt
    req_resp = 0x0003  AUTHEN_WRITE
    key_mac  = HMAC-SHA256(key, data||nonce||wr_cnt||addr||blk_cnt||result||req_resp)
               // 284 B; stuff and key_mac are not in the MAC input
    ioctl SG_IO
         │
 [1] virtqueue  desc0 = req (RO)  desc1 = rsp (WO)
     BAR1 notify  →  VirtioRpmb::HandleCommand
 [2] check region 0–3, frame_num 1–256
     memcpy packet_->req_frame
     magic = 0x5652504d  ("VRPM")
 [3] virtq_frontend_to_avail(slot 0)
     eventpair  →  host worker
 [4] ufs_rpmb_request
       SECURITY PROTOCOL OUT  frames
       RESULT_RD OUT
       SECURITY PROTOCOL IN   response
 [5] device
       MAC match?  wr_cnt == programmed+1?
       yes: program data, bump counter
       no:  result = auth / counter failure
 [6] copy rsp_frame → guest desc1
     *used = header + rsp_bytes
     VIRTUAL IRQ  →  guest complete
```

Read / get-counter skip RESULT_RD: OUT then IN only.

### 10. Session start (SOS Vmm)

```text
 Vmm::InitializeVirtioRpmb     // not QEMU
    rpmb_vmid = user_vmid + 1
    VirtioRpmb::Init
       open /dev/.../vrpmb
       GRT_VRPMB_IOCTL_START     { vmid }
          alloc session, VMO, eventpair
          thrd  ufs-rpmb-vq  affinity 0x20 (pCPU 5)
       GET_HANDLE shm VMO
       GET_HANDLE eventpair end 0
       map shm, virtq_frontend_init
       PollAsync queue 0
    PciBus::Connect
```

UOS `platform_init_rpmb` **Connects PCI without `Init()`**.
Backend session / shm are not created on that path. SOS is the
product path that actually talks to `vrpmb`.

### Frame

Packed 512 bytes (`ufs_virtio_rpmb_frame_t` /
`struct rpmb_data_frame`):

| Offset | Field | Size |
| --- | --- | --- |
| 0 | stuff | 196 |
| 196 | key_mac | 32 |
| 228 | data | 256 |
| 484 | nonce | 16 |
| 500 | wr_cnt | 4 |
| 504 | addr | 2 |
| 506 | blk_cnt | 2 |
| 508 | result | 2 |
| 510 | req_resp | 2 |

`req_resp` is big-endian. Guest → device:

| req | Name | Host `ufs_rpmb_request` |
| --- | --- | --- |
| `0x0001` | key-program | OUT + RESULT_RD |
| `0x0002` | read-counter | OUT + IN |
| `0x0003` | authenticated-write | OUT + RESULT_RD |
| `0x0004` | authenticated-read | OUT + IN |
| `0x0005` | result-read | **not** a virtio standalone; host uses it after write/key |
| `0x0006` / `0x0007` | sec-wpcb | virtio names them; host switch is **unsupported** |

Response types are `req << 8` (`0x0100` … `0x0400`).

### Two clients, one LU

| Client | How it reaches the LU | MAC |
| --- | --- | --- |
| Guest virtio-rpmb | this section | client fills `key_mac` |
| Host helpers `ufs_rpmb_read` / `_write` | `rpmb_get_mac` / `rpmb_verify_mac` in `ufs_rpmb.c` | host |
| HEE Trusty keymaster / storage | `SYS_PLATFORM_GET_RPMB_KEY` (hwcrypto UUID only) + storage proxy | Trusty |
| SOS `storageproxyd` | `-t ufs` / `virt` → `/dev/rpmb` or virtio SG_IO | proxy |

Two **different** key paths. Do not mix them:

| Who | How |
| --- | --- |
| HEE hwcrypto (`SYS_PLATFORM_GET_RPMB_KEY`) | ATF `SMC_FC_PLAT_GET_RPMB_KEY` at `board_mtk_ahyp_init`, cached `rpmb_key[32]`; ioctl only if caller UUID is hwcrypto |
| Host UFS `rpmb_get_mac` (host helpers, not virtio) | SIP `MTK_SIP_UFS_CONTROL` / `UFS_MTK_SIP_RPMB_KEY` per region |
| Virtio frames | client already filled `key_mac`; `ufs_rpmb_request` does not look up a key |

Info: `SYS_PLATFORM_GET_RPMB_INFO` (`dcfg_hyp_rpmb_t`, programmed
flag).

## Forwarding

`TeeSmcHandler`:

| Vendor | What happens |
| --- | --- |
| FF-A | `ffa_smc_handler`. Direct VM→SP only. Share ≤ 1 MiB (256 pages), ≤ 32 regions |
| Savior (`ZX_TEE_VENDOR_NEBULA`) | `arm_smccc_smc` passthrough, `x7` already vmid |
| OP-TEE | strip reserved SHM cap, require dynamic SHM |
| Trustonic | `MC_FC_VM_CREATE/DESTROY/REGISTER_SHM` special-cased |
| NONE | `default_smc_handler` |

## SOS / UOS GPR window

See diagram 7. Host kernel → EL3 at SOS VCPU create.

## Pitfalls

- **Guest HVC is not TEE.** Guest `hvc` returns `x0 = -1`.
  Drivers must `smc`.
- **Do not trust guest `x7`.** Savior overwrites it.
- **Double `TeeVmCreate`.** Bitmap rejects; UOS start logs
  `tee vm already exists`.
- **AHYP vs TEE TIPC kick.** Wrong bootmode uses the wrong
  doorbell (VCPU kick vs NS IRQ).
- **Hung TEE SMC.** There is no tee-smc-timeout node in this
  tree. The VCPU thread blocks in `arm_smccc_smc` until EL3
  returns.
- **TIPC vs vtee IRQ numbers.** [`system.md`](system.md) table.
  Mixing 286 and 174 is a silent hang, not a probe fail.
- **QEMU has no RPMB.** `InitializeVirtioRpmb` is under
  `!is_qemu_platform()`.
- **RPMB backend node missing.** SOS treats `Init()` failure as
  skip, not a Vmm abort.
- **UOS RPMB `Init()` not called.** PCI device exists; `vrpmb`
  session does not. Expect SOS virtio-rpmb unless that path is
  fixed.
- **RPMB queue depth 1.** A second SG_IO waits on `ipc_lock_` /
  host `hba->lock`.
- **RPMB region is guest-chosen.** Session vmid ≠ RPMB region.
- **RPMB worker pinned to pCPU 5.** Occupancy PAUSE of that core
  stalls RPMB, not the VCPU that issued SG_IO.
- **Virtio device id 56 vs 28.** Vmm advertises `VIRTIO_ID_UFS_RPMB`
  **56**. In-tree ALPS `virtio_rpmb.c` `#define`s that to
  `VIRTIO_ID_RPMB` **28** unless the kernel uapi already has 56
  (it does not in `kernel-6.6/include/uapi/linux/virtio_ids.h`).
  A guest built that way will not bind the host device.
- **HMAC input is 284 B from `data` through `req_resp`.** Not the
  whole 512 B frame.

## Terminology

| Term | Meaning here |
| --- | --- |
| TEE / BL32 | Secure EL1 payload. Not a Guest |
| SPD | EL3 dispatcher that `ERET`s into TEE at boot |
| FF-A | Firmware Framework for Arm. Optional transport |
| TIPC | Trusty IPC. Host syscalls vs guest virtio |
| vtee | Guest virtio frontend for BL32 |
| `SOS_STATE` | PA of SOS `GuestState` published to TEE |
| client id | kernel VMID placed in `x7` |
| HEE Trusty | TAs inside Savior; leftover HYP stdcall, not BL32 |
| `SMC_TEE_SECURE_ID` | `x7` high 16 bits = 0 → `TeeSmcHandler` |
| `SMC_HYP_SECURE_ID` | `x7` high 16 bits = 1 → VHM/GPU or `sm_handle_smc_call` |
| `smc_irq` | VIRTUAL doorbell for VM ringbuf/notifybuf; not TEE complete |
| RPMB | UFS replay-protected LU; HMAC + monotonic counter |
| Frame | 512-byte JEDEC RPMB data frame |
| virtio-rpmb | Guest FE + Vmm `VirtioRpmb`; host virtio id **56** |
| vrpmb | Host UFS ioctl backend `/dev/.../sdc/vrpmb` |
| rpmb_vmid | `user_vmid + 1`, session index, not kernel VMID |

## Code

| Step | File |
| --- | --- |
| Probe / notify / forward | `zircon/kernel/arch/arm64/hypervisor/tee/tee.cpp` |
| FF-A | `tee/ffa.cpp`, `include/arch/arm64/tee/ffa.h` |
| FF-A FID switch | `ffa.cpp` `guest_ffa_smc_call` |
| SMC intercept | `vmexit.cpp` `handle_smc_instruction` |
| `TeeVmCreate` | `guest.cpp`, `syscalls/hypervisor.cpp` |
| Host TIPC syscalls | `zircon/kernel/syscalls/trusty.cpp` |
| Guest vqueue kick | `zircon/trusty/lk/trusty/lib/trusty/tipc_dev.cpp` |
| Guest vtee | `linux/drivers/nebula/vtee.c` |
| Lua IRQs / `setSmcIrq` | `garnet/bin/guest/sos/qemu/sos.lua`, `sos_mt8678.lua` |
| HEE leftover | `zircon/trusty/lk/trusty/lib/sm/sm.c` `sm_handle_smc_call` |
| Guest TIPC | `linux/drivers/nebula/trusty-ipc.c` |
| SOS_STATE | `vcpu.cpp` `Vcpu::Create` |
| Guest virtio-rpmb FE | `alps-v0/.../virtio_rpmb/virtio_rpmb.c` |
| Vmm SOS RPMB | `garnet/bin/guest/vmm.cc` `InitializeVirtioRpmb` |
| Virtio RPMB BE | `garnet/lib/machina/virtio_rpmb.cc` |
| Host vrpmb session | `garnet/drivers/block/ufs/vrpmb/virtio_rpmb.c` |
| UFS LU | `garnet/drivers/block/ufs/core/ufs_rpmb.c` `ufs_rpmb_request` |
| RPMB key ioctl | `zircon/third_party/lib/thyper/board_mtk_comm.cpp` |
| Trusty storage proxy | `yocto/.../mtk_storageproxyd/trusty/storage/proxy/rpmb.c` |

---

Prev: [`gpu.md`](gpu.md) · Next: [`guest.md`](guest.md) · [`README.md`](README.md)

Everyday's a Hustle.
