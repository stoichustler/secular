# Scheduler

Crook's CPU time is one LK scheduler. A VCPU is a **pinned host
thread** on that scheduler. Jobs and processes do not have run
queues; they group capabilities. Cross-VM policy (`VcpuGroup`) then
moves those VCPU threads' priority, slice, and RUN/PAUSE/OFFLINE
bits when a guest is busy, in IRQ, or starving.

Read [`bootstrap.md`](bootstrap.md) for the job tree at boot,
[`interrupt.md`](interrupt.md) for IRQ → IPI wakeup of a pinned
VCPU, [`memory.md`](memory.md) for aspaces.

This note is the map of that path: Job / Process / Thread objects,
how ordinary threads are picked, **thread vs VCPU context switch**,
**priority inheritance** on kernel mutexes, how a VCPU is scheduled
against them, how RT and interrupt hints raise host priority, how
lua / ioctl configure it, and how VCPUs are bound to pCPUs.

## Framework

```text
 JobDispatcher  (tree, rights, OOM importance — not a run queue)
    │
    ├─ child JobDispatcher
    └─ ProcessDispatcher
           aspace_  TYPE_USER
           │
           └─ ThreadDispatcher[]
                  thread_   LK thread_t
                  aspace attached  →  TTBR0 on context switch

 Vcpu  (also an LK thread_t, THREAD_FLAG_VCPU, affinity = one pCPU)
    │
    ▼
 per-CPU run_queue[0..31]     sched.c
    │
    ▼
 VcpuGroup[pCPU]              WITH_CROSS_VM_SCHED
    period / budget
    AddSchedHint  (IRQ / RT / overload)
    ApplyVcpuPolicy  RUN / PAUSE / SHARE / OFFLINE
```

Two clocks on one core:

| Layer | Who | What it decides |
| --- | --- | --- |
| LK `sched.c` | every `thread_t` | which thread runs **now** on this pCPU |
| `VcpuGroup` | VCPU threads on that pCPU | priority + slice this period; which VM may occupy the core |

A guest FIFO thread does not appear in `run_queue`. Crook sees it
only as guest `preempt_count` / `policy` sampled from guest RAM
(`SchedParams`), then bumps the **host** VCPU thread.

## Objects

```text
 zx::job  ──► JobDispatcher
 zx::process ──► ProcessDispatcher ──► VmAspace TYPE_USER
 zx::thread  ──► ThreadDispatcher ──► thread_t
 zx::vcpu    ──► VcpuDispatcher   ──► Vcpu ── thread_t (pinned)
```

Jobs nest. A process belongs to one job. Threads belong to one
process. Killing a job kills descendant processes; that is lifecycle,
not scheduling.

Boot tree (from [`bootstrap.md`](bootstrap.md) (11)):

```text
 kernel root job
 └─ job "root"          (PA_JOB_DEFAULT)
      ├─ process devmgr
      ├─ job zircon-services     crashlogger, fshost, …
      └─ job fuchsia
           appmgr → sysmgr → guest_manager
                each VCPU thread lives in guest_manager's process
```

`ZX_PROP_JOB_IMPORTANCE` ranks jobs for OOM kill. It does **not**
change `effec_priority`. Job policy (`ZX_POL_*`) is syscall allow /
deny (e.g. Trusty).

`ThreadDispatcher` states: INITIAL → INITIALIZED → RUNNING /
SUSPENDED → DYING → DEAD. The LK `thread_t` is created in
`Initialize` (`thread_create_etc`, `StartRoutine` →
`arch_enter_uspace`). `thread_resume` puts it on a run queue.

A VCPU thread never enters EL0. `Vcpu::Resume` is the loop:
fill `ICH_LR`, `eret` guest, handle VM-exit, repeat. Same
`thread_t` fields as a user thread: `base_priority`,
`remaining_time_slice`, `cpu_affinity`.

## Host thread scheduling

32 levels (`NUM_PRIORITIES`). Per CPU: `run_queue[prio]` + bitmap.
Highest set bit wins. Empty → idle (`IDLE_PRIORITY = 0`).

```text
effec_priority = max(base_priority + priority_boost, inherited_priority)
```

