# Flows

**After this:** you can walk a log from reset to GPU grant using
only F1–F18. **First read:** F1, F2, F5, F9, then the log table.
**Next:** [`bootstrap.md`](bootstrap.md). Prev: [`system.md`](system.md).

End-to-end pictures. Each flow is **who → numbered steps →
decision → return**. Detail of objects lives in the plane notes;
this page is the map you walk with a log.

Actors:

```text
 ATF        EL3
 Savior      host kernel (EL1h)
 EL2        el2.S trampoline  (HVC #4 resume)
 VMM        guest_manager / machina in EL0
 Guest      SOS / UOS / tbox EL1h
 TEE        S-EL1
```

| # | Flow | When you need it | Plane |
| --- | --- | --- | --- |
| F1 | Cold boot → SOS first instruction | never reaches guest | [`bootstrap.md`](bootstrap.md) |
| F2 | One `zx_vcpu_resume` trip | every later flow’s inner loop | [`cpu.md`](cpu.md) |
| F3 | Guest `SMC` dispatch | VHM / GPU / PSCI / TEE | [`cpu.md`](cpu.md) |
| F4 | Data abort (map / trap / DABT) | guest load/store fault | [`memory.md`](memory.md) |
| F5 | Passthrough SPI, HW=1, guest EOI | IRQ storm / lost SPI | [`interrupt.md`](interrupt.md) |
| F6 | Virtual inject (virtio / SGI / MSI) | virtio IRQ, MSI | [`interrupt.md`](interrupt.md) |
| F7 | VHM SYNC MMIO (UOS waits, SOS completes) | UOS hung on a BAR | [`vhm.md`](vhm.md) |
| F8 | SOS creates UOS | UOS never appears | [`vhm.md`](vhm.md) |
| F9 | GPU REQUEST → GRANT / preempt | black screen / grant timeout | [`gpu.md`](gpu.md) |
| F10 | `PSCI CPU_ON` first vs already-off | secondary CPU missing | [`cpu.md`](cpu.md) |
| F11 | s2idle → host PM → wake | GPUEB off / no wake | [`pm.md`](pm.md) |
| F12 | Guest reboot `RESET` vs `RESET2` | reboot loop, no expdb | [`vmm.md`](vmm.md) |
| F13 | lua `setVmem` → stage-2 PTE | device not visible | [`memory.md`](memory.md) |
| F14 | Guest SMC → TEE / FF-A | TEE hang | [`tee.md`](tee.md) |
| F15 | Occupancy doorbell | guest ignored PAUSE | [`scheduler.md`](scheduler.md) [`guest.md`](guest.md) |
| F16 | Watchdog miss | VM relaunch / SoC reset | [`vmm.md`](vmm.md) |
| F17 | Cross-VM lock destroy | lock stuck after VM death | [`guest.md`](guest.md) |
| F18 | ipc-mbox doorbell | peer VM never sees TX | [`system.md`](system.md) |

---

## F1  Cold boot → SOS first instruction

```text
 CPU reset
   │
   ▼
[1] BL1  EL3 ROM
   │  copy BL2, ERET
   ▼
[2] BL2  load FIP: BL31, BL32, BL33(Savior combo), REE extras
   │
   ▼
[3] BL31  EL3 runtime
   │  SPD ERET S-EL1 TEE, SMC back
   │  ERET EL2h into boot-shim
   ▼
[4] boot-shim  PIC, MMU off, DTB → BOOTDATA
   │  br kernel _start
   ▼
[5] _start
      arm64_elX_to_el1   HCR=RW|APK|API|ATA, eret EL1h
   │  MMU on, br KERNEL_BASE
   ▼
[6] lk_main → userboot eret EL0t → devmgr → appmgr → sysmgr
   │
   ▼
[7] sysmgr LaunchService GuestManager
   │  catalog auto_start SOS
   ▼
[8] CreateApplication file://sos
      Vmm::Initialize  lua maps, VGIC traps, virtio
      StartPrimaryVcpu  zx_vcpu_create pin, WriteState x0=DTB
   │
   ▼
[9] zx_vcpu_resume → F2 → eret guest EL1h
      PC = DTB+64KiB, DAIF masked   Linux ARM64 protocol
```

