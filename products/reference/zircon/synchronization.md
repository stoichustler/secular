# Synchronization

Secular boots a Zircon kernel on `qemu-system-aarch64` (virt, arm64). The
kernel runs at EL1 on several PEs that share DRAM through the physmap
(`memory.md`). Those PEs reorder loads and stores, and the compiler reorders
C++. This note is the map of that ordering path: hardware barriers, C++
atomics, the locks built on them, and the wait queues that block instead of
spin.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read `cpu.md` for `x20` / idle / `WFI`, `memory.md` for shareability and
`tlbi`, `interrupt.md` for DAIF. This note assumes inner-shareable RAM
(MAIR index 2) and that IRQs can preempt a thread that is not in a spinlock.

## Synchronization

### Framework

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

### Flow

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

### Memory barriers

Three layers. Mixing them without knowing which layer you are on is how
races survive review.

#### 1. Compiler barrier

`ktl::atomic_signal_fence(order)` and GCC `"memory"` clobber: the compiler
must not move memory ops across the fence. No instruction is emitted. Used
around `TPIDR_EL1` / preemption flags in `thread.h` so the compiler cannot
cache `Thread::Current` across a reschedule.

#### 2. Hardware barriers (ARM64)

From `zircon/kernel/lib/arch/arm64/include/lib/arch/intrin.h`. Immediate is
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
  turning the MMU on) → `DSB`. `cpu.md` idle is `DSB SY` then `WFI`.
  `memory.md` PTE publish is `dsb ishst` then a store-release of the parent.
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

### Locks

Always take them with `Guard<LockType, Option>` (`lockdep.h`). Do not call
`Acquire`/`Release` by hand unless a comment says why. `IrqSave` saves DAIF
and disables IRQs for the scope; use it on every `SpinLock`.

#### SpinLock

`zircon/kernel/include/kernel/spinlock.h`, ARM loop in
`zircon/kernel/arch/arm64/src/spinlock.rs`.

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

`zircon/kernel/include/kernel/mutex.h`.

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

`zircon/kernel/lib/kconcurrent/`. A `ChainLock` is a spinlock you take in a
**transaction** (`ChainLockTransaction` / `CLT_TAG("…")`) so several objects
(thread + wait queue + scheduler) can be locked without a fixed global
order. Conflict → drop all, bump `chain_lock_conflict_id`, retry.

Used by: `WaitQueue` block/wake, `Mutex` contended path, `Scheduler`
reschedule (the “scheduler token”). Do not invent a new chain-lock protocol
without reading `lib/kconcurrent`. For ordinary driver/kernel data, use
`SpinLock` or `Mutex`.

#### BrwLock

`zircon/kernel/include/kernel/brwlock.h`. Blocking reader-writer lock.
Many readers or one writer. Optional PI via `OwnedWaitQueue`. Writer-biased
enough that a writer does not starve behind an endless reader stream. Use
when the read side is the common path and may block.

### Waiters

Locks decide **mutual exclusion**. These decide **sleep**.

| Type | Semantics | IRQ-safe signal? |
| --- | --- | --- |
| `WaitQueue` | unordered (really WAVL) set of blocked threads | no (needs thread locks) |
| `OwnedWaitQueue` | `WaitQueue` + PI owner | no |
| `Event` | sticky signal; `Wait` returns the status | yes, if preempt disabled |
| `AutounsignalEvent` | wakes one; auto-clear if someone was waiting | same |
| `Semaphore` | counting; `Post` release-synchronizes with `Wait` | no |

`Event::Wait` from IRQ context is illegal. `Event::Signal` from IRQ is
legal **only** with preemption already disabled (`interrupt.md`
`int_handler_start` does that).

Blocking a thread is: set `THREAD_BLOCKED`, enqueue on the wait queue,
`Scheduler::Reschedule` (`cpu.md` STR). Wake is the reverse plus maybe an
IPI `RESCHEDULE` if the target CPU is in `WFI`.

### IRQ and preemption as exclusion

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

### Userspace

The console image's `zxsh` / `component_manager` do not spin in the kernel
sense. They use:

- `zx_futex_wait` / `wake` / `requeue` (`zircon/kernel/lib/syscalls/futex.rs`)
  — kernel wait queue keyed by a user word.
- `sync_mutex_t` (`zircon/system/ulib/sync`) — futex-backed, non-recursive,
  `zx_instant_mono_t` timeouts. Prefer this over libc `mutex_t`.
- `sync_completion` / `sync_condition` — same library.

Kernel `Mutex` is **not** a futex; it is a `Thread*` + `WaitQueue`. Do not
call `sync_mutex_lock` in kernel code.

### How to choose

```text
IRQ context or must not reschedule?
  yes → SpinLock + IrqSave          (never Mutex)
  no  →
       short + contended globally?  → CriticalMutex
       readers common, may block?   → BrwLock
       otherwise                    → Mutex
Need to wait for a flag, not a critical section?
  → Event / Semaphore / WaitQueue
Need several objects at once (sched + thread)?
  → existing ChainLock transaction, do not roll your own
```

On the console path the locks you will actually hit:

| Site | Lock |
| --- | --- |
| `Scheduler::queue_lock_` | `MonitoredSpinLock` + `IrqSave` |
| thread list / wait queues | `ChainLock` + `SpinLock` |
| `Dispatcher::get_lock()` | `CriticalMutex` |
| UART / GIC driver tables | `SpinLock` or none (IRQs off) |
| `zxsh` libc | `sync_mutex` → futex |

### Where in the tree

Read in this order.

| File | Why |
| --- | --- |
| `zircon/kernel/lib/arch/arm64/include/lib/arch/intrin.h` | `DMB`/`DSB`/`ISB`, `ThreadMemoryBarrier` |
| `zircon/kernel/arch/arm64/src/spinlock.rs` | `ldaxr` / `wfe` / `Release` store |
| `zircon/kernel/include/arch/spinlock.h` | `arch_spin_lock_t` word |
| `zircon/kernel/include/kernel/spinlock.h` | `SpinLock`, IRQ-off contract |
| `zircon/kernel/include/kernel/lockdep.h` | `Guard<…, IrqSave>` |
| `zircon/kernel/include/kernel/mutex.h` | `Mutex`, `CriticalMutex`, 150 µs spin |
| `zircon/kernel/include/kernel/brwlock.h` | reader-writer |
| `zircon/kernel/lib/kconcurrent/include/lib/kconcurrent/chainlock.h` | `CLT_TAG` |
| `zircon/kernel/include/kernel/wait.h` | `WaitQueue` / block reasons |
| `zircon/kernel/include/kernel/event.h` | signal from IRQ rules |
| `zircon/kernel/include/kernel/semaphore.h` | `Post` release / `Wait` acquire |
| `zircon/kernel/include/kernel/auto_preempt_disabler.h` | preempt off RAII |
| `zircon/kernel/include/kernel/interrupt.h` | `int_handler_start` |
| `zircon/kernel/lib/syscalls/membarrier_ffi.cc` | user membarrier → `DMB`/`ISB` |
| `zircon/kernel/lib/syscalls/futex.rs` | user wait |
| `zircon/system/ulib/sync/include/lib/sync/mutex.h` | `sync_mutex_t` |

Suggested first pass: **`intrin.h` barriers → `spinlock.rs` acquire →
`Guard<SpinLock, IrqSave>` → `Mutex` fast CAS vs wait-queue slow path**.
That is enough to see why UART IRQ code uses a spinlock, and why
`component_manager` blocking on a channel uses `CriticalMutex`.

---

Everyday's a Hustle.
