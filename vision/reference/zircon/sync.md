---
name: zircon-sync
description: >
  Use when changing kernel locks, wait queues, futexes, or memory ordering on the Secular console image.
---

# Zircon Synchronization: Barriers, Locks, and WaitQueues

Stop when a waiter is READY again.

## When to use

* Editing `SpinLock`, `Mutex`, `CriticalMutex`, `WaitQueue`, or `zx_futex_*`.
* A deadlock, missed wakeup, or IRQ-context sleep.

## When not to use

* Port / async_loop wait — `async.md`.
* Which thread runs after wake — `cpu.md`.

> [!NOTE]
> Console image: Zircon kernel + linux-arm64-boot-shim + BOOTFS + serial
> console + `zxsh`. The kernel is an EL2 VHE host.

## 1. Framework

```text
┌─────────────────────────────────────────────────────────────────────────┐
│ PE  (weakly ordered AArch64)                                            │
│   compiler:  atomic_signal_fence / "memory" clobber                     │
│   CPU:       DMB  (order)   DSB  (complete)   ISB  (fetch)              │
│   atomics:   ldaxr / stlxr / stlr   C++ memory_order_*                  │
└───────────────┬───────────────────────────────┬─────────────────────────┘
                │                               │
                ▼                               ▼
┌───────────────────────────┐     ┌───────────────────────────────────────┐
│ cannot block              │     │ may block                             │
│ SpinLock  (IRQs off)      │     │ Mutex / CriticalMutex                 │
│ ChainLock (transaction)   │     │ BrwLock  (readers + one writer)       │
│ Guard<…, IrqSave>         │     │ Event / Semaphore / WaitQueue         │
└───────────────────────────┘     └───────────────────┬───────────────────┘
                                                      │
                                                      ▼
                                      Thread READY → BLOCKED → Reschedule
                                      futex (user) → zx_futex_* → WaitQueue
```

Three questions, one primitive each:

| Need | Primitive |
| --- | --- |
| Order of memory, no lock | barrier / C++ `memory_order` |
| Mutual exclusion, cannot sleep | `SpinLock` (IRQs off) |
| Mutual exclusion, may sleep | `Mutex` / `CriticalMutex` / `BrwLock` |
| Sleep until a condition | `WaitQueue` / `Event` / futex |

## 2. Flow

Uncontended `Mutex` is an atomic; contended it spins then blocks. `SpinLock`
never blocks.

```text
      Acquire
         │
         ▼
┌─────────────────┐
│ atomic try      │  Mutex: CAS holder; SpinLock: ldaxr/stxr
└────────┬────────┘
         │ fail
         ▼
    SpinLock? ──yes──► WFE until ldaxr sees 0, then stxr   (IRQs stay off)
         │ no
         ▼
    spin ≤ 150 µs on the holder CPU
         │ still held
         ▼
┌─────────────────┐
│ WaitQueue.Block │  thread BLOCKED, Reschedule
└────────┬────────┘
         │
    holder Release ──wake one──► READY ──► Reschedule
```

## 3. Memory barriers

Three layers. Mixing them without knowing which layer you are on is how
races survive review.

#### 1. Compiler barrier

`ktl::atomic_signal_fence(order)` and GCC `"memory"` clobber: the compiler
must not move memory ops across the fence. No instruction is emitted. Used
around `TPIDR_EL1` / preemption flags in `thread.h` so the compiler cannot
cache `Thread::Current` across a reschedule.

#### 2. Hardware barriers (ARM64)

From [`zircon/kernel/lib/arch/arm64/include/lib/arch/intrin.h`](../../../zircon/kernel/lib/arch/arm64/include/lib/arch/intrin.h). Immediate is
the ACLE domain:

| Immediate | Domain | Direction |
| --- | --- | --- |
| `ARM_MB_SY` (`0xf`) | full system | all |
| `ARM_MB_ISH` (`0xb`) | inner shareable (other PEs) | all |
| `ARM_MB_ISHST` (`0xa`) | inner shareable | stores |
| `ARM_MB_ISHLD` (`0x9`) | inner shareable | loads |
| `ARM_MB_NSH` | non-shareable (this PE) | all |