Stop reading bootstrap when (9) `eret`s. Further exits are F2–F14.
Per-node swimlanes: [`bootstrap.md`](bootstrap.md) **Diagrams** B1–B10.

---

## F2  One `zx_vcpu_resume` trip

VMM thread, already pinned. Interrupts off around the EL2 hop.

```text
 VMM  zx_vcpu_resume(vcpu, &packet)
   │
   ▼
 Savior  Vcpu::Resume
   [1] timer_maybe_interrupt          CNTV expired → PPI 27 PHYSICAL
   [2] VcpuGroup::OnVcpuResumePre / ApplyVcpuPolicy  prio / slice / occupancy
   [3] IRQs off
   [4] gich_maybe_interrupt           bitmap → ICH_LR, maybe UIE
   [5] arm64_el2_resume
         hvc #4  →  el2_hvc_resume
         msr vttbr, msr hcr (guest mask, no TGE/E2H)
         load GPRs, eret EL1h
   │
   ├──────── guest runs ─────────────────────────────────┐
   │                                                     │
   │  physical IRQ/FIQ (HCR.IMO)                         │
   │     EL2  x0 = ZX_ERR_NEXT                           │
   │     Resume swallows, loop unless THREAD_SIGNAL_KILL │
   │                                                     │
   │  sync trap (WFI, SMC, abort, sysreg, …)             │
   │     EL2  save El2State, HCR=RW, vttbr=0, eret host  │
   │     x0 = ZX_OK                                      │
   │     vmexit_handler                                  │
   │        ┌─ ZX_OK ──────── kernel handled, loop       │
   │        ├─ ZX_ERR_NEXT ── packet filled, return ZX_OK│
   │        │               to syscall (VMM HandlePacket)│
   │        ├─ ZX_ERR_CANCELED  SYSTEM_RESET → VMM stop  │
   │        └─ other error      VMM Stop                 │
   └─────────────────────────────────────────────────────┘
```

VMM `Vcpu::Loop` only sees syscall `ZX_OK` + packet, or a stop
error. `GUEST_BELL` never arrives here (queued on the trap port).

---

## F3  Guest `SMC` dispatch

Always `next_pc` first. `secure_id = x7[31:16]`.

`x1` of `SMC_SC_NBL_VHM_REQ` is decoded in **that Vmm**, not
the kernel: `HC_ID = (x1>>24)&0xff` → `0x80` ACRN / `0xA0` IPC
/ `0xB0` guest-call. [`system.md`](system.md).

```text
 Guest  smc
   │  HCR.TSC
   ▼
 EL2  EC=SMC  →  handle_smc_instruction
   │
   ├─ validate_hypervcall fail (HYP FID, bad args)
   │     inject IABT into guest, ZX_OK
   │
   ├─ ENTITY_NEBULA && HYP_SECURE_ID
   │     │
   │     ├─ SMC_SC_NBL_VHM_REQ          (stdcall)
   │     │     x1==0xB1000003  dump GIC, ZX_OK  (test)
   │     │     else packet VHM_REQ  ZX_ERR_NEXT  → that Vmm
   │     │     Vmm HandleNebulaHyperCall: 0x80 ACRN / 0xA0 IPC / 0xB0 guest-call
   │     │
   │     ├─ SMC_FC_NBL_VHM_REQ          (fast, kernel)
   │     │     x1=0  INJECT_HW_IRQ      interrupt_set_pending(x2)
   │     │     x1=1  INJECT_VIRT_IRQ    inject_virt_irq(vmid, vec)
   │     │     x1=2  NOTIFY_VMLOG       vcpu_kick(0)
   │     │     x1=3  GPU_ARBITER        HandleSmc(x2,x3,x4)
   │     │     x1=4  NOTIFY_VMLOG_SINK  vq_wakeup(x2)
   │     │
   │     └─ else  sm_handle_smc_call    Trusty-in-HEE
   │
   ├─ PSCI64_* / HYPERVISER_GUEST_*
   │     FIDs: [`cpu.md`](cpu.md) PSCI table
   │     CPU_ON / OFF / SUSPEND / RESET / RESET2 / OFF / SYSTEM_SUSPEND
   │     GUEST_OS_RESUME / PM_STATE / CPUFREQ / IRQ_EVENT / READ / WRITE / UNLOCK
   │
   └─ else  TeeVendor::TeeSmcHandler    F14
```

