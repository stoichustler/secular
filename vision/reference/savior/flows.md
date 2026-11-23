# Flows

End-to-end pictures. Each flow is **who → numbered steps →
decision → return**. Detail of objects lives in the plane notes;
this page is the map you walk with a log.

Actors:

```text
 ATF        EL3
 Savior      host kernel (VHE EL2 looking like EL1)
 EL2        el2_vhe.S trampoline  (mt8678: el2.S HVC)
 VMM        guest_manager / machina in EL0
 Guest      SOS / UOS / tbox EL1h
 TEE        S-EL1
```

| # | Flow | Plane |
| --- | --- | --- |
| F1 | Cold boot → SOS first instruction | [`bootstrap.md`](bootstrap.md) |
| F2 | One `zx_vcpu_resume` trip | [`cpu.md`](cpu.md) |
| F3 | Guest `SMC` dispatch | [`cpu.md`](cpu.md) [`vhm.md`](vhm.md) [`tee.md`](tee.md) |
| F4 | Data abort (map / trap / VHM / DABT) | [`memory.md`](memory.md) |
| F5 | Passthrough SPI, HW=1, guest EOI | [`interrupt.md`](interrupt.md) |
| F6 | Virtual inject (virtio / SGI / MSI) | [`interrupt.md`](interrupt.md) |
| F7 | VHM SYNC MMIO (UOS waits, SOS completes) | [`vhm.md`](vhm.md) |
| F8 | SOS creates UOS | [`vhm.md`](vhm.md) [`vmm.md`](vmm.md) |
| F9 | GPU REQUEST → GRANT / preempt | [`gpu.md`](gpu.md) |
| F10 | `PSCI CPU_ON` first vs already-off | [`cpu.md`](cpu.md) |
| F11 | s2idle → host PM → wake | [`pm.md`](pm.md) |
| F12 | Guest reboot `RESET` vs `RESET2` | [`vmm.md`](vmm.md) |
| F13 | lua `setVmem` → stage-2 PTE | [`memory.md`](memory.md) [`devices.md`](devices.md) |
| F14 | Guest SMC → TEE / FF-A | [`tee.md`](tee.md) |

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
      this tree: arm64_elX_to_el2   HCR.E2H|TGE, stay EL2
      mt8678:    arm64_elX_to_el1   eret EL1h
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
   [1] timer_maybe_interrupt          CNTV expired → PPI 27 VIRTUAL
   [2] VcpuGroup::SchedVcpuResumePre  prio / slice   (scheduler.md)
   [3] IRQs off
   [4] gich_maybe_interrupt           bitmap → ICH_LR, maybe UIE
   [5] arm64_el2_resume
         this:  b el2_hvc_resume
         mt8678: hvc #4
         bic TGE, orr E2H, msr hcr, vttbr
         load GPRs, eret EL1h
   │
   ├──────── guest runs ─────────────────────────────────┐
   │                                                     │
   │  physical IRQ/FIQ (HCR.IMO)                         │
   │     EL2  x0 = ZX_ERR_NEXT                           │
   │     Resume swallows, loop unless THREAD_SIGNAL_KILL │
   │                                                     │
   │  sync trap (WFI, SMC, abort, sysreg, …)             │
   │     EL2  save El2State, HCR=RW|TGE|E2H, br lr       │
   │     x0 = ZX_OK                                      │
   │     vmexit_handler                                  │
   │        ┌─ ZX_OK ──────── kernel handled, loop       │
   │        ├─ ZX_ERR_NEXT ── packet filled, return ZX_OK│
   │        │                 to syscall (VMM HandlePacket)
   │        ├─ ZX_ERR_CANCELED  SYSTEM_RESET → VMM stop  │
   │        └─ other error      VMM Stop                 │
   └─────────────────────────────────────────────────────┘
