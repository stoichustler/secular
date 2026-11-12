---
name: zircon-cpu
description: >
  Use when changing threads, context switch, syscalls, idle/WFI, SMP bring-up, or VCPU enter on the Secular console image.
---

# Zircon CPU: Thread, Idle, and VCPU

Stop at idle `WFI` and at `zx_vcpu_enter`.

## When to use

* Editing `Thread`, `arch_context_switch`, `do_syscall`, idle, or `zx_vcpu_enter`.
* A hang that is scheduler/idle rather than a missing IRQ.

## When not to use

* IRQ acknowledge / EOI — `interrupt.md`.
* Which lock to take — `sync.md`.

> [!NOTE]
> Console image: Zircon kernel + linux-arm64-boot-shim + BOOTFS + serial
> console + `zxsh`. The kernel is an EL2 VHE host.

## 1. Framework

One PE, two software identities, three register pictures, one local scheduler.

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ QEMU virt PE  (EL2 VHE host, cpu_num ← MPIDR_EL1)                        │
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
│  user_thread_              │              │                              │
│  THREAD_FLAG_VCPU (opt.)   │              │                              │
└────────────┬───────────────┘              └──────────────────────────────┘
             │
             ▼
   ThreadDispatcher → ProcessDispatcher → JobDispatcher
   (userspace objects; see Job, process, thread)
```

## 2. Flow

Two switches, not one. A syscall or IRQ from EL0 is the same `Thread*`
on a different exception level. A scheduler switch is a different
`Thread*` (and maybe a different aspace). SMP bring-up of a secondary
is the third path.

```text
 EL0  svc / IRQ / fault                 timer PPI / SGI / block / unblock
        │                                         │
        ▼                                         ▼
 iframe on this thread's kstack          Scheduler::Reschedule
        │                                 pick Ready, else idle
        ▼                                         │
 kernel C++ on SP_EL1                             ▼
 (syscall wrapper, IRQ, fault)           arch_context_switch
        │                                 FPU, TPIDR_EL0, debug
        │  may block ──► Reschedule ──►  arm64_context_switch
        │                                 TTBR0 if aspace changed
        ▼                                         │
 eret to EL0  (same thread)              run new thread
                                         (eret to EL0 if it was in user)

CPU0 ─ PSCI CPU_ON ─► secondary.S ── MMU on ─► idle thread ─► scheduler

 EL0  zx_vcpu_enter                    guest WFI / host IRQ / Kick
        │                                         │
        ▼                                         ▼
 Vcpu::Enter  (same Thread*, FLAG_VCPU)   Wait on AutounsignalEvent
        arm64_el2_enter  EL2 world switch         or ZX_ERR_NEXT (stay in loop)
        guest EL1  ← not a scheduler switch       IPI to last_cpu_ if RUNNING