Guest `hvc` is **not** this flow: `x0 = -1`, resume.

---

## F4  Data abort

IPA = `HPFAR_EL2` | (`FAR_EL2` page offset).

```text
 Guest  ldr/str  IPA
   │  S2 miss or device PTE
   ▼
 EL2  EC=DABT  handle_data_abort
   │
   ▼
 traps.FindTrap(BELL, ipa)
   │
   ├─ NOT_FOUND
   │     handle_page_fault  (demand RAM)
   │       OK  →  PTE filled, ZX_OK, loop F2
   │       fail → inject guest DABT (VBAR_EL1+0x200)
   │
   └─ found  trap.kind
         │
         ├─ GUEST_TRAP_BELL
         │     write only; queue ZX_PKT_TYPE_GUEST_BELL on trap port
         │     ZX_OK  (VMM waiter, not Vcpu::Loop)
         │
         ├─ GUEST_TRAP_MEM
         │     fill packet GUEST_MEM, ZX_ERR_NEXT
         │     VMM HandleMem  emulate (GICD, virtio, …)
         │     unhandled → zx_vcpu_inject_exception
         │
         └─ (no VHM trap kinds in this tree; F7 uses MEM / BELL)
```

ISV=0 on `GUEST_TRAP_MEM`: `ZX_ERR_IO_DATA_INTEGRITY`, not a Kick.

---

## F5  Passthrough SPI (HW=1)

Wrong HW bit = storm or loss. [`interrupt.md`](interrupt.md).

```text
 Device  raises SPI
   │
   ▼
 Host GICD pending
   │
   ├─ VCPU in guest on this pCPU, HCR.IMO
   │     EL2 IRQ vector  x0=ZX_ERR_NEXT
   │     F2 swallows, next enter picks LR
   │
   └─ host handler  arm_gicv3
         VcpuIrqHandler
           vcpus_[cpu % n]  (exclusive: that one VCPU)
           PhysicalInterrupt
             bitmap PHYSICAL
             Signal + IPI pinned pCPU
   │
   ▼
 Resume  gich_maybe_interrupt
   LR = PENDING|GROUP1|VID|HARDWARE|PID
   prio 0 if INTID<32 else 0x10
   host handler returned IRQ_EOI_PRIORITY_DROP   (must NOT DIR)
   │
   ▼
 eret guest  takes vIRQ
 guest EOI/DIR
   HW=1  →  GIC deactivates **physical** INTID
```

---

## F6  Virtual inject

```text
 Source
   virtio BE     zx_vcpu_interrupt
   guest SGI     ICC_SGI1R trap  handle_sgi
   MSI LPI       HC_INJECT_MSI → GicIts::InjectMsi / GicDistributor
   GPU GRANT     VcpuDispatcher::Interrupt VIRTUAL
   │
   ▼
 Vcpu::Interrupt(VIRTUAL)
   assert: not a passthrough SPI of this VM
   bitmap VIRTUAL
   Signal + IPI if running / s2idle
   │
   ▼
 gich_maybe_interrupt
   LR = PENDING|GROUP1|VID     no HARDWARE
   │
   ▼
 guest EOI  →  LR clear only   (no physical DIR)
```

SGI never becomes a host SGI.

---

## F7  VHM SYNC MMIO

EL0 path. Slot `n` = UOS VCPU `n`. Kernel only posts the abort
as `ZX_GUEST_TRAP_MEM`; UOS `VhmDevice` fills the ioreq slot.

```text
 UOS  ldr/str  IPA in MEM trap (VhmDevice mapping)
   │
   ▼
 [1] EL2 DABT  packet GUEST_MEM  →  UOS Vcpu::HandleMem
 [2] VhmDevice::Read / Write
       slot[vcpu_id]  FREE → PENDING
       type=MMIO  dir, addr, size, value
 [3] channel.call VHM_IO_REQUEST     UOS VCPU blocked
 [4] SOS vhm-sos-thread
       Interrupt(kick_irq)           PHYSICAL SPI (QEMU 254 / SoC 287)
       Wait(event[vcpu])
   │
   ▼
 [5] SOS  HSM  (passthrough SPI)
       slot PROCESSING
       emulate device
       write result, processed=COMPLETE
 [6] SOS  SMC  HC_NOTIFY_REQUEST_FINISH
       packet VHM_REQ  →  signal_ioreq_completion
         require COMPLETE already
         FREE + event.signal
 [7] vhm-sos-thread Wait returns, channel response
 [8] UOS call returns
       read: copy slot value into IoValue
       VMM completes MEM packet, F2 loops
```