| Instruction | Completes | Secular wrapper |
| --- | --- | --- |
| `DMB` | orders memory accesses; they need not have reached RAM | `arch::ThreadMemoryBarrier()` = `dmb sy`; `StoreMemoryBarrier()` = `dmb ishst` |
| `DSB` | waits until prior accesses (and TLB/cache ops) complete | `arch::DeviceMemoryBarrier()` = `dsb sy` |
| `ISB` | flushes fetched instructions; needed after `msr` of control regs | `__isb(ARM_MB_SY)` |

Rules of thumb on this kernel:

- **Order between CPUs, RAM is cacheable** → `DMB` / C++ acquire-release.
- **Must finish before the next thing** (context switch, `WFI`, `tlbi`,
  turning the MMU on) → `DSB`. [`cpu.md`](cpu.md) idle is `DSB SY` then `WFI`.
  [`memory.md`](memory.md) PTE publish is `dsb ishst` then a store-release of the parent.
- **After writing SCTLR/TCR/TTBR/VBAR** → `ISB` so the next instruction
  sees the new control state.
- **User `zx_membarrier`** → `cpp_membarrier_data_barrier` is `DMB SY`;
  instruction membarrier is `DMB` then `ISB`.

`ldaxr` / `stlr` are **implicit** barriers on that location. A
`memory_order_acquire` load is enough to read what a `release` store
published; an extra `DMB` on that path is usually redundant.

#### 3. C++ `memory_order`

`ktl::atomic` is the portable spelling. Mapping on ARM64:

| `memory_order` | Typical emit | Use |
| --- | --- | --- |
| `relaxed` | plain `ldr`/`str` | counters, `koid`, “am I held” predicates |
| `acquire` | `ldar` / `ldaxr` | lock take, “see the critical section” |
| `release` | `stlr` | lock drop, “publish the critical section” |
| `acq_rel` | both | RMW that is both take and drop |
| `seq_cst` | extra `DMB` | almost never in hot kernel paths |

Spinlock unlock is `AtomicU32::store(0, Release)` → `STLR`. Mutex holder
pointer is the same idea: publish payload with release, observers acquire.

## 4. Locks

Always take them with `Guard<LockType, Option>` (`lockdep.h`). Do not call
`Acquire`/`Release` by hand unless a comment says why. `IrqSave` saves DAIF
and disables IRQs for the scope; use it on every `SpinLock`.

#### SpinLock

[`zircon/kernel/include/kernel/spinlock.h`](../../../zircon/kernel/include/kernel/spinlock.h), ARM loop in
[`zircon/kernel/arch/arm64/src/spinlock.rs`](../../../zircon/kernel/arch/arm64/src/spinlock.rs).

- Word is `cpu_num + 1`, or `0` if free. Held-by-this-CPU is a relaxed load.
- Take: `sevl; wfe; ldaxr; cbnz → wfe; stxr` until success. IRQs **must**
  already be off (`DEBUG_ASSERT(arch_ints_disabled())`).
- Drop: `num_spinlocks--`, `store(0, Release)`.
- Never blocks. Never holds across a call that might reschedule.
- `MonitoredSpinLock` feeds `lockup_detector` (`Acquire("name")`).
- Scheduler `queue_lock_` is this: `Guard<MonitoredSpinLock, IrqSave>`.

If you take a spinlock with IRQs on, the same CPU can interrupt and try the
same lock → deadlock. `num_spinlocks` on `arm64_percpu` exists so
`arch_blocking_disallowed` can assert you do not block while holding one.

#### Mutex

[`zircon/kernel/include/kernel/mutex.h`](../../../zircon/kernel/include/kernel/mutex.h).

- Fast path: atomic CAS of `val_` from `0` to `Thread*`.
- Slow path: spin up to `SPIN_MAX_DURATION` (150 µs) while the holder is
  still on the CPU that first acquired it, then `WaitQueue` block.
- Contested bit in `val_` low bit; waiters sit on `LazyOwnedWaitQueue`
  (priority inheritance when a waiter has a higher profile).