| Constant | Value |
| --- | --- |
| `LOWEST_PRIORITY` / idle | 0 |
| `LOW_PRIORITY` | 8 |
| `DEFAULT_PRIORITY` | 16 |
| `HIGH_PRIORITY` | 24 |
| `DPC_PRIORITY` | 30 |
| `HIGHEST_PRIORITY` | 31 |

Boost is ±1 (`MAX_PRIORITY_ADJ`) for fairness; RT and idle skip it.
Priority inherit: a mutex holder takes the waiter's priority
(`inherited_priority`).

Time slice: `remaining_time_slice` ticks down; expiry deboosts and
requeues at the tail. `THREAD_FLAG_REAL_TIME` cancels the preemption
timer — the thread runs until it blocks or a **higher** priority
thread appears.

CPU pick (`find_cpu_mask`):

```text
 affinity ∩ active CPUs
 prefer last_cpu if still allowed
 else a random allowed CPU
 Crook/Trusty: prefer current CPU if it is in the mask
```

Insert: local CPU → `local_resched`; remote → IPI reschedule.
`WITH_LIB_SM`: a CPU may **steal** a higher-priority thread whose
affinity includes it (`sched_find_remote_thread_above_priority`).

Ordinary user threads stay at `DEFAULT_PRIORITY` unless something
calls `thread_set_priority` / `thread_set_real_time`. Kernel DPC is
30. Idle is 0.

```text
 timer / IRQ
    │
    ▼  thread_preempt / thread_reschedule
 sched_get_top_thread(cpu)
    highest run_queue with a thread whose affinity includes cpu
    │
    ▼  vmm_context_switch if aspace differs
 final_context_switch  (EL1 host; VCPU then eret guest itself)
```

## Context switch

Two nested switches. The **thread** switch is always EL1 host
(`sched.c` → `arch_context_switch`). A **VCPU** switch is an EL2
world switch (`arm64_el2_resume` / `el2_guest_exit`) while that
same host thread stays current. The scheduler never swaps threads
while the CPU is in guest EL1: `HCR.IMO` takes IRQ to EL2 first.

```text
 EL0 user thread          guest EL1h
    │  svc / IRQ             │  HCR.IMO IRQ / trap
    ▼                        ▼
 EL1 iframe (user GPRs)   EL2 el2_guest_exit  (guest GPRs → El2State)
    │                        │
    ▼                        ▼
 sched_resched_internal   Vcpu::Resume  (still this thread_t)
    thread CS                 loop: vmexit_handler or re-enter
```

### Thread (`thread_t`)

`sched_resched_internal` (ints off, `thread_lock` held):

1. `sched_get_top_thread`. Same thread → return.
2. Account `runtime_ns` / remaining slice; refill slice if 0.
3. `curr_cpu` / idle / RT flags; arm or cancel preempt timer.
4. If `old->aspace != new->aspace`: `vmm_context_switch` →
   `TTBR0_EL1` + `TCR` ([`memory.md`](memory.md)).
5. `final_context_switch`: `set_current_thread(new)` then
   `arch_context_switch`.

`arch_context_switch` (`thread.cpp` / `asm.S`):

```text
 DSB                         // TLB ops visible if the thread migrates
 new->arch.current_percpu_ptr = this CPU
 arm64_fpu_context_switch    // lazy FP
 arm64_context_switch(&old->arch.sp, new->arch.sp)
    push x29,x30 … x19,x20
    mrs  tpidr_el0, tpidrro_el0; push
    str  sp, [old_sp]
    mov  sp, new_sp
    pop  tpidr*; msr
    pop  x19…x30
    ret                      // into new thread's saved lr
```

Callee-saved only (`x19–x30`, `tpidr_el0`, `tpidrro_el0`, FPU).
`x0–x18` are caller-saved or already in an exception iframe.
User EL0 GPRs live on that iframe (`arm64_iframe_long`), pointed
from `arch.suspended_general_regs` if the thread is stopped. First
run: `arch_thread_initialize` plants `lr = StartRoutine` on the
kernel stack.

User thread: EL0 ↔ EL1 is `svc`/`eret` ([`bootstrap.md`](bootstrap.md)
`arch_enter_uspace`). Thread CS is the extra step when the **host**
picks a different `thread_t`.

### VCPU (world switch)

The VCPU **is** a host thread. While `Vcpu::Resume` runs, that
`thread_t` is `THREAD_RUNNING`. Entering the guest is not a
`sched.c` switch.

