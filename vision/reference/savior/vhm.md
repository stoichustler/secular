# VHM

**After this:** you can say why UOS hung on a BAR (SYNC vs BELL).
**First read:** Principle + diagrams 1–6. **Next:** [`gpu.md`](gpu.md).

VHM is the **ACRN-shaped FE/BE** between SOS and UOS. SOS Linux
thinks it is ACRN (`/dev/acrn_hsm`). Savior is the hypervisor
behind those hypercalls. UOS devices that SOS must emulate
(virtio-be, some PCI BARs, mailbox) are not identity-mapped:
they **trap** into an ioreq slot, SOS handles them, then kicks
the UOS VCPU.

tbox does not use this path. GPU SWA does not use this path.
This note is SOS creating UOS, the ioreq page, SYNC vs BELL.
This tree has **no** in-kernel `Vhm`: every `HC_*` and every
UOS MMIO abort is EL0.

FIDL `VhmService`: [`fidl.md`](fidl.md).
Read [`vmm.md`](vmm.md) for `file://uos`, [`cpu.md`](cpu.md) for
the SMC intercept, [`interrupt.md`](interrupt.md) for virq/MSI,
[`devices.md`](devices.md) for which MMIO is trapped,
[`memory.md`](memory.md) for the unmapped IPA.

## Principle

1. **SOS is the backend, UOS is the frontend.** SOS issues
   `SMC_SC_NBL_VHM_REQ` with ACRN `HC_*`. Savior posts
   `ZX_PKT_GUEST_VCPU_VHM_REQ` to the SOS Vmm (`VhmServiceImpl`).
   UOS never talks ACRN; its Vmm (`VhmDevice`) only installs
   traps and shares the ioreq page.

2. **One 4 KiB ioreq page, 16 slots of 256 bytes.** Slot `n` is
   UOS VCPU `n`. Slots 14–15 are sideband BELL channels
   (`SIDEBAND_CHANNEL_VCPU_ID(chn) = 15 - chn`). Layout
   **must** match `struct acrn_io_request` (`processed` at offset
   136). SOS and the UOS Vmm share that page; they do not share a
   lock — they share the ACRN `processed` protocol.

3. **SYNC blocks the UOS VCPU; BELL does not.** A
   `ZX_GUEST_TRAP_MEM` abort fills a slot, kicks SOS
   (`kick_irq`), and waits for `COMPLETE`. A `ZX_GUEST_TRAP_BELL`
   abort is handled on a trapbell-handler thread and resumes UOS
   immediately. Wrong trap type on a BAR is a hang (SYNC) or a
   lost write (BELL).

4. **Kernel only posts the packet.** There is no `WITH_VHM_IN_KERNEL`,
   no `vhm.cpp`, no `zx_guest_vhm_setup` / `zx_guest_add_mmio_mapping`.
   Trap kinds are `ZX_GUEST_TRAP_BELL` / `MEM` / `IO` only. Unknown
   kind → `ZX_ERR_INVALID_ARGS`, not `ACCESS_DENIED`. The kernel
   does not parse `x1`; SOS Vmm `HandleVhmRequest` switches on it.

5. **Kick is a passthrough SPI into SOS, not a virtual IRQ.**
   `GicDistributor::Interrupt(kick_irq)` (QEMU 254 / SoC 287)
   becomes PHYSICAL pending because lua `setVhmIrq` listed that
   SPI. Missing that line means HSM never fires.

## Framework

```text
 SOS Linux  /dev/acrn_hsm
    ACRN_IOCTL_CREATE_VM / START_VM / SET_IOREQ / ADD_MMIO
    acrn_hypercall5  →  SMC_SC_NBL_VHM_REQ
       x0 = SMC_SC_NBL_VHM_REQ
       x1 = HC_*
       x2 = GPA of payload
       x7.secure_id = SMC_HYP_SECURE_ID          (1 << 16)
              │  HCR.TSC
              ▼
 Savior  handle_smc_instruction
    packet ZX_PKT_GUEST_VCPU_VHM_REQ     (every HC_*)
              │
              ▼
 SOS Vmm  VhmServiceImpl::HandleVhmRequest
    HC_CREATE_VM    VmContext(vmid, vcpu_num)
    HC_SET_IOREQ    GPA of 4 KiB page
    HC_ADD_MMIO     list for UOS
    HC_START_VM     TeeVmCreate + CreateApplication("file://uos")
    HC_INJECT_MSI   channel VHM_INTR_INJECT → UOS GicIts / GICD
    HC_NOTIFY       signal_ioreq_completion
              │
              ▼
 UOS Vmm  VhmDevice
    GetIoRequestVmo + RegisterInterruptListener
    guest_->CreateMapping  MEM (SYNC) or BELL
              │
              ▼
 UOS EL1 store to trapped IPA
    DATA_ABORT  packet MEM / BELL
    VhmDevice::Read / Write  (EL0)
```

