# CPU

**After this:** you can explain one `zx_vcpu_resume` and one
guest `SMC`. **First read:** Diagrams 1–5, then EL2.
**Next:** [`exceptions.md`](exceptions.md) · [`memory.md`](memory.md).

Savior's host CPU is a Zircon / LK kernel. Product HEE **drops to
EL1h** at `_start` (`arm64_elX_to_el1`). EL2 is a trampoline
(`el2.S`) with its own `VBAR_EL2`, identity `TTBR0_EL2`, and
per-CPU stack, turned on at first `Guest::Create` via `HVC #2`.
A guest CPU is a **VCPU**: a pinned host `thread_t` that calls
`arm64_el2_resume` (`HVC #4`, not a `b`), loads guest state, and
`eret`s to guest EL1h. The guest never owns the physical core.
Returning from the guest is an intercept that **`eret`s back to
host EL1 on that same thread**, not a thread switch.

Read [`README.md`](README.md) for the `HVC` trampoline,
[`bootstrap.md`](bootstrap.md) for the first `eret` into
guest EL1, [`scheduler.md`](scheduler.md) for how that thread is
picked against other VCPUs, [`interrupt.md`](interrupt.md) for
`ICH_LR` / IPI wakeup, [`memory.md`](memory.md) for `VTTBR`,
[`vmm.md`](vmm.md) for the userspace resume loop,
[`pm.md`](pm.md) for host suspend after all VMs park,
[`vhm.md`](vhm.md) / [`tee.md`](tee.md) for SMC classes.

Host `svc` is a Zircon syscall ([`microkernel.md`](microkernel.md)).
Guest `smc` is not. This note is the map of that path: host EL0
`svc` vs guest EL2 exit, the VCPU object, EL2 enter/exit, the
`Resume` loop, every intercept class, PSCI (ON/OFF/SUSPEND/RESET),
idle, and SMP.

Start with **Diagrams**, then the sections they index.

## Framework

Two exception paths share one core. They do not share a vector
table.

```text
 Host EL0 thread                         Guest EL1h  (this VCPU thread)
      │ svc / IRQ                              │  HCR.TSC / TWI / IMO
      ▼                                        ▼
 VBAR_EL1  exceptions.S                  VBAR_EL2  arm64_el2_exec_table
      iframe  x0–x30, ELR, SPSR                el2_guest_exit  (el2.S)
      │                                        save GuestState into El2State
      ▼                                        HCR = RW, VTTBR=0, eret host EL1
 arm64_syscall / platform_irq                  │
      eret EL0t                                ▼
                                         Vcpu::Resume
                                           vmexit_handler   or  physical IRQ
                                           loop: arm64_el2_resume  (hvc #4)
                                           or return packet / error to VMM
```

```text
 zx_vcpu_create
    Vcpu::Create
       AllocVpid
       pin_thread_to_cpu(bind pCPU)
       El2State + GichState
       ELR_EL2 = entry, SPSR = EL1h | DAIF
       HCR = VM|PTW|FMO|IMO|FB|BSU_IS|TWI|TSC|RW|APK|API|ATA|TID3
       THREAD_FLAG_VCPU
              │
              ▼
 zx_vcpu_resume   (VMM thread, already pinned)
    Vcpu::Resume
       gich_maybe_interrupt → ICH_LR
       arm64_el2_resume          hvc #4  →  el2_hvc_resume
          msr vttbr, hcr (guest mask), tpidr_el2
          load guest GPRs + sysregs
          eret guest EL1h
              │  intercept
              ▼
       el2_guest_exit
          save GPRs, HCR=RW, vttbr=0, eret host EL1
       ZX_ERR_NEXT  physical IRQ  →  loop (unless KILL)
       ZX_OK        vmexit_handler
```

Three nested “switches”. Only one of them is a thread context
switch.

| Switch | Instruction | What changes |
| --- | --- | --- |
| Host user ↔ kernel | `svc` / `eret` EL0t | EL, `TTBR0` already this process |
| Host thread ↔ thread | `arch_context_switch` | `x19–x30`, `TTBR0`, current `thread_t` |
| Host ↔ guest | `HVC #4` / `eret` | `VTTBR`, `HCR.VM`, guest GPRs. **Same** `thread_t` |

SOS → UOS on one pCPU is: SOS intercept → SOS thread scheduled
out → UOS thread scheduled in → UOS `eret`. There is no
guest→guest `eret`.

## Principle

1. **The VCPU thread is the guest.** Guest Linux CFS / FIFO /
   `handle_irq` live in guest RAM. Savior does not walk them except
   as `SchedParams` sampled for [`scheduler.md`](scheduler.md).
   Power, idle, and ON/OFF are states of this host thread.

2. **Host is EL1h; EL2 is a trampoline.** `_start` calls
   `arm64_elX_to_el1`: `HCR.RW|APK|API|ATA`, `VTTBR=0`, `eret`
   EL1h. Kernel `TTBR1` / `VBAR_EL1` are real EL1. Guest entry is
   `arm64_el2_resume` → `HVC #4` → `el2_hvc_resume`: set
   `VM`/`IMO`/…, `eret` EL1h. Exit writes `HCR=RW`, `VTTBR=0`,
   `eret` host EL1. First `Guest::Create` turns EL2 MMU on
   (`HVC #2`: identity `TTBR0_EL2`, per-CPU `SP_EL2`,
   `VBAR_EL2 = arm64_el2_exec_table`). `venus-tee.mk` uses the
   same trampoline without `WITH_CROSS_VM_SCHED` / `WITH_VIRT_SERROR`.