```

## 3. Physical CPUs on QEMU virt

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
[`zircon/kernel/arch/arm64/mp.cc`](../../../zircon/kernel/arch/arm64/mp.cc)). `cpu_num` 0 is the boot CPU. Secondaries
are brought up with PSCI `CPU_ON` ([`zircon/kernel/dev/psci/`](../../../zircon/kernel/dev/psci/),
[`zircon/kernel/arch/arm64/phys/psci.cc`](../../../zircon/kernel/arch/arm64/phys/psci.cc)).

Two software identities live on every PE while it is in the kernel:

```text
TPIDR_EL1  →  Thread.arch_.thread_pointer_location
x20        →  arm64_percpu  (this PE; callee-saved, -ffixed-x20)
```

`__builtin_thread_pointer()` with `-mtp=el1` reads `TPIDR_EL1`. Subtract
`offsetof(Thread, arch_.thread_pointer_location)` to get the `Thread*`.
`arch_curr_cpu_num()` is a load from `[x20, #cpu_num]`.

`arm64_percpu` is cache-line aligned so adjacent CPUs do not share a line.
It points at the arch-agnostic `percpu` ([`zircon/kernel/include/kernel/percpu.h`](../../../zircon/kernel/include/kernel/percpu.h)),
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

## 4. CPU context

There are three register pictures, not one.

#### 1. Exception iframe

Synchronous exceptions, IRQs, FIQs, and SErrors all push an `iframe_t`
([`zircon/kernel/arch/arm64/include/arch/regs.h`](../../../zircon/kernel/arch/arm64/include/arch/regs.h)) on the kernel stack:

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
([`zircon/kernel/arch/arm64/include/arch/arm64.h`](../../../zircon/kernel/arch/arm64/include/arch/arm64.h)):

```text
x19, (x20 slot unused), x21–x29, lr
```

`x20` is the percpu pointer and is the same on every thread of this CPU, so
it is not swapped. `arch_thread.sp` points at this frame. First-time threads
get a fake frame with `lr = entry_point` (`arch_thread_initialize` in
[`zircon/kernel/arch/arm64/thread.cc`](../../../zircon/kernel/arch/arm64/thread.cc)).

`arm64_context_switch` ([`zircon/kernel/arch/arm64/asm.S`](../../../zircon/kernel/arch/arm64/asm.S)) stores the old
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

The first trip to EL0 is `arm64_uspace_entry`
([`zircon/kernel/arch/arm64/uspace_entry.S`](../../../zircon/kernel/arch/arm64/uspace_entry.S)): restore the iframe,
`msr sp_el0`, `eret` to `ELR_EL1`. Kernel `SP_EL1` is the thread's
kernel stack top. Later returns from syscall / IRQ / fault use
`arm64_exc_shared_restore_lower_el`, not this path. The full EL0 ↔ EL1
story, and how it differs from a scheduler switch, is **User ↔ kernel**
below.

## 5. User ↔ kernel

A PE is always running some `Thread`. That thread is either in the
kernel (EL1, `SPSel=1`, `SP_EL1`) or in userspace (EL0, user PC, `SP_EL0`).
Crossing that boundary is **not** a scheduler context switch. The
`Thread*` in `TPIDR_EL1` does not change. `TTBR0` does not change. The
same kernel stack that took the exception is the stack that will
`eret`.

The scheduler switch in **CPU context** / **CPU scheduler** is the
other machine: a different `Thread*`, a different kernel stack, maybe a
different `TTBR0`. It happens only while both threads are in the
kernel. A user thread that is "running on the CPU" is actually sitting
in EL1 with an iframe on its kernel stack, or it has already `eret`
back to EL0.

```text
                  EL0  (user)
                    │
     svc / IRQ / fault / first entry
                    │
                    ▼
 ┌──────────────────────────────────────────────────────────────┐
 │  this Thread, kernel stack (SP_EL1)                          │
 │                                                              │
 │   iframe_t     GPRs, ELR, SPSR, SP_EL0                       │
 │   C++ / Rust   syscall wrapper, IRQ, page fault, …           │
 │                                                              │
 │   may call Scheduler::Reschedule  ──►  other Thread*         │
 │   (iframe stays on this stack until we run again)            │
 └──────────────────────────────────────────────────────────────┘
                    │
              eret / arm64_uspace_entry
                    │
                    ▼
                  EL0  (user)
```

Two stacks per user thread, always:

| Stack | Register | Who allocates it | What lives on it |
| --- | --- | --- | --- |
| Kernel | `SP_EL1` | `KernelStack` on `Thread` (2 pages by default) | iframe, kernel frames, syscall C++ |
| User | `SP_EL0` | the process (`zx_vmar_map`) | user C / Rust / zxsh frames |

Kernel-only threads (idle, DPC, bootstrap) have a kernel stack and no
user stack. They never `eret`.

#### EL0 → EL1 (exception)

Hardware takes the vector in `VBAR_EL1` (`arm64_el1_exception` in
[`zircon/kernel/arch/arm64/exceptions.S`](../../../zircon/kernel/arch/arm64/exceptions.S)). From lower EL the slots are
`sync_lower_a64`, `irq_lower_a64`, `serror_lower_a64`. The vector:

1. Pushes an `iframe_t` on `SP_EL1` (`iframe.exc.save.el0`): `x0`–`x29`,
   `ELR_EL1` (user PC, also left in `x10`), `SPSR_EL1`, `x30`, `SP_EL0`.
2. Restores kernel invariants: `x20` = this PE's `arm64_percpu*`,
   shadow-call SP from `arch_thread`.
3. Branches on why we entered:

| Cause | ESR / vector | Next |
| --- | --- | --- |
| Syscall | `ESR_EC_SVC64` | `arm64_syscall_dispatcher` |
| IRQ | IRQ vector | `arm64_irq` |
| Fault / BRK / … | other sync | `arm64_sync_exception` |
| SError | SError vector | `arm64_serror_exception` |

`x0`–`x7` are left live through the save so the syscall path can use
them as arguments without reloading the iframe. `x16` still holds the
syscall number.

Returning from EL1 to a **kernel** interrupt is `arm64_exc_shared_restore`
(do not restore `x20`; this PE's percpu pointer may have changed if we
were migrated). Returning to **user** is `arm64_exc_shared_restore_lower_el`:
write `SP_EL0` from the iframe USP, restore user `MDSCR`, restore GPRs
including `x20`, `eret`.

#### EL1 → EL0 (first entry)

A new user thread does not return from an exception. It is a kernel
`Thread` whose entry is `ThreadDispatcher::StartRoutine`:

```text
Thread::Create(StartRoutine)
        │
        ▼
arch_thread_initialize   fake switch frame, lr = entry
        │
        ▼
scheduler Resume  →  StartRoutine
        │
        ▼
arch_prepare_uspace      iframe: ELR=pc, USP=sp, spsr=EL0t, x0/x1=args
        │
        ▼
ZX_EXCP_THREAD_STARTING  (debugger, if attached)
        │
        ▼
arch_enter_uspace → arm64_uspace_entry
        SP_EL1 = kernel stack top
        SP_EL0 / ELR / SPSR from iframe
        eret                     ──►  EL0 at user pc
```

`spsr = 0` means EL0t, DAIF clear, NZCV clear. `TPIDR_EL0` is written
from `UserEntryState.tp` and is not in the iframe.

After that first `eret`, every later kernel entry is an exception and
every later return is `arm64_exc_shared_restore_lower_el`, not
`arm64_uspace_entry`.

#### How this meets a scheduler switch

A user thread that blocks in a syscall, or is preempted by a timer IRQ,
is still that same `Thread`. The iframe is on **its** kernel stack. The
scheduler then switches to another `Thread*` (`arm64_context_switch`
saves callee-saved regs and `SP_EL1`). When this thread is picked
again, it resumes in the kernel at the instruction after the switch,
finishes the syscall or IRQ, and only then `eret`s to EL0.

```text
 zxsh in EL0
    │  svc  (e.g. zx_object_wait_one on UART)
    ▼
 iframe on zxsh's kstack
    │  wrapper blocks → WaitQueue
    ▼
 Scheduler::Reschedule
    │  this CPU → idle (or another Ready thread)
    ▼
 IdlePowerThread  WFI
    │  UART IRQ
    ▼
 Reschedule picks zxsh's Thread
    │  SP_EL1 is again zxsh's kstack, iframe still there
    ▼
 syscall returns, eret to EL0
```

Switching from one user process to another also switches `TTBR0` (see
**CPU context**, `ArmArchVmAspace::ContextSwitch`). Switching between
two threads of the same process does not.

## 6. System calls

Userspace does not `svc` from arbitrary code. Every Zircon syscall is a
vDSO stub. The vDSO is mapped into every process (`vdso_code_address`
on `ProcessDispatcher`). The kernel checks that `ELR_EL1` at the `svc`
is an approved site in that mapping; otherwise it is
`ZX_EXCP_POLICY_CODE_BAD_SYSCALL`.

#### User stub

[`zircon/kernel/lib/userabi/vdso/zircon-syscall-arm64.S`](../../../zircon/kernel/lib/userabi/vdso/zircon-syscall-arm64.S):

```text
mov x16, #ZX_SYS_<name>
svc #0
CODE_SYSRET_<name>_VIA_<caller>     ; approved return site
dsb nsh / isb / brk 0               ; 12-byte speculation barrier
                                    ; kernel advances ELR past this
```

Arguments stay in `x0`–`x7` (AAPCS64). The eighth argument, if any, is
still in `x7`; the user PC is passed separately because `x0`–`x7` are
full. Status comes back in `x0` (`iframe.r[0]`).

#### Kernel dispatch

```text
EL0  svc #0
        │
        ▼
arm64_el1_exception_sync_lower_a64
        iframe.exc.save.el0
        ESR_EC_SVC64 ?
        │ yes
        ▼
arm64_syscall_dispatcher
        speculation_clear_gprs     (x8, x11–x15, x17, x19, x21–x30)
        x16 < ZX_SYS_COUNT ?
        │ yes                    │ no
        ▼                        ▼
  16-byte trampoline        unknown_syscall
  syscall_dispatcher N, name
        mov xN, x10   (user PC)  ; or push PC if nargs==8
        bl wrapper_<name>
        │
        ▼
  do_syscall  (syscalls.cc)
        do_syscall_pre     IRQs on, ktrace, current process
        valid_pc(ELR − vdso_code) ?
        │ yes                    │ no
        ▼                        ▼
  sys_<name>(...)           sys_invalid_syscall
        │
        ▼
  do_syscall_post    IRQs off, {status, IsSignaled()}
        │
        ▼
.Lpost_syscall
        iframe.r[0] = status
        ELR += 12               skip vDSO speculation barrier
        if signaled: arch_iframe_process_pending_signals
        arm64_exc_shared_restore_lower_el
        eret
```

`wrapper_<name>` is stamped from generated `kernel-wrappers.inc` (included by
[`syscalls.cc`](../../../zircon/kernel/lib/syscalls/syscalls.cc)). It marshals
iframe / register arguments into a typed `sys_<name>`. The
implementations live under [`zircon/kernel/lib/syscalls/`](../../../zircon/kernel/lib/syscalls/)
([`process.rs`](../../../zircon/kernel/lib/syscalls/process.rs), [`thread.rs`](../../../zircon/kernel/lib/syscalls/thread.rs), [`job.rs`](../../../zircon/kernel/lib/syscalls/job.rs), [`task.rs`](../../../zircon/kernel/lib/syscalls/task.rs), …). `KERNEL_SYSCALL`
and `BLOCKING_SYSCALL` share this path; a blocking syscall sets
`ThreadDispatcher::Blocked` (`FUTEX`, `CHANNEL`, `WAIT_ONE`, …) and
may `Reschedule`.

IRQs are **off** at vector entry, **on** for the body of `sys_*`
(`do_syscall_pre`), **off** again before checking thread signals
(`do_syscall_post`). That closes the lost-wakeup window: once signals
are sampled, they are either consumed or the thread is back in EL0.

#### Syscalls that create the objects below

| Syscall | What it builds |
| --- | --- |
| `zx_job_create` | child `JobDispatcher` of a parent job |
| `zx_process_create` | `ProcessDispatcher` + root VMAR, registered with the job |
| `zx_thread_create` | `ThreadDispatcher` + kernel `Thread` (not yet running) |
| `zx_process_start` | first thread: `MakeRunnable`, process `INITIAL` → `RUNNING` |
| `zx_thread_start` | later threads of an already-running process (`sys_thread_start_regs`) |
| `zx_thread_exit` | current thread `DYING` → `DEAD` |
| `zx_process_exit` | current process `DYING`, then `ExitCurrent` |
| `zx_task_kill` | kill a **job** or **process** (`ZX_ERR_NOT_SUPPORTED` on a thread) |

`zx_process_start` is deliberately the only way to transfer a handle
into a new process as `arg1`, and it may run only once (the first
thread). Extra threads take integer arguments, not handles.

## 7. Job, process, thread

The CPU runs a `Thread`. Userspace names three kernel objects that
wrap that:

```text
JobDispatcher          tree of jobs and processes
  │                    policy, exception propagation, enumeration
  │
  ├── JobDispatcher    (child job)
  │
  └── ProcessDispatcher
        handle table, VmAspace (TTBR0), futex state
        │
        └── ThreadDispatcher     userspace-visible thread
              └── Thread         what the scheduler actually runs
                    iframe / switch frame / kernel stack
                    TPIDR_EL1 points here
```

A **job** is not scheduled. A **process** is not scheduled. Only a
`Thread` is. `ProcessDispatcher::GetCurrent()` is
`Thread::Current` → `user_thread()` → `process()`. Kernel-only threads
have `user_thread() == nullptr` and no process.

The job tree exists because refcounts run the other way: the scheduler
holds the `Thread`, the `Thread` holds the `ThreadDispatcher`, the
dispatcher holds the process. Without jobs there would be no way to
find or kill a process that had dropped every handle.

#### Job

`JobDispatcher` ([`zircon/kernel/object/include/object/job_dispatcher.h`](../../../zircon/kernel/object/include/object/job_dispatcher.h)).
Each job has one parent (null for the root) and any number of child
jobs and child processes. `max_height` is 32 at the root and drops by
one per level; a job with `max_height == 0` cannot have children.

What a job is for:

1. **Enumerate and kill** the living tasks under it, even if userspace
   holds no handles to them.
2. **Policy** (`JobPolicy`) that capabilities cannot express: deny
   `ZX_POL_NEW_PROCESS`, `ZX_POL_NEW_CHANNEL`, WX mappings, …
   Policy is set only while the job is empty.
3. **Exception propagation**: unhandled thread exceptions walk
   thread → process → job → parent jobs.

The kernel creates the **root job** (`CreateRootJob`, name `"root"`).
`userboot_init` creates process `"userboot"` as a child of that root
and dups the root-job handle into userboot's bootstrap channel. If the
root job becomes childless, `RootJobObserver` calls `platform_halt`.

Signals: `ZX_JOB_NO_PROCESSES`, `ZX_JOB_NO_JOBS`, `ZX_JOB_NO_CHILDREN`
while empty; `ZX_TASK_TERMINATED` once `DEAD`.

#### Process

`ProcessDispatcher`. Owns, via `ShareableProcessState`:

| Piece | Role |
| --- | --- |
| `HandleTable` | process-local handle values → kernel objects + rights |
| `VmAspace` | user TTBR0 (`USER_ASPACE`, see [`memory.md`](memory.md)) |
| `FutexContext` | `zx_futex_wait` / `wake` state |

Plus a list of `ThreadDispatcher`, a parent `JobDispatcher`, and
exceptionates (normal + debug). `zx_process_create` also returns a
handle to the root VMAR so userspace can map the binary, stack, and
vDSO.

`related_koid` of a process is the job. `related_koid` of a thread is
the process.

Shared / restricted aspaces (`ZX_PROCESS_SHARED`,
`zx_process_create_shared`) exist for Starnix-style split address
spaces. The console image does not use them: userboot, component
manager, and `zxsh` are ordinary processes with one aspace.

#### Thread: two objects

| Object | Class | Visible to userspace? | What it is |
| --- | --- | --- | --- |
| Kernel thread | `Thread` | no | scheduler entity, kstack, `arch_thread`, `thread_state` |
| User thread | `ThreadDispatcher` | yes (`ZX_OBJ_TYPE_THREAD`) | handle, lifecycle, exceptionate, user entry regs |

`zx_thread_create` allocates both: `ThreadDispatcher::create` builds
the dispatcher, then `Thread::Create` with entry `StartRoutine` and a
fair default profile. `initialize()` attaches the process aspace to
the kernel thread (`INITIALIZED`). Nothing runs until `zx_process_start`
or `zx_thread_start` calls `MakeRunnable`, which `Resume`s the
kernel thread.

Idle, DPC, and the CPU0 bootstrap thread are `Thread`s with no
dispatcher. They never enter EL0.

## 8. Lifecycle

Three machines. Do not conflate them.

| Machine | Where | Who |
| --- | --- | --- |
| `thread_state` | [`scheduler_state.h`](../../../zircon/kernel/include/kernel/scheduler_state.h) | every `Thread` (Ready / Running / Blocked / …) |
| `ThreadDispatcher` lifecycle | [`thread_state.rs`](../../../zircon/kernel/object/thread_state.rs) | user threads only |
| `ProcessDispatcher::State` / `JobDispatcher::State` | [`process_dispatcher.h`](../../../zircon/kernel/object/include/object/process_dispatcher.h), [`job_dispatcher.h`](../../../zircon/kernel/object/include/object/job_dispatcher.h) | the objects userspace waits on |

`thread_state` is **CPU STR** above. It answers "is this `Thread` on a
run queue?". The machines below answer "is this job/process/thread
object still alive?". A user thread that is `Lifecycle::Running` spends
most of its life `BLOCKED` in the scheduler (syscall wait) or `READY`
on some other CPU.

#### Job

```text
 Create / CreateRootJob
        │
        ▼
     ┌───────┐  Kill() / last child gone after Kill
     │ READY │──────────────────────────────────────► KILLING
     └───────┘                                         │
       accepts children                                │ children killed
       policy may be set only while empty              ▼
                                                    ┌──────┐
                                                    │ DEAD │  ZX_TASK_TERMINATED
                                                    └──────┘
```

`Kill` is depth-first: this job `READY` → `KILLING`, then child jobs,
then child processes. `DEAD` only when `KILLING` **and** both child
lists are empty (`IsReadyForDeadTransitionLocked`). A job that was
never killed and still has a parent stays `READY` even with no
children; the empty signals fire instead.

#### Process

```text
 zx_process_create
        │
        ▼
    ┌─────────┐  first thread MakeRunnable
    │ INITIAL │──────────────────────────► RUNNING
    └─────────┘                             │
      no threads                            │ zx_process_exit / Kill
      aspace + handle table exist           ▼
                                         ┌───────┐
                                         │ DYING │  KillAllThreads
                                         └───┬───┘
                                             │ last thread RemoveThread
                                             ▼
                                          ┌──────┐
                                          │ DEAD │  ZX_TASK_TERMINATED
                                          └──────┘
                                             handle table / aspace torn down
                                             removed from the job
```

`zx_process_start` is the `INITIAL` → `RUNNING` edge. Further
`zx_thread_create` / `zx_thread_start` is allowed only in `RUNNING`.
`Exit` (the current process) and `Kill` (any process) both enter
`DYING` and broadcast `ThreadDispatcher::Kill`. The last thread's
`RemoveThread` moves the process to `DEAD` and
`FinishDeadTransition`. A last thread that exits by itself
(`zx_thread_exit`) does the same from `RUNNING`, skipping `DYING`.

A process with no threads in `INITIAL` that is killed goes `INITIAL` →
`DEAD` directly.

#### User thread

`Lifecycle` in [`zircon/kernel/object/thread_state.rs`](../../../zircon/kernel/object/thread_state.rs). The only
non-monotonic edge is `SUSPENDED` → `RUNNING`.

```text
 zx_thread_create
        │
        ▼
    ┌─────────┐  initialize()  ┌─────────────┐  MakeRunnable
    │ INITIAL ├───────────────►│ INITIALIZED ├─────────────► RUNNING
    └─────────┘                └─────────────┘                 │
      dispatcher + Thread        aspace attached               │
      not on a run queue         waiting for start             ├── Suspend ──► SUSPENDED
                                                               │                  │
                                                               │                  └── Resume ──► RUNNING
                                                               │
                                                               ├── Kill / Exit ──► DYING
                                                               │                     │
                                                               │                     ▼
                                                               └─────────────────► DEAD
                                                                 ExitingCurrent, drop from process
```

Exception handling is a second axis (`Idle` / `Unprocessed` /
`TryNext` / `Resume`) and is orthogonal to suspend. A thread may be
both `SUSPENDED` and in an exception; it does not execute user
instructions until both are cleared. Only `RUNNING`, `SUSPENDED`, and
`DYING` threads participate in exception handling.

`Kill` on `INITIAL` / `INITIALIZED` leaves the object there (never
started). On `RUNNING` / `SUSPENDED` it posts `THREAD_SIGNAL_KILL` on
the kernel `Thread` and moves to `DYING`. `zx_task_kill` on a thread
handle is `ZX_ERR_NOT_SUPPORTED`; threads die by `zx_thread_exit`,
process/job kill, or the kernel kill signal.

`ExitingCurrent` (on the dying thread itself): `DYING` → debugger
`ZX_EXCP_THREAD_EXITING` → `DEAD` → `ResetCoreThread` →
`process()->RemoveThread`. If that was the last thread, the process
goes `DEAD` in the same call.

#### Console boot, one picture

```text
root job                         READY  (max_height 32)
  └── process "userboot"         INITIAL ──start──► RUNNING
        └── thread               INITIALIZED ──MakeRunnable──► RUNNING
              Thread             READY → RUNNING
              eret EL0           userboot unpacks BOOTFS
                                 launches component_manager in a child
                                 process of the same job tree
                                   └── console-launcher → zxsh
```

`zxsh` blocked on UART is: process `RUNNING`, `ThreadDispatcher`
`RUNNING`, kernel `Thread` `BLOCKED`, that CPU's scheduler on
`IdlePowerThread` (`WFI`).

## 9. Cache

AArch64 has separate I and D caches. The kernel talks to them with
`DC` / `IC` by MVA, line size from `CTR_EL0` (`arm64_dcache_size`).
ARM names two points on that path (ARM DDI 0487):

| Point | Full name | Until which observers agree on a location |
| --- | --- | --- |
| **PoU** | Point of Unification | This PE's I-cache, D-cache, and table walks. Typically where I and D meet (often L2). `dc cvau`. |
| **PoC** | Point of Coherence | **All** agents that can access that location: other PEs, DMA, SMMU. Typically RAM / last-level cache. `dc cvac` / `civac` / `ivac`. |

PoU is closer to the PE than PoC. Cleaning to PoU is enough for this
core to execute stores as instructions. Cleaning to PoC is required
before another observer (another CPU, a device, a guest) must see the
bytes.

| Function | Instruction | Point |
| --- | --- | --- |
| `arch_clean_cache_range` | `dc cvac` | PoC |
| `arch_clean_invalidate_cache_range` | `dc civac` | PoC, clean+invalidate |
| `arch_invalidate_cache_range` | `dc ivac` | PoC, invalidate |
| `arm64_clean_cache_range_pou` | `dc cvau` | PoU |
| `arch_sync_cache_range` | `dc cvau` + `ic ialluis` | make newly written text executable |

`arch_sync_cache_range` is what `ArmArchVmAspace::Map` uses for executable
pages: D-cache clean to **PoU** so this PE's I-cache can see the stores,
then `ic ialluis` (I-cache invalidate, Inner Shareable) so other PEs
drop stale I-lines. A `dsb ish` + `isb` orders it.

SCTLR_EL1.C / .I enable D-cache and I-cache; they only help once the MMU is
on ([`memory.md`](memory.md)). MAIR indices decide whether a page is cached at all:

| AttrIndx | Type |
| --- | --- |
| 0 | Device-nGnRnE |
| 1 | Device-nGnRE (MMIO, GIC, UART) |
| 2 | Normal Write-Back (RAM, physmap, kernel) |
| 3 | Normal non-cacheable |

Do not run cache-by-MVA on device memory. GIC / UART mappings are
`UNCACHED_DEVICE`.

## 10. CPU scheduler

Each CPU has its own `Scheduler` in `percpu`. There is no global run queue.
A thread is **Ready** on exactly one CPU's queue, **Running** on that CPU,
or blocked on a wait queue.

Two disciplines share the queue (`SchedDiscipline` in
[`zircon/kernel/include/kernel/scheduler_state.h`](../../../zircon/kernel/include/kernel/scheduler_state.h)):

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
[`zircon/kernel/kernel/scheduler.rs`](../../../zircon/kernel/kernel/scheduler.rs), C++ FFI in [`scheduler.cc`](../../../zircon/kernel/kernel/scheduler.cc) /
[`scheduler.h`](../../../zircon/kernel/include/kernel/scheduler.h). Follow the Rust `Reschedule` path, then the C++
`arch_context_switch`.

A guest VCPU is **not** a third discipline. It is a Fair (or Deadline,
if userspace applied a profile) thread with `THREAD_FLAG_VCPU`. See
**VCPU scheduling**.

## 11. VCPU scheduling

Secular's hypervisor does **not** add a second run queue. Guest tasks
never appear in `Scheduler`. A VCPU is a **host `Thread*`** that spends
most of its RUNNING time inside `arm64_el2_enter` (guest EL1), and
**blocks** like any other thread when the guest executes `WFI`.

That is the whole bet. The optimizations below are about *not*
returning to the VMM, *not* spinning in idle, and *hitting the right
pCPU* with an IPI. They are not Crook's `VcpuGroup` (priority/slice
policy per VM). This tree has no `VcpuGroup`, no RUN/PAUSE/SHARE
bits, and does not pin a VCPU to a pCPU by default.

