# Power

Guest idle is a VCPU state ([`cpu.md`](cpu.md)). **Host suspend**
is a second machine: devices, irq-quiet, then idle CPUs may
`PSCI CPU_SUSPEND` into ATF. This tree’s `hyp_power.cpp` is that
machine. mt8678 is the short legacy path only.

Read [`vmm.md`](vmm.md) for s2idle / STR from the guest’s point
of view, [`cpu.md`](cpu.md) for `WaitForSuspend` / `CPU_OFF`,
[`interrupt.md`](interrupt.md) for wakeup SPIs.

This note is the host gate, DTS profile, `HyperPmManager`
phases, `GuestPmCoordinator` tickets, and the idle-loop contract.

## Principle

1. **Guests park first.** `hyper_pm_suspend()` is a no-op unless
   `is_all_vm_suspend()`. A wake IRQ that unparks any VCPU must
   abort arming, or idle cores will `HARD_OFF` GPUEB while a
   guest still runs.

2. **Two implementations, one C API.** DTS `/features/pm` sets
   `ZX_PM_PROFILE_MANAGER`. On: `HyperPmManager`. Off: legacy
   DDK + `pdev` + idle gate (`hyper_pm_legacy_suspend`). Callers
   (`handle_vcpu_suspend`, idle thread, GIC) only see
   `hyper_pm_suspend` / `resume` / `idle_may_enter_atf`.

3. **The idle gate is not the manager state.**
   `hyper_suspend_state` is what `arch_idle_thread_routine` and
   `gic_handle_irq` look at. Manager `State`/`Phase` are private.
   Gate values: `RUNNING`, `PREPARE` (devices going down, no
   ATF), `SUSPEND` (idle may enter ATF), `SUSPEND_HOLD` (a
   wakeup-source lock is held; stay out of ATF).

4. **Lock order is `vm_spinlock` then `hyper_pm_lock`.**
   Admission to the pCPU mask (`hyper_pm_begin_cpu_suspend`)
   takes both. Never the reverse. Leaving `STATE_SUSPEND` uses
   the same order and IPIs CPUs already in ATF.

## Framework

```text
 last VCPU of last VM  PSCI CPU_SUSPEND S2IDLE
    vcpu_enter_suspend → GUEST_PM_SUSPEND_TO_IDLE
    hyper_pm_suspend()
         │
         ├─ no /features/pm     hyper_pm_legacy_suspend
         │     DDK freeze → pdev → gate=SUSPEND → idle ATF
         │
         └─ /features/pm        HyperPmManager::Suspend
               WAIT_WAKELOCK
               DDK  (devmgr DeviceSuspend)
               KERNEL_PDEV  (cpufreq, uart, …)
               NOIRQ  (optional: mask non-wake SPIs)
               ARMED  gate=SUSPEND
               idle CPUs  hyper_pm_idle_may_enter_atf
                          _cpu_suspend → ATF
 wake SPI / HYPERVISER_GUEST_OS_RESUME
    hyper_pm_notify_wakeup_irq   (IRQ context)
    worker  CompleteResume
    gate=RUNNING
    guests  WaitForSuspend returns
```

Swimlane: [`flows.md`](flows.md) F11.

## Diagrams

### 1. Gate vs manager

```text
 hyper_suspend_state          HyperPmManager::state_ / phase_
 (idle + GIC)                 (worker + Suspend/Resume)

 RUNNING                      RUNNING / Phase::IDLE
 PREPARE                      SUSPEND + DDK/PDEV/NOIRQ in flight
 SUSPEND                      SUSPEND + Phase::SUSPEND_ARMED
 SUSPEND_HOLD                 same, wakeup-source Lock() held
```

Idle may call ATF **only** on `SUSPEND` **and**
`is_all_vm_suspend()`. `HOLD` and `PREPARE` keep CPUs in WFI.

### 2. Manager phases

```text
 IDLE
   │  last VM parked
   ▼
 SUSPEND_WAIT_WAKELOCK     wait until no wakeup-source locks
   ▼
 SUSPEND_DDK               SuspendDispatcher → user DDK
   ▼
 SUSPEND_KERNEL_PDEV       pdev registry (cpufreq, uart)
   ▼
 SUSPEND_NOIRQ             optional irq-quiet, freeze wake table
   ▼
 SUSPEND_ARMED             gate SUSPEND; last idle CPU → ATF
   │  wake
   ▼
 RESUME_NOIRQ → RESUME_DDK → RUNNING
```