3. **Kernel-handled vs VMM-handled.** `vmexit_handler` returns
   `ZX_OK` to **resume** (WFI, most SMC/PSCI, SGI, stage-2 fault
   that the PMM can fill). It returns `ZX_ERR_NEXT` with a
   `zx_port_packet_t` to **leave** `Resume` so the VMM emulates
   MMIO, starts a secondary VCPU, or stops the VM. Physical IRQ
   during guest is `ZX_ERR_NEXT` from EL2 itself; `Resume`
   swallows it and loops unless the thread is killed.

4. **Idle is wait-for-interrupt on the VCPU thread, not
   `thread_sleep` of the host idle thread.** Guest `WFI` (trapped
   because `HCR.TWI`) and `PSCI CPU_SUSPEND` both block on
   `GichState::Wait`. A pending LR or bitmap wakes them. The host
   idle thread only runs when **no** VCPU thread is runnable on
   that pCPU.

5. **SMP is PSCI on this Guest, plus pCPU hotplug.** First
   `CPU_ON` for a vCPU that does not exist yet is a
   `ZX_PKT_GUEST_VCPU_STARTUP` packet; the VMM creates the VCPU.
   Later `CPU_ON` of `STATE_PWROFF` is in-kernel
   (`handle_vcpu_on` + `ResumeThread`). Last VCPU `CPU_OFF` does
   **not** take the pCPU down (`#if 0` in `handle_vcpu_off`).

## Diagrams

### 1. Two vector tables (host `VBAR_EL1`, guest `VBAR_EL2`)

```text
 Host hits VBAR_EL1 = arm64_el1_exception_base  (exceptions.S)
 Guest hits VBAR_EL2 = arm64_el2_exec_table     (el2.S)

 EL0t  svc ──► arm64_syscall_dispatcher
 EL0t  IRQ ──► platform_irq
 guest SMC / WFI / abort / IRQ
               VTTBR≠0  ──► el2_guest_exit / el2_guest_exit_or_fp_resume
 host HVC from EL1 (resume / on / tlbi / …)
               VTTBR==0 ──► hvc jump table  (index 4 = el2_hvc_resume)
 guest phys IRQ (HCR.IMO)
               ──► el2_guest_exit   return ZX_ERR_NEXT
```

`VTTBR_EL2 != 0` means “this exception is from the guest”. Host
`HVC` immediates: 0 PSCI, 1 mexec, 2 on/off, 3 TLBI, **4 resume**,
5 sysreg, 6/7 CPU suspend/resume (`WITH_LIB_SM`).

### 2. Two nested switches (no guest→guest `eret`)

```text
              EL0 user thread                 guest EL1h (SOS or UOS)
                   │  svc / IRQ                    │  HCR.IMO / trap
                   ▼                               ▼
              EL1 iframe                      EL2 el2_guest_exit
                   │                          guest GPRs → El2State
                   ▼                               │
           sched_resched_internal                  ▼
              THREAD context switch           Vcpu::Resume
              x19–x30, TTBR0                  still THIS thread_t
                   │                               │
                   ▼                               ▼
              another thread_t                eret same guest
                                              or CS to other VCPU
                                              then THAT VCPU eret

  SOS guest ──exit── SOS thread ──CS── UOS thread ──enter── UOS guest
                    (EL1 host)         (EL1 host)         (EL2 trampoline)
```

### 3. VCPU object

```text
 GuestDispatcher          ZX_OBJ_TYPE_GUEST
    guest_ : Guest
       gpas_     TYPE_GUEST_PHYS  VTTBR
       traps_    MMIO holes
       vmid_     1 SOS / 2 ALPS / 3 tbox
       vcpus_[8]
       vpid_allocator_
              │
              ▼
 VcpuDispatcher           ZX_OBJ_TYPE_VCPU
    vcpu_ : Vcpu
       thread_        current thread, pinned
       bind_cpu_num_
       el2_state_     El2State page  (tpidr_el2 while in guest)
       gich_state_    bitmap + Wait event + vtimer
       hcr_
       state_         INIT / RUNNING / STANDBY / SUSPEND / PWROFF
       vcpu_group_    WITH_CROSS_VM_SCHED
```

`Vcpu::id() = vpid_ - 1`. VPID 1 is VCPU 0. Default pin is
`(vpid-1) % ncpu`, overridden by `zx_guest_set_cpu_affinity`.

### 4. `El2State` (what EL2 saves)

```text
 El2State                    one page, PA passed to el2_hvc_resume
  resume / passive_mode
  guest_state
     x[31]                   GPRs (x0 is SMC FID / PSCI function)
     fp_state
     system_state            EL1 sysregs + ELR_EL2 + SPSR_EL2 + VMPIDR
     cntv_ctl / cntv_cval
     esr_el2, far_el2, hpfar_el2, cntvoff_el2
  host_state                 x18–x29 + host sysregs  (callee-saved)
  ich_state                  VMCR, ELRSR, LR[0..15], APR
```

Guest timer is `CNTV_*_EL0`. EL2 disables it on exit
(`cntv_ctl_el0 = 0`) unless passive mode, and restores host
`CNTHCTL_EL2` so EL1 may touch the physical timer.

### 5. Resume loop

```text
 zx_vcpu_resume
    Vcpu::Resume
       do
          timer_maybe_interrupt          CNTV expired? inject PPI 27 PHYSICAL
          OnVcpuResumePre / ApplyVcpuPolicy  priority / slice  (scheduler.md)
          IRQs off
          gich_maybe_interrupt           bitmap → ICH_LR, maybe UIE
          arm64_el2_resume
             hvc #4 → el2_hvc_resume
             msr vttbr_el2, hcr_el2 (guest mask)
             load guest, eret EL1h
                  │
                  ├─ guest IRQ / FIQ   status = ZX_ERR_NEXT
                  │     swallow, loop unless THREAD_SIGNAL_KILL
                  ├─ trap              status = ZX_OK
                  │     vmexit_handler
                  │        ZX_OK        loop (kernel handled)
                  │        ZX_ERR_NEXT  break, packet to VMM
                  └─ other             return error
       while ZX_OK
```

