# TEE

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
   `TeeSmcHandler` writes `x7 = vmid` (unless FF-A). Secure world
   uses that as client id. SOS and UOS therefore cannot impersonate
   each other by forging `x7`.

3. **Probe once at `LK_INIT_LEVEL_PLATFORM`, AHYP only.** Order:
   FF-A ≥ 1.1 → Savior `VM_DESTROYED` → OP-TEE `VM_DESTROYED` →
   Trustonic `MC_FC_INFO`. Failure → `ZX_TEE_VENDOR_NONE` (SMC
   still runs a default passthrough).

4. **VM lifecycle is a notify, not a partition spawn.**
   `zx_guest_tee_vm_create` / `Guest::TeeVmCreate` →
   `NotifyVmCreated`. SOS also calls it on `HC_START_VM`. Bitmap
   `vmid_bitmap_` prevents double-create. Destroy retries Savior
   `SM_ERR_BUSY` up to 100 × 100 ms.

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

### 2. Probe

```text
 ffa_probe  version ≥ 1.1, map RX/TX, PARTITION_INFO_GET
    OK  →  ffa_enabled_, vendor from partition UUID
 NEBULA_SMC_FC_VM_DESTROYED  →  ZX_TEE_VENDOR_NEBULA
 OPTEE_SMC_VM_DESTROYED      →  ZX_TEE_VENDOR_OPTEE
 MC_FC_INFO                  →  ZX_TEE_VENDOR_TRUSTONIC
 else                        →  ZX_TEE_VENDOR_NONE
```

### 3. Guest TIPC on AHYP

```text
 Guest virtio-TIPC / vtee
    lua  setTipcVqNotifierIrq(245)  setVteeNotifierIrq(513)
    zx_vqueue_notifier_create(kick_irq, vmid)
    zx_vqueue_notifier_bind_vcpu
    kick  →  trusty::vcpu_kick  (this tree, AHYP)
             (TEE bootmode kicks NS IRQ instead)
```

## Forwarding

`TeeSmcHandler`:

| Vendor | What happens |
| --- | --- |
| FF-A | `ffa_smc_handler`. Direct VM→SP only. Share ≤ 512 KiB, ≤ 32 regions |
| Savior (`ZX_TEE_VENDOR_NEBULA`) | `arm_smccc_smc` passthrough, `x7` already vmid |
| OP-TEE | strip reserved SHM cap, require dynamic SHM |
| Trustonic | `MC_FC_VM_CREATE/DESTROY/REGISTER_SHM` special-cased |
| NONE | `default_smc_handler` |

Optional DTB `/features/tee-smc-timeout/timeout-us`: log when a
forwarded SMC exceeds that runtime on the VCPU thread.

### FF-A (when `ffa_probe` succeeds)

Per-VM `ffa_ctx`: RX/TX IPA, refcnt, `dying`. Host keeps one
global RX/TX page pair for EL3.

| FID | Savior |
| --- | --- |
| `FFA_ID_GET` | `x1 = vmid`, success in kernel |
| `FFA_RXTX_MAP_32/64` | map guest pages into host VMAR, cap `FFA_MAX_RXTX_PAGE_COUNT` |
| `FFA_RXTX_UNMAP` | unmap, wait refcnt 0 if dying |
| `FFA_PARTITION_INFO_GET` | copy host-probed SP info into guest RX |
| `FFA_RX_RELEASE` | clear busy |
| `FFA_MEM_SHARE_32/64` | VM→one SP, ≤ 512 KiB, ≤ 32 regions |
| `FFA_MEM_FRAG_TX` | fragment of a share |
| `FFA_MSG_SEND_DIRECT_REQ_*` | `guest_smc_call` under `ffa_direct_msg_lock` (VM→SP only) |
| `FFA_FEATURES` / `VERSION` / `MEM_RECLAIM` / notifications / `FFA_RUN` | passthrough `guest_smc_call` |
| other FID | `x7 = vmid`, passthrough |

VM create/destroy: `FFA_MSG_SEND_VM_CREATED/DESTROYED` to
`ffa_partition_info.id`, retry `INTERRUPTED`/`RETRY`. Then
`create_ffa_ctx` / `destory_ffa_ctx`.

Unimplemented FFA ops stay in the header comment list; they are
not silently success.

## SOS / UOS GPR window

On `Vcpu::Create`, SOS and ALPS SMC EL3:

```text
 SMC_FC_NBL_SOS_STATE  /  SMC_FC_NBL_UOS_STATE
    x1 = vpid-1
    x2 = &El2State.guest_state   (PA)
```

TEE can then read that guest's GPRs. Host kernel → EL3, not a
guest SMC. tbox does not register.

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

## Terminology

| Term | Meaning here |
| --- | --- |
| TEE / BL32 | Secure EL1 payload. Not a Guest |
| SPD | EL3 dispatcher that `ERET`s into TEE at boot |
| FF-A | Firmware Framework for Arm. Optional transport |
| TIPC | Trusty IPC. Host syscalls vs guest virtio |
| `SOS_STATE` | PA of SOS `GuestState` published to TEE |
| client id | kernel VMID placed in `x7` |

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

Everyday's a Hustle.