- IRQs may stay on. The holder **may** block (unlike `SpinLock`).
- Not recursive: lock it twice on the same thread and you deadlock.

`CriticalMutex` is a `Mutex` that also sets a **timeslice extension** so the
holder is not preempted for up to that 150 µs. Use it for short, hot,
process-wide object locks (channels, dispatchers). It is not a spinlock:
contenders still block after the spin, and IRQs stay enabled.

#### ChainLock

[`zircon/kernel/lib/kconcurrent/`](../../../zircon/kernel/lib/kconcurrent/). A `ChainLock` is a spinlock you take in a
**transaction** (`ChainLockTransaction` / `CLT_TAG("…")`) so several objects
(thread + wait queue + scheduler) can be locked without a fixed global
order. Conflict → drop all, bump `chain_lock_conflict_id`, retry.

Used by: `WaitQueue` block/wake, `Mutex` contended path, `Scheduler`
reschedule (the “scheduler token”). Do not invent a new chain-lock protocol
without reading `lib/kconcurrent`. For ordinary driver/kernel data, use
`SpinLock` or `Mutex`.

#### BrwLock

[`zircon/kernel/include/kernel/brwlock.h`](../../../zircon/kernel/include/kernel/brwlock.h). Blocking reader-writer lock.
Many readers or one writer. Optional PI via `OwnedWaitQueue`. Writer-biased
enough that a writer does not starve behind an endless reader stream. Use
when the read side is the common path and may block.

## 5. Waiters

Locks decide **mutual exclusion**. These decide **sleep**.

| Type | Semantics | IRQ-safe signal? |
| --- | --- | --- |
| `WaitQueue` | unordered (really WAVL) set of blocked threads | no (needs thread locks) |
| `OwnedWaitQueue` | `WaitQueue` + PI owner | no |
| `Event` | sticky signal; `Wait` returns the status | yes, if preempt disabled |
| `AutounsignalEvent` | wakes one; auto-clear if someone was waiting | same |
| `Semaphore` | counting; `Post` release-synchronizes with `Wait` | no |

`Event::Wait` from IRQ context is illegal. `Event::Signal` from IRQ is
legal **only** with preemption already disabled ([`interrupt.md`](interrupt.md)
`int_handler_start` does that).

Blocking a thread is: set `THREAD_BLOCKED`, enqueue on the wait queue,
`Scheduler::Reschedule` ([`cpu.md`](cpu.md) STR). Wake is the reverse plus maybe an
IPI `RESCHEDULE` if the target CPU is in `WFI`.

## 6. IRQ and preemption as exclusion

Not locks, but they close the same races:

| Tool | What it excludes |
| --- | --- |
| `arch_disable_ints` / `Guard<SpinLock, IrqSave>` | this CPU's IRQ handlers |
| `AutoPreemptDisabler` | migration and local reschedule |
| `arch_blocking_disallowed` | `Wait` / `Mutex` slow path (set in IRQ and while spinning) |
| `int_handler_start` | preempt + eager resched + blocking, for the IRQ body |

A spinlock critical section is “IRQs off + lock word”. A mutex critical
section is “this thread holds `val_`; others block”. Mixing: you may take a
`SpinLock` while holding a `Mutex`; you must not take a `Mutex` while
holding a `SpinLock`.

## 7. Userspace

The console image's `zxsh` / `component_manager` do not spin in the kernel
sense. They use futexes (next section) via
`sync_mutex_t` / `sync_completion` / `sync_condition`
([`zircon/system/ulib/sync`](../../../zircon/system/ulib/sync)). Prefer
`sync_mutex_t` over libc `mutex_t`. Kernel `Mutex` is **not** a futex;
do not call `sync_mutex_lock` in kernel code.

## 8. Futex

**Principle:** the lock word lives in **user** memory. The kernel only
sleeps and wakes. Uncontended take/drop is a user atomic (`ldaxr`/`stlr`);
a syscall happens only when a thread must block or must wake a blocker.