### 6. Intercept classes (`ESR_EL2.EC`)

```text
  0b000001  WFI/WFE     handle_wfi_wfe_instruction
  0b010110  HVC         x0 = -1, next_pc, ZX_OK   (guest HVC unused)
  0b010111  SMC         handle_smc_instruction    (HCR.TSC)
  0b011000  SYSREG      handle_system_instruction (TID3, SGI, SCTLR, …)
  0b100000  IABT        handle_instruction_abort  (stage-2 / demand)
  0b100100  DABT        handle_data_abort         (trap or fault)
  FP / SVE              el2_fp_resume / el2_sve_resume  (lazy, no C)
```

Physical IRQ is **not** an EC in `vmexit_handler`. EL2 returns
`ZX_ERR_NEXT` from the IRQ vector.

### 7. PSCI on this VCPU

```text
 guest  SMC  (HCR.TSC, EL2, then handle_smc_instruction)

 PSCI64_CPU_ON
    no VcpuDispatcher yet  →  packet STARTUP {id, entry}  ZX_ERR_NEXT
    STATE_PWROFF           →  handle_vcpu_on + ResumeThread
    already RUNNING/INIT   →  SUCCESS, resume

 PSCI64_CPU_OFF
    STATE_PWROFF, maybe pCPU down, SuspendThread
    later ResumeThread resets El2State and returns to guest

 PSCI64_CPU_SUSPEND
    pending LR / bitmap?  SUCCESS without sleep
    entry==0              STATE_STANDBY, WaitForSuspend
    S2IDLE                STATE_SUSPEND, last VM may arm host idle ATF

 PSCI64_SYSTEM_RESET      snapshot + ZX_ERR_CANCELED  →  VMM relaunch
 PSCI64_SYSTEM_RESET2     snapshot + packet STOP      →  VMM teardown
 PSCI64_SYSTEM_OFF        all VMs ZX_ERR_UNAVAILABLE  →  VMM SHUTDOWN
 PSCI64_SYSTEM_SUSPEND    handle_system_suspend  (STR)
```

There is no SOS→ATF platform RESET. The old `do_psci_call` sits
after `return ZX_ERR_CANCELED` and is dead.

### 8. Idle depths

```text
 guest WFI
    HCR.TWI  →  EL2
    LR in use?  resume immediately
    else GichState::Wait  (vtimer oneshot if CNTV enabled)

 PSCI CPU_SUSPEND  entry=0
    STATE_STANDBY
    WaitForSuspend

 PSCI CPU_SUSPEND  S2IDLE
    STATE_SUSPEND
    last VCPU of all VMs  →  GUEST_PM_SUSPEND_TO_IDLE
                             hyper_pm_suspend  (host devices / ATF)

 PSCI SYSTEM_SUSPEND / last VCPU STR
    GUEST_PM_SUSPEND_TO_RAM
    pCPU≠0  may CPU_OFF
    pCPU==0  PSCI64_SYSTEM_SUSPEND to ATF
```

Wake: inject wakeup SPI + `GichState::Signal`, or
`HYPERVISER_GUEST_OS_RESUME`. ALPS drops SPIs not on the wakeup
list while s2idle (`CheckSkipWakeup`).

## Objects

| Object | Role |
| --- | --- |
| `Guest` | One VM. Stage-2 aspace, trap map, VCPU table, PM aggregate |
| `Vcpu` | One guest CPU. Pinned `thread_t`, `El2State`, `GichState`, `hcr_`, power `state_` |
| `VcpuDispatcher` | Zircon handle over `Vcpu`. Table `[vmid][vcpuid]` for IRQ routing |
| `GuestDispatcher` | Zircon handle over `Guest`. `zx_guest_create_new` also returns the guest VMAR |
| `El2State` | What EL2 reads/writes. Guest GPRs + EL1 sysregs + host callee-saved + ICH |
| `GichState` | Software bitmap of pending virtual/physical IRQs, Wait event, timers |
| `vcpu_info_percpu` | Per pCPU: how many VCPU threads, how many active, suspend list |

Kernel VMID vs user vmid is the same table as [`vmm.md`](vmm.md)
and [`README.md`](README.md). `UserVmidtoGuestVmid` adds
`ZX_VMID_USER_TO_GUEST_OFFSET` (SOS −1 → 1).

### VCPU power state

```text
                 Create
                   │
                   ▼
                 INIT  (STARTUP packet outstanding, or just created)
                   │
                   ▼
                RUNNING ⇄ STANDBY     (WFI / CPU_SUSPEND entry=0)
                      ⇄ SUSPEND     (S2IDLE)
                      ⇄ PWROFF      (CPU_OFF)
```

`VcpuDispatcher::Create` sets `STATE_RUNNING` immediately. First
`CPU_ON` of a not-yet-created VCPU sets the **caller** to
`STATE_INIT` only for the packet window.

Guest PM (`GUEST_PM_*`) is **derived from all VCPUs** of that VM
(`Guest::PmStateLocked`):

| PM | When |
| --- | --- |
| `RUNNING` | any VCPU RUNNING / INIT |
| `SUSPEND_TO_IDLE` | all VCPUs SUSPEND / PWROFF (s2idle) |
| `SUSPEND_TO_RAM` | `handle_system_suspend` |
| `POWER_OFF` | no VCPU 0 / all off |
| `NOT_CREATED` | guest not in the table |