BELL: `ZX_GUEST_TRAP_BELL` → trapbell-handler thread, not the
UOS VCPU thread. Sideband slots 14/15 (`_VHM_USE_CHANNEL_`).
Reads on BELL: not supported.

CREATE/START/NOTIFY/MSI are all EL0 (F8). Kernel only posts
`ZX_PKT_GUEST_VCPU_VHM_REQ` for `SMC_SC_NBL_VHM_REQ`.

---

## F8  SOS creates UOS

```text
 SOS  open /dev/acrn_hsm
   │
   ▼
 [1] ioctl CREATE_VM
       SMC HC_CREATE_VM  →  packet VHM_REQ  →  SOS Vmm
       VhmServiceImpl::create_vm
         license NMT_ACT_FEAT_MAX_VM
         VmContext(vmid, vcpu_num, affinity)
 [2] SET_IOREQ / SET_VM_CONFIG / ADD_MMIO_MAPPING
 [3] ioctl START_VM
       TeeVmCreate(vmid)                 F14 notify
       CreateApplication("file://uos", argv=vmid)
       WaitForVmReady  5 s
   │
   ▼
 [4] UOS  main_uos
       FetchVmConfig, guest.Init(vmems)
       VhmDevice::Init
         GetIoRequestVmo / RegisterInterruptListener
         GetMmioMappings  →  guest_->CreateMapping (MEM or BELL)
       StartVcpu(0)  same factory as SOS
 [5] SOS  Wait returns 0
```

UOS never opens `/dev/acrn_hsm`.

---

## F9  GPU REQUEST → GRANT / preempt

Exclusive owner. MMIO in S2 only while Granted.

```text
 Idle GPU, UOS REQUEST
   UOS mali_arb  SMC  FC_ID_GPU_ARBITER  x2=REQUEST
     HandleRequest
       no current_vmidx_ → GrantVm(UOS)
         MapGpu  MapDevice identity
         EnableGpuIrq  PHYSICAL Mali IRQs → UOS VCPUs
         InjectGrant  VIRTUAL GRANT IRQ
         StartTimer PREPARING
   UOS GRANT IRQ → programs GPU → SMC ACTIVE
     stop PREPARING; ALPS writes GPUEB magic

 SOS owns, UOS REQUEST
   UOS → Pending, start SCHED  min(SOS slice, UOS deadline)
   SCHED fire
     InjectStop  STOP IRQ to SOS
     SOS STOPPING timer
   SOS drain, SMC STOPPED
     UnmapGpu SOS, dummy Mali handlers
     GrantVm(UOS)  as above

 STOPPING timeout
   SOS  →  RCVR IRQ + recovery timer
   not SOS → Lost, then grant Pending
```

tbox does not get `/arbiter`. SWA is Mali time-slice only.

---

## F10  `PSCI CPU_ON`

```text
 Guest  SMC PSCI64_CPU_ON  x1=mpidr  x2=entry
   │
   ▼
 handle_smc_instruction
   vcpu_id = arch_cpu_mpidr_to_cpu_num(mpidr)
   │
   ├─ no VcpuDispatcher[id]
   │     packet STARTUP {id, entry}     ZX_ERR_NEXT
   │     x0 = PSCI_SUCCESS
   │     VCPU 0 Loop  HandleVcpu
   │       guest_->StartVcpu(entry, id)
   │         zx_vcpu_create on VCPU 0 factory
   │         pin bind_cpus[id]
   │         WriteState, Resume loop   new host thread
   │
   ├─ STATE_PWROFF
   │     handle_vcpu_on
   │       if pCPU offline: suspend_for_cpu_on + wait
   │       ResumeThread
   │     ResetEl2State(entry)  (on the off VCPU after SuspendThread)
   │
   └─ already RUNNING / INIT
         x0 = PSCI_SUCCESS, resume
```

