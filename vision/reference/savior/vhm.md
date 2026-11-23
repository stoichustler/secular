# VHM

VHM is the **ACRN-shaped FE/BE** between SOS and UOS. SOS Linux
thinks it is ACRN (`/dev/acrn_hsm`). Savior is the hypervisor
behind those hypercalls. UOS devices that SOS must emulate
(virtio-be, some PCI BARs, mailbox) are not identity-mapped:
they **trap** into an ioreq slot, SOS handles them, then kicks
the UOS VCPU.

tbox does not use this path. GPU SWA does not use this path.
This note is SOS creating UOS, the ioreq page, SYNC vs BELL,
and the two implementations (kernel vs EL0).

Read [`vmm.md`](vmm.md) for `file://uos`, [`cpu.md`](cpu.md) for
the SMC intercept, [`interrupt.md`](interrupt.md) for virq/MSI,
[`devices.md`](devices.md) for which MMIO is trapped,
[`memory.md`](memory.md) for the unmapped IPA.

## Principle

1. **SOS is the backend, UOS is the frontend.** SOS issues
   `SMC_SC_NBL_VHM_REQ` with ACRN `HC_*`. Savior either handles
   the call in EL1 or posts `ZX_PKT_GUEST_VCPU_VHM_REQ` to the
   SOS Vmm (`VhmServiceImpl`). UOS never talks ACRN; its Vmm
   (`VhmDevice`) only installs traps and shares the ioreq page.

2. **One 4 KiB ioreq page, 16 slots of 256 bytes.** Slot `n` is
   UOS VCPU `n`. Slots 14–15 are sideband BELL channels. Layout
   **must** match `struct acrn_io_request` (`processed` at offset
   136). SOS and Savior share that page; they do not share a lock
   — they share the ACRN `processed` protocol.

3. **SYNC blocks the UOS VCPU; BELL does not.** A `ZX_GUEST_TRAP_VHM_MEM`
   abort fills a slot, kicks SOS (`kick_irq`), and waits for
   `COMPLETE`. A `ZX_GUEST_TRAP_VHM_BELL` abort enqueues work and
   resumes UOS immediately. Wrong trap type on a BAR is a hang
   (SYNC) or a lost write (BELL).

4. **This tree vs mt8678.** This HEE sets `WITH_VHM_IN_KERNEL`:
   UOS MMIO abort is `Vhm::Read`/`Write`/`HandleBellTrap` in the
   kernel; SOS `HC_NOTIFY_REQUEST_FINISH` is
   `SignalIoreqCompletion`. **Create/start/config still bounce
   to SOS EL0** (`ZX_PKT_GUEST_VCPU_VHM_REQ`). mt8678 has no
   `vhm.cpp`: every VHM MMIO and every `HC_*` is EL0.

## Framework

```text
 SOS Linux  /dev/acrn_hsm
    ACRN_IOCTL_CREATE_VM / START_VM / SET_IOREQ / ADD_MMIO
    acrn_hypercall5  →  SMC_SC_NBL_VHM_REQ
       x0 = SMC_SC_NBL_VHM_REQ
       x1 = HC_*
       x2 = GPA of payload
       x7.secure_id = SMC_HYP_SECURE_ID
              │  HCR.TSC
              ▼
 Savior  handle_smc_instruction
    in-kernel (this tree): HC_NOTIFY_REQUEST_FINISH, MSI if MSI_IN_KERNEL
    UOS abort (this tree): Vhm::Read/Write/BELL  — no packet
    else packet ZX_PKT_GUEST_VCPU_VHM_REQ  (CREATE/START/… always EL0)
              │
              ▼
 SOS Vmm  VhmServiceImpl::HandleVhmRequest
    HC_CREATE_VM    VmContext(vmid, vcpu_num)
    HC_SET_IOREQ    GPA of 4 KiB page
    HC_ADD_MMIO     list for UOS
    HC_START_VM     TeeVmCreate + CreateApplication("file://uos")
              │
              ▼
 UOS Vmm  VhmDevice
    zx_guest_vhm_setup(kick_irq, ioreq VMO)
    zx_guest_add_mmio_mapping  SYNC or BELL
              │
              ▼
 UOS EL1 store to trapped IPA
    DATA_ABORT
    this tree: Vhm::Write / Read / HandleBellTrap
    mt8678:    packet MEM to UOS Vmm, then SOS
```

Userspace **cannot** `zx_guest_set_trap(ZX_GUEST_TRAP_VHM_*)`.
`GuestDispatcher::SetTrap` returns `ZX_ERR_ACCESS_DENIED`. Only
`zx_guest_add_mmio_mapping`.

## Diagrams

### 1. Two VMM processes, one ioreq page