Userspace: `bin/vmm` starts one std::thread per VCPU, then
`zx_vcpu_create` + `zx_vcpu_enter` loop
([`userspace/hypervisor/bin/vmm/vcpu.cc`](../../../userspace/hypervisor/bin/vmm/vcpu.cc),
[`userspace/hypervisor/README.md`](../../../userspace/hypervisor/README.md)).
Guest IRQs and MMIO traps: [`interrupt.md`](interrupt.md).

#### Framework

```text
 pCPU  Scheduler  Fair / Deadline   (unchanged)
        │
        ▼
 Thread*  THREAD_FLAG_VCPU   (at most one Vcpu per thread)
        │  last_cpu_ via SetMigrateFn
        │
        ├── RUNNING  ── arm64_el2_enter ── guest EL1  (world switch, same Thread*)
        │                 host IRQ  →  ZX_ERR_NEXT  →  re-enter  (no VMM)
        │                 MMIO/SMC  →  packet  →  EL0 VMM
        │
        └── BLOCKED  guest WFI  →  InterruptTracker AutounsignalEvent
                         inject IRQ / Kick / timer  →  READY
```

| Switch | What changes | Scheduler? |
| --- | --- | --- |
| `arch_context_switch` | different `Thread*` | yes |
| `arm64_el2_enter` / vmexit | same `Thread*`, EL1 ↔ guest EL1 via EL2 | **no** |
| guest `WFI` | this `Thread*` READY → BLOCKED | yes (yields the pCPU) |
| `Vcpu::Interrupt` while RUNNING | IPI to `last_cpu_` → physical vmexit | maybe, after `AutoGich` drops |