`is_all_vm_suspend()` is the host-wide gate used by
`hyper_pm_suspend`.

## Host exception path

Host userspace never sees EL2. `VBAR_EL1` is
`arm64_el1_exception_base` (`exceptions.S`).

```text
 EL0t  svc
    ESR.EC = SVC
    arm64_syscall_dispatcher
       x16 = syscall number
       .Lcall_wrapper_table[x16]
       arm64_syscall  /  generated wrapper
    frame->r[0] = status
    eret EL0t

 EL0 / EL1  IRQ
    platform_irq  →  arm_gicv3
    maybe InterruptEventDispatcher → Vcpu::Interrupt
    maybe IPI to a pinned VCPU   (interrupt.md)
```

`zx_vcpu_resume` is an ordinary syscall on the VMM's VCPU thread.
The guest round-trip happens **inside** that syscall, with
interrupts off. HEE (and TEE) enter EL2 with `HVC #4`.

Host `svc`/IRQ stay on `VBAR_EL1`. Guest exits always hit
`VBAR_EL2`.

## EL2

Product HEE is non-VHE. `el2.S` is the live file: two vector
tables (init vs exec), host `HVC` immediates, guest `eret`.

Boot (`arm64_elX_to_el1`):

```text
 already EL2  (ATF SPSR)
 init_el2_state
 VBAR_EL2 = arm64_el2_init_table
 HCR_EL2 = RW | APK | API | ATA
 VTTBR_EL2 = 0
 eret EL1h                   kernel uses real EL1 names
```

First `Guest::Create` calls `arm64_el2_on(table.Base(), stack.Top())`
per CPU (`HVC #2`): identity `El2TranslationTable` (L0/L1/L2 of
Zircon DRAM) becomes `TTBR0_EL2`, per-CPU `El2Stack` becomes
`SP_EL2`, `VBAR_EL2 = arm64_el2_exec_table`, EL2 MMU on.

`El2CpuState` is a singleton, refcounted by `num_guests`:

```text
 first Guest::Create
    El2CpuState::Create
    percpu_exec OnTask → arm64_el2_on
    unmask maintenance IRQ (ICH)
 last Guest destroyed
    num_guests==0 → arm64_el2_off per CPU
```

`WITH_SVE_SUPPORT` is not compiled. `el2.S` sets `CPTR_EL2_RES1
= 0x32ff` so the SVE trap is off; there is no `el2_sve_resume`.

`WITH_VIRT_SERROR` (this HEE **true**) adds `HCR.AMO` so physical
SError is taken at EL2 while the guest runs. SOS
`MaybeInjectVabt` sets `HCR.VSE`; other VMs `MaskVabt`.

### Guest enter (`el2_hvc_resume`)

`arm64_el2_resume` is `hvc 4`. Arguments: `x0=VTTBR`,
`x1=El2State` **PA**, `x2=hcr_`.

```text
 switch_to_guest                 msr vttbr_el2, x0
 tpidr_el2 = El2State
 first resume?  tlbi vmalle1
 msr hcr_el2, x2                 VM|IMO|TWI|TSC|…  (no TGE/E2H)
 CNTHCTL_EL2 = EL1PCTEN          (guest may read CNTPCT, not CNTP)
 CPTR_EL2    traps FP (lazy)
 save host x18–x30 + sysregs
 load guest sysregs via EL1 names (sctlr_el1 / ttbr0_el1 / …)
 load guest GPRs
 eret                            ELR_EL2 / SPSR_EL2 = EL1h | DAIF
```

`HCR` at Create (and every enter):

| Bit | Why |
| --- | --- |
| `VM` | stage-2 on |
| `PTW` | S2 permission fault on table walk |
| `FMO` / `IMO` | physical FIQ/IRQ taken to EL2, not guest |
| `FB` / `BSU_IS` | inner-shareable barrier upgrade |
| `TWI` | trap `WFI` |
| `TSC` | trap `SMC` |
| `RW` | EL1 is AArch64 |
| `APK` / `API` / `ATA` | pointer auth |
| `TID3` | trap ID-group-3 registers (feature hiding) |
| `AMO` | optional, `WITH_VIRT_SERROR` |

`TWE` (WFE) and `TVM` / `DC` are **off** on the product path.
`WFE` still reaches `handle_wfi_wfe_instruction` only if it
trapped for another reason; the WFE case is `thread_reschedule()`
and resume.

### Guest exit (`el2_guest_exit`)

```text
 save guest GPRs + sysregs + ESR/FAR/HPFAR
 maybe swap FP
 CNTV_CTL_EL0 = 0
 CNTHCTL_EL2 = EL1PCTEN|EL1PCEN
 HCR_EL2 = RW                    (stage-2 off)
 switch_to_host                  vttbr = 0
 eret host EL1                   x0 = return_code
```

Return codes into `Vcpu::Resume`:

| `x0` | Meaning |
| --- | --- |
| `ZX_OK` | sync trap, run `vmexit_handler` |
| `ZX_ERR_NEXT` | physical IRQ/FIQ/SError while in guest |
| other | `arm64_el2_resume` failed |

## Resume

`Vcpu::Resume` is the in-kernel body of `zx_vcpu_resume`. The
calling thread **must** be the pinned one
(`check_pinned_cpu_invariant`).

Before `HVC`:

1. `timer_maybe_interrupt` — if guest `CNTV` expired, track PPI 27.
2. `VcpuGroup::OnVcpuResumePre` / `ApplyVcpuPolicy` — may raise priority / cut slice
   ([`scheduler.md`](scheduler.md)).