There is no `FutexDispatcher` and no handle. The key is the user VA
(`FutexId` = detagged pointer `>> 2`, so the word must be 4-byte
aligned). The wait queue sits in the process's `FutexContext`
([`futex_context.rs`](../../../zircon/kernel/object/futex_context.rs),
on `ShareableProcessState` — shared processes share the table).

Wait and wake must be atomic with respect to each other: `futex_wait`
re-reads `*value_ptr` under the futex spinlock and compares it to
`current_value`. If they differ, it returns `ZX_ERR_BAD_STATE` and does
**not** sleep. That closes the lost-wakeup race a user mutex would
otherwise have (unlock + wake racing with “see it is locked, then wait”).

#### Why (advantage)

| vs | Futex wins because |
| --- | --- |
| Kernel `Mutex` | No kernel object per lock. A process can have thousands of `sync_mutex_t` on the stack or in structs; the kernel allocates a `FutexState` only while someone is **waiting**. |
| Always-syscall Event / Channel | Uncontended path never enters EL1. `component_manager` locks that do not collide cost one atomic, not a `svc`. |
| Spin forever | Contended path blocks (`THREAD_BLOCKED`) and lets the CPU run someone else / `WFI` (`cpu.md`). |
| One Event per condition | One word is the protocol. Userspace invents mutex, condvar, barrier, seqcount; the kernel stays generic. |
| Channel between processes | A futex in a **shared VMO** is IPC with no handle transfer and no 64 KiB messages (`ipc.md`). |

Cost: the protocol is yours. Forget the `*ptr == expected` check, or
wake without storing a new value, and you deadlock or miss a wakeup.
Owner / PI is optional and only within the same futex context (same
process, or shared aspace).

#### Framework

```text
 userspace (EL0)                         kernel (EL1)
 ┌─────────────────────────┐             ┌─────────────────────────────┐
 │ zx_futex_t word         │             │ FutexContext                │
 │  (stack / heap / VMO)   │◄── VA key ──│  hash(FutexId)              │
 │ protocol is user-defined│             │  FutexState pool            │
 │                         │             │    2 per thread (requeue)   │
 │  uncontended: atomic    │             │  active: OwnedWaitQueue     │
 │  contended: svc         │             │    optional owner Thread*   │
 └─────────────────────────┘             └──────────────┬──────────────┘
        sync_mutex_t.futex                              │
        = 0 free, else holder                           ▼
                                               WaitQueue  BLOCKED → READY
```

| Syscall | Role |
| --- | --- |
| `zx_futex_wait(ptr, expected, owner, deadline)` | if `*ptr == expected`, sleep; else `BAD_STATE` |
| `zx_futex_wake(ptr, n)` | ready up to `n` waiters |
| `zx_futex_wake_single_owner(ptr)` | wake one and make it the owner (PI) |
| `zx_futex_requeue(wake, n, expected, other, m, owner)` | wake `n` on `wake`; move `m` waiters to `other` |
| `zx_futex_get_owner(ptr, &koid)` | who the kernel thinks owns it |

`requeue` is the condvar move: waiters leave the cond futex and attach
to the mutex futex without bouncing through EL0. The pool keeps **two**
`FutexState`s per thread so two keys can be locked at once.

#### Flow

Uncontended mutex (no syscall):

```text
 lock:   ldaxr  word==0 ? stxr thread_id : contended
 unlock: stlr   0
```

Contended:

```text
 A  sees word != 0
        │  (optional: mark “has waiters” in the user word)
        ▼
    zx_futex_wait(&word, expected, owner=B, deadline)
        │  under FutexState spinlock, IRQs off:
        │    copy *word from user
        │    *word != expected  →  ZX_ERR_BAD_STATE  (do not sleep)
        │    *word == expected  →  set owner, Block on OwnedWaitQueue
        ▼
    A  BLOCKED   CPU may idle (cpu.md)

 B  unlock
        stlr new_value          (0, or next owner)
        zx_futex_wake(&word, 1)
        │  or wake_single_owner: wake one, AssignWoken
        ▼
    A  READY  (maybe IPI RESCHEDULE)
        retries the user atomic
```

Condvar (`requeue`):