#### Diagrams

Four posters: identity, enter loop, idle yield, IRQ injection.

##### 1. One host thread is the VCPU

`zx_vcpu_create` binds to **current** thread. A second create on the
same thread is `ZX_ERR_BAD_STATE`. Destroy clears the flag and the
migrate function.

```text
 VMM process
   std::thread "vcpu-0"
        zx_vcpu_create(guest, entry)     Thread::Current
              │
              ▼
        thread->set_vcpu(true)           THREAD_FLAG_VCPU
        thread->SetMigrateFn(Vcpu::Migrate)
              │
              ▼
        zx_vcpu_enter  ──►  Vcpu::Enter must be that same Thread*
                            else ZX_ERR_BAD_STATE
```

`THREAD_FLAG_VCPU` is a **tag**, not a scheduling class. The dump
shows `Vc`. `scheduler.rs` never branches on it. Fair/Deadline and
affinity still apply (`zx_object_set_profile` from userspace).

##### 2. Enter loop: keep the guest hot

Physical IRQs and extra virtual IRQs that did not fit in GIC list
registers must not bounce through EL0.

```text
 Vcpu::Enter
   CheckKillOrSuspendSignal
   timer_maybe_interrupt     CNTV expired?  Track timer PPI
   gich_maybe_interrupt      pending bitmap → ICH LRs
                             SGI/PPI prio 0, SPI prio 0x10
   AutoGich                  IRQs off, blocking disallowed
     kicked_? → ZX_ERR_CANCELED
     arm64_el2_enter(VTTBR, el2_state, HCR)
   restore GICH, IRQs on     ← preempt / Reschedule possible here

   ZX_ERR_NEXT  physical IRQ or LR underflow maintenance
        │  ktrace, count, status = ZX_OK
        │  do **not** return a packet
        ▼
   loop  (guest stays on this Thread*)

   ZX_OK + packet   MMIO / SMC / unhandled  →  return to VMM
   Kick             kicked_ + Cancel wait + IPI
```