3. Interrupts off.
4. `gich_maybe_interrupt` — pop bitmap into free `ICH_LR`. If the
   bitmap still has IRQs and there is more than one LR, set
   `ICH_HCR.UIE` so an empty list causes a maintenance exit.
5. Optional pending `InjectException` (unhandled MMIO → guest DABT).

After `HVC`:

1. `gic_read_gich_state` + `TrackAllListRegisters` — LRs that the
   guest EOI'd drop out of the bitmap.
2. Interrupts on.
3. `VcpuGroup::OnVcpuResumePost`.
4. Branch on `status` as in diagram 5.

`ZX_ERR_NEXT` from `vmexit_handler` becomes `ZX_OK` **to the
syscall** (`SetVMCallMode` first). The VMM sees `zx_vcpu_resume`
success plus a filled `zx_port_packet_t`.

Userspace loop (`garnet/lib/machina/vcpu.cc` `Vcpu::Loop`):

```text
 for (;;)
    zx_vcpu_resume(&packet)
    if status != ZX_OK
       STOP / CANCELED / UNAVAILABLE  →  guest_->Stop, thread exits
       else error
    HandlePacket
       GUEST_VCPU  STARTUP / STOP / VHM_REQ / SGI …
       GUEST_MEM   emulate MMIO (GICD, virtio, …)
       unhandled MEM  →  zx_vcpu_inject_exception
```

`GUEST_BELL` never reaches this loop: the kernel queued it on the
trap's port already.

## Intercepts

`vmexit_handler` switches on `ExceptionSyndrome(esr_el2).ec`.
Every handler that consumes the instruction calls `next_pc`
(`ELR_EL2 += 4`, or 2 if 16-bit).

### WFI / WFE

WFE: `thread_reschedule()`, `ZX_OK`.

WFI: if any LR is valid, resume (an IRQ is already in the guest
CPU interface). Else arm a oneshot from `CNTV_CVAL` if the virtual
timer is enabled, then `GichState::Wait` forever. `Vcpu::Interrupt`
signals that event. `HCR.TWI` makes this trap; without it the
guest would WFI on the physical core and miss host IRQs.

### Guest HVC

Set `x0 = -1`, advance PC, `ZX_OK`. Guest hypercalls on this
product are **SMC** (`HCR.TSC`), not HVC. Host HVC is the EL2
control plane above.

### SMC  (`handle_smc_instruction`)

Always `next_pc` first. Then:

**FID layout** (`hvcall.h`):

```text
 x0  bit 31     fastcall (1 = FC, 0 = SC)
     bit 30     SMC64
     29:24      entity   SMC_ENTITY_NEBULA = 52
     15:0       function
 x7  31:16      SMC_SECURE_ID   0 = TEE, 1 = HYP
```

`SMC_SC_NBL_VHM_REQ` = stdcall entity 52 fn 256 → `0x34000100`.
`SMC_FC_NBL_VHM_REQ` = fastcall same → `0xB4000100`.
`validate_hypervcall`: those two FIDs **require** `x7` HYP id;
mismatch injects IABT (`VBAR_EL1+0x200`, ESR EC=0x21). HYP id on
a non-Nebula entity is a warning and still proceeds (SMCCC 1.1
`r7` unknown → treat as TEE passthrough later).

**Savior hypercalls** (`SMC_ENTITY_NEBULA` and
`SMC_HYP_SECURE_ID` in `x7`). Test `x1 == 0xB1000003` dumps GIC
in-kernel and does **not** post a packet.

| `x0` | `x1` | Kernel |
| --- | --- | --- |
| `SMC_SC_NBL_VHM_REQ` | ACRN / IPC / guest-call | packet `VHM_REQ`, `ZX_ERR_NEXT` |
| `SMC_FC_NBL_VHM_REQ` | `FC_ID_INJECT_HW_IRQ` (0) | `interrupt_set_pending(x2)` on **host GIC** |
| | `FC_ID_INJECT_VIRT_IRQ` (1) | `inject_virt_irq(x2=vmid, x3=vec)` |
| | `FC_ID_NOTIFY_VMLOG` (2) | `trusty::vcpu_kick(0)` |
| | `FC_ID_GPU_ARBITER` (3) | `handle_gpu_arbiter_cmd(x2,x3,x4)` |
| | `FC_ID_NOTIFY_VMLOG_SINK` (4) | `trusty::vq_wakeup(x2)` |
| | other | `x0 = ZX_ERR_INVALID_ARGS` |
| other HYP stdcall | | `sm_handle_smc_call` (Trusty-in-HEE) |

Illegal Savior parameters inject an instruction abort into the
guest instead of forwarding.

**PSCI** (`dev/psci.h`) — diagram 7. Always `next_pc` first;
`x0` on return is PSCI status unless noted.

| `x0` | Name | Kernel |
| --- | --- | --- |
| `0x84000000` | `PSCI_VERSION` | `(1<<16)\|1` |
| `0xC4000003` | `CPU_ON` | STARTUP packet or `handle_vcpu_on` |
| `0x84000002` | `CPU_OFF` | `handle_vcpu_off`; KILLED packet if fail |
| `0xC4000004` | `AFFINITY_INFO` | always `PSCI_0_2_AFFINITY_LEVEL_OFF` |
| `0xC4000001` | `CPU_SUSPEND` | `handle_vcpu_suspend`; `x1`: S2IDLE=`0x020180ff`, STANDBY=`0x00010000` |
| `0x8400000B` | `CPU_FREEZE` | `PSCI_NOT_SUPPORTED` |
| `0xC400000E` | `SYSTEM_SUSPEND` | `handle_system_suspend` STR |
| `0x84000009` | `SYSTEM_RESET` | snapshot + `ZX_ERR_CANCELED` (dead ATF call after return) |
| `0xC4000012` | `SYSTEM_RESET2` | snapshot + packet `STOP` |
| `0x84000008` | `SYSTEM_OFF` | `ZX_ERR_UNAVAILABLE` |