```text
 waiters on futex C  (condition)
        │
 signal: zx_futex_requeue(C, wake=1, expected, M, rest, owner)
        │  1 sleeper → READY (will take mutex M)
        │  the rest  → now wait on mutex futex M
        ▼
 no thundering herd of “wake all, then all sleep on M”
```

Lost-wakeup window the kernel closes:

```text
 B unlock + wake          A “still locked, I'll wait”
        │                        │
        │     if A waits *after* B's wake, and does not
        │     re-read the word, A sleeps forever
        ▼
 wait's compare of *ptr == expected is inside the same
 lock that wake takes. Either A sees the new value and
 returns BAD_STATE, or B's wake finds A on the queue.
```

## 9. How to choose

```text
┌────────────────────────────────┬───────────────────────────────────┐
│ IRQ / must not reschedule?     │ SpinLock + IrqSave (never Mutex)  │
│ short + contended globally?    │ CriticalMutex                     │
│ readers common, may block?     │ BrwLock                           │
│ otherwise mutual exclusion     │ Mutex                             │
│ wait for a flag, not a CS?     │ Event / Semaphore / WaitQueue     │
│ userspace mutex / condvar?     │ futex (`sync_mutex_t`)            │
│ several objects at once?       │ existing ChainLock transaction    │
└────────────────────────────────┴───────────────────────────────────┘
```

On the console path the locks you will actually hit:

| Site | Lock |
| --- | --- |
| `Scheduler::queue_lock_` | `MonitoredSpinLock` + `IrqSave` |
| thread list / wait queues | `ChainLock` + `SpinLock` |
| `Dispatcher::get_lock()` | `CriticalMutex` |
| UART / GIC driver tables | `SpinLock` or none (IRQs off) |
| `zxsh` libc | `sync_mutex` → futex |

## 10. References

Read in this order.

| File | Why |
| --- | --- |
| [`zircon/kernel/lib/arch/arm64/include/lib/arch/intrin.h`](../../../zircon/kernel/lib/arch/arm64/include/lib/arch/intrin.h) | `DMB`/`DSB`/`ISB`, `ThreadMemoryBarrier` |
| [`zircon/kernel/arch/arm64/src/spinlock.rs`](../../../zircon/kernel/arch/arm64/src/spinlock.rs) | `ldaxr` / `wfe` / `Release` store |
| [`zircon/kernel/include/arch/spinlock.h`](../../../zircon/kernel/include/arch/spinlock.h) | `arch_spin_lock_t` word |
| [`zircon/kernel/include/kernel/spinlock.h`](../../../zircon/kernel/include/kernel/spinlock.h) | `SpinLock`, IRQ-off contract |
| [`zircon/kernel/include/kernel/lockdep.h`](../../../zircon/kernel/include/kernel/lockdep.h) | `Guard<…, IrqSave>` |
| [`zircon/kernel/include/kernel/mutex.h`](../../../zircon/kernel/include/kernel/mutex.h) | `Mutex`, `CriticalMutex`, 150 µs spin |
| [`zircon/kernel/include/kernel/brwlock.h`](../../../zircon/kernel/include/kernel/brwlock.h) | reader-writer |
| [`zircon/kernel/lib/kconcurrent/include/lib/kconcurrent/chainlock.h`](../../../zircon/kernel/lib/kconcurrent/include/lib/kconcurrent/chainlock.h) | `CLT_TAG` |
| [`zircon/kernel/include/kernel/wait.h`](../../../zircon/kernel/include/kernel/wait.h) | `WaitQueue` / block reasons |
| [`zircon/kernel/include/kernel/event.h`](../../../zircon/kernel/include/kernel/event.h) | signal from IRQ rules |
| [`zircon/kernel/include/kernel/semaphore.h`](../../../zircon/kernel/include/kernel/semaphore.h) | `Post` release / `Wait` acquire |
| [`zircon/kernel/include/kernel/auto_preempt_disabler.h`](../../../zircon/kernel/include/kernel/auto_preempt_disabler.h) | preempt off RAII |
| [`zircon/kernel/include/kernel/interrupt.h`](../../../zircon/kernel/include/kernel/interrupt.h) | `int_handler_start` |
| [`zircon/kernel/lib/syscalls/membarrier_ffi.cc`](../../../zircon/kernel/lib/syscalls/membarrier_ffi.cc) | user membarrier → `DMB`/`ISB` |
| [`zircon/kernel/lib/syscalls/futex.rs`](../../../zircon/kernel/lib/syscalls/futex.rs) | `sys_futex_{wait,wake,requeue}` |
| [`zircon/kernel/object/futex_context.rs`](../../../zircon/kernel/object/futex_context.rs) | `FutexId`, wait/wake atomicity, owner |
| [`zircon/system/ulib/sync/include/lib/sync/mutex.h`](../../../zircon/system/ulib/sync/include/lib/sync/mutex.h) | `sync_mutex_t` |