```

VMM `Vcpu::Loop` only sees syscall `ZX_OK` + packet, or a stop
error. `GUEST_BELL` never arrives here (queued on the trap port).

---

## F3  Guest `SMC` dispatch

Always `next_pc` first. `secure_id = x7[31:16]`.

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
   │     ├─ SMC_SC_NBL_VHM_REQ          (stdcall, ACRN HC_*)
   │     │     handle_hypercall_in_el1
   │     │       MSI?     MsiRouter::Inject     ZX_OK
   │     │       NOTIFY?  Vhm::SignalIoreqCompletion
   │     │       else     packet VHM_REQ        ZX_ERR_NEXT  → SOS Vmm
   │     │
   │     ├─ SMC_FC_NBL_VHM_REQ          (fast)
   │     │     x1=0  INJECT_HW_IRQ      interrupt_set_pending
   │     │     x1=1  INJECT_VIRT_IRQ    (CROSS_VM_SCHED)
   │     │     x1=2  NOTIFY_VMLOG       vcpu_kick(0)
   │     │     x1=3  GPU_ARBITER        GpuScheduler::HandleSmc
   │     │     x1=4  NOTIFY_VMLOG_SINK  vq_wakeup
   │     │
   │     └─ else  sm_handle_smc_call    Trusty-in-HEE
   │
   ├─ PSCI64_* / HYPERVISER_GUEST_*
   │     CPU_ON / OFF / SUSPEND / RESET / RESET2 / OFF / SYSTEM_SUSPEND
   │     GUEST_OS_RESUME / PM_STATE / CPUFREQ
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
         ├─ GUEST_TRAP_VHM_BELL     F7 BELL
         └─ GUEST_TRAP_VHM_MEM      F7 SYNC
```

ISV=0 on VHM_MEM: inject DABT, **do not** Kick SOS.

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
   MSI LPI       MsiRouter::Inject  or GicIts::InjectMsi
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

This tree, `WITH_VHM_IN_KERNEL`. Slot `n` = UOS VCPU `n`.

```text
 UOS  ldr/str  IPA in VHM_MEM trap
   │
   ▼
 [1] EL2 DABT  Vhm::Read / Write
 [2] AcquireSlot(vcpu_id)
       FREE → PENDING
       type=MMIO  dir, addr, size, value
 [3] Kick()  interrupt_set_pending(kick_irq=254)
 [4] Wait  IoreqEvent            UOS VCPU blocked
   │
   ▼
 [5] SOS  passthrough SPI 254  (HSM)
       slot PROCESSING
       emulate device
       write result, processed=COMPLETE
 [6] SOS  SMC  HC_NOTIFY_REQUEST_FINISH
       handle_vhm_hypercall  (kernel, SOS only)
       SignalIoreqCompletion
         require COMPLETE already
         FREE + Event
   │
   ▼
 [7] UOS Wait returns
       read: ApplyMmioLoad → Xt
       next_pc, ZX_OK        F2 loops, no VMM packet
```

BELL: [1] `HandleBellTrap` enqueue, **next_pc immediately**.
Worker uses slots 14/15, then Kick. Reads on BELL: not supported.

CREATE/START still `ZX_ERR_NEXT` → SOS Vmm (F8). Only NOTIFY/MSI
and the abort path are in-kernel.

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
         GetIoRequestVmo / GetKickIrq / GetMmioMappings
         zx_guest_vhm_setup
         zx_guest_add_mmio_mapping  SYNC or BELL
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
   │       pcpu_hp_request_up
   │       ResumeThread
   │       ResetEl2State(entry)
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
        yes → hyper_pm_suspend          (pm.md)
                /features/pm ? HyperPmManager
                  WAIT_WAKELOCK → DDK → PDEV → NOIRQ → ARMED
                  gate = SUSPEND
                else legacy DDK+pdev+gate
     WaitForSuspend
   │
   ▼
 idle thread
   hyper_pm_idle_may_enter_atf?
     gate==SUSPEND && all VMs parked → _cpu_suspend ATF
   │
   ▼
 wake SPI / HYPERVISER_GUEST_OS_RESUME
   hyper_pm_notify_wakeup_irq     IRQ context
   worker CompleteResume          gate=RUNNING
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
   └─ platform RESET (SOS + WDT_PROFILE_NOSINK)
         ATF  real reset  →  F1
```

Watchdog miss also `Stop(CANCELED)` → same relaunch.

---

## F13  lua `setVmem` → stage-2 PTE

```text
 sos.lua  setVmem("IO", 0, 0, 1GiB, kDevice, 0)
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
VCPU 0 also `SMC_FC_NBL_SOS_STATE` / `UOS_STATE` with
`&El2State.guest_state` PA.

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

---

Everyday's a Hustle.