```text
 SOS Vmm                         UOS Vmm
  VhmServiceImpl                  VhmDevice
  owns VmContext[vmid]            zx_guest_vhm_setup
         │                               │
         │  ioreq VMO (SOS GPA)          │ kernel maps same page
         ▼                               ▼
        VhmIoreqBuffer  4096 B   slots[16] × 256 B
         slot[vcpu]  PENDING → PROCESSING → COMPLETE → FREE
         slot[14], slot[15]  BELL sideband
```

### 2. SYNC MMIO (UOS load/store waits)

```text
 UOS  ldr/str  trapped IPA
    EL2  DABT  ZX_GUEST_TRAP_VHM_MEM
    Vhm::Read / Write
       AcquireSlot(vcpu_id)
       slot.type = MMIO, dir, addr, size, value
       processed = PENDING
       Kick()  interrupt_set_pending(kick_irq)   lua setVhmIrq(254)
       Wait  IoreqEvent
 SOS  HSM IRQ
    read slot, emulate, write result
    SMC  HC_NOTIFY_REQUEST_FINISH
    Vhm::SignalIoreqCompletion
       processed already COMPLETE → FREE + Event
 UOS  VCPU unblocks, next_pc, Xt filled on read
```

### 3. BELL (write posted)

```text
 UOS  str  BELL IPA
    EL2  DABT  ZX_GUEST_TRAP_VHM_BELL
    Vhm::HandleBellTrap
       EnqueueBellWork
       next_pc, ZX_OK          UOS already running
 BellWorker  (2 threads, slots 14/15)
    WriteAsync into sideband slot
    Kick SOS
```

Reads on a BELL trap are `ZX_ERR_NOT_SUPPORTED`.

### 4. SOS creates UOS

```text
 SOS  ACRN_IOCTL_CREATE_VM
    SMC HC_CREATE_VM  acrn_vm_creation {vmid, vcpu_num, affinity}
    license NMT_ACT_FEAT_MAX_VM
    VmContext
 SOS  HC_SET_VM_CONFIG / HC_ADD_MMIO_MAPPING / HC_SET_IOREQ_BUFFER
 SOS  HC_START_VM
    guest_->TeeVmCreate(vmid)          TEE sees the new VM
    CreateApplication("file://uos", argv=vmid)
    wait 5 s  VCPU 0 ready
 UOS  main_uos
    same Guest::Init / StartVcpu as SOS, plus VhmDevice::Init
```

## Hypercalls

`SMC_SC_NBL_VHM_REQ` (yielding, ACRN). `x1` is the command.

| `x1` | Name | Who | Action |
| --- | --- | --- | --- |
| `0x80000010` | `HC_CREATE_VM` | SOS Vmm | `VmContext`, license max VM |
| `0x80000011` | `HC_DESTROY_VM` | SOS Vmm | `TeeVmDestroy`, GPU sched destroy |
| `0x80000012` | `HC_START_VM` | SOS Vmm | `file://uos` |
| `0x80000017` | `HC_SET_VM_CONFIG` | SOS Vmm | lua/config VMO for UOS |
| `0x80000018` | `HC_ADD_MMIO_MAPPING` | SOS Vmm | pushed to UOS `MappingListener` |
| `0x80000023` | `HC_INJECT_MSI` | kernel if `MSI_IN_KERNEL`, else SOS Vmm | SPI/LPI inject |
| `0x80000030` | `HC_SET_IOREQ_BUFFER` | SOS Vmm | GPA of the 4 KiB page |
| `0x80000031` | `HC_NOTIFY_REQUEST_FINISH` | kernel `Vhm` if in-kernel | FREE + wake |
| `0x80000090` | `HC_CALL` | SOS Vmm | `vm_call` message channel |

`SMC_FC_NBL_VHM_REQ` is a **different**, fast path (`x1 = FC_ID_*`):
inject HW/virt IRQ, GPU arbiter, vmlog kick. Not ACRN ioreq.
See [`cpu.md`](cpu.md) and [`gpu.md`](gpu.md).

## Slot protocol

```text
 FREE  ──UOS abort──►  PENDING  ──SOS IRQ──►  PROCESSING
                                              │
                         SOS complete         │
                              │               ▼
                              └─────────►  COMPLETE
                                              │
                         HC_NOTIFY_REQUEST_FINISH
                                              ▼
                                            FREE
```

`SignalIoreqCompletion` requires `processed == COMPLETE` already.
`ZX_ERR_BAD_STATE` is a protocol bug (SOS may be killed).
`ZX_ERR_CANCELED` / `INTERNAL_INTR_KILLED` mean teardown — do
**not** inject a guest abort (`VhmIsTeardownStatus`).

## Objects