Any wake during prepare → `RollbackSuspend` (resume what was
frozen) then either retry or `retry_cancelled_` if a VM already
ran.

### 3. DTS profile (`pm_profile_init`)

```text
 /features/power-watcher     ZX_PM_PROFILE_PW_WATCHER
 /features/pm                ZX_PM_PROFILE_MANAGER | GRAPH
   /features/pm/exec/noirq   ZX_PM_PROFILE_NOIRQ
   /features/pm/exec/policy  level << POLICY_SHIFT
```

QEMU without `/features/pm` is legacy. Product SoC that wants
the 3900-line manager must ship that node. mt8678 HEE has no
`HyperPmManager` class of this size.

## Guest tickets

`GuestPmCoordinator` (this tree, manager on):

```text
 BeginGuestSuspend(vmid)     ticket {generation, counted}
 IsCancelled                 wake during freeze
 CompleteGuestSuspend        slept=true  →  IDLE if in_flight==0
 CancelGuestSuspend          REQUESTED → CANCELLED, inject wake
```

A pending LR must not become a fake `PSCI_SUCCESS`
([`cpu.md`](cpu.md)). `Complete(..., slept=false)` keeps the
round REQUESTED so other VCPUs still freeze.

## Idle loop

```text
 arch_idle_thread_routine
    if hyper_pm_idle_may_enter_atf():
       if hyper_pm_begin_cpu_suspend():
          _cpu_suspend()          ATF
          hyper_pm_end_cpu_suspend()
```

`begin()` fails if the gate moved or a VM unparked between the
check and the lock. That CPU stays in WFI.

GIC: `hyper_pm_notify_wakeup_irq` on a wake SPI while gate ≠
`RUNNING`. IRQ-safe: sticky flag + signal the PM worker.
`PREPARE` aborts arming; `SUSPEND`/`HOLD` runs `CompleteResume`
on the worker.

## STR vs s2idle (host)

| Guest | Host |
| --- | --- |
| All VCPUs `STATE_SUSPEND` (s2idle) | `hyper_pm_suspend` as above |
| `PSCI SYSTEM_SUSPEND` (STR) | `handle_system_suspend`: last thread on pCPU0 → ATF `SYSTEM_SUSPEND`; other pCPUs may `CPU_OFF` |
| `CPU_OFF` | pCPU hotplug, not this gate |

Watchdog `OnSuspend` is userspace ([`vmm.md`](vmm.md)); it does
not drive `hyper_suspend_state`.

## Pitfalls

- **Wake then idle ATF.** If `is_all_vm_suspend()` is not in
  `idle_may_enter_atf`, a guest that just resumed loses GPUEB.
  The check is there for that.
- **`PREPARE` vs `SUSPEND`.** Devices are already freezing in
  `PREPARE`; idle must **not** enter ATF yet.
- **Lock order.** `hyper_pm_lock` while holding `vm_spinlock` is
  OK; the reverse deadlocks with `SetVcpuState`.
- **Legacy vs manager.** `hyper_pm_notify_guest_state_change` is
  a no-op when manager is off. Do not expect the worker.
- **DDK hard timeout.** Manager can `ForceRebootForPmFailure`
  if `WaitForDeviceSuspend` never returns. Legacy just aborts.

## Terminology

| Term | Meaning here |
| --- | --- |
| feat_pm / manager | DTS `/features/pm` → `HyperPmManager` |
| Gate | `hyper_suspend_state`, idle + GIC |
| Armed | Gate `SUSPEND`, idle may ATF |
| HOLD | Wake lock held after arming |
| Ticket | Per-VM in-flight s2idle generation |
| noirq | Mask host non-wake SPIs around ATF |
| Legacy | DDK+pdev+gate, no worker |

## Code

| Step | File |
| --- | --- |
| Gate / C API | `zircon/kernel/arch/arm64/include/arch/hyper_pm.h` |
| Manager + coordinator | `zircon/kernel/arch/arm64/hypervisor/hyp_power.cpp` |
| `hyper_pm_suspend` entry | same, bottom of file |
| Guest s2idle | `vmexit.cpp` `handle_vcpu_suspend` |
| Idle loop | `zircon/kernel/arch/arm64/asm.S` / thread idle |
| GIC wake hook | `arm_gicv3.c` `hyper_pm_notify_wakeup_irq` |
| DTS profile | `hyp_power.cpp` `pm_profile_init` |

---

Everyday's a Hustle.