First `CPU_ON` of a missing VCPU is **not** created in the kernel.

---

## F11  s2idle → host PM → wake

```text
 each VCPU  PSCI CPU_SUSPEND  S2IDLE
   pending LR / bitmap?  SUCCESS, do not sleep
   else
     vcpu_enter_suspend  STATE_SUSPEND
     last VCPU of this VM  GUEST_PM_SUSPEND_TO_IDLE
     tbox: notify_modem_suspend
     is_all_vm_suspend?
        yes → DeviceSuspendNotify + lk_init DEVICE_SUSPEND
              hyper_suspend_state = HYPER_PM_SUSPEND
              wakeup_all_other_cpus
     WaitForSuspend
   │
   ▼
 idle thread
   HYPER_PM_SUSPEND && all VMs parked && no wakeup sources
     → _cpu_suspend(PSCI64_CPU_SUSPEND, S2IDLE) ATF
   │
   ▼
 wake SPI / HYPERVISER_GUEST_OS_RESUME
   first VCPU out of WaitForSuspend
     DeviceResumeNotify + lk_init DEVICE_RESUME
     hyper_suspend_state = HYPER_PM_RUNNING
   inject wakeup SPI, Signal
   SetVcpuState(RUNNING)
   SetGuestEntry(entry)           next eret = PSCI return
```

ALPS: `CheckSkipWakeup` drops SPIs not on the wakeup list.

---

## F12  Guest reboot

```text
 Linux reboot
   │
   ├─ PSCI SYSTEM_RESET          (non-platform)
   │     vmexit  ZX_ERR_CANCELED
   │     Vcpu::Loop  guest_->Stop
   │     Vmm  REBOOT_REQUIRED
   │     teardown, GuestManager  delay 1 s  Launch()  new Vmm
   │     no BL2, no SYS_NEBULA_RAMCONSOLE_DUMP_T
   │
   ├─ PSCI SYSTEM_RESET2
   │     packet GUEST_VCPU_STOP
   │     same teardown / relaunch
   │
   └─ (no platform RESET in this tree: SOS RESET is also canceled
         after ramconsole_snapshot_take; ATF only on whole-SoC WDT)
```

Watchdog miss also `Stop(CANCELED)` → same relaunch.

---

## F13  lua `setVmem` → stage-2 PTE

```text
 QEMU sos.lua  setVmem("IO", 0, 1GiB, kCached, 0)   (5-arg, GPA=HPA)
 SoC product.lua  per-device 6-arg product_config:setVmem  (no 1GiB IO)
   │
   ▼
 Vmm::Initialize
   ioctl guest_memory_allocator     RAM windows
   append VmemSpec
   guest_.Init(vmem)
     zx_guest_create_new            Guest + guest VMAR
     for each spec
       kCached / kDevice
         physical VMO, GPA = HPA, MapRange
       kDemand
         anonymous VMO, fault in later  (F4 NOT_FOUND)
       kMpu
         skip, then UnmapPhysicalMemory
     zx_guest_set_trap  UnmapRange  VGIC / virtio / SMMU
   │
   ▼
 Guest IPA
   mapped identity     SOS 0–1GiB IO, RAM
   trap hole           GICD, virtio, VHM BAR
   absent              SMPU, GPU until GRANT
```

CPU, IRQ, DMA are three legs ([`devices.md`](devices.md)).
SMMU STE S2TTB = same root as `VTTBR`.

---

## F14  Guest SMC → TEE / FF-A

```text
 Guest  smc  (not HYP, not PSCI)
   │
   ▼
 TeeSmcHandler(guest_state, vmid)
   │
   ├─ FF-A enabled
   │     is_ffa_call(x0)?
   │       yes  guest_ffa_smc_call
   │         ID_GET / RXTX_MAP / SHARE / DIRECT_REQ / …
   │         DIRECT_REQ  guest_smc_call under ffa_direct_msg_lock
   │       no   x7=vmid, guest_smc_call
   │
   └─ legacy
         x7 = vmid                  cannot forge client id
         Savior / OP-TEE / Trustonic / default
           arm_smccc_smc → EL3 → SPD → S-EL1
```