`AutoGich` sets GICH `HCR.En`, and `HCR.Uie` (underflow) when there
are still pending vectors and more than one LR. When the guest
drains the LRs, a **maintenance interrupt** VM-exits so the host can
inject the rest — lower latency than waiting for the next guest
`WFI` or a timer.

Between loop iterations IRQs are on. A host timer that unblocked a
higher-priority thread can `Reschedule` before the next
`arm64_el2_enter`. The guest is not non-preemptible forever; only
inside `AutoGich`.

##### 3. Guest idle yields the pCPU

`HCR_EL2.TWI` (and `TWE`) trap wait instructions. `WFE` is skipped
(`zx::ok()`, resume). `WFI` with no active LR **blocks the host
thread**.

```text
 guest  WFI
        │  trap EL2
        ▼
 handle_wfi_wfe_instruction
        LR in use?  →  resume guest (an IRQ is already active)
        CNTV enabled and already due?  →  resume
        else deadline = guest CNTV  (or infinite)
        ▼
 InterruptTracker::Wait(deadline)
        event_.Wait     Thread BLOCKED
        │
        ▼
 Scheduler::Reschedule
        other Ready thread, else idle  (this pCPU is free)

 inject / Kick / timeout
        event_.Signal   (Kick uses ZX_ERR_INTERNAL_INTR_RETRY)
        Thread READY    →  Vcpu::Enter loop again
```

That is the scheduling optimization that matters on a loaded host:
an idle Linux guest is a blocked Zircon thread, not a spinning PE.
`ktrace_vcpu(VCPU_BLOCK/UNBLOCK, VCPU_INTERRUPT)` marks the wait.

##### 4. Inject without waiting for the next enter

A virtual IRQ (vgic, virtio, Kick) must reach a **running** guest
now, not at the next `zx_vcpu_enter` from the VMM.

```text
 other CPU / VMM
   zx_vcpu_interrupt(vector)  or  Vcpu::Kick
        │
        ▼
 GichState::Interrupt(vector)     pending bitmap + event_.Signal
 Kick: kicked_=true, Cancel()     wakes a WFI waiter
        │
        ▼
 InterruptCpu
   Thread list lock + thread lock
   state == RUNNING && last_cpu_ valid?
        yes →  mp_interrupt(MASK, last_cpu_)   GIC SGI
        no  →  nothing; Wait/enter will see pending / kicked_
        │
        ▼
 running guest: physical IRQ → EL2 → ZX_ERR_NEXT
        gich_maybe_interrupt fills LRs
        re-enter guest  (still no VMM)
```

`last_cpu_` is **not** hard affinity. The thread may migrate.
`SetMigrateFn` updates it:

| Stage | `last_cpu_` |
| --- | --- |
| Save | `INVALID_CPU` (do not IPI mid-migrate) |
| Restore | `thread->LastCpuLocked()` |
| Exiting | `thread_ = nullptr` |

Wrong `last_cpu_` would IPI a PE that is no longer running this
VCPU. Save/Restore close that window.

#### Flow

End-to-end, one guest timer tick while another VM's VCPU is idle
on the same pCPU.

```text
 VCPU-A  RUNNING, in guest
 VCPU-B  BLOCKED  (guest WFI on this pCPU? no — B is on its last_cpu)

 host timer IRQ on A's PE
        EL2 vmexit  ZX_ERR_NEXT
        AutoGich drops, IRQs on
        IRQ handler may Unblock some thread
        Reschedule if that thread is higher than A
        else A loops, timer_maybe_interrupt, re-enter

 VCPU-B's CNTV due
        Wait timeout  →  ZX_OK  →  Enter
        Track timer vector, fill LR, el2_enter
        guest takes the PPI
```

Create/enter/kick from the VMM:

```text
 vcpu-N thread
   zx_vcpu_create  →  FLAG_VCPU, MigrateFn, vpid, ICH/HCR
   zx_vcpu_write_state  (x0 = DTB / boot_ptr)
   loop:
     zx_vcpu_enter(&packet)
        ZX_ERR_CANCELED  Kick / kill / suspend
        ZX_OK            packet: MMIO trap, etc.  →  HandlePacket
        then enter again
   ~Vcpu  kick + join
```

#### What this is not

| Crook (`crook/scheduler.md`) | Secular Zircon |
| --- | --- |
| `VcpuGroup` per pCPU, RUN/PAUSE/SHARE | no |
| priority/slice rewritten at every Resume | Fair/Deadline unchanged unless userspace sets a profile |
| VCPU pinned to one pCPU | migratable; `last_cpu_` is a cache for IPI |
| guest FIFO / `preempt_count` sampled from guest RAM | host never looks at guest tasks |
| lua / ioctl scheduling policy | VMM thread + ordinary `zx` profile |

Do not port Crook policy into `scheduler.rs`. If a guest needs
latency, give its **host** VCPU thread a Deadline profile from
userspace.

#### Where (VCPU)

| File | Why |
| --- | --- |
| [`zircon/kernel/arch/arm64/hypervisor/vcpu.cc`](../../../zircon/kernel/arch/arm64/hypervisor/vcpu.cc) | Create, Enter loop, Kick, InterruptCpu, AutoGich |
| [`zircon/kernel/arch/arm64/hypervisor/vmexit.cc`](../../../zircon/kernel/arch/arm64/hypervisor/vmexit.cc) | WFI wait, timer_maybe_interrupt |
| [`zircon/kernel/hypervisor/include/hypervisor/interrupt_tracker.h`](../../../zircon/kernel/hypervisor/include/hypervisor/interrupt_tracker.h) | pending bitmap + `AutounsignalEvent` |
| [`zircon/kernel/include/kernel/thread.h`](../../../zircon/kernel/include/kernel/thread.h) | `THREAD_FLAG_VCPU` |
| [`zircon/kernel/object/vcpu.rs`](../../../zircon/kernel/object/vcpu.rs) | `zx_vcpu_*` object; `cpp_vcpu_enter` |
| [`userspace/hypervisor/bin/vmm/vcpu.cc`](../../../userspace/hypervisor/bin/vmm/vcpu.cc) | one host thread per VCPU, enter loop |

## 12. CPU STR

STR here is **state transition**: two machines, not one. Threads move through
`thread_state`. The CPU's idle/power thread has a second machine
(`IdlePowerThread::State`) that decides whether this PE should run work,
`wfi`, suspend, or go offline.

#### Thread states

`enum thread_state` in [`zircon/kernel/include/kernel/scheduler_state.h`](../../../zircon/kernel/include/kernel/scheduler_state.h). A
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

This machine is **not** `ThreadDispatcher::Lifecycle` or
`ProcessDispatcher::State`. Those are the object lifetimes in
**Lifecycle**. A user thread can be `Lifecycle::Running` and
`thread_state::BLOCKED` at the same time.

#### IdlePowerThread states

`IdlePowerThread::State` in [`zircon/kernel/include/kernel/idle_power_thread.h`](../../../zircon/kernel/include/kernel/idle_power_thread.h).
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

## 13. When the CPU enters idle

"Idle" means **this PE is running `IdlePowerThread::Run`**, not that the
hardware is off. The scheduler picks that thread in
`evaluate_next_thread` ([`scheduler.rs`](../../../zircon/kernel/kernel/scheduler.rs)):

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

Wakeup from idle is an **IRQ** (see [`interrupt.md`](interrupt.md)): timer PPI, SGI
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

## 14. Idle

Idle is a **per-CPU kernel thread**, not a special CPU mode bit. Hardware
low power is a `WFI` (or a platform suspend) **inside** that thread.

#### What runs

`percpu.idle_power_thread` ([`zircon/kernel/include/kernel/percpu.h`](../../../zircon/kernel/include/kernel/percpu.h)).
Entry is `IdlePowerThread::Run` ([`idle_power_thread.cc`](../../../zircon/kernel/kernel/idle_power_thread.cc)). CPU0's boot
thread becomes this after `kernel_init`; secondaries become it in
[`secondary.cc`](../../../zircon/kernel/arch/arm64/secondary.cc) (`thread->task_state().entry() == IdlePowerThread::Run`).

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

`ArchIdlePowerThread::EnterIdleState` ([`zircon/kernel/arch/arm64/arch.cc`](../../../zircon/kernel/arch/arm64/arch.cc)):

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

Panic halt ([`platform.cc`](../../../zircon/kernel/platform/generic-arm/platform.cc)) is a different loop: IRQs disabled forever and
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
| Thread STR | [`scheduler_state.h`](../../../zircon/kernel/include/kernel/scheduler_state.h) `thread_state` |
| Power STR | [`idle_power_thread.h`](../../../zircon/kernel/include/kernel/idle_power_thread.h) `State` / `pending_power_work` |
| Who is selected | [`scheduler.rs`](../../../zircon/kernel/kernel/scheduler.rs) `evaluate_next_thread` |
| Idle run loop | [`idle_power_thread.cc`](../../../zircon/kernel/kernel/idle_power_thread.cc) `IdlePowerThread::Run` |
| `DSB` + `WFI` | [`arch.cc`](../../../zircon/kernel/arch/arm64/arch.cc) `ArchIdlePowerThread::EnterIdleState` |
| Rules for idle entry | [`include/arch/ops.h`](../../../zircon/kernel/include/arch/ops.h) `ArchIdlePowerThread` |
| Secondary becomes idle | [`secondary.cc`](../../../zircon/kernel/arch/arm64/secondary.cc), [`thread.cc`](../../../zircon/kernel/arch/arm64/thread.cc) |
| Offline | [`mp.cc`](../../../zircon/kernel/arch/arm64/mp.cc) `mp_unplug_current_cpu` |
| IRQ that ends `WFI` | [`interrupt.md`](interrupt.md) |

## 15. SMP bring-up

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

## 16. Basic registers (CPU)

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

## 17. References

Read in this order.

