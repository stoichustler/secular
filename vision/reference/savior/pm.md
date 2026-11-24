# Power

**After this:** you can say why idle entered ATF.
**First read:** Framework + diagrams 1–5. **Next:** [`devices.md`](devices.md).

Guest idle is a VCPU state ([`cpu.md`](cpu.md)). **Host suspend**
is a second machine: devices freeze, then idle CPUs may
`PSCI CPU_SUSPEND` into ATF. This tree has no `HyperPmManager`.
The gate is a two-state `hyper_suspend_state` armed inside
`handle_vcpu_suspend`. `hyp_power.cpp` is VM-table / modem
helpers, not a 3900-line manager.

Read [`vmm.md`](vmm.md) for s2idle / STR from the guest’s point
of view, [`cpu.md`](cpu.md) for `WaitForSuspend` / `CPU_OFF`,
[`interrupt.md`](interrupt.md) for wakeup SPIs.

This note is the host gate, `SuspendDispatcher` DDK, the idle-loop
contract, and tbox modem notify.

## Principle

1. **Guests park first.** Idle CPUs may enter ATF only if
   `hyper_suspend_state == HYPER_PM_SUSPEND` **and**
   `is_all_vm_suspend()` **and** no wakeup-source lock is held.
   A wake IRQ that unparks any VCPU must flip the gate back, or
   idle cores will `CPU_SUSPEND` while a guest still runs.

2. **One C path, no DTS profile.** There is no `/features/pm`
   and no `ZX_PM_PROFILE_MANAGER`. `handle_vcpu_suspend` itself
   does DDK `DeviceSuspendNotify` + `lk_init_level(DEVICE_SUSPEND)`
   then stores `HYPER_PM_SUSPEND`. Callers only see that atomic
   plus `is_all_vm_suspend()`.

3. **The gate has two values.** `HYPER_PM_RUNNING` (0) and
   `HYPER_PM_SUSPEND` (1). There is no `PREPARE` / `SUSPEND_HOLD`
   / worker thread. `arch_idle_thread_routine` and the VCPU that
   leaves `WaitForSuspend` look at the same atomic.

4. **Lock is `hyper_suspend_lock` around the DDK round.**
   `vm_spinlock` protects the `guests[]` table used by
   `is_all_vm_suspend`. Do not reverse that with `SetVcpuState`.

5. **Pending LR is not sleep.** `handle_vcpu_suspend` returns
   `PSCI_SUCCESS` (`ZX_OK`) without parking if `ICH_LR` is in
   use or the bitmap is pending. Otherwise the wake would be
   lost.

6. **Three gates before ATF are one invariant.** Idle may
   `_cpu_suspend` only if `hyper_suspend_state == SUSPEND` **and**
   `is_all_vm_suspend()` **and** no wakeup-source lock. Drop any
   one and GPUEB can power-gate while a guest still has Mali
   mapped. The guest parks first; the host idle loop only
   *follows*.

## Framework

```text
 last VCPU of last VM  PSCI CPU_SUSPEND S2IDLE
    handle_vcpu_suspend
       pending LR / bitmap?  SUCCESS, do not sleep
       STATE_SUSPEND
       last VCPU of this VM  GUEST_PM_SUSPEND_TO_IDLE
       tbox: notify_modem_suspend
       is_all_vm_suspend?
            DeviceSuspendNotify          (user DDK)
            lk_init DEVICE_SUSPEND       (kernel pdev)
            hyper_suspend_state = HYPER_PM_SUSPEND
            wakeup_all_other_cpus
       WaitForSuspend
              │
              ▼
 idle thread
    HYPER_PM_SUSPEND && is_all_vm_suspend()
    && hyper_wakeup_locked_empty()
         _cpu_suspend(PSCI64_CPU_SUSPEND, S2IDLE)  ATF
 wake SPI / HYPERVISER_GUEST_OS_RESUME
    first VCPU out of WaitForSuspend
         hyper_suspend_state = HYPER_PM_RUNNING
         DeviceResumeNotify + lk_init DEVICE_RESUME
    guests  WaitForSuspend returns
```

Swimlane: [`flows.md`](flows.md) F11.

## Diagrams

### 1. Gate

```text
 hyper_suspend_state          who writes it

 HYPER_PM_RUNNING  (0)        boot; first VCPU leaving WaitForSuspend
 HYPER_PM_SUSPEND  (1)        last VM parked, DDK+pdev already frozen
```

Idle may call ATF **only** on `HYPER_PM_SUSPEND` **and**
`is_all_vm_suspend()` **and** no locked wakeup source. Otherwise
the idle thread stays in `WFI`.

### 2. `is_all_vm_suspend`

Counted under `vm_spinlock`. POWER_OFF counts as parked.

```text
 for vmid in 0..MAX_VM_NUM-1
    guests[vmid] live?
       active++
       PmState in {SUSPEND_TO_IDLE, POWER_OFF}?  parked++
 return active >= 1  &&  parked == active
```