| Object | Role |
| --- | --- |
| `Vhm` | Kernel, per `Guest`. Maps ioreq VMO, SYNC/BELL, Kick SPI |
| `VhmIoreqBuffer` | 16 × `VhmIoreqSlot`, one page |
| `VhmServiceImpl` | SOS Vmm FIDL `machina.VhmService` |
| `VmContext` | One UOS being created from SOS |
| `VhmDevice` | UOS Vmm: setup, mappings, kick IRQ listener |
| Linux `acrn_hsm` | SOS `/dev/acrn_hsm`, ioctls → SMC |

Lua: SOS `setVhmIrq(254)` is the kick SPI the kernel pends.

Swimlanes: [`flows.md`](flows.md) F7 (SYNC), F8 (create UOS).

## Flows

### Cold UOS from SOS

```text
 SOS  open /dev/acrn_hsm
    ioctl CREATE_VM     HC_CREATE_VM
    ioctl SET_IOREQ     HC_SET_IOREQ_BUFFER
    ioctl ADD_MMIODEV   HC_ADD_MMIO_MAPPING   (list)
    ioctl START_VM      HC_START_VM
       TeeVmCreate
       file://uos
 UOS  VhmDevice::Init
    GetIoRequestVmo + GetKickIrq + GetMmioMappings
    zx_guest_vhm_setup
    for each mapping: zx_guest_add_mmio_mapping
    StartVcpu(0)
 SOS  WaitForVmReady  5 s
```

### UOS MMIO read, this tree

```text
 UOS  ldr  IPA in VHM_MEM trap
    handle_data_abort  ISV must be 1
    Vhm::Read
       Kick SOS, wait COMPLETE
       ApplyMmioLoad into Xt
    next_pc, ZX_OK          kernel handled, no VMM packet
```

ISV=0 (unknown access size): inject guest DABT, do **not** Kick
SOS.

## Pitfalls

- **SYNC vs BELL on the same BAR.** A doorbell mapped SYNC
  serializes UOS behind SOS. A data register mapped BELL loses
  reads.
- **Slot still PENDING at teardown.** Waiters must see
  `VhmIsTeardownStatus` and not inject.
- **Userspace `set_trap(VHM_*)`.** Denied. Use
  `zx_guest_add_mmio_mapping`.
- **mt8678:** no `vhm.cpp`. If you look for `Vhm::Write` there,
  the path is SOS Vmm `HandleMem` instead.
- **License `NMT_ACT_FEAT_MAX_VM`.** `create_vm` refuses when
  `vm_map_.size()+1 >= license`.
- **ISV=0 on SYNC.** Unknown access size: inject guest DABT, do
  **not** Kick SOS (would hang a slot forever).
- **BELL overflow.** Work pool 16, log watermark 256. A flood
  of doorbells drops work; UOS thinks the write posted.
- **Kick is physical pending**, not `zx_vcpu_interrupt`. SOS
  must own SPI 254 as passthrough (`setVhmIrq`) or the HSM IRQ
  never fires.
- **`HC_NOTIFY` from UOS.** Kernel `hypercall_is_sos` denies
  (`ZX_ERR_ACCESS_DENIED`). Only SOS completes slots.

## Terminology

| Term | Meaning here |
| --- | --- |
| VHM | Virtual Hardware Manager. ACRN FE/BE on Savior |
| FE / BE | UOS frontend / SOS backend |
| ioreq | Shared 4 KiB request page |
| SYNC | Trap that blocks the UOS VCPU |
| BELL | Posted write, sideband slots 14–15 |
| kick IRQ | Host SPI that wakes SOS HSM (`setVhmIrq`) |
| `HC_*` | ACRN hypercall id in `x1` of `SMC_SC_NBL_VHM_REQ` |

## Code

| Step | File |
| --- | --- |
| Kernel Vhm | `zircon/kernel/arch/arm64/hypervisor/vhm.cpp` |
| Slot layout | `zircon/kernel/arch/arm64/include/arch/vhm.h` |
| DABT VHM traps | `vmexit.cpp` `handle_data_abort` |
| SMC dispatch | `vmexit.cpp` `handle_smc_instruction` |
| SOS Vmm | `garnet/lib/machina/vhm_service_impl.cc` |
| UOS Vmm | `garnet/lib/machina/vhm_device.cc` |
| FIDL | `garnet/lib/machina/fidl/vhm.fidl` |
| UOS entry | `garnet/bin/guest/main_uos.cc` |
| Lua kick | `garnet/bin/guest/sos/qemu/sos.lua` `setVhmIrq` |
| Linux HSM | `linux/drivers/virt/acrn/` |
| UAPI | `linux/include/uapi/linux/acrn.h` |

---

Everyday's a Hustle.