| File | Why |
| --- | --- |
| [`zircon/kernel/arch/arm64/include/arch/regs.h`](../../../zircon/kernel/arch/arm64/include/arch/regs.h) | `iframe_t` |
| [`zircon/kernel/arch/arm64/include/arch/arch_thread.h`](../../../zircon/kernel/arch/arm64/include/arch/arch_thread.h) | `arch_thread`, FPU, TLS offsets |
| [`zircon/kernel/arch/arm64/include/arch/arm64.h`](../../../zircon/kernel/arch/arm64/include/arch/arm64.h) | context-switch frame |
| [`zircon/kernel/arch/arm64/include/arch/arm64/mp.h`](../../../zircon/kernel/arch/arm64/include/arch/arm64/mp.h) | `arm64_percpu`, `x20` |
| [`zircon/kernel/arch/arm64/include/arch/current_thread.h`](../../../zircon/kernel/arch/arm64/include/arch/current_thread.h) | `TPIDR_EL1` → `Thread*` |
| [`zircon/kernel/include/kernel/percpu.h`](../../../zircon/kernel/include/kernel/percpu.h) | `Scheduler` / timers / idle |
| [`zircon/kernel/arch/arm64/thread.cc`](../../../zircon/kernel/arch/arm64/thread.cc) | `arch_context_switch` |
| [`zircon/kernel/arch/arm64/asm.S`](../../../zircon/kernel/arch/arm64/asm.S) | `arm64_context_switch` |
| [`zircon/kernel/arch/arm64/src/fpu.rs`](../../../zircon/kernel/arch/arm64/src/fpu.rs) | FPU save/restore |
| [`zircon/kernel/arch/arm64/cache-ops.S`](../../../zircon/kernel/arch/arm64/cache-ops.S) | `dc` / `ic` |
| [`zircon/kernel/include/kernel/scheduler_state.h`](../../../zircon/kernel/include/kernel/scheduler_state.h) | Fair / Deadline; `thread_state` STR |
| [`zircon/kernel/kernel/scheduler.rs`](../../../zircon/kernel/kernel/scheduler.rs) | pick next thread; idle as last resort; **no** VCPU branch |
| [`zircon/kernel/kernel/scheduler.cc`](../../../zircon/kernel/kernel/scheduler.cc) | C++ FFI into the scheduler |
| [`zircon/kernel/arch/arm64/hypervisor/vcpu.cc`](../../../zircon/kernel/arch/arm64/hypervisor/vcpu.cc) | VCPU enter loop, Kick IPI, `THREAD_FLAG_VCPU` |
| [`zircon/kernel/include/kernel/idle_power_thread.h`](../../../zircon/kernel/include/kernel/idle_power_thread.h) | Active / Wakeup / Suspend / Offline |
| [`zircon/kernel/kernel/idle_power_thread.cc`](../../../zircon/kernel/kernel/idle_power_thread.cc) | `Run` loop; when to `WFI` |
| [`zircon/kernel/include/arch/ops.h`](../../../zircon/kernel/include/arch/ops.h) | `EnterIdleState` IRQ/preempt rules |
| [`zircon/kernel/arch/arm64/arch.cc`](../../../zircon/kernel/arch/arm64/arch.cc) | `DSB` + `WFI`; `arch_enter_uspace` |
| [`zircon/kernel/arch/arm64/secondary.S`](../../../zircon/kernel/arch/arm64/secondary.S) | AP MMU on + jump to high VA |
| [`zircon/kernel/arch/arm64/secondary.cc`](../../../zircon/kernel/arch/arm64/secondary.cc) | AP idle thread |
| [`zircon/kernel/arch/arm64/mp.cc`](../../../zircon/kernel/arch/arm64/mp.cc) | MPIDR ↔ `cpu_num` |
| [`zircon/kernel/dev/psci/`](../../../zircon/kernel/dev/psci/) | `CPU_ON` / `CPU_OFF` |
| [`zircon/kernel/arch/arm64/exceptions.S`](../../../zircon/kernel/arch/arm64/exceptions.S) | VBAR; EL0 save; syscall trampolines; `eret` |
| [`zircon/kernel/arch/arm64/uspace_entry.S`](../../../zircon/kernel/arch/arm64/uspace_entry.S) | first `eret` to EL0 |
| [`zircon/kernel/lib/userabi/vdso/zircon-syscall-arm64.S`](../../../zircon/kernel/lib/userabi/vdso/zircon-syscall-arm64.S) | vDSO `svc` stub + speculation barrier |
| [`zircon/kernel/lib/syscalls/syscalls.cc`](../../../zircon/kernel/lib/syscalls/syscalls.cc) | `do_syscall` pre/post; `wrapper_*` |
| [`job.rs`](../../../zircon/kernel/lib/syscalls/job.rs), [`process.rs`](../../../zircon/kernel/lib/syscalls/process.rs), [`thread.rs`](../../../zircon/kernel/lib/syscalls/thread.rs), [`task.rs`](../../../zircon/kernel/lib/syscalls/task.rs) | `sys_job_*` / `sys_process_*` / `sys_thread_*` / `sys_task_kill` |
| [`zircon/kernel/object/include/object/job_dispatcher.h`](../../../zircon/kernel/object/include/object/job_dispatcher.h) | job tree, policy, `READY`/`KILLING`/`DEAD` |
| [`zircon/kernel/object/include/object/process_dispatcher.h`](../../../zircon/kernel/object/include/object/process_dispatcher.h) | process `INITIAL`/`RUNNING`/`DYING`/`DEAD` |
| [`zircon/kernel/object/include/object/thread_dispatcher.h`](../../../zircon/kernel/object/include/object/thread_dispatcher.h) | user thread; `StartRoutine`; `Blocked` |
| [`zircon/kernel/object/thread_state.rs`](../../../zircon/kernel/object/thread_state.rs) | `Lifecycle` / `Exception` state machine |
| [`zircon/kernel/object/thread_dispatcher.rs`](../../../zircon/kernel/object/thread_dispatcher.rs) | `create` / `initialize` / `start` / `kill` |
| [`zircon/kernel/include/kernel/thread.h`](../../../zircon/kernel/include/kernel/thread.h) | kernel `Thread`; `user_thread()` |
| [`zircon/kernel/lib/userabi/userabi.rs`](../../../zircon/kernel/lib/userabi/userabi.rs) | `userboot_init`: first process + root job handle |
| [`zircon/kernel/vm/include/vm/kstack.h`](../../../zircon/kernel/vm/include/vm/kstack.h) | per-thread kernel stack |

Suggested first pass: **iframe + `arch_thread` → EL0 `svc` in
[`exceptions.S`](../../../zircon/kernel/arch/arm64/exceptions.S) → `do_syscall` → `arch_context_switch` →
[`scheduler.rs`](../../../zircon/kernel/kernel/scheduler.rs) Reschedule → `evaluate_next_thread` (idle) →
`IdlePowerThread::Run` / `WFI` → `StartRoutine` / `arm64_uspace_entry` →
job/process/thread create in [`userabi.rs`](../../../zircon/kernel/lib/userabi/userabi.rs)**. That is enough to see a
timer IRQ preempt `zxsh`, a syscall block leave its CPU in `WFI`, a
second QEMU vCPU come online as idle, userboot's first `eret`, and a
guest `WFI` block the host VCPU thread (same scheduler).

IRQ delivery itself is [`interrupt.md`](interrupt.md). Locks and barriers are
[`sync.md`](sync.md). The first process is [`bootstrap.md`](bootstrap.md). User TTBR0
is [`memory.md`](memory.md). The port loop is [`async.md`](async.md).

## Anti-patterns

* **Assume the kernel runs at EL1.** This product is EL2 VHE with EL1-named sysregs.
* **Block in IRQ context with `Mutex`.** Use `SpinLock` + `IrqSave`.
* **Treat a VCPU as a second scheduler.** It is the same `Thread*` with `THREAD_FLAG_VCPU`.

## First-pass checklist

- [ ] iframe + `TPIDR_EL1` → `Thread*`
- [ ] EL0 `svc` → `do_syscall` → `eret`
- [ ] `Reschedule` → idle `WFI` or another thread

## Related

* [`bootstrap.md`](bootstrap.md)
* [`memory.md`](memory.md)
* [`cpu.md`](cpu.md)
* [`interrupt.md`](interrupt.md)
* [`ipc.md`](ipc.md)
* [`async.md`](async.md)
* [`sync.md`](sync.md)

## 18. Terminology