Suggested first pass: **`intrin.h` barriers → `spinlock.rs` acquire →
`Guard<SpinLock, IrqSave>` → `Mutex` fast CAS vs wait-queue slow path →
`futex_wait` compare-and-block**. That is enough to see why UART IRQ
code uses a spinlock, why `component_manager` blocking on a channel
uses `CriticalMutex`, and why `zxsh` libc mutexes never syscall until
they collide.

## Anti-patterns

* **`Mutex` in IRQ or with IRQs off.** Use `SpinLock` + `IrqSave`.
* **Sleep on a SpinLock.** It never blocks; you deadlock the CPU.
* **Futex wait without re-reading the word after wake.** Classic lost-wakeup.

## First-pass checklist

- [ ] IRQ? → SpinLock + IrqSave
- [ ] May block? → Mutex / CriticalMutex / WaitQueue
- [ ] Userspace? → futex, not kernel Mutex

## Related

* [`bootstrap.md`](bootstrap.md)
* [`memory.md`](memory.md)
* [`cpu.md`](cpu.md)
* [`interrupt.md`](interrupt.md)
* [`ipc.md`](ipc.md)
* [`async.md`](async.md)
* [`sync.md`](sync.md)

## 11. Terminology

| Term | Meaning in this note |
| --- | --- |
| `DMB` | Data Memory Barrier. Orders accesses; they need not have reached RAM. |
| `DSB` | Data Synchronization Barrier. Waits until prior accesses / TLBI / cache ops complete. |
| `ISB` | Instruction Synchronization Barrier. Refetch after `msr` of control regs. |
| Inner shareable | Other PEs on this QEMU virt. Secular RAM is this (`DMB ISH`). |
| `memory_order_acquire` / `release` | C++: `ldar` / `stlr`. Lock take sees the critical section; drop publishes it. |
| `SpinLock` | IRQ-off mutex on a word. `ldaxr`/`stxr` + `WFE`. Never blocks. |
| `IrqSave` | `Guard` option: save DAIF, mask IRQs for the scope. Required with `SpinLock`. |
| `Mutex` | Blocking lock. Fast CAS of `Thread*`; slow path spins 150 µs then `WaitQueue`. |
| `CriticalMutex` | `Mutex` plus a timeslice extension so the holder is not preempted for that 150 µs. Dispatcher locks. |
| `ChainLock` | Spinlock taken in a retryable transaction. Scheduler + wait queues. |
| `BrwLock` | Blocking readers-writer. |
| `WaitQueue` | Set of `BLOCKED` threads. Wake → `READY` + maybe IPI `RESCHEDULE`. |
| PI | Priority inheritance. `OwnedWaitQueue` / contested `Mutex`. |
| Futex | User `zx_futex_t` + kernel `WaitQueue` keyed by VA. No handle. Uncontended = user atomic. |
| `FutexId` | Detagged user pointer `>> 2`. Word must be 4-byte aligned. |
| `ZX_ERR_BAD_STATE` (wait) | `*ptr != expected`: do not sleep (avoids lost wakeup). |
| Owner / PI | Optional `Thread*` on a futex. `wake_single_owner` hands the lock to the woken waiter. |
| `arch_blocking_disallowed` | True in IRQ and while a spinlock is held. `Wait` / `Mutex` slow path will assert. |