`arm64_el2_resume(vttbr, el2_state_pa, hcr)` → `HVC` →
`el2_hvc_resume`:

```text
 msr vttbr_el2, vttbr          // stage-2 on
 msr tpidr_el2, el2_state
 first enter: tlbi vmalle1
 msr hcr_el2, hcr              // VM|PTW|IMO|FMO|…
 host_state stp                // host x18–x30
 system_state str  HS_         // host SP_EL0/1, TTBR0/1, SCTLR, TCR, …
 guest_enter_state             // CNTV_CVAL/CTL, CNTVOFF
 system_state ldr  GS_         // guest EL1 sysregs
 guest_state ldp               // guest x0–x30
 eret                          // SPSR_EL2 / ELR_EL2 → guest EL1h
```

Exit (`el2_guest_exit`, any EL2 vector except FP trap):

```text
 guest_state stp               // guest GPRs → El2State
 system_state str  GS_         // guest TTBR/SCTLR/SPSR/ELR_EL1…
 guest_exit_state              // ESR/FAR/HPFAR, CNTV*
 system_state ldr  HS_         // host sysregs back
 host_state ldp
 msr cntv_ctl_el0, 0           // stop guest vtimer on host
 msr hcr_el2, RW               // traps off
 msr vttbr_el2, xzr            // stage-2 off
 eret                          // back to arm64_el2_resume's caller (EL1)
```

`El2State` holds both worlds: `GuestState` (x0–x30 + EL1 sysregs +
vtimer + ESR/FAR/HPFAR) and `HostState` (callee-saved + host EL1
sysregs + FP). ICH LRs are written **before** enter
(`gich_maybe_interrupt`) and read **after** exit
(`gic_read_gich_state`).

`Vcpu::Resume` loop after EL1 return:

```text
 ZX_ERR_NEXT  physical IRQ while in guest → if not KILL, re-enter
 ZX_OK        trap / abort / HVC → vmexit_handler; packet or loop
 other        fail out of Resume  (thread may then block / CS)
```

### Nesting

```text
 pCPU 0
   thread A  user EL0
   IRQ → EL1 iframe(A)
   sched  →  thread CS  A → VCPU-SOS
                TTBR0: user aspace → kernel (VCPU aspace is kernel)
                save A x19–x30; restore VCPU x19–x30
   VCPU-SOS  Vcpu::Resume
      el2_hvc_resume  host_state save, guest SOS eret
      guest SOS EL1h
      SPI  HCR.IMO → EL2 exit → EL1 Resume
      AddSchedHint / or vmexit_handler
      re-enter SOS   or   thread CS to VCPU-UOS / user B
```