## Diagrams

### 1. Two VMM processes, one ioreq page

```text
 SOS Vmm                         UOS Vmm
  VhmServiceImpl                  VhmDevice
  owns VmContext[vmid]            FIDL GetIoRequestVmo
         │                               │
         │  ioreq VMO (SOS GPA)          │ both map the same 4 KiB
         ▼                               ▼
        VhmIoreqBuffer  4096 B   slots[16] × 256 B
         slot[vcpu]  PENDING → PROCESSING → COMPLETE → FREE
         slot[14], slot[15]  BELL sideband
```

SOS maps it with `zx_vmo_create_physical(ioreq_buf GPA)`. UOS
maps the same VMO via FIDL. `processed` is the only handshake.

### 2. Slot layout (`struct acrn_io_request`, 256 B)

Must match Linux `include/uapi/linux/acrn.h`. Offset 136 is
load-bearing: UOS Vmm `ATOMIC_STORE` (`SEQ_CST`); SOS Linux
HSM `smp_store_release(COMPLETE)` then `HC_NOTIFY`; SOS Vmm
`ATOMIC_LOAD` COMPLETE then `ATOMIC_STORE` FREE. Same word,
two C libraries ([`shm.md`](shm.md)).

```text
 offset
   0    type                 ACRN_IOREQ_TYPE_MMIO
   4    completion_polling   0 = notify; 1 = SOS polls (unused here)
   8    status               zx_status_t back to UOS on SYNC
  12    reserved0[13]
  64    reqs.mmio
          direction          READ / WRITE
          address            trapped IPA
          size               access_size
          value              write data in; read data out
 128    reserved1
 132    kernel_handled
 136    processed            FREE / PENDING / PROCESSING / COMPLETE
 256    ── next slot ──

 page = 16 × 256 = 4096
   slot[0..13]   UOS VCPU 0..13  (SYNC)
   slot[14]      BELL sideband channel 1
   slot[15]      BELL sideband channel 0
```

`SIDEBAND_CHANNEL_VCPU_ID(chn) = 15 - chn`. Trapbell thread
`trapbell-handler-N` uses slot `15-N`.

### 3. SYNC MMIO (UOS load/store waits)

UOS VCPU thread is the waiter. SOS HSM is the worker. One slot
per UOS VCPU: a second abort on the same VCPU cannot start until
`FREE`.

```text
 UOS EL1        UOS VCPU thread      SOS vhm-sos-thread     SOS Linux HSM
    │                  │                    │                    │
    │ ldr/str IPA      │                    │                    │
    │  S2 trap MEM     │                    │                    │
    ▼                  │                    │                    │
 EL2 DABT ────────────►│ HandleMem          │                    │
                       │  slot = MMIO       │                    │
                       │  processed=PENDING │                    │
                       │  channel.call      │                    │
                       │   VHM_IO_REQUEST ─►│                    │
                       │    BLOCKED         │ Interrupt(kick_irq)│
                       │                    │  PHYSICAL SPI ────►│
                       │                    │  Wait(event[vcpu]) │
                       │                    │                    ▼
                       │                    │              HSM PENDING
                       │                    │              PROCESSING
                       │                    │              emulate BAR
                       │                    │              COMPLETE
                       │                    │              SMC HC_NOTIFY
                       │                    │◄── FREE + event ── SOS Vmm
                       │                    │  channel response
                       │◄── call returns ───┤
                       │  copy mmio.value
                       │  complete MEM packet, F2 loops
```

Kick is still `GicDistributor::Interrupt(kick_irq_)` (QEMU 254 /
SoC 287), a passthrough SPI (F5, HW=1). The channel only
serializes UOS Vmm ↔ SOS Vmm; HSM still does the emulate.
Without `_VHM_USE_CHANNEL_`, UOS Vmm would `Kick()` + FIDL
`WaitForIoRequestCompletion` itself.

