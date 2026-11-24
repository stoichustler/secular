# TEE

**After this:** you can say why an SMC never reached TEE.
**First read:** Diagrams 1–5. **Next:** [`guest.md`](guest.md).

Guests do not enter S-EL1. A guest `SMC` is trapped (`HCR.TSC`)
and Savior either consumes it (PSCI, VHM, GPU) or **forwards** it
to EL3 / TEE with the guest's VMID in `x7`. TIPC and vtee are the
virtio-shaped control plane for Trusty on AHYP.

Read [`cpu.md`](cpu.md) for the SMC intercept, [`bootstrap.md`](bootstrap.md)
for SPD → BL32 before Savior, [`vmm.md`](vmm.md) for
`TeeVmCreate` on guest start, [`devices.md`](devices.md) for
notifier IRQs.

This note is probe, VM create/destroy notify, SMC forward (Savior /
OP-TEE / Trustonic / FF-A), and guest TIPC.

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
(`zx_tipc_port_create`, numbers ≥ `0x10000`). Guest Linux uses
virtio-TIPC + `vtee` with `SMC_TEE_SECURE_ID`, which **does**
go through `TeeSmcHandler`.

Swimlane: [`flows.md`](flows.md) F3 (SMC class) then F14 (TEE/FF-A).

## Diagrams

### 1. SMC classes

```text
 guest  SMC  x0 = FID

 ENTITY_NEBULA + HYP_SECURE_ID     hypervisor  (VHM, GPU, IRQ, vmlog)
 PSCI64 / HYPERVISER_GUEST_*       PSCI / PM   (cpu.md)
 everything else                   TEE
```

Illegal HYP parameters (`secure_id != SMC_HYP_SECURE_ID` on a
HYP FID) inject a guest IABT (`VBAR_EL1+0x200`). They are not
forwarded.

### 2. Probe (once, AHYP)

```text
 LK_INIT_LEVEL_PLATFORM  probe_tee_vendor
    bootmode != AHYP?                 return
    kernel.tee.probe == false?        return
    TeeVendor::Probe
       │
       ├─ ffa_probe
       │     VERSION ≥ 1.1
       │     map host RX/TX
       │     PARTITION_INFO_GET → vendor UUID
       │     OK  →  ffa_enabled_, stop
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
 Guest::Create / SOS HC_START_VM
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
          Trustonic  swallow VM_DESTROY / REGISTER_SHM
                     VM_CREATE success → SetVmidBitmap
          NONE       passthrough
```

Optional DTB `/features/tee-smc-timeout/timeout-us`: log when a
forwarded SMC exceeds that runtime on the VCPU thread. Missing
node = check off; a hung TEE hangs that VCPU.

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
 Guest virtio-TIPC / vtee
    lua  setTipcVqNotifierIrq     QEMU SOS 245 / SoC SOS 286
         setVteeNotifierIrq       QEMU SOS 513 / SoC SOS 174
                                  SoC UOS 179 / tbox 173
    zx_vqueue_notifier_create(kick_irq, vmid)
    zx_vqueue_notifier_bind_vcpu
         │
         ▼  guest kicks the virtqueue
    AHYP      trusty::vcpu_kick  that VCPU
    TEE mode  NS IRQ instead
```

Wrong bootmode uses the wrong doorbell → hung IPC.

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
- **Timeout DTB missing.** Check is off; hung TEE SMC hangs
  the VCPU thread.
- **TIPC vs vtee IRQ numbers.** [`system.md`](system.md) table.
  Mixing 286 and 174 is a silent hang, not a probe fail.

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
| HEE Trusty | TAs inside Savior; not this forward path |

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
| Lua IRQs | `garnet/bin/guest/sos/qemu/sos.lua` |

---

Prev: [`gpu.md`](gpu.md) · Next: [`guest.md`](guest.md) · [`README.md`](README.md)

Everyday's a Hustle.