A single leftover `GUEST_PM_RUNNING` (one VCPU still
RUNNING/INIT) blocks the gate. tbox POWER_OFF with SOS/UOS
s2idle still arms it.

### 3. `handle_vcpu_suspend` (s2idle)

```text
 pending ICH_LR or bitmap?     return ZX_OK (fake success would lose the wake)
 entry != 0 && S2IDLE
    STATE_SUSPEND
    last VCPU of this VM → GUEST_PM_SUSPEND_TO_IDLE
    tbox: notify_modem_suspend (SPM 0x1c000000)
    ALPS: send_vm_status_message VMCTL_SUSPENDED
    is_all_vm_suspend && still RUNNING
       DDK freeze → kernel DEVICE_SUSPEND → gate=SUSPEND
       wakeup_all_other_cpus          (idle threads re-check the gate)
    WaitForSuspend                    (this VCPU thread blocks)
    if gate still SUSPEND             (we are the first to leave)
       gate=RUNNING
       wakeup_all_other_cpus
       DDK thaw → kernel DEVICE_RESUME
    GUEST_PM_RUNNING, tbox notify_modem_resume
    SetGuestEntry(entry)              next eret = PSCI return
    STATE_RUNNING
 entry == 0
    STATE_STANDBY, WaitForSuspend     (shallower; no DDK)
```

`WaitForSuspend` is `GichState::Wait`. A pending LR, bitmap, or
`Signal` (wakeup SPI / `HYPERVISER_GUEST_OS_RESUME`) unblocks it.

### 4. Idle thread → ATF (the actual CPU_SUSPEND)

Three checks, every loop. Missing any one of them is “GPUEB
off while a guest runs”.

```text
 arch_idle_thread_routine   (per pCPU, never returns)
    for (;;)
       if gate == SUSPEND
          && is_all_vm_suspend()
          && WakeupSourceDispatcher::CheckAllWakeupSources == ZX_OK
             slp_trace = STAGE_1
             _cpu_suspend(PSCI64_CPU_SUSPEND, PSCI_IDLE_STATE_S2IDLE)
                │
                ▼
             ATF  CPU in s2idle
                wake SPI / timer
                │
                ▼
             return to idle thread
             slp_ret = psci_ret
             if this pCPU is no longer idle → thread_reschedule
       wfi
```

There is no `hyper_pm_notify_wakeup_irq` GIC hook. Wakeup is the
normal passthrough SPI / `HYPERVISER_GUEST_OS_RESUME` path that
`Signal`s the parked VCPU; that VCPU then flips the gate.

**SPM mailbox (tbox).** Physical `0x1c000000`, 4 KiB, mapped the
first time tbox sets user vmid (`init_mtk_spm_map`).

| Offset | Name | Suspend | Resume |
| --- | --- | --- | --- |
| `0x904` | `VLP_AO_RSVD1` | `|= 1<<2` | `&= ~(1<<2)` |
| `0x938` | `AP2MD … IRQ_CLEAR` | `0x3` | `0x3` |
| `0x934` | `AP2MD … IRQ_SET` | `1<<0` | `1<<1` |

**DDK freeze.** `DeviceSuspendNotify` sets
`ZX_DEVICE_SUSPEND` on the global `SuspendDispatcher`; host
drivers increment a complete-count until
`ShouldContinueSuspend` is false, then
`lk_init_level(DEVICE_SUSPEND)`. Reverse on resume. No per-VM
ticket.

**What ATF sees.** `_cpu_suspend(PSCI64_CPU_SUSPEND,
PSCI_IDLE_STATE_S2IDLE)` with `power_state = 0x020180ff`.
Savior does **not** program GPUEB/SPM from this path; BL31
applies the SoC idle profile. If the gate is armed while a
guest still holds Mali (`MapDevice`), GPUEB can power-gate
under the owner — that is the “black screen after idle”
failure. `notify_modem_suspend` (tbox SPM mailbox) is a
separate write, not this PSCI.

### 5. Full s2idle → ATF → wake

```text
 SOS VCPU     UOS VCPU     tbox VCPU     idle pCPU0      ATF
    │            │            │             │             │
    │ PSCI S2IDLE│            │             │             │
    │ park       │            │             │             │
    │            │ PSCI S2IDLE│             │             │
    │            │ park       │ PSCI S2IDLE │             │
    │            │            │ park        │             │
    │            │            │ last VM     │             │
    │            │            │ DDK freeze  │             │
    │            │            │ gate=SUSPEND│             │
    │            │            │ wakeup_all ─┼──►          │
    │ Wait       │ Wait       │ Wait        │  three OK   │
    │            │            │             │  _cpu_suspend
    │            │            │             ├────────────►│
    │            │            │             │             │ s2idle
    │            │            │  wake SPI   │             │
    │            │            │  Signal ────┼─────────────┤
    │            │            │  leave Wait │             │
    │            │            │  gate=RUN   │             │
    │            │            │  DDK thaw   │             │
    │            │            │  SetGuestEntry            │
    │ resume     │ resume     │ resume      │ wfi (gate≠SUSPEND)
```

