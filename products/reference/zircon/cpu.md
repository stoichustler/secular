# CPU

Secular boots a Zircon kernel on `qemu-system-aarch64` (virt, arm64). The
kernel runs at EL1. Each physical CPU is a Cortex-A model that QEMU presents
as an independent PE: it has its own GPRs, EL1 system registers, caches, and
GIC redistributor. This note is the map of that CPU path: how a PE finds the
current thread, what is saved on exception and on context switch, how caches
are maintained, and how the scheduler picks the next thread.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read `memory.md` first for the MMU / TTBR picture. This note assumes that
map: kernel at TTBR1, user at TTBR0, physmap for kernel access to RAM.

## CPU

### Framework

One PE, two software identities, three register pictures, one local scheduler.

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ QEMU virt PE  (EL1, cpu_num ← MPIDR_EL1)                                 │
│                                                                          │
│   GPRs x0–x30     ELR_EL1 / SPSR_EL1     DAIF     I$ / D$ (CTR_EL0)      │
│                                                                          │
│   TPIDR_EL1 ──► Thread*          x20 (-ffixed-x20) ──► arm64_percpu*     │
└────────────┬─────────────────────────────────────────────┬───────────────┘
             │                                             │
             ▼                                             ▼
┌────────────────────────────┐              ┌──────────────────────────────┐
│ Thread                     │              │ percpu                       │
│  iframe_t   (exception)    │              │  Scheduler  Fair / Deadline  │
│  switch frame (x19,x21–lr) │              │  TimerQueue                  │
│  fpstate / tpidr_el0       │              │  idle_power_thread           │
│  current_percpu_ptr        │              │  DpcRunner / cpu_stats       │
└────────────────────────────┘              └──────────────────────────────┘
```

### Flow

Reschedule on this CPU, then SMP bring-up of a secondary.

```text
 timer PPI / SGI RESCHEDULE / block / unblock
                    │
                    ▼
         ┌──────────────────────┐
         │ Scheduler::Reschedule│  IRQs off, this CPU's queue only
         │ pick Ready, else idle│
         └──────────┬───────────┘
                    ▼
         ┌──────────────────────┐
         │ arch_context_switch  │  FPU, TPIDR_EL0, debug, Spectre
         └──────────┬───────────┘
                    ▼
         ┌──────────────────────┐
         │ arm64_context_switch │  sp swap, msr tpidr_el1
         └──────────┬───────────┘
                    ▼
              aspace changed?
               │           │
              yes          no
               ▼           │
         ContextSwitch     │
         TTBR0 / TCR       │
               │           │
               └─────┬─────┘
                     ▼
               run new thread
               (eret to EL0 if user)

CPU0 ─ PSCI CPU_ON ─► secondary.S ── MMU on ─► idle thread ─► scheduler
```

### Physical CPUs on QEMU virt

`make run` / `make kick` default to eight vCPUs (`-smp 8`). QEMU virt numbers
them with MPIDR affinity:

```text
MPIDR_EL1
  AFF0  cpu in cluster     bits [7:0]
  AFF1  cluster            bits [15:8]
  AFF2                     bits [23:16]
  AFF3                     bits [39:32]