| Term | Meaning in this note |
| --- | --- |
| PE | Processing Element. One QEMU vCPU; one set of GPRs, EL1 sysregs, GIC redistributor. |
| EL0 | Exception level 0. Userspace. `zxsh`, userboot, component_manager. |
| EL1 | Exception level 1. EL1-named sysregs; on this product the kernel is an EL2 VHE host. |
| EL2 VHE | Host at EL2 with `HCR_EL2.{RW,E2H,TGE}`. Looks like EL1 to most kernel code. |
| `eret` | Exception return. Hardware restores `PSTATE` from `SPSR_EL1` and `PC` from `ELR_EL1`. The only way back to EL0. |
| `svc` | Supervisor call. EL0 instruction that takes a sync exception; Zircon uses `svc #0` with the syscall number in `x16`. |
| iframe | `iframe_t`. The 272-byte exception frame on the kernel stack: GPRs, `ELR`, `SPSR`, `SP_EL0`. Syscalls reuse it as `syscall_regs_t`. |
| Context-switch frame | `arm64_context_switch_frame`. Callee-saved integer regs (`x19`, `x21`–`lr`) swapped by `arm64_context_switch`. Not an iframe. |
| Scheduler context switch | Changing `Thread*` on this PE. Saves the switch frame, FPU, user TLS; may switch `TTBR0`. Both threads stay in the kernel (EL2 VHE). |
| Exception / syscall switch | Same `Thread*`, EL0 ↔ kernel. Pushes or pops an iframe. `TPIDR_EL1` unchanged. |
| Kernel stack | `SP_EL1` for this `Thread`. Holds the iframe and kernel C++ frames. Default 2 pages. |
| User stack | `SP_EL0`. Mapped in the process aspace by userspace. |
| `TPIDR_EL1` | Kernel TLS. Holds the current `Thread`'s `thread_pointer_location`; source of `Thread::Current`. |
| `TPIDR_EL0` | User TLS. Swapped on scheduler switch with the thread. |
| `x20` | Fixed by the ABI as `arm64_percpu*` while in the kernel (`-ffixed-x20`). Saved in the iframe on EL0 entry, not swapped on context switch. |
| `TTBR0_EL1` | User page-table base + ASID. Per process (`VmAspace`). Switched only when the new thread's aspace differs. |
| `TTBR1_EL1` | Kernel page-table base. Shared. Never switched on a thread change. See [`memory.md`](memory.md). |
| vDSO | Virtual DSO mapped into every process. The only approved `svc` sites live here. Kernel rejects a syscall whose `ELR` is not in this mapping. |
| Syscall number | Integer in `x16`. Index into the 16-byte trampoline table; must be `< ZX_SYS_COUNT`. |
| `wrapper_<name>` | Generated veneer. Validates / marshals arguments, calls `sys_<name>`. |
| `sys_<name>` | The real syscall body ([`zircon/kernel/lib/syscalls/`](../../../zircon/kernel/lib/syscalls/)). |
| Blocking syscall | A `sys_*` that may wait (channel, futex, `zx_object_wait_*`, …). Sets `ThreadDispatcher::Blocked` and can `Reschedule`. |
| `THREAD_FLAG_VCPU` | This `Thread*` owns a `Vcpu`. At most one. Not a scheduling class. |
| World switch | `arm64_el2_enter`: same host thread, guest EL1 via EL2. Not `arch_context_switch`. |
| `ZX_ERR_NEXT` | Physical IRQ (or LR underflow) vmexit; `Vcpu::Enter` re-enters, no VMM packet. |
| Guest `WFI` | Trapped (`HCR.TWI`); host thread `BLOCKED` on `InterruptTracker` until IRQ/Kick/timer. |
| `last_cpu_` | Cached pCPU for IPI on inject/Kick. Updated by `SetMigrateFn`. Not hard pin. |
| Thread signal | Pending work on a `Thread` (`KILL`, `SUSPEND`, policy exception, …). Sampled with IRQs off on the way back to EL0. |
| `Thread` | Kernel scheduler object. Has a kstack, `arch_thread`, `thread_state`. What `TPIDR_EL1` names. |
| `ThreadDispatcher` | Userspace-visible thread (`ZX_OBJ_TYPE_THREAD`). Owns a `Thread` from create until `ExitingCurrent`. |
| Kernel-only thread | A `Thread` with `user_thread() == nullptr`. Idle, DPC, bootstrap. Never `eret`s. |
| Process | `ProcessDispatcher`. Handle table, user aspace, list of threads, parent job. Not a runnable entity. |
| Job | `JobDispatcher`. Tree node over jobs and processes. Policy, kill, exception propagation. Not a runnable entity. |
| Root job | Job with parent `nullptr`, `max_height` 32, created by the kernel. Userboot is its first process. Becoming childless halts the system. |
| Handle | Process-local integer naming a kernel object with a rights mask. Not a pointer; not valid in another process. |
| Handle table | Per-process (`ShareableProcessState`). `zx_process_create` / `zx_thread_create` insert the new handles here, in the *caller*. |
| KOID | Kernel object ID. Stable for the life of the object. `related_koid` of a thread is its process; of a process, its job. |
| Task | Job, process, or thread. `zx_task_kill` accepts a job or process handle, not a thread. |
| `thread_state` | Scheduler STR of a `Thread`: `INITIAL`, `READY`, `RUNNING`, `BLOCKED`, `SLEEPING`, `SUSPENDED`, `DEATH`. |
| `Lifecycle` | Object STR of a `ThreadDispatcher`: `INITIAL` → `INITIALIZED` → `RUNNING` ↔ `SUSPENDED` → `DYING` → `DEAD`. |
| Process `State` | `INITIAL` → `RUNNING` → `DYING` → `DEAD`. `RUNNING` means "has started at least one thread", not "a CPU is in this process right now". |
| Job `State` | `READY` (accepts children) → `KILLING` → `DEAD` (no children left after a kill). |
| `ZX_TASK_TERMINATED` | Waitable signal set when a job, process, or thread reaches `DEAD`. Same bit as `ZX_PROCESS_TERMINATED` / `ZX_THREAD_TERMINATED`. |
| Fair / Deadline | The two scheduler disciplines. See **CPU scheduler**. Independent of job/process policy. |
| Idle | This PE is running `IdlePowerThread`, usually in `WFI`. Not "no `Thread`". Not "CPU offline". |
| ASID | Address Space ID in `TTBR0`. Identifies the user aspace in the TLB so a process switch does not need a full TLB flush. |
| PoU | Point of Unification. The point at which this PE's I-cache, D-cache, and translation-table walks are guaranteed to see the same copy of a location (ARM DDI 0487). Closer to the PE than PoC; typically where I and D caches meet. `dc cvau` cleans to PoU. Used before this core executes newly written text (`arch_sync_cache_range`). |
| PoC | Point of Coherence. The point at which **all** agents that can access a location (other PEs, DMA, SMMU) are guaranteed to see the same copy. Further out than PoU; typically last-level cache / RAM. `dc cvac` / `civac` / `ivac` operate to PoC. |
| `max_height` | Remaining job-tree depth. Root 32; each child is parent minus one. Zero means no further children. |
| Policy | Job-level restrictions (`ZX_POL_NEW_*`, WX, bad handle, …) inherited by child jobs and enforced on syscalls of descendant processes. |
| Exceptionate | Channel that receives thread/process/job exceptions. Walk is thread → process → job → parent jobs. |
| Restricted mode | Optional split aspace (private + shared) used by Starnix. Not on the console boot path. ARM32 `svc` is rejected unless this is set. |