### 4. BELL (write posted)

The UOS VCPU is **already running** when SOS sees the slot.
Reads on a BELL trap are `ZX_ERR_NOT_SUPPORTED`.

```text
 UOS EL1  str  BELL IPA
    EL2  DABT  ZX_GUEST_TRAP_BELL
    queued on trap port  (not Vcpu::Loop)
         │
         ▼
 trapbell-handler-N thread          (not the UOS VCPU thread)
    sscanf name → chn
    vcpu_id = 15 - chn              slot 14 or 15
    VhmDevice::WriteInternalAsync
       memcpy value
       processed = PENDING
       channel write  VHM_IO_REQUEST_ASYNC
    return ZX_OK                    UOS already in guest
         │
         ▼
 SOS  vhm-sos-thread
    Interrupt(kick_irq_)            same PHYSICAL SPI as SYNC
    does **not** Wait
 SOS  HSM  emulate, COMPLETE, HC_NOTIFY
    signal_ioreq_completion
       VHM_IO_REQUEST_ASYNC_RESPONSE  (does not block UOS)
```

`_VHM_USE_CHANNEL_` is on in this tree. SYNC uses
`zx::channel.call` (`VHM_IO_REQUEST`) instead of FIDL
`WaitForIoRequestCompletion`. BELL uses a one-way
`VHM_IO_REQUEST_ASYNC`.

### 5. SOS creates UOS

UOS is **not** in the catalog. Only SOS `HC_START_VM` launches it.
Wait is 5 s on `vm_ready_event_` (UOS VCPU 0 `eret`).

```text
 SOS  open /dev/acrn_hsm
    │
    ▼
 [1] ioctl CREATE_VM
       acrn_hypercall5(HC_CREATE_VM, GPA of acrn_vm_creation)
         {vmid, vcpu_num, cpu_affinity}
       packet VHM_REQ → SOS Vmm
       license NMT_ACT_FEAT_MAX_VM
         refuse if vm_map_.size()+1 >= license
       affinity bits must be < zx_system_get_num_cpus()
       VmContext(vmid, vcpu_num, affinity, kick_irq)
         16 ioreq events + vm_ready_event
    │
    ▼
 [2] SET_IOREQ_BUFFER     GPA of 4 KiB → map + FREE every slot
     SET_VM_CONFIG        lua/config bytes for UOS
     ADD_MMIO_MAPPING     list of {base, size, trap_type}
    │
    ▼
 [3] ioctl START_VM
       TeeVmCreate(user vmid)              F14, before UOS eret
       PostTask  CreateApplication("file://uos", argv=vmid)
       wait_one(kSignalVmReady, 5 s)
    │
    ▼
 [4] UOS  main_uos
       FetchVmConfig, guest.Init(vmems)
       VhmDevice::Init
         GetIoRequestVmo / RegisterInterruptListener
         SetupChannel  (cli/srv pair, UOS viewpoint)
         GetMmioMappings → guest_->CreateMapping (MEM or BELL)
       StartVcpu(0)  same factory as SOS
       vm_ready_event_.signal            SOS wait returns 0
```

UOS never opens `/dev/acrn_hsm`. Destroy: `TeeVmDestroy` + GPU
`destroyGpuSched` + EINT cleanup + `KillAndWait` the UOS process.

### 6. MSI inject (SOS → UOS)

Not ioreq. SOS BE wants an IRQ in UOS.

```text
 SOS  virtio-be / PCI MSI
    SMC  HC_INJECT_MSI  x2=vmid  x3=GPA of acrn_msi_entry
       {msi_addr, msi_data}
    packet VHM_REQ → inject_msi
       channel  VHM_INTR_INJECT  {dev_id=addr, evt_id=data}
         │
         ▼
 UOS Vmm  OnInterrupt / GicIts
    evt_id == -1  →  GicDistributor SPI
    else          →  GicIts::InjectMsi  (LPI, virtual)
```

Kernel `SMC_FC_NBL_VHM_REQ` `INJECT_HW_IRQ` is a **different**
path ([`interrupt.md`](interrupt.md)): it never visits SOS Vmm.

### 7. Kernel vs EL0 (who sees `x1`)