```

Zircon maps that to a dense `cpu_num_t` at boot (`arch_register_mpid` in
`zircon/kernel/arch/arm64/mp.cc`). `cpu_num` 0 is the boot CPU. Secondaries
are brought up with PSCI `CPU_ON` (`zircon/kernel/dev/psci/`,
`zircon/kernel/arch/arm64/phys/psci.cc`).

Two software identities live on every PE while it is in the kernel:

```text
TPIDR_EL1  →  Thread.arch_.thread_pointer_location
x20        →  arm64_percpu  (this PE; callee-saved, -ffixed-x20)
```

`__builtin_thread_pointer()` with `-mtp=el1` reads `TPIDR_EL1`. Subtract
`offsetof(Thread, arch_.thread_pointer_location)` to get the `Thread*`.
`arch_curr_cpu_num()` is a load from `[x20, #cpu_num]`.

`arm64_percpu` is cache-line aligned so adjacent CPUs do not share a line.
It points at the arch-agnostic `percpu` (`zircon/kernel/include/kernel/percpu.h`),
which owns that CPU's `Scheduler`, `TimerQueue`, idle thread, and stats.

```text
PE (QEMU vCPU)
  MPIDR_EL1 ──────────────► cpu_num
  TPIDR_EL1 ──────────────► current Thread
  x20 ────────────────────► arm64_percpu
                              high_level_percpu ──► percpu
                                                      scheduler
                                                      timer_queue
                                                      idle_power_thread
```

### CPU context

There are three register pictures, not one.

#### 1. Exception iframe

Synchronous exceptions, IRQs, FIQs, and SErrors all push an `iframe_t`
(`zircon/kernel/arch/arm64/include/arch/regs.h`) on the kernel stack:

| Offset | Field | Hardware |
| --- | --- | --- |
| `r[0]`…`r[29]` | `x0`–`x29` | GPRs |
| `elr` | `ELR_EL1` | interrupted PC |
| `spsr` | `SPSR_EL1` | interrupted PSTATE (EL, DAIF, NZCV) |
| `lr` | `x30` | link register |
| `usp` | `SP_EL0` (from EL0) or pre-push SP (from EL1) | user / interrupted stack |

`ELR` sits immediately after `r[29]` so a fatal exception can plant `x29` on
that pair and unwind through the faulting PC. Size is 272 bytes, 16-byte
aligned. Syscalls reuse the same layout (`syscall_regs_t`).

`x20` is saved in the iframe like any other GPR, then overwritten with the
percpu pointer from `arch_thread.current_percpu_ptr` so IRQ/exception C++
sees a valid `x20`. Restore puts the interrupted `x20` back.

#### 2. Context-switch frame

A voluntary switch does **not** save the full iframe. Callee-saved integer
state lives in `arm64_context_switch_frame`
(`zircon/kernel/arch/arm64/include/arch/arm64.h`):

```text
x19, (x20 slot unused), x21–x29, lr
```

`x20` is the percpu pointer and is the same on every thread of this CPU, so
it is not swapped. `arch_thread.sp` points at this frame. First-time threads
get a fake frame with `lr = entry_point` (`arch_thread_initialize` in
`zircon/kernel/arch/arm64/thread.cc`).

`arm64_context_switch` (`zircon/kernel/arch/arm64/asm.S`) stores the old
frame, loads the new one, and `msr tpidr_el1` to the new thread pointer.

Also swapped around the assembly switch:

| State | Where | When |
| --- | --- | --- |
| FPU / SIMD (`q0`–`q31`, `FPCR`, `FPSR`; SVE `p0`–`p15`, `FFR` if present) | `arch_thread.fpstate` | every switch unless user state already saved |
| `TPIDR_EL0`, `TPIDRRO_EL0` | `arch_thread` | user TLS |
| HW debug (`MDSCR`, breakpoints) | `arch_thread.debug_state` | if `track_debug_state` |
| `TTBR0_EL1` / `TCR_EL1` | `ArmArchVmAspace::ContextSwitch` | if aspaces differ |
| Spectre V2 IBP flush | `arm64_uarch_do_spectre_v2_mitigation` | user→user aspace change on CPUs that need it |

A `dsb` (`arch::DeviceMemoryBarrier`) runs before the switch so pending TLB
or cache ops complete if the thread migrates to another CPU.

#### 3. User entry

Returning to EL0 is `arm64_uspace_entry` (`zircon/kernel/arch/arm64/uspace_entry.S`):
restore the iframe, `msr spsel` / `sp_el0`, `eret` to `ELR_EL1`. Kernel
`SP_EL1` is the thread's kernel stack top.

### Cache

AArch64 has separate I and D caches. The kernel talks to them with
`DC` / `IC` by MVA, line size from `CTR_EL0` (`arm64_dcache_size`).

| Function | Instruction | Point |
| --- | --- | --- |
| `arch_clean_cache_range` | `dc cvac` | PoC (Point of Coherence, RAM) |
| `arch_clean_invalidate_cache_range` | `dc civac` | PoC, clean+invalidate |
| `arch_invalidate_cache_range` | `dc ivac` | PoC, invalidate |
| `arm64_clean_cache_range_pou` | `dc cvau` | PoU (Point of Unification, I+D) |
| `arch_sync_cache_range` | `dc cvau` + `ic ialluis` | make newly written text executable |

`arch_sync_cache_range` is what `ArmArchVmAspace::Map` uses for executable
pages: D-cache clean to PoU so the I-cache (and other PEs) see the bytes,
then I-cache invalidate. A `dsb ish` + `isb` orders it.

SCTLR_EL1.C / .I enable D-cache and I-cache; they only help once the MMU is
on (`memory.md`). MAIR indices decide whether a page is cached at all:

| AttrIndx | Type |
| --- | --- |
| 0 | Device-nGnRnE |
| 1 | Device-nGnRE (MMIO, GIC, UART) |
| 2 | Normal Write-Back (RAM, physmap, kernel) |
| 3 | Normal non-cacheable |

Do not run cache-by-MVA on device memory. GIC / UART mappings are
`UNCACHED_DEVICE`.

### CPU scheduler

Each CPU has its own `Scheduler` in `percpu`. There is no global run queue.
A thread is **Ready** on exactly one CPU's queue, **Running** on that CPU,
or blocked on a wait queue.

Two disciplines share the queue (`SchedDiscipline` in
`zircon/kernel/include/kernel/scheduler_state.h`):

| Discipline | Parameters | Who |
| --- | --- | --- |
| Fair | `weight = 1.1^(priority - 31)` | default kernel and user threads |
| Deadline | `capacity` / `deadline` (utilization) | latency-critical work |

Fair threads share a 10 ms default period (`SchedDefaultFairPeriod`). Adjacent
priorities differ by about 4–5% bandwidth. Deadline threads are always
inheritable; they skip ahead of fair work when eligible.

```text
timer IRQ / IPI / block / unblock
        │
        ▼
Scheduler::Reschedule  (this CPU, IRQs off, queue lock)
        │  pick earliest eligible Ready thread
        │  idle_power_thread if the queue is empty
        ▼
arch_context_switch(old, new)
        │  FPU, TPIDR_EL0, debug, Spectre
        ▼
arm64_context_switch  (integer frame + TPIDR_EL1)
        │
        ▼
ArmArchVmAspace::ContextSwitch   if aspace changed
        TTBR0 / TCR
```

Wakeup of a thread that should run on another CPU sends an IPI
(`mp_reschedule` → GIC SGI). The remote CPU's reschedule IRQ
(`mp_mbx_reschedule_irq`) sets a preempt pending; `arm64_irq` calls
`Thread::Current::Preempt()` on the way out.

The idle thread is selected only as a last resort (or when power work is
pending). That path is **When the CPU enters idle** and **Idle** below.

Implementation is split: selection and queues in
`zircon/kernel/kernel/scheduler.rs`, C++ FFI in `scheduler.cc` /
`scheduler.h`. Follow the Rust `Reschedule` path, then the C++
`arch_context_switch`.

### CPU STR

STR here is **state transition**: two machines, not one. Threads move through
`thread_state`. The CPU's idle/power thread has a second machine
(`IdlePowerThread::State`) that decides whether this PE should run work,
`wfi`, suspend, or go offline.

#### Thread states

`enum thread_state` in `zircon/kernel/include/kernel/scheduler_state.h`. A
thread is **Ready on exactly one CPU's queue**, **Running on that CPU**, or
off the run queue (blocked / sleeping / …).

```text
                    spawn / Unblock / timeout
                              │
                              ▼
 ┌─────────┐  Reschedule   ┌─────────┐  Reschedule   ┌──────────┐
 │ INITIAL ├──────────────►│  READY  │◄──────────────┤ RUNNING  │
 └─────────┘               └────┬────┘               └────┬─────┘
                                │                         │
                                │                         ├── block ──────────► BLOCKED
                                │                         │                     BLOCKED_READ_LOCK
                                │                         ├── sleep ──────────► SLEEPING
                                │                         ├── debugger ───────► SUSPENDED
                                │                         └── exit ───────────► DEATH
                                │
                                └──── Unblock / sleep end / resume ──► READY
```

| State | On a run queue? | Who runs it |
| --- | --- | --- |
| `INITIAL` | no | not started |
| `READY` | yes, one CPU | waiting for `Reschedule` |
| `RUNNING` | no | this PE, `TPIDR_EL1` |
| `BLOCKED` / `BLOCKED_READ_LOCK` | no | wait queue / lock |
| `SLEEPING` | no | timer |
| `SUSPENDED` | no | exception / debugger |
| `DEATH` | no | exiting |

The idle/power thread is a real `Thread`. It stays **Ready/Running on its
own CPU** and is **not** in the fair or deadline queues. The scheduler
points at it when there is nothing else (or power work is pending).

#### IdlePowerThread states

`IdlePowerThread::State` in `zircon/kernel/include/kernel/idle_power_thread.h`.
`current` vs `target` is a two-field machine: `pending_power_work()` is true
when `current` is not Active/Wakeup **or** `current != target`.

```text
                    current == target
         ┌──────────────────────────────────────┐
         │                                      │
         ▼                                      │
     ┌────────┐  target=Suspend   ┌──────────┐  │
     │ Active │──────────────────►│ Suspend  │──┘  WFI / platform_suspend_cpu
     └───┬────┘                   └────┬─────┘
         │  Wakeup (same as Active     │
         │  for scheduling; signals    │ wake / IPI
         │  a wake occurred)           ▼
         │                        ┌──────────┐
         │                        │  Wakeup  │──► Reschedule, then Active
         │                        └──────────┘
         │
         │ target=Offline
         ▼
     ┌─────────┐
     │ Offline │  mp_unplug_current_cpu → FlushAndHalt
     └─────────┘  (PSCI CPU_OFF on real hardware; QEMU parks the vCPU)
```

| State | Meaning on this PE |
| --- | --- |
| `Active` | normal: run queue, then idle `wfi` if empty |
| `Wakeup` | treat as Active; a wake vector fired during suspend |
| `Suspend` | stay on the idle thread; `wfi` or `platform_suspend_cpu` |
| `Offline` | this CPU is leaving the scheduler; halt |

Secondary bring-up ends in Active on the idle thread. Console boot does not
exercise Suspend/Offline unless something calls the power APIs.

### When the CPU enters idle

"Idle" means **this PE is running `IdlePowerThread::Run`**, not that the
hardware is off. The scheduler picks that thread in
`evaluate_next_thread` (`scheduler.rs`):

```text
Reschedule
    │
    ▼
 pending_power_work()?  ──yes──► idle/power thread     (1)
    │ no
    ▼
 current still Ready, not idle, not migrating?
    │ yes: maybe keep current (deadline/fair compare)
    ▼
 deadline queue has eligible work?  ──yes──► that thread
    │ no
    ▼
 fair queue non-empty?  ──yes──► that thread
    │ no
    ▼
 steal_work from another CPU?  ──yes──► stolen thread
    │ no
    ▼
 idle/power thread                                 (2)
```

So the CPU **enters idle** when:

1. **Power work is pending** — `current`/`target` are not both Active (or
   Wakeup). The idle thread must run even if the fair queue has work, so it
   can Suspend or go Offline.
2. **No local eligible work, and steal failed** — deadline empty, fair
   empty, no stolen thread. This is the common console-image case: `zxsh`
   blocked on UART, other CPUs have nothing to run.

It does **not** enter idle merely because the current thread blocked. Block
calls `Reschedule`; if another Ready thread exists on this CPU, that one
runs. Idle is the **empty-queue** (or power) outcome.

Wakeup from idle is an **IRQ** (see `interrupt.md`): timer PPI, SGI
`RESCHEDULE`, UART SPI. `wfi` returns, `IdlePowerThread::Run` loops,
preempt pending → `Reschedule` → a real thread if one is Ready.

```text
 other thread blocks / last Ready leaves
           │
           ▼
┌─────────────────────┐
│ evaluate_next_thread│
│  steal fails        │
└──────────┬──────────┘
           ▼
┌─────────────────────┐
│ IdlePowerThread::Run│  IRQs off, preempt off
└──────────┬──────────┘
           ▼
┌─────────────────────┐
│ EnterIdleState      │  DSB + WFI
└──────────┬──────────┘
           │
     IRQ (timer / IPI / UART)
           │
           ▼
      exception → maybe Preempt
           │
           ▼
      Reschedule → work or WFI again
```

### Idle

Idle is a **per-CPU kernel thread**, not a special CPU mode bit. Hardware
low power is a `WFI` (or a platform suspend) **inside** that thread.

#### What runs

`percpu.idle_power_thread` (`zircon/kernel/include/kernel/percpu.h`).
Entry is `IdlePowerThread::Run` (`idle_power_thread.cc`). CPU0's boot
thread becomes this after `kernel_init`; secondaries become it in
`secondary.cc` (`thread->task_state().entry() == IdlePowerThread::Run`).

The run loop (IRQs and preemption disabled each iteration):

1. If `target != current`, perform the STR (Active / Wakeup / Suspend /
   Offline). Offline calls `mp_unplug_current_cpu` and does not return.
   After Active/Wakeup, `Thread::Current::Reschedule()` so the real queue
   is evaluated.
2. If a preempt is already pending, `continue` (do not `wfi`, or a wakeup
   IRQ already in the pipeline would be delayed).
3. If `current == Suspend`: boot CPU may `platform_suspend_system`; else
   `platform_suspend_cpu` if the platform supports it; else `WFI`.
4. Else **idle**: timestamp, `ArchIdlePowerThread::EnterIdleState()`, add
   the delta to `Scheduler` idle time.

#### WFI on ARM64

`ArchIdlePowerThread::EnterIdleState` (`zircon/kernel/arch/arm64/arch.cc`):

```text
DSB SY     make prior stores (including "I am idle") visible to other PEs
WFI        wait for interrupt; PE may clock-gate
```

ARM DDI 0487 K14.2.3: `WFI`/`WFE` need an explicit `DSB` if earlier memory
accesses must be visible. QEMU still models `WFI` as "block this vCPU until
an IRQ"; on a real SoC it is the cheap idle. Interrupts stay **masked**
across this `WFI` on arm64; the taken IRQ still wakes the PE, then the
vector runs after `wfi` returns (DAIF.I is still set until the handler
path). The loop then re-enables via the guards falling out of scope.

Callers must: preemption off, IRQs off. The arch routine may briefly
unmask on other architectures just before the wait; arm64 does not.

#### What idle is not

| Idle is | Idle is not |
| --- | --- |
| the idle/power **thread** scheduled | the PE erased from `cpu_num` |
| `WFI` until the next IRQ | spinning on a `for(;;)` (that is panic halt) |
| counted in scheduler idle time | a Fair/Deadline queue entry |
| still taking timer ticks (lockup detector) | "CPU offline" (`State::Offline`) |

Panic halt (`platform.cc`) is a different loop: IRQs disabled forever and
`wfi` with no scheduler. That is not IdlePowerThread.

#### Steal before idle

Empty local queues do not idle immediately. `evaluate_next_thread` returns
`steal_work`; `Reschedule` drops the queue lock and tries to pull a Ready
thread from another CPU. Only if that returns null does it select
`cpp_scheduler_get_idle_thread`. Eight QEMU vCPUs therefore spend most of
the console's life in `WFI` on seven PEs, while CPU0 runs `zxsh` / UART.

#### Knowledge map

| Topic | Where |
| --- | --- |
| Thread STR | `scheduler_state.h` `thread_state` |
| Power STR | `idle_power_thread.h` `State` / `pending_power_work` |
| Who is selected | `scheduler.rs` `evaluate_next_thread` |
| Idle run loop | `idle_power_thread.cc` `IdlePowerThread::Run` |
| `DSB` + `WFI` | `arch.cc` `ArchIdlePowerThread::EnterIdleState` |
| Rules for idle entry | `include/arch/ops.h` `ArchIdlePowerThread` |
| Secondary becomes idle | `secondary.cc`, `thread.cc` |
| Offline | `mp.cc` `mp_unplug_current_cpu` |
| IRQ that ends `WFI` | `interrupt.md` |

### SMP bring-up

```text
CPU0  (already at EL1, MMU on)
  PSCI CPU_ON(mpidr, arm64_secondary_start, stack)
        │
CPU n  EL1, MMU off, identity later
  secondary.S
    load MAIR / TCR / TTBR0 / TTBR1
    SCTLR.M on, jump to kernel VA
    TCR EPD0 (drop identity TTBR0)
    TPIDR_EL1 = fake arch_thread on the boot stack
    x20 = this CPU's arm64_percpu
  secondary.cc
    arm64_cpu_early_init  (VBAR, SCTLR bits, percpu)
    interrupt_init_percpu (unmask SGIs)
    become the idle thread, enter the scheduler
```

Until `mp_set_curr_cpu_online(true)`, that CPU is not a reschedule target.

### Basic registers (CPU)

| Register | Role |
| --- | --- |
| `x0`–`x7` | args / return; syscall args stay in `x0`–`x7` through the iframe save |
| `x18` | platform register; not a free temp |
| `x19`–`x28` | callee-saved; `x20` is **fixed** as `arm64_percpu*` |
| `x29` | frame pointer |
| `x30` / `lr` | link register |
| `sp` | `SPSel=1` → `SP_EL1` (kernel); user stack is `SP_EL0` |
| `pc` | not a GPR; `ELR_EL1` on exception, `ret` / `eret` to resume |
| `TPIDR_EL1` | current `Thread` (kernel TLS) |
| `TPIDR_EL0` / `TPIDRRO_EL0` | user TLS |
| `MPIDR_EL1` | this PE's affinity; `cpu_num` lookup |
| `VBAR_EL1` | exception vectors (`arm64_el1_exception`) |
| `DAIF` | Debug / SError / IRQ / FIQ mask; `msr daifset, #6` masks IRQ+SError |
| `CurrentEL` | 1 for the kernel; physboot may start at EL2 and drop |
| `CTR_EL0` | cache line size |
| `CPACR_EL1` | FPU / SVE trap; first user FP use may trap then enable |

Exception entry always runs with IRQs masked. `arch_enable_ints` is
`daifclr #6`. `arch_ints_disabled` tests DAIF.I (bit 7).

### Where in the tree

Read in this order.

| File | Why |
| --- | --- |
| `zircon/kernel/arch/arm64/include/arch/regs.h` | `iframe_t` |
| `zircon/kernel/arch/arm64/include/arch/arch_thread.h` | `arch_thread`, FPU, TLS offsets |
| `zircon/kernel/arch/arm64/include/arch/arm64.h` | context-switch frame |
| `zircon/kernel/arch/arm64/include/arch/arm64/mp.h` | `arm64_percpu`, `x20` |
| `zircon/kernel/arch/arm64/include/arch/current_thread.h` | `TPIDR_EL1` → `Thread*` |
| `zircon/kernel/include/kernel/percpu.h` | `Scheduler` / timers / idle |
| `zircon/kernel/arch/arm64/thread.cc` | `arch_context_switch` |
| `zircon/kernel/arch/arm64/asm.S` | `arm64_context_switch` |
| `zircon/kernel/arch/arm64/src/fpu.rs` | FPU save/restore |
| `zircon/kernel/arch/arm64/cache-ops.S` | `dc` / `ic` |
| `zircon/kernel/include/kernel/scheduler_state.h` | Fair / Deadline; `thread_state` STR |
| `zircon/kernel/kernel/scheduler.rs` | pick next thread; idle as last resort |
| `zircon/kernel/kernel/scheduler.cc` | C++ FFI into the scheduler |
| `zircon/kernel/include/kernel/idle_power_thread.h` | Active / Wakeup / Suspend / Offline |
| `zircon/kernel/kernel/idle_power_thread.cc` | `Run` loop; when to `WFI` |
| `zircon/kernel/include/arch/ops.h` | `EnterIdleState` IRQ/preempt rules |
| `zircon/kernel/arch/arm64/arch.cc` | `DSB` + `WFI` |
| `zircon/kernel/arch/arm64/secondary.S` | AP MMU on + jump to high VA |
| `zircon/kernel/arch/arm64/secondary.cc` | AP idle thread |
| `zircon/kernel/arch/arm64/mp.cc` | MPIDR ↔ `cpu_num` |
| `zircon/kernel/dev/psci/` | `CPU_ON` / `CPU_OFF` |
| `zircon/kernel/arch/arm64/uspace_entry.S` | `eret` to EL0 |

Suggested first pass: **iframe + `arch_thread` → `arch_context_switch` →
`scheduler.rs` Reschedule → `evaluate_next_thread` (idle) →
`IdlePowerThread::Run` / `WFI` → `secondary.S`**. That is enough to see a
timer IRQ preempt `zxsh`, a second QEMU vCPU come online as idle, and a
blocked shell leave its CPU in `WFI`.

IRQ delivery itself is `interrupt.md`. Locks and barriers are
`synchronization.md`.

---

Everyday's a Hustle.