**`HYPERVISER_GUEST_*`** (Savior, not PSCI). SOS pokes another
VM. `x1` user vmid unless noted.

| `x0` | Name | Action |
| --- | --- | --- |
| `0xF0000000` | `OS_RESUME` | `handle_wakeup_guest_os(vmid, x2)` |
| `0xF0000001` | `OS_PM_STATE` | `get_pmstate_form_vmid`; retry 2×10 ms if still s2idle |
| `0xF0000002` | `OS_CPUFREQ` | `handle_vm_cpufrq` |
| `0xF0000003` | `OS_IRQ_EVENT` | `handle_irq_event(from, to, irq)` VCPU 0 VIRTUAL |
| `0xF0000004` | `OS_READ` | own ring/notify buf → x0–x3 |
| `0xF0000005` | `OS_WRITE` | other VM ring + `smc_irq`; `target=0xFF` → GuestManager |
| `0xF0000006` | `OS_UNLOCK` | inject `cvm_lock_irq` all VMs |
| `0xF0000100` | `TBOX_BOOT_COUNT` | `tbox_boot_count` |
| `0xF0000101` | `VCPU_MODE_BUDGET` | `SetGroupBudget` |
| `0xF0000102` | `VCPU_VM_PRIORITY` | hint mode/prio |
| `0xF0000103` | `ECALL_END` | clear flags, restore budget/period |

Unlisted FIDs fall through to TEE.

### System instruction  (`HCR.TID3` and friends)

| Register | Action |
| --- | --- |
| `SCTLR_EL1` write, `M=1` | drop `HCR.DC`; if `C=1` also drop `HCR.TVM`; cache-clean S2 |
| `TCR` / `TTBR0` / `TTBR1` | store into `GuestState` EL1 fields |
| `ID_AA64*` / `ID_*` / `MVFR*` | `handle_tid3_reg` — sanitized feature IDs |
| `ICC_SGI1R_EL1` write | `handle_sgi` — guest IPI, virtual inject |
| unknown | `ZX_ERR_NOT_SUPPORTED` |

SGI never becomes a host SGI. See [`interrupt.md`](interrupt.md).

### Instruction / data abort

IPA = `HPFAR_EL2` plus page-offset bits of `FAR_EL2`.

**No trap** (`ZX_ERR_NOT_FOUND`): `vmm_guest_page_fault_handler`.
Demand-paged guest RAM fills a PTE and `Resume` loops. Still
failing → inject a guest data abort (`VBAR_EL1 + 0x200`).

**Trap present:**

| Kind | Kernel | Userspace |
| --- | --- | --- |
| `ZX_GUEST_TRAP_BELL` | queue `ZX_PKT_TYPE_GUEST_BELL` on the trap port, `ZX_OK` | waiter on that port (VHM BELL uses this) |
| `ZX_GUEST_TRAP_MEM` | fill `ZX_PKT_TYPE_GUEST_MEM`, `ZX_ERR_NEXT` | `HandleMem` emulates (VGIC, virtio, VHM SYNC) |

Unhandled MEM in the VMM injects a guest DABT
(`zx_vcpu_inject_exception`) rather than killing the VCPU thread.
Fields and host exception ports: [`exceptions.md`](exceptions.md).

## PSCI, idle, SMP

### `CPU_ON`

`x1` = target MPIDR, `x2` = entry.

`arch_cpu_mpidr_to_cpu_num` → vCPU id. If that id has no
`VcpuDispatcher` yet, the kernel does **not** create it. It posts
`ZX_PKT_GUEST_VCPU_STARTUP` and returns `PSCI_SUCCESS` in `x0`.
The VMM (`HandleVcpu`) must `zx_vcpu_create` on VCPU 0's thread
factory, pin, `WriteState`, `Resume`. That is why secondary VCPUs
are created from VCPU 0 ([`vmm.md`](vmm.md) Launch).

If the VCPU exists and is `PWROFF`, `handle_vcpu_on`:

- if the pCPU is offline, `suspend_for_cpu_on` and wait
- `ResumeThread` — the parked VCPU thread continues after
  `SuspendThread`; `ResetEl2State(entry)` runs on the off
  VCPU after `SuspendThread` returns

### `CPU_OFF`

`handle_vcpu_off`: `STATE_PWROFF`, drop `vcpu_thread_active`,
cancel timers, `SuspendThread`. The pCPU-offline body is
`#if 0`. When someone `ResumeThread`s, the function
resets EL2/VCPU state and returns `ZX_OK` to the SMC handler,
which writes `PSCI_SUCCESS` and the next `Resume` `eret`s at the
saved entry.

Killing the thread while off surfaces as
`ZX_PKT_GUEST_VCPU_KILLED` / `ZX_ERR_CANCELED`.

### WFI vs `CPU_SUSPEND` vs STR

| Guest op | VCPU `state_` | Blocks on | Host PM |
| --- | --- | --- | --- |
| `WFI` | stays RUNNING | `GichState::Wait` | none |
| `CPU_SUSPEND` entry=0 | STANDBY | `WaitForSuspend` | none |
| `CPU_SUSPEND` S2IDLE | SUSPEND | `WaitForSuspend` | last VM → `hyper_pm_suspend` |
| `SYSTEM_SUSPEND` | (thread parked) | `SuspendThread` | pCPU0 → ATF `SYSTEM_SUSPEND` |