```text
 guest  smc
    x0 = SMC_SC_NBL_VHM_REQ     yielding, ACRN / guest-call / IPC
    x1 = HC_* / HC_GUEST_* / HC_IPC_*
         │
         ▼
 handle_smc_instruction
    validate_hypervcall (HYP FID + secure_id)
    fill packet VHM_REQ, ZX_ERR_NEXT
    kernel does **not** switch on x1
         │
         ▼
 that guest's Vmm
    SOS  HandleNebulaHyperCall → VhmServiceImpl::HandleVhmRequest
    UOS  FXL_CHECK  (no VhmServiceImpl)

 guest  smc
    x0 = SMC_FC_NBL_VHM_REQ     fast, kernel
    x1 = FC_ID_INJECT_HW_IRQ / INJECT_VIRT_IRQ /
         GPU_ARBITER / NOTIFY_VMLOG / NOTIFY_VMLOG_SINK
    never a VHM_REQ packet
```

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

`signal_ioreq_completion` requires `processed == COMPLETE` already.
SOS HSM does `smp_store_release(COMPLETE)` then
`hcall_notify_req_finish`. Destroy toggles
`kSignalIoreqTermination` so a waiter does not hang forever.

## Hypercalls

`SMC_SC_NBL_VHM_REQ` (yielding, ACRN). `x1` is the command.
**All** of these bounce to SOS EL0.

| `x1` | Name | Who | Action |
| --- | --- | --- | --- |
| `0x80000010` | `HC_CREATE_VM` | SOS Vmm | `VmContext`, license max VM |
| `0x80000011` | `HC_DESTROY_VM` | SOS Vmm | `TeeVmDestroy`, GPU sched destroy |
| `0x80000012` | `HC_START_VM` | SOS Vmm | `file://uos`, 5 s ready |
| `0x80000017` | `HC_SET_VM_CONFIG` | SOS Vmm | lua/config VMO for UOS |
| `0x80000018` | `HC_ADD_MMIO_MAPPING` | SOS Vmm | pushed to UOS `MappingListener` |
| `0x80000023` | `HC_INJECT_MSI` | SOS Vmm | SPI (`evt_id==-1`) or LPI via `GicIts` |
| `0x80000030` | `HC_SET_IOREQ_BUFFER` | SOS Vmm | GPA of the 4 KiB page |
| `0x80000031` | `HC_NOTIFY_REQUEST_FINISH` | SOS Vmm | FREE + wake |
| `0x80000090` | `HC_CALL` | SOS Vmm | `vm_call` message channel |

Payloads (GPA in `x2`, SOS physmap). Layout must match
`acrn_common.h` / `uapi/linux/acrn.h`.

| `HC_*` | Struct | Fields this tree uses |
| --- | --- | --- |
| `CREATE_VM` | `acrn_vm_creation` | `vmid`, `vcpu_num`, `cpu_affinity` (bit i → pCPU of VCPU i), `name`, `vm_flag` |
| `SET_IOREQ_BUFFER` | GPA | 4 KiB page of 16×256 slots |
| `ADD_MMIO_MAPPING` | `acrn_mmio_mapping` | `base`, `size`, `trap_type` (MEM vs BELL) |
| `INJECT_MSI` | `acrn_msi_entry` | `msi_addr` (dest), `msi_data` (`evt_id==-1` → SPI else LPI) |
| `NOTIFY_REQUEST_FINISH` | vmid + vcpu in regs | slot → FREE |
| `SET_VM_CONFIG` | bytes | lua/config VMO for UOS |
| `START_VM` | vmid | `TeeVmCreate` + `file://uos` |
| `HC_CALL` | SOS GPA tx/rx | opaque bytes → UOS `MessageListener.OnNewCall` |

`HC_CALL` (`vm_call`): SOS GPA `tx_buf`/`rx_buf` + sizes.
SOS Vmm copies tx into a VMO, FIDL `OnNewCall` to UOS, copies
the reply VMO back into SOS `rx_buf`. Return value is rx size
or −1. No in-kernel parse of the payload. UOS must
`RegisterMessageListener` first or the call fails
`message listener not ready`.

`SMC_FC_NBL_VHM_REQ` is a **different**, fast path (`x1 = FC_ID_*`):
inject HW/virt IRQ, GPU arbiter, vmlog kick. Not ACRN ioreq.
See [`cpu.md`](cpu.md) and [`gpu.md`](gpu.md).

IPC doorbells (`HC_IPC_*` = `0xA0000000+`) share the same
`SMC_SC_NBL_VHM_REQ` packet. [`system.md`](system.md) **IPC**.