ALPS `CheckSkipWakeup` drops SPIs not on the wakeup list while
s2idle ([`cpu.md`](cpu.md)). SOS/tbox only log.

### 6. STR (`PSCI SYSTEM_SUSPEND`)

Different from s2idle. Last thread on pCPU 0 calls ATF
`SYSTEM_SUSPEND`. Other pCPUs may `CPU_OFF`.

```text
 guest  PSCI SYSTEM_SUSPEND
    handle_system_suspend
       GUEST_PM_SUSPEND_TO_RAM
       vcpu_thread_active--
       last thread on this pCPU?
          pCPU ≠ 0   suspend_for_cpu_off; SuspendThread
          pCPU == 0  PSCI64_SYSTEM_SUSPEND  ATF
 resume
    platform_cpu_on if the pCPU was off
    ResumeThread
    SetGuestEntry(entry)
    GUEST_PM_RUNNING
```

Watchdog `OnSuspend` is userspace ([`vmm.md`](vmm.md)); it does
not drive `hyper_suspend_state`.

### 7. What `hyp_power.cpp` actually is

Not a manager. It holds:

| Helper | Role |
| --- | --- |
| `guests[MAX_VM_NUM]` | VM table under `vm_spinlock` |
| `is_all_vm_suspend` | every live Guest is IDLE or POWER_OFF |
| `populate_guest` / `depopulate_guest` | `Guest::Create` / destroy |
| `notify_modem_suspend` / `resume` | tbox SPM mailbox |
| `inject_sos_wakeupirq` | VIRTUAL inject into SOS VCPU 0 |
| `handle_irq_event` | cross-VM IRQ into target VCPU 0 |
| `vcpu_info_percpu[]` | per-pCPU VCPU lists, `power_on_cpu` wait |

## Idle loop

See diagram 4. `hyper_wakeup_locked_empty` is
`WakeupSourceDispatcher::CheckAllWakeupSources`. If a
wakeup-source lock is held, that CPU stays in WFI.

## STR vs s2idle (host)

| Guest | Host |
| --- | --- |
| All VCPUs `STATE_SUSPEND` (s2idle) | gate + idle `_cpu_suspend` as above |
| `PSCI SYSTEM_SUSPEND` (STR) | `handle_system_suspend`: last thread on pCPU0 → ATF `SYSTEM_SUSPEND`; other pCPUs may `CPU_OFF` |
| `CPU_OFF` | parks the VCPU thread (`#if 0` pCPU hotplug) |

## Pitfalls

- **Wake then idle ATF.** If `is_all_vm_suspend()` is not in
  the idle check, a guest that just resumed loses GPUEB.
  The check is there for that.
- **Pending LR is not sleep.** Returning `PSCI_SUCCESS` without
  parking would lose the wake. The pending-LR check exists.
- **No manager worker.** Do not expect `GuestPmCoordinator`
  tickets, `PREPARE`, `HOLD`, or a DTS `/features/pm` node.
- **tbox SPM.** `init_mtk_spm_map` runs the first time tbox
  sets its user vmid. Missing that map means modem notify
  writes a null vaddr.
- **Gate vs DDK order.** Devices freeze **before** the gate
  flips to `SUSPEND`. Idle must not enter ATF until that store.
- **First VCPU out thaws.** The first `WaitForSuspend` return
  while the gate is still `SUSPEND` does the DDK resume. Later
  VCPUs see `RUNNING` and skip.

## Terminology

| Term | Meaning here |
| --- | --- |
| Gate | `hyper_suspend_state`: RUNNING or SUSPEND |
| Armed | Gate `SUSPEND`, idle may ATF |
| s2idle | `PSCI CPU_SUSPEND` `PSCI_IDLE_STATE_S2IDLE` |
| STR | `PSCI SYSTEM_SUSPEND` |
| Legacy | This tree: DDK + pdev + gate, no HyperPmManager |
| Ticket | **not present** (no `GuestPmCoordinator`) |

## Code

| Step | File |
| --- | --- |
| Gate + `GUEST_PM_*` | `zircon/kernel/arch/arm64/include/arch/hypervisor.h` |
| VM table / modem / `is_all_vm_suspend` | `zircon/kernel/arch/arm64/hypervisor/hyp_power.cpp` |
| s2idle entry / DDK freeze | `vmexit.cpp` `handle_vcpu_suspend` |
| Idle loop | `zircon/kernel/arch/arm64/arch.cpp` `arch_idle_thread_routine` |
| Wakeup-source check | `WakeupSourceDispatcher::CheckAllWakeupSources` |

---

Prev: [`vmm.md`](vmm.md) · Next: [`devices.md`](devices.md) · [`README.md`](README.md)

Everyday's a Hustle.