`WaitForSuspend` refuses to sleep if an LR is valid or the bitmap
is pending — a wake IRQ must not be turned into a fake
`PSCI_SUCCESS`. After 20 s uptime, S2IDLE also skips the vtimer
oneshot (the guest is not expected to wake on CNTV).

`handle_wakeup_guest_os`:

- s2idle: inject the wakeup SPI on every `STATE_SUSPEND` VCPU;
  VCPU 0 retries `WakeupSuspend` (up to 10 × 10 ms)
- STR: `pcpu_ensure_online` if needed, `ResumeThread`
- still freezing: `guest_pm_cancel_suspend` + inject so the next
  PSCI sees pending IRQs

### Pin and IPI

`pin_thread_to_cpu` sets `thread->cpu_affinity` to one bit.
`Vcpu::Interrupt` / `InterruptCpu` send a host IPI to that pCPU
if the VCPU is running (or the pCPU is in s2idle) so EL2 exits
and the new LR is picked up. That is the
[`interrupt.md`](interrupt.md) wakeup path, not a guest SGI.

Lua `cpus` / `SetCpuAffinity` fills `Guest::cpu_binding_table_`.
Default pin is `(vpid-1) % ncpu`, or `cpu_binding_table_[cpu_of(vpid)]`
if set. `VcpuGroup::Add` is a userspace ioctl after create, not
inside `Vcpu::Create`.

## Syscalls

All hypervisor syscalls in `syscalls/hypervisor.cpp` plus GPU.
Rights are on the handle. `hcr` to `zx_vcpu_create` is **ignored**.
Object lifetime, `El2State` / `GichState` / `TrapMap`:
[`objects.md`](objects.md).

| Call | Rights | Kernel |
| --- | --- | --- |
| `zx_guest_create_new` | hypervisor resource | `GuestDispatcher::Create` + guest VMAR. AHYP only |
| `zx_guest_create` | hypervisor resource | physmem VMO + `SetupPHyp`. PHYP; refused QEMU_AHYP |
| `zx_guest_set_trap` / `remove_trap` | WRITE | `TrapMap` + `UnmapRange` |
| `zx_guest_set_cpu_affinity` | WRITE | `cpu_binding_table_` |
| `zx_guest_set_wakeup_irqs` | WRITE | s2idle list; `0xdeadbeef` cut_point required |
| `zx_guest_set_user_vmid` | WRITE | user vmid; tbox first time `init_mtk_spm_map` |
| `zx_guest_set_smc_irq` | WRITE | virtual IRQ when a forwarded SMC completes |
| `zx_guest_set_cvm_lock_irq` / `_vmo` | WRITE | [`guest.md`](guest.md) cross-VM lock |
| `zx_guest_tee_vm_create` / `_destroy` | WRITE | [`tee.md`](tee.md) |
| `zx_vcpu_create` | guest WRITE | pin, `STATE_RUNNING` |
| `zx_vcpu_resume` | EXECUTE | `Vcpu::Resume` |
| `zx_vcpu_interrupt` | SIGNAL | VIRTUAL |
| `zx_vcpu_stop` | SIGNAL | `Vcpu::Stop` |
| `zx_vcpu_on` / `zx_vcpu_off` | EXECUTE | `handle_vcpu_on` / `_off` (also PSCI) |
| `zx_vcpu_read/write_state` | READ/WRITE | `El2State` |
| `zx_vcpu_inject_exception` | WRITE | pending DABT, next enter |
| `zx_vcpu_dump_state` | READ | proto dump for watchdog |
| `zx_poweron_phy_cpu` | — | pCPU hotplug (`power_on_cpu`) |
| `zx_gpu_scheduler_*` | GPU rights | [`gpu.md`](gpu.md); AHYP only |

`zx_interrupt_bind_vcpu` (passthrough) is
[`interrupt.md`](interrupt.md), not this table.

Swimlanes for resume / SMC / CPU_ON / s2idle: [`flows.md`](flows.md)
F2, F3, F10, F11.

## Flows

### Primary VCPU first enter

```text
 Vmm  StartVcpu(0)
    thrd_create  already running on some pCPU
    zx_vcpu_create(guest, 0, entry)
       pin to bind_cpus[0]
       ELR_EL2 = Image (DTB+64 KiB)
       SPSR     = EL1h | DAIF
       SOS/ALPS: SMC_FC_NBL_SOS_STATE / UOS_STATE  (TEE can see GPRs)
    WriteState  x0 = DTB, others 0
    GICR BindVcpu
    loop  zx_vcpu_resume
       first arm64_el2_resume
       tlbi vmalle1
       eret guest  (Linux ARM64 protocol)
```

### Guest WFI, then a passthrough SPI

```text
 guest  wfi
    EL2  EC=WFI
    no LR  →  GichState::Wait
 device SPI
    host GIC  →  VcpuIrqHandler
    Vcpu::Interrupt(PHYSICAL)  bitmap + Signal + IPI
    Wait returns
    Resume  gich_maybe_interrupt  LR HW=1
    eret guest  takes the vIRQ
    guest EOI  deactivates physical INTID
```

### Secondary CPU_ON (first time)

```text
 guest  PSCI CPU_ON  mpidr=cpu1, entry
    SMC  EL2
    no VcpuDispatcher[1]
    packet STARTUP {id=1, entry}
    zx_vcpu_resume returns ZX_OK
 VCPU 0 Loop  HandleVcpu
    guest_->StartVcpu(entry, 1)
       zx_vcpu_create on the factory
       pin bind_cpus[1]
       WriteState, Resume loop on a new host thread
```