Switching SOS VCPU → UOS VCPU is **two** steps: SOS must `el2_guest_exit`
first (still SOS's `thread_t`), then `sched.c` swaps to UOS's
`thread_t`, then UOS `Resume` does `el2_hvc_resume`. There is no
direct guest→guest `eret`.

A host IRQ that arrives **in guest** is therefore: EL2 save guest →
EL1 host IRQ → maybe thread CS → later Resume restores **this**
VCPU's `El2State`. Guest GPRs never sit in `context_switch_frame`.

| | Thread CS | VCPU world switch |
| --- | --- | --- |
| Trigger | `thread_reschedule` / preempt / block | `Vcpu::Resume` / EL2 trap |
| EL | EL1 host | EL2, then `eret` EL1h or back EL1 |
| Saved | `x19–x30`, TPIDR, FPU, `arch.sp` | full guest GPRs + EL1 sysregs in `El2State` |
| MMU | `TTBR0` if user aspace changes | `VTTBR` on enter, 0 on exit; host TTBR1 stays |
| Timer | host preempt timer | guest `CNTV_*` parked; host physical timer enabled on exit |
| Who | any `thread_t` | only while that VCPU thread is current |

## Priority inheritance

Yes. Kernel `mutex_t` donates the waiter's `effec_priority` to the
holder (`sched_inherit_priority` in `mutex.c`). User futexes do
**not**.

Wait queues are sorted by `effec_priority` (one list head per
priority). `wait_queue_blocked_priority` is the highest waiter.

```text
 high  B  waits on mutex held by low  A
    mutex_acquire
      sched_inherit_priority(A, B.effec_priority)
      A.inherited_priority = B.effec
      A.effec = max(A.base+boost, inherited)
      if A is READY:  requeue at the new level
      if A is RUNNING and dropped:  resched
    B  wait_queue_block

 A  mutex_release
    wake highest waiter
    new holder inherits remaining waiters' max  (or -1)
    A drops inherit iff mutexes_held == 0
```

`inherited_priority` is **one slot**, not a donor tree. It only
moves **up** (`pri <= inherited` is ignored). It is cleared (`-1`)
only when the thread holds **zero** mutexes. Nested acquire still
propagates: if A (already inherited 20) blocks on C's mutex, C
inherits A's `effec` (20). That is the chain.

Limits:

- kernel mutex only (`mutex_acquire` / `mutex_release`)
- no PI on futex, condvar, or guest locks
- `Vcpu::SetPriority` calls `sched_inherit_priority(thread, -1)` —
  a VCPU hint **drops** any inherited boost
- RT/idle threads skip the ±1 fairness boost, but they **can**
  still inherit (and donate)

This is inversion avoidance for **host** kernel critical sections
(VMM workers, device threads), not a POSIX PI mutex for guest
apps.

## RT and interrupt

Two RT stories: host `THREAD_FLAG_REAL_TIME`, and guest FIFO/RR
sampled into a VCPU hint.

### Host RT

`thread_set_real_time` / `thread_set_realtime_slice`:

| Call | Effect |
| --- | --- |
| slice `-1` | `THREAD_FLAG_REAL_TIME`; cancel preempt timer |
| slice `0` | clear RT, `thread_yield` |
| slice `> 0` | one-shot slice (capped 1 s), then back to normal |

`thread_is_realtime` is `FLAG_REAL_TIME && base_priority > 16`.
Switching **to** RT/idle cancels that CPU's preempt timer; switching
**from** RT arms it. Boost/deboost skip RT. Trusty NS IRQ / idle /
stdcall threads and the CPU-unplug helper use this.

A VCPU thread is **not** permanently RT. `AddSchedHint` raises
`base_priority` and may call `SetRealTimeSlice` for a short window
(`kRTSlice` 200 µs, `kIPITimeSlice` 100 µs). After the hint mode
drops, it is a normal CFS-like thread again.

### Guest RT → host hint

Policy1 reads guest `policy` (`SCHED_FIFO` / `SCHED_RR`) from
`SchedParams` and sets `STATUS_RT_RUNNING`. `AddSchedHint` then
picks `SCHED_MODE_RT`: host prio 18, slice 200 µs. Guest CFS stays
at NORMAL (16).

Guest spinlock / `preempt_count` → `PREEMPT_DISABLE` (same prio
band, 200 µs) so the holder is not preempted mid-lock by the other
VM on a SHARE core.

### Interrupt path

```text
 device SPI  →  host GIC  →  VcpuIrqHandler
    PhysicalInterrupt  bitmap + Signal
    InterruptCpu  IPI to bind_pcpu     (if VCPU running or s2idle)
         │
         ▼  VM exit / wake
 GetSchedStatus
    gich Pending           STATUS_IRQ_PENDING
    pending SGI/PPI < 32   STATUS_IPI_HANDLING
    guest SPSR I-bit       STATUS_IRQ_DISABLED
    vtimer in ICH_LR       force SCHED_MODE_IRQ
         │
 AddSchedHint  →  SCHED_MODE_IRQ
    host prio 20   slice 100 µs
    thread_reschedule
         │
         ▼  VCPU preempts the other VM on this pCPU
    gich_maybe_interrupt  ICH_LR  eret guest
```

Optimizations that keep IRQ latency down:

| Knob | What it does |
| --- | --- |
| Pin + IPI | inject never waits for a migrate |
| Exclusive SPI bind | handler runs on the VCPU's pCPU ([`interrupt.md`](interrupt.md)) |
| `SCHED_MODE_IRQ` | 20 > NORMAL 16, short slice so it yields after EOI |
| Occupancy SHARE | IRQ hint still preempts; PAUSE/OFFLINE does not run |
| `kGichHcrUie` | LR full → maintenance exit, inject remaining |
| Overload exempt | IRQ / preempt-disable **do not** drop to prio 14 |
| `thread_preempt_disable` around hint | compute new prio atomically, then one resched |

Do **not** rely on host PI for guest IRQ: the VCPU thread is not
blocked on a mutex while the guest handles the interrupt. The hint
is the inversion workaround across VMs.

Latency recipe: exclusive `bind_pcpus` + exclusive SPI + occupancy
not SHARE + Policy1 so guest FIFO/IRQ is visible.

## VCPU scheduling

`Vcpu::Create`:

```text
 cpu_num = guest->GetBindPCpuNum(vpid)   // lua bind_pcpus, else cpu_of(vpid)
 thread  = pin_thread_to_cpu(cpu_num)    // affinity = 1<<cpu_num
 bind_cpu_num_ = cpu_num
 set_to_vcputhread(thread, vmid, pcpu)
 VcpuGroup::GetVcpuGroup(pcpu)->Add(vcpu)
```

The VCPU **is** the current thread of the VMM worker that called
`zx_vcpu_create` / `zx_vcpu_resume`. Pinning means it will not
migrate. Interrupt inject (`Vcpu::InterruptCpu`) IPIs that pCPU.

### Period / budget (`WITH_CROSS_VM_SCHED`)

One `VcpuGroup` per pCPU. All VCPUs bound to that pCPU sit on
`vcpu_list_`. A period timer (`kDefaultPeriod = 5 ms`, lua
`setPeriods`) refills each VCPU's budget (`kBudgetMin = 500 µs`).

On `Vcpu::Resume`, before `eret`:

```text
 SchedResumePeriodTimer
 SchedVcpuResumePre     // Policy0 = nop, Policy1/2 = AddSchedHint*
 if need_sched_:  thread_reschedule / yield
 gich_maybe_interrupt
 eret guest
```

`AddSchedHint` reads guest status (running, IRQ, preempt-disable,
FIFO/RR) and maps it to a **host** mode:

| Mode | Typical cause | Host priority | Slice |
| --- | --- | --- | --- |
| `OVERLOAD` | period runtime > budget, no spare | 14 (`kLowPriority`) | budget |
| `NORMAL` | default | 16 | remaining budget |
| `RT` | guest `SCHED_FIFO`/`RR` | 18 (`kLooperPriority`) | 200 µs |
| `PREEMPT_DISABLE` | guest `preempt_count` | 18 (`kSpinLockPriority`) | 200 µs |
| `IRQ` | guest in IRQ / pending / vtimer LR | 20 (`kIRQPriority`) | 100 µs |
| `VM_CALL` | hypercall path | 24 (`kVMCallPriority`) | — |

`SetPriority` / `SetTimeSlice` write the VCPU's `thread_t`. Then
`SCHED_HINT_RESULT_RESCHEDULE` so a higher VCPU on this pCPU can
preempt immediately.

Policy1 also peeks guest `SchedParams` (SOS/UOS export
`preempt_count` and `policy` via shared DRAM). Policy2 is the same
budget math without that peek.

Overload with leftover group budget (`ex_remain`) can still run at
normal priority. Overload with **no** leftover drops the thread so
the other VM on that core can catch up.

### Static / dynamic occupancy

`ApplyVcpuPolicy` (shared `sched_control`, min interval 8 ms) sets
per-`(vmid, vcpu_id)` occupancy. Indices: SOS=1, Android UOS=2,
tbox=3.

| `kVcpuPolicy` | Meaning |
| --- | --- |
| `RUN` | this VM's VCPU may occupy that pCPU |
| `PAUSE` | VCPU thread suspended |
| `OFFLINE` | treated as not in the VM |
| `SHARE` | counts as half a pCPU in util math |

Named layouts (`sched.h`):

| Policy | Shape (name is SOS_UOS_TBOX-ish) |
| --- | --- |
| `NONE` | everyone RUN |
| `3_5_0` | SOS 3 cores, UOS 5, tbox 0 |
| `5_6_0` / `5_5_0` / `5_5_2` | static masks in `vcpu_group.cpp` |
| `8_8_DYNAMIC` | steal a pCPU toward the loaded VM (`DoPolicy_8_8_Dynamic`) |

Dynamic: compare SOS vs UOS util vs 40% / 75% watermarks;
`FindLowUtilVCPU` / `FindFitUtilVCPU` pick a core (masks `0x9d`,
big-core preference `0x70` / `0xf`). `CalcBigCorePolicy` pins the
big core. High load on one VM **pauses** the other's VCPU on that
pCPU so the busy guest gets a whole core.

Urgent IRQ still wins on a shared core via `AddSchedHint` even when
the occupancy policy is SHARE.

## Configuration

Lua / proto (consumed in `vmm.cc`):

```text
setBindcpus(pcpu)            // one per VCPU, length == num_cpus
setPeriods(ns)               // VcpuGroup period, one per group/pCPU
setBudgets(ns)               // per-VCPU budget
setSchedPriority(prio)       // host priority table by mode
setSchedTimeslice(ns)        // slice table: RT, spinlock, IRQ, …
setVcpuPriority(prio)
setSchedPolicy(policy)       // VcpuGroup SCHED_POLICY_DEFAULT/1/2
setSchedIrq(spi)
setSchedId(id)
```

Then `SYS_PLATFORM_SET_SCHED_PARAM` ioctls:

| `sched_param_req.type` | Effect |
| --- | --- |
| `PERIOD` | `VcpuGroup::SetPeriod` |
| `BUDGET` | `Vcpu::SetBudget` |
| `PRIORITY` / `TIMESLICE` | per guest-status host prio/slice |
| `VCPU_PRIORITY` | `SetPriority` on that handle |
| `POLICY` | `VcpuGroup::SetSchedPolicy` → Policy0/1/2 resume hook |
| `DRAM_INFO` / `VM_MEM_INFO` / `VM_*_OFF` | `SchedParams` so Policy1 can read guest `preempt_count` |

Runtime occupancy (`NONE`, `3_5_0`, `8_8_DYNAMIC`, …) lives in
`sched_control` shared memory, not lua. Userspace writes `ctl->policy`;
`ApplyVcpuPolicy` on the next Resume (if 8 ms elapsed).

`setSchedPolicy` (0/1/2) is the **hint algorithm**. Occupancy policy
is the **who may run on this pCPU** table. Both apply.

## VCPU bind (pCPU)

```text
 lua bind_pcpus = [p0, p1, p2, p3]     length == guest num_cpus
 zx_guest_set_cpu_affinity
   Guest::cpu_binding_table_[vpid] = pcpu
 Vcpu::Create
   GetBindPCpuNum(vpid) or cpu_of(vpid)
   pin_thread_to_cpu(pcpu)             affinity mask = that bit only
   VcpuGroup::GetVcpuGroup(pcpu)->Add
 machina::Vcpu::SetGroup(pcpu)         userspace side of the same pin
```

If `bind_pcpus` is omitted, `hypervisor::cpu_of(vpid)` (usually
`vpid % ncpu`) is the default pin. UOS `main_uos.cc` may
`zx_poweron_phy_cpu` before bind.

Pin is **hard**: `check_pinned_cpu_invariant` on Resume. Inject and
`DumpState` IPI that pCPU. SPI exclusive bind
([`interrupt.md`](interrupt.md)) should use the **same** pCPU as the
target VCPU, or the IRQ handler runs on the wrong core and then IPIs.

Typical SoC picture (8 cores, policy `3_5_0`):

```text
 pCPU   0  1  2  3  4  5  6  7
 SOS    R  R  .  .  R  .  .  .
 UOS    R  R  R  R  R  .  .  .
 tbox   .  .  .  .  .  .  .  .
```

`R` = RUN. Empty = PAUSE/OFFLINE. Shared cores (e.g. pCPU 0) run
SOS and UOS VCPU threads back-to-back; IRQ/RT hints decide who
preempts. Exclusive cores skip that fight.

Latency-sensitive passthrough: exclusive VCPU bind + exclusive SPI
bind + that pCPU not SHARE. Same advice as interrupt.md.

## Flows

### Ordinary thread

```text
 thread_resume(t)
   find_cpu_and_insert  (affinity)
   IPI if remote
 timer tick / block
   deboost if quantum gone
   sched_get_top_thread
   context switch  (TTBR0 if user)
```

### Priority inheritance

```text
 B (prio 24)  mutex_acquire  held by A (prio 16)
   A.inherited = 24, requeue / keep running
 A  mutex_release
   wake B; A.inherited = -1 if last mutex
```

### VCPU vs host thread on one pCPU

```text
 VCPU thread prio 16, slice leftover budget
 guest takes IRQ  →  VM exit  →  AddSchedHint IRQ  →  prio 20, 100 µs
 host DPC (30) still preempts it
 guest FIFO     →  RT mode, prio 18
 budget blown   →  OVERLOAD prio 14  →  other VM's VCPU (16) runs
```

### Thread CS vs VCPU enter/exit

```text
 sched  old user → VCPU thread
   vmm_context_switch TTBR0
   arm64_context_switch  (x19–x30, sp)
 Vcpu::Resume
   HVC  el2_hvc_resume  VTTBR on, eret guest
 guest EL1h
   trap  el2_guest_exit  El2State, VTTBR=0, eret EL1
   vmexit_handler  or  thread CS to another VCPU
```

### Occupancy change

```text
 userspace  sched_control.policy = 8_8_DYNAMIC
 Vcpu::Resume  ApplyVcpuPolicy  (if > 8 ms)
   util SOS 80%, UOS 30%
   FindLowUtilVCPU(UOS) → pause UOS vcpu on that pCPU
   SOS VCPU thread gets the core to itself
```

## Terminology

| Term | Meaning here |
| --- | --- |
| Job | Capability tree + OOM rank. Not a scheduler entity. |
| `thread_t` | LK schedulable. User thread **and** VCPU thread. |
| `effec_priority` | `max(base+boost, inherited)`. Run-queue key. |
| Quantum / slice | Time until deboost + tail requeue. RT skips the timer. |
| Affinity | CPU mask. VCPU mask is a single bit (pin). |
| `VcpuGroup` | Per-pCPU list of VCPUs sharing that core. |
| Budget | CPU time a VCPU may consume per group period. |
| Occupancy policy | RUN / PAUSE / SHARE / OFFLINE per `(VM, vcpu_id)`. |
| Hint policy | Policy0/1/2: how Resume maps guest status → host prio. |
| `bind_pcpus` | Lua array: VCPU index → pCPU. |
| PI | Kernel mutex donates waiter `effec` to holder. Not futex. |
| `inherited_priority` | One slot. `-1` = off. Cleared at `mutexes_held == 0`. |
| Host RT | `THREAD_FLAG_REAL_TIME`: no quantum timer, no ±1 boost. |
| Guest RT hint | Guest FIFO/RR → `SCHED_MODE_RT` on the VCPU thread. |
| Thread CS | EL1: callee-saved + TTBR0. `arch_context_switch`. |
| VCPU world switch | EL2: `El2State` GPRs/sysregs, `VTTBR` on/off, `eret`. |
| `El2State` | Guest + host register file for one VCPU. |

## Code

| Step | File |
| --- | --- |
| Job / process / thread | `zircon/kernel/object/include/object/{job,process,thread}_dispatcher.h` |
| LK thread + priority | `zircon/kernel/include/kernel/thread.h` |
| Run queues / inherit / CS | `zircon/kernel/kernel/sched.c` `sched_resched_internal` |
| Thread CS frame | `zircon/kernel/arch/arm64/thread.cpp`, `asm.S` `arm64_context_switch` |
| VCPU enter/exit | `zircon/kernel/arch/arm64/hypervisor/el2.S` `el2_hvc_resume` / `el2_guest_exit` |
| `Vcpu::Resume` | `zircon/kernel/arch/arm64/hypervisor/vcpu.cpp` |
| Mutex PI | `zircon/kernel/kernel/mutex.c` |
| Wait queue (prio sorted) | `zircon/kernel/kernel/wait.c` |
| Host RT flag / slice | `zircon/kernel/kernel/thread.c` `thread_set_real_time` |
| Pin VCPU thread | `zircon/kernel/lib/hypervisor/cpu.cpp` `pin_thread_to_cpu` |
| `Vcpu::Create` / `AddSchedHint` | `zircon/kernel/arch/arm64/hypervisor/vcpu.cpp` |
| `VcpuGroup` / occupancy | `zircon/third_party/lib/thyper/vcpu_group.cpp` |
| Policy enums | `zircon/kernel/include/kernel/sched.h` |
| Bind table | `zircon/kernel/arch/arm64/hypervisor/guest.cpp` `SetCpuAffinity` |
| Lua knobs | `garnet/bin/guest/guest_config.h` |
| Apply lua → ioctl | `garnet/bin/guest/vmm.cc` |

---

Everyday's a Hustle.