VM create: `TeeVmCreate` → `NotifyVmCreated` (FF-A
`VM_CREATED` or vendor SMC) **before** UOS `eret`. SOS/ALPS
SOS VCPU 0 also `SMC_FC_NBL_SOS_STATE` with
`&El2State.guest_state` PA. There is no `UOS_STATE` in this tree.

---

## F15  Occupancy doorbell

Host writes who may sit; guest parks the Linux CPU. The host
VCPU thread stays READY.

```text
 occupancy table changed  (Resume, ≥ 8 ms)
    write shm occupancy[VM][vCPU]     (no dmb here; bound IPI `smp_mb` before SGI)
    Interrupt(sched_irq)  that VM VCPU 0 VIRTUAL
         │
         ▼
 guest  nebula_sched_irq_handler
    apply RUN / PAUSE / SHARE / OFFLINE
    PAUSE  →  Linux CPU parked, CFS skips it
    host VCPU thread still READY
```

Missing module → doorbell ignored. [`guest.md`](guest.md)
diagram 2, [`scheduler.md`](scheduler.md) N3.

---

## F16  Watchdog miss

Two independent loops. Per-VM miss relaunches the Vmm. GRT miss
resets the SoC.

```text
 Guest Linux  HC_GUEST_WDTK_KICK  every < 30 s
    SOS Vmm  Watchdog.Heartbeat
       miss  →  dump VCPU, Stop(CANCELED)
               GuestManager relaunch  (1 s)

 GuestManagerReporter
    10 s ioctl GRT WDT
    miss × 3  (and an auto_start guest not RUNNING / not in grace)
       →  host hardware WDT  →  whole-SoC reset  →  BL2 kedump
```

Probe hang (`Bind` timeout × 2) also `Kill()`s the Vmm and
relaunches. [`vmm.md`](vmm.md) Watchdog.

---

## F17  Cross-VM lock, VM dies holding it

```text
 guest A  ldaxr/stlxr  slot  cnt=LOCK current_vmid=A
    VM A destroyed
         │
         ▼
 Savior  cross_vm_lock_check(A)
    holder == A  →  UNLOCK, drop A's bit
    inject cvm_lock_irq  remaining creators  VCPU 0 VIRTUAL
```

Host does not take the lock on the live path. Missing VMO →
cleanup is a no-op. [`guest.md`](guest.md) diagram 5.

---

## F18  ipc-mbox doorbell

Not VHM ioreq. Channel GPA `0x800000000 + id × 8 KiB`
(4 KiB TX + 4 KiB RX). Doorbells are `HC_IPC_*` on
`SMC_SC_NBL_VHM_REQ`.

```text
 guest A  write 4 KiB TX          (no dmb; SMC ≠ dmb ish)
    SMC  HC_IPC_SET_RX_READY / VM_WRITE
       packet VHM_REQ → that Vmm IpcService
       inject rx_ready  VIRTUAL into peer
 guest B  IRQ  copy RX
    SMC  HC_IPC_SET_TX_DONE
       inject tx_done  VIRTUAL into A
```

DTS interrupt order is **tx_done then rx_ready**. UOS has no
lua pairs: it is an `IpcClient` of SOS (user vmid 0 → way 0).
[`system.md`](system.md) **IPC**.

---

## How to read a log against these

| You see | Jump |
| --- | --- |
| `bind phy cpu` then silent | F1 [8]–[9], F2 first resume |
| `Unhandled data abort` | F4 NOT_FOUND |
| IRQ storm / `irq_monitor` | F5 HW bit |
| UOS hang on a BAR | F7 slot stuck PENDING |
| `file://uos` never ready | F8 WaitForVmReady 5 s |
| GPU `STOPPING` timeout | F9 yield fail |
| `trying to get vcpudisp error` | F10 factory missing |
| GPUEB off while guest runs | F11 idle ATF without all-VM park |
| reboot loop, no expdb file | F12 RESET, [`logging.md`](logging.md) |
| occupancy doorbell ignored | F15, [`guest.md`](guest.md) |
| VM relaunch / SoC WDT | F16 |
| lock stuck after VM death | F17 |
| ipc-mbox peer silent | F18 |

---

Prev: [`system.md`](system.md) · Next: [`bootstrap.md`](bootstrap.md)

Everyday's a Hustle.