### S2idle then resume

```text
 each VCPU  PSCI CPU_SUSPEND  S2IDLE
    no pending LR
    vcpu_enter_suspend
    last one  GUEST_PM_SUSPEND_TO_IDLE
              tbox: notify_modem_suspend
              is_all_vm_suspend → hyper_pm_suspend
    WaitForSuspend
 wakeup SPI / HYPERVISER_GUEST_OS_RESUME
    inject + Signal
    SetVcpuState(RUNNING), GUEST_PM_RUNNING
    hyper_pm_resume
    SetGuestEntry(entry)  next eret at the PSCI return
```

## Pitfalls

- **WFI without `HCR.TWI`.** Guest waits in EL1; host IRQs are
  taken to EL2 (`IMO`) but a WFI that is not trapped can still
  miss the software Wait path. Product HCR always sets `TWI`.
- **`CPU_ON` before the VMM factory is registered.** STARTUP
  packet has nowhere to go; secondary CPUs never appear.
- **Fake `PSCI_SUCCESS` on S2IDLE.** If Wait is skipped while an
  IRQ is pending, Linux thinks suspend succeeded and loses the
  wake. The pending-LR check exists for this.
- **RESET vs RESET2.** Both call `ramconsole_snapshot_take`
  first. `SYSTEM_RESET` is then `ZX_ERR_CANCELED`
  (VMM `REBOOT_REQUIRED`, relaunch). `SYSTEM_RESET2` is
  `GUEST_VCPU_STOP`. Neither enters BL2; [`logging.md`](logging.md).
  There is no SOS→ATF platform RESET in this tree (that call is
  dead code after the `return`).
- **`SYSTEM_OFF`.** All VMs `ZX_ERR_UNAVAILABLE` (VMM `SHUTDOWN`),
  not SOS→ATF.
- **Pin invariant.** `zx_vcpu_resume` from the wrong pCPU returns
  `ZX_ERR_BAD_STATE`. Do not `thread_set_cpu_affinity` after
  create. `zx_vcpu_create`'s `hcr` argument is accepted and
  **ignored**; `hcr_` is the hardcoded mask in `Vcpu::Create`.
- **Guest HVC is a no-op.** Drivers that issue `hvc #0` see
  `x0 = -1`. Use SMC.
- **Unhandled MEM must not kill the VCPU thread.** Machina
  injects a guest DABT and continues. A `ZX_ERR_*` from
  `zx_vcpu_resume` **does** stop the VM.

## Terminology

| Term | Meaning here |
| --- | --- |
| VCPU | Pinned host thread that `eret`s into one guest CPU |
| pCPU | Physical core. Affinity of that thread |
| `El2State` | Page EL2 uses to swap host/guest registers |
| Intercept / vmexit | EL2 taken from guest EL1; C handler may resume or return a packet |
| `HCR.TSC` / `TWI` / `IMO` | Trap SMC, trap WFI, take physical IRQ at EL2 |
| PSCI | Guest `SMC` power API. Savior implements it; ATF sees it only for SoC-wide ops |
| S2IDLE | All VCPUs `STATE_SUSPEND`. `GUEST_PM_SUSPEND_TO_IDLE` |
| STR | `GUEST_PM_SUSPEND_TO_RAM`. May `CPU_OFF` secondary pCPUs |
| STARTUP packet | First `CPU_ON` of a VCPU the kernel does not have yet |
| EL2 trampoline | Host EL1h; guest enter/exit via `el2.S` `HVC #4` / `eret` |

## Code

| Step | File |
| --- | --- |
| Host vectors / `svc` | `zircon/kernel/arch/arm64/exceptions.S` |
| Guest enter / exit / HVC table | `zircon/kernel/arch/arm64/hypervisor/el2.S` |
| First/last guest EL2 on (`HVC #2`) | `el2_cpu_state.cpp` |
| Syscall table | `zircon/kernel/syscalls/hypervisor.cpp` |
| Boot drop to EL1h | `zircon/kernel/arch/arm64/asm.S` `arm64_elX_to_el1` |
| HEE flags | `zircon/kernel/project/venus-hee.mk` |
| `El2State` layout / HCR bits | `zircon/kernel/arch/arm64/include/arch/arm64/el2_state.h` |
| `Guest` / `Vcpu` / PM | `zircon/kernel/arch/arm64/include/arch/hypervisor.h` |
| `Vcpu::Create` / `Resume` / `Interrupt` | `zircon/kernel/arch/arm64/hypervisor/vcpu.cpp` |
| `vmexit_handler` / PSCI / abort | `zircon/kernel/arch/arm64/hypervisor/vmexit.cpp` |
| Exception classes | `vmexit_priv.h` |
| Host suspend / PM aggregate | `hyp_power.cpp` |
| Pin | `zircon/kernel/lib/hypervisor/cpu.cpp` |
| `THREAD_FLAG_VCPU` | `zircon/kernel/kernel/thread.c` `set_to_vcputhread` |
| Dispatchers | `zircon/kernel/object/vcpu_dispatcher.cpp`, `guest_dispatcher.cpp` |
| Syscalls | `zircon/kernel/syscalls/hypervisor.cpp` |
| Boot modes | `zircon/kernel/arch/arm64/include/arch/arm64.h` |
| VMM resume loop | `garnet/lib/machina/vcpu.cc` |
| VCPU 0 factory | `garnet/bin/guest/vmm.cc` `StartPrimaryVcpu` |

---

Prev: [`bootstrap.md`](bootstrap.md) · Next: [`memory.md`](memory.md) · [`README.md`](README.md)

Everyday's a Hustle.