## Objects

| Object | Role |
| --- | --- |
| `VhmServiceImpl` | SOS Vmm FIDL `machina.VhmService`; all `HC_*` |
| `VmContext` | One UOS being created from SOS |
| `VhmDevice` | UOS Vmm: ioreq map, MEM/BELL handlers, kick IRQ listener |
| Linux `acrn_hsm` | SOS `/dev/acrn_hsm`, ioctls → SMC |
| Linux `drivers/vhm/` | legacy wrap of the same hypercalls |
| `vhm-sos-thread` | SOS side of the channel (`_VHM_USE_CHANNEL_`) |
| `trapbell-handler-N` | BELL abort thread, slot 15−N |

Lua: SOS `setVhmIrq(254)` (QEMU) / 287 (SoC) is the kick SPI.

Swimlanes: [`flows.md`](flows.md) F7 (SYNC), F8 (create UOS).

## Pitfalls

- **SYNC vs BELL on the same BAR.** A doorbell mapped SYNC
  serializes UOS behind SOS. A data register mapped BELL loses
  reads.
- **UOS `HC_*`.** UOS never registers `HandleNebulaHyperCall`.
  Only SOS `VhmServiceImpl` handles the packet.
- **PCI BAR realloc.** `HandleMmioArm` `ZX_ERR_PCI_BAR_REALLOC`:
  remove traps on old/new IPA, `CreateMapping` the new BAR. A
  UOS BAR move that does not hit this path leaves a stale trap.
- **ISV=0 on MEM.** `!data_abort.valid` → `ZX_ERR_IO_DATA_INTEGRITY`,
  not a Kick (would hang a slot forever).
- **Kick is `GicDistributor::Interrupt(254/287)`**, which is
  physical pending because that SPI is passthrough (`setVhmIrq`).
  Missing that lua line means HSM never fires.
- **License `NMT_ACT_FEAT_MAX_VM`.** `create_vm` refuses when
  `vm_map_.size()+1 >= license`.
- **No `zx_guest_vhm_setup`.** UOS uses FIDL `GetIoRequestVmo` +
  `CreateMapping`.
- **5 s `WaitForVmReady`.** UOS VCPU 0 must `eret` before SOS
  `START_VM` returns. A stage-2 hole on the UOS entry page looks
  like “UOS never appeared”.
- **Slot still PENDING.** SOS HSM not running, kick SPI unbound,
  or `HC_NOTIFY` never issued. UOS VCPU stays in
  `WaitForIoRequestCompletion`.

## Terminology

| Term | Meaning here |
| --- | --- |
| VHM | Virtual Hardware Manager. ACRN FE/BE on Savior |
| FE / BE | UOS frontend / SOS backend |
| ioreq | Shared 4 KiB request page |
| SYNC | `ZX_GUEST_TRAP_MEM` that blocks the UOS VCPU |
| BELL | `ZX_GUEST_TRAP_BELL`, posted write, slots 14–15 |
| kick IRQ | Host SPI that wakes SOS HSM (`setVhmIrq`) |
| `HC_*` | ACRN hypercall id in `x1` of `SMC_SC_NBL_VHM_REQ` |
| `_VHM_USE_CHANNEL_` | This tree: zx::channel instead of FIDL wait |

## Code

| Step | File |
| --- | --- |
| SMC dispatch | `vmexit.cpp` `handle_smc_instruction` |
| SOS Vmm | `garnet/lib/machina/vhm_service_impl.cc` |
| UOS Vmm | `garnet/lib/machina/vhm_device.cc` |
| Channel cmds | `garnet/lib/machina/vhm_chan_message.h` |
| FIDL | `garnet/lib/machina/fidl/vhm.fidl` |
| UOS entry | `garnet/bin/guest/main_uos.cc` |
| Lua kick | `garnet/bin/guest/sos/qemu/sos.lua` `setVhmIrq` |
| Linux HSM | `linux/drivers/virt/acrn/` |
| SMC wrapper | `linux/arch/arm64/include/asm/acrn.h` `acrn_hypercall5` |
| Legacy wrap | `linux/drivers/vhm/` |
| UAPI | `linux/include/uapi/linux/acrn.h` |

---

Prev: [`devices.md`](devices.md) · Next: [`gpu.md`](gpu.md) · [`README.md`](README.md)

Everyday's a Hustle.
