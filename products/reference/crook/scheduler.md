# Scheduler

Crook's CPU time is one LK scheduler. A VCPU is a **pinned host
thread** on that scheduler. Jobs and processes do not have run
queues; they group capabilities. Cross-VM policy (`VcpuGroup`) then
moves those VCPU threads' priority, slice, and RUN/PAUSE/SHARE/OFFLINE
bits when a guest is busy, in IRQ, or starving.

Occupancy bits live in shared DRAM. The host never suspends a
VCPU thread. Guest **`nebula_sched`** parks the Linux CPU when
the doorbell fires. Policy1 also needs that module: it shows
Crook where guest `preempt_count` / `policy` live, so a FIFO
or IRQ can raise the **host** VCPU.

**VCPU scheduling is the product path.** Two VMs share cores. The
host never sees guest tasks — only a VCPU thread whose priority and
slice change at every `Resume`. Burst IRQ, guest FIFO, spinlock, and
budget overrun are not extra schedulers; they are **mode switches**
on that thread.

Read [`bootstrap.md`](bootstrap.md) for the job tree at boot,
[`interrupt.md`](interrupt.md) for IRQ → IPI wakeup of a pinned
VCPU, [`memory.md`](memory.md) for aspaces.

This note is the map of that path: Job / Process / Thread objects,
how ordinary threads are picked, **thread vs VCPU context switch**,
**priority inheritance** on kernel mutexes, how a VCPU is scheduled
against them, how RT and interrupt hints raise host priority, how
lua / ioctl configure it, how VCPUs are bound to pCPUs, how
**strategy switches** under burst IRQ and load, and how the guest
**`nebula_sched`** module parks CPUs and feeds Policy1.

Start with **Diagrams** (eight posters + nebula_sched), then the
sections they index.

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
         │  shm + doorbell (sched_irq)
         ▼
 guest nebula_sched           SOS=1 / UOS=2 / tbox=3
    park / unpark this VM's CPUs
    show Crook where guest current lives
    publish "FIFO on this pCPU"
```

Three planes on one core:

| Layer | Who | What it decides |
| --- | --- | --- |
| LK `sched.c` | every `thread_t` | which thread runs **now** on this pCPU |
| `VcpuGroup` | VCPU threads on that pCPU | priority + slice this period; which VM may occupy the core |
| guest `nebula_sched` | this VM's Linux CPUs | park / unpark; keep the other VM's FIFO off this core |

A guest FIFO thread does not appear in `run_queue`. Crook sees it
only as guest `preempt_count` / `policy` sampled from guest RAM
(`SchedParams`), then bumps the **host** VCPU thread. Occupancy
is slower (≥ 8 ms) and needs the guest module to park the CPU.

Figures below are the visual map. Detail follows in **VCPU
scheduling**, **Strategy switch**, and **nebula_sched**.

## Diagrams

Eight posters for the host path, then **nebula_sched** posters for
the guest actuator. Read them top to bottom the first time; later
use them as the index for the sections that follow.

### 1. Two layers on one pCPU

LK picks a `thread_t`. `VcpuGroup` only writes that thread's
priority and slice. Guest tasks never enter the run queue.

```text
                    pCPU N  (VCPU threads are pinned here)
 ┌──────────────────────────────────────────────────────────────────┐
 │  Layer 2   VcpuGroup[N]     period 5 ms                          │
 │            vcpu_list_:  SOS.vcpu_k   UOS.vcpu_k   (tbox …)       │
 │            AddSchedHint  guest status ──► host prio / slice      │
 │            ApplyVcpuPolicy  RUN / PAUSE / SHARE / OFFLINE        │
 └───────────────┬──────────────────────────────┬───────────────────┘
                 │ SetPriority / SetTimeSlice   │ occupancy shm
                 ▼                              ▼
 ┌──────────────────────────────────────────────────────────────────┐
 │  Layer 1   sched.c  run_queue[0..31]  highest bit wins           │
 │                                                                  │
 │   31  DPC (host, still beats every VCPU)                         │
 │   24  VM_CALL          ■  VMM trap on this VCPU thread           │
 │   20  IRQ  100 µs      ■  READY + pending IRQ steals the core    │
 │   18  RT / PREEMPT     ■  guest FIFO or preempt_count            │
 │   16  NORMAL           ■■ SOS and UOS default                    │
 │   14  OVERLOAD         ■  budget blown, no leftover              │
 │    0  idle                                                       │
 │                                                                  │
 │            sched_get_top_thread  →  current thread_t             │
 └──────────────────────────────────────────────────────────────────┘

  guest Linux  CFS / FIFO / handle_irq / spinlock
       │  only visible as SchedParams + GICH bitmap
       ▼
  host VCPU thread_t.base_priority / remaining_time_slice
```

### 2. Two nested switches (no guest→guest `eret`)

```text
                 EL0 user thread              guest EL1h (SOS or UOS)
                      │  svc / IRQ                   │  HCR.IMO / trap
                      ▼                              ▼
                 EL1 iframe                     EL2 el2_guest_exit
                      │                         guest GPRs → El2State
                      ▼                              │
              sched_resched_internal                 ▼
                 THREAD context switch          Vcpu::Resume
                 x19–x30, TTBR0                 still THIS thread_t
                      │                              │
                      ▼                              ▼
                 another thread_t               eret same guest
                                                or CS to other VCPU
                                                then THAT VCPU eret

  SOS guest ──exit── SOS thread ──CS── UOS thread ──enter── UOS guest
                    (EL2)              (EL1)              (EL2)
```

### 3. Hint mode ladder

Numeric order = preemption rank. Slice is how long that rank lasts.

```text
  prio
   24 ████████████████████████  VM_CALL     packet to VMM (GICD/VHM)
   20 ████████████████          IRQ         100 µs   pending / in-IRQ / vtimer
   18 ████████████████          RT          200 µs   guest FIFO/RR
   18 ████████████████          PREEMPT     200 µs   guest preempt_count
   16 ████████                  NORMAL               leftover budget
   14 ██████                    OVERLOAD             runtime > budget, no spare
    0 ░░                        idle

  IRQ wins over PREEMPT wins over RT wins over NORMAL.
  IRQ / PREEMPT  do not drop to OVERLOAD.
  Host DPC 30 still preempts every VCPU.
```

### 4. `Vcpu::Resume` is the scheduling point

Every strategy switch is **before** `eret`.

```text
  zx_vcpu_resume  (this thread is the VCPU)
        │
        ▼
  ┌─ loop ─────────────────────────────────────────────────────────┐
  │  1  period timer arm (5 ms)                                    │
  │  2  SchedVcpuResumePre     GetSchedStatus + AddSchedHint       │
  │  3  ApplyVcpuPolicy        occupancy, ≥ 8 ms                   │
  │  4  need_sched_ ?  thread_reschedule / yield  ──► maybe CS     │
  │  5  gich_maybe_interrupt   bitmap → ICH_LR  (UIE if leftover)  │
  │  6  arm64_el2_resume       VTTBR on, eret guest EL1h           │
  │                      ┌──── guest runs ────┐                    │
  │                      │  SPI  HCR.IMO      │                    │
  │                      │  trap / HVC / WFI  │                    │
  │                      └─────────┬──────────┘                    │
  │  7  el2_guest_exit     VTTBR=0, El2State                       │
  │  8  need_sched_ = true                                         │
  │     physical IRQ  →  loop (ZX_OK)                              │
  │     handled trap  →  loop                                      │
  │     packet        →  SetVMCallMode prio 24, return EL0 VMM     │
  └────────────────────────────────────────────────────────────────┘
```

### 5. Shared core, one period (no IRQ)

pCPU 0 SHARE. Period 5 ms. Each VCPU budget 2 ms.

```text
  time  0          2 ms        4 ms        5 ms
        |-----------|-----------|-----------|
  SOS   [ NORMAL 16 ████████ ][ OVERLOAD 14 ]
  UOS   [  READY             ][ NORMAL 16 ████████████ ]
        ^ eret SOS            ^ budget blown, CS
                              ^ UOS wins at 16 > 14
        period_tick ─────────────────────────^ ResetPeriodRuntime
```

Exclusive cores (policy `3_5_0` pCPU 1, 3, 4, …) skip this fight:
only one VM's VCPU is RUN.

### 6. Burst IRQ steal (the other VM)

UOS is running. SPI belongs to SOS. SHARE core.

```text
  time →
  UOS   [======== guest EL1h ========] READY  [==== NORMAL 16 ====]
  SOS    READY                        [ IRQ 20 ████ 100 µs ] NORMAL
  SPI ────────────────────────────────●
                                      │
                                      ▼
                    VcpuIrqHandler → SOS bitmap + IPI
                    UOS HCR.IMO → EL2 exit (UOS still current thread)
                    Policy1 walk group:
                      UOS  RUNNING  → hysteresis, no extra boost
                      SOS  READY + IRQ_PENDING → raise 20 / 100 µs
                    thread_reschedule  CS UOS → SOS
                    SOS gich_maybe_interrupt  eret  guest EOI
                    slice ends / no IRQ bits → drop 16, UOS runs

  hysteresis:
    READY  + IRQ  →  steal (raise to 20)
    RUNNING + more IRQs  →  already has the core, do not re-boost
```

Same-VM storm: RUNNING + IRQ stays; fill up to 16 `ICH_LR`, leftover
`UIE`. No second steal.

### 7. Occupancy `3_5_0` (who may sit on the core)

`R` = RUN, `.` = PAUSE/OFFLINE. Only pCPU 0 is shared.

```text
            little              mid / big
         ┌─────────────────┬──────────────────┐
   pCPU  │ 0    1    2    3│  4    5    6    7│
         ├─────────────────┼──────────────────┤
   SOS   │ R    R    .    .│  R    .    .    .│   3 cores
   UOS   │ R    .    .    R│  .    R    R    R│   5 cores
   tbox  │ .    .    .    .│  .    .    .    .│
         └─────────────────┴──────────────────┘
              ▲
              SHARE fight: hint prio decides
              all other R are exclusive

  8_8_DYNAMIC (slow, ≥ 8 ms): compare SOS/UOS util to 40% / 75%,
  PAUSE the idle VM on one pCPU, inject sched_irq; guest parks
  that vCPU. Not a host thread_suspend.
```

### 8. Who writes the `thread_t`

```text
  guest RAM                    GICH bitmap              occupancy shm
  policy / preempt_count       IRQ pending              RUN/PAUSE/SHARE
         │                            │                        │
         └──────────┬─────────────────┘                        │
                    ▼                                          │
           GetSchedStatus                                      │
                    │                                          │
                    ▼                                          │
           AddSchedHint  ── hysteresis ── SetPriority/Slice    │
                    │                                          │
                    ▼                                          ▼
           thread_t on run_queue[prio]              guest parks vCPU
                    │                                (nebula_sched)
                    ▼
           sched_get_top_thread  →  eret that VCPU
```

Guest actuator posters live in **nebula_sched**.

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

A VCPU thread never enters EL0 while the guest runs. `Vcpu::Resume`
is the loop: fill `ICH_LR`, `eret` guest, handle VM-exit, repeat.
Same `thread_t` fields as a user thread: `base_priority`,
`remaining_time_slice`, `cpu_affinity`. When Resume returns a
packet, **that same thread** is back in the VMM at EL0 (GICD / VHM
trap). `SetVMCallMode` raises it so the trap does not sit behind
another VM.

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
thread appears. `THREAD_MIN_TIME_SLICE` is 50 µs.

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
A VCPU mask is one bit, so it never migrates.

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
 ZX_OK        trap / abort / HVC → vmexit_handler
              ZX_OK     handled in kernel, loop
              ZX_ERR_NEXT  packet for VMM: SetVMCallMode, return to EL0
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
apps. Do **not** rely on it for guest IRQ: the VCPU thread is not
blocked on a mutex while the guest handles the interrupt. The hint
is the inversion workaround across VMs.

## VCPU scheduling

The VCPU **is** the current thread of the VMM worker that called
`zx_vcpu_create` / `zx_vcpu_resume`. Pinning means it will not
migrate. Interrupt inject (`Vcpu::InterruptCpu`) IPIs that pCPU.

`Vcpu::Create`:

```text
 cpu_num = guest->GetBindPCpuNum(vpid)   // lua bind_pcpus, else cpu_of(vpid)
 thread  = pin_thread_to_cpu(cpu_num)    // affinity = 1<<cpu_num
 bind_cpu_num_ = cpu_num
 set_to_vcputhread(thread, vmid, pcpu)
 VcpuGroup::GetVcpuGroup(pcpu)->Add(vcpu)
```

One `VcpuGroup` per pCPU. All VCPUs bound to that pCPU sit on
`vcpu_list_`. Shared cores (occupancy SHARE, or two RUN VCPUs on
the same pCPU) are where hint policy actually fights.

Two knobs, applied at different rates:

| Knob | Who | Rate | What |
| --- | --- | --- | --- |
| Hint policy 0/1/2 | `setSchedPolicy` | every VM-exit | host prio + slice from guest status |
| Occupancy | `sched_control.policy` | ≥ 8 ms | RUN / PAUSE / SHARE / OFFLINE per `(VM, vcpu_id)` |

### Resume loop (the scheduling point)

Every strategy switch happens here, **before** `eret`. Policy0 is
a nop (`OnVcpuResumePreDefault`). Policy1/2 only run when the
previous exit set `need_sched_`.

```text
 zx_vcpu_resume
    check_pinned_cpu_invariant
    SchedResumePeriodTimer          // Policy1/2: arm 5 ms oneshot
    do {
        timer_maybe_interrupt
        SchedVcpuResumePre          // Policy1/2: AddSchedHint*
        ApplyVcpuPolicy             // occupancy, min 8 ms
        if need_sched_: continue    // resched already ran; retry
        ints off
        gich_maybe_interrupt        // Pop bitmap → ICH_LR
        ICH_HCR.En [| UIE]          // UIE if leftover pending
        running_ = true
        arm64_el2_resume            // HVC → eret guest
        running_ = false
        gic_read_gich_state
        TrackAllListRegisters
        need_sched_ = true          // Policy1/2
        ints on
        ZX_ERR_NEXT → if not KILL, status = ZX_OK   // physical IRQ
        ZX_OK       → vmexit_handler
    } while status == ZX_OK
    ZX_ERR_NEXT (packet) → SetVMCallMode (prio 24), return to VMM
```

`period_tick` (5 ms, lua `setPeriods`) calls `RefillPeriod`:
`ResetPeriodRuntime` on every VCPU in the group. Policy2 also
restores `GetSchedPriority()` so a leftover IRQ boost does not
leak across the period.

### Period / budget

Defaults: `kDefaultPeriod = 5 ms`, `kBudgetMin = 500 µs`. Lua
`setBudgets` is per VCPU. Group leftover:

```text
 budget_total  = Σ budget of VCPUs on this pCPU
 ex_runtime    = Σ max(runtime − budget, 0)
 ex_remain     = max(period − budget_total, 0) − ex_runtime
 overload      = (period_runtime + 50 µs) > budget
```

Overload with leftover (`ex_remain ≥ 50 µs`) still runs at the
current mode's priority — the core had spare. Overload with **no**
leftover drops the thread to 14 so the other VM on that core can
catch up, **unless** the mode is IRQ / PREEMPT_DISABLE (see
**Strategy switch**).

### Guest status → host mode

`GetSchedStatus` samples the VCPU, not the guest task list:

| Bit | Source |
| --- | --- |
| `STATUS_IRQ_PENDING` | GICH bitmap non-empty |
| `STATUS_IPI_HANDLING` | pending vector < 32 (SGI/PPI) |
| `STATUS_IRQ_DISABLED` | guest `SPSR` I/F set (not WFI/WFE exit) |
| `STATUS_IN_IRQ` | Policy1: guest `preempt_count` HARDIRQ/SOFTIRQ/NMI |
| `STATUS_PREEMPT_DISABLED` | Policy1: guest `preempt_count` PREEMPT_MASK |
| `STATUS_RT_RUNNING` | Policy1: guest `policy` FIFO/RR |
| `STATUS_RUNNING` / `READY` | host `thread_t` state |

Policy1 peeks guest RAM via `SchedParams` (SOS/UOS export
`preempt_count` and `policy` through shared DRAM: ioctl
`DRAM_INFO` / `VM_MEM_INFO` / `VM_*_OFF`). Policy2 is the same
budget math without that peek (`AddSchedHint2`).

`AddSchedHint` maps status to a **host** mode, then writes the
VCPU's `thread_t`:

| Mode | Typical cause | Host priority | Slice |
| --- | --- | --- | --- |
| `OVERLOAD` | period runtime > budget, no spare | 14 (`kLowPriority`) | budget |
| `NORMAL` | default | 16 | remaining budget |
| `RT` | guest `SCHED_FIFO`/`RR` | 18 (`kLooperPriority`) | 200 µs |
| `PREEMPT_DISABLE` | guest `preempt_count` | 18 (`kSpinLockPriority`) | 200 µs |
| `IRQ` | guest in IRQ / pending / vtimer LR | 20 (`kIRQPriority`) | 100 µs |
| `VM_CALL` | Resume returned a VMM packet | 24 (`kVMCallPriority`) | — |

Numeric order is the ranking:

```text
 OVERLOAD(0) < NORMAL(1) < RT(2) < PREEMPT_DISABLE(3) < IRQ(4) < VM_CALL(5)
```

`SetPriority` / `SetTimeSlice` write the VCPU's `thread_t`. Then
`SCHED_HINT_RESULT_RESCHEDULE` so a higher VCPU on this pCPU can
preempt immediately. `thread_preempt_disable` around the group
walk makes the new prios visible in one resched.

Lua `setSchedPriority` / `setSchedTimeslice` override the 18/20
and 200/100 µs tables (`vcpu_priority_buf` / `vcpu_budget_buf`).

### Mode hysteresis

`AddSchedHint` does **not** jump to the matching mode every exit.
Direction depends on whether **this** VCPU's host thread is
already `THREAD_RUNNING` (the current core occupant):

```text
                  not RUNNING (READY / blocked)     RUNNING (has the core)
 IRQ / I-bit      raise toward IRQ                  drop only if mode > IRQ
 preempt_disable  raise toward PREEMPT_DISABLE      drop only if mode > PREEMPT
 RT               raise toward RT                   drop only if mode > RT
 none of the above                                  NORMAL
```

After that, two overrides:

1. vtimer pending in the bitmap or sitting in an `ICH_LR` →
   force `SCHED_MODE_IRQ` (guest timer must not wait a period).
2. `mode ≥ PREEMPT_DISABLE` → `overload = false` (IRQ / spinlock
   do not drop to 14).

Then: if the chosen mode equals `sched_mode_`, return 0 (no
resched). Else write prio/slice and `RESCHEDULE`.

That is the burst-IRQ contract:

- **Other VM** is READY + IRQ → raise to 20, steal the core.
- **Current VM** already RUNNING + more IRQs → do **not** keep
  boosting. It already owns the pCPU. Short 100 µs slice only
  applies if it entered IRQ mode on the way in (typically a
  previous steal).
- **Current VM** in `VM_CALL` (24) + IRQ → drop to IRQ (20) so a
  VMM trap cannot outrank a pending interrupt forever.

### Occupancy

`ApplyVcpuPolicy` (shared `sched_control`, min interval 8 ms)
writes `nebula_sched_info.vcpu_policies[vmid][vcpu_id]` and
injects that VM's `sched_irq` into VCPU 0. The **guest** then
parks / unparks that vCPU. The host thread is not
`thread_suspend`'d; a PAUSE VCPU that still `eret`s is a guest
bug. Indices: SOS=1, Android UOS=2, tbox=3.

| `kVcpuPolicy` | Meaning |
| --- | --- |
| `RUN` | this VM's VCPU may occupy that pCPU; counts a full core in util |
| `PAUSE` | guest should not run that vCPU; host hint still applies if it does |
| `OFFLINE` | treated as not in the VM |
| `SHARE` | counts as half a pCPU in util math; both VMs may RUN here |

Named layouts (`sched.h`). `R` = RUN, `S` = SHARE, `.` = PAUSE/OFFLINE:

```text
 policy     pCPU   0  1  2  3  4  5  6  7
 NONE       SOS    R  R  R  R  R  R  R  R
            UOS    R  R  R  R  R  R  R  R
            tbox   R  R  R  R  R  R  R  R

 3_5_0      SOS    R  R  .  .  R  .  .  .
            UOS    R  .  .  R  .  R  R  R
            tbox   .  .  .  .  .  .  .  .

 5_6_0      SOS    R  S  .  .  R  S  S  .
            UOS    R  S  .  R  .  S  S  R

 5_5_0      SOS    R  R  .  .  R  S  S  .
            UOS    R  .  .  R  .  S  S  R

 5_5_2      SOS    R  .  S  .  S  S  S  .     // mt8676
            UOS    R  .  .  R  .  S  S  R
            tbox   R  .  .  .  S  .  .  .
```

`8_8_DYNAMIC` (`DoPolicy_8_8_Dynamic`): compare SOS vs UOS util
vs 40% / 75% watermarks; `FindLowUtilVCPU` / `FindFitUtilVCPU`
pick a core (skip mask `0x9d`, little preference `0xf`, big
preference `0x70`). `CalcBigCorePolicy` pins pCPU 7: UOS > 75%
for 500 ms → SOS PAUSE, UOS RUN; else SHARE/SHARE. High load on
one VM **pauses** the other's VCPU on that pCPU so the busy guest
gets a whole core.

Urgent IRQ still wins on a shared core via `AddSchedHint` even
when occupancy is SHARE. PAUSE/OFFLINE is a guest-side "do not
run"; it is not a host-priority lockout. The guest module that
actually parks the CPU is **nebula_sched**.

## Strategy switch

Same `thread_t`, different (prio, slice, occupancy) under load.
The switch is `AddSchedHint` + `ApplyVcpuPolicy`. Nothing else
picks a VM.

### Burst IRQ

Goal: the target VCPU runs **now**, injects as many vectors as
LRs allow, then **gives the core back**. A storm must not pin
prio 20 for a whole period.

```text
 device SPI  →  host GIC  →  VcpuIrqHandler
    PhysicalInterrupt  bitmap + Signal
    InterruptCpu  IPI to bind_pcpu     (if any VCPU in the group is
                                        running, or the pCPU is s2idle)
         │
         ▼  HCR.IMO  EL2 exit  (current guest, whoever it is)
 GetSchedStatus + AddSchedHint
    gich Pending           STATUS_IRQ_PENDING
    pending SGI/PPI < 32   STATUS_IPI_HANDLING
    guest SPSR I-bit       STATUS_IRQ_DISABLED
    guest hardirq count    STATUS_IN_IRQ          (Policy1)
    vtimer in ICH_LR       force SCHED_MODE_IRQ
         │
    READY VCPU  →  raise IRQ  prio 20  slice 100 µs  RESCHEDULE
    RUNNING VCPU →  hysteresis, no extra boost
         │
         ▼  thread CS if the other VM won
    gich_maybe_interrupt  ICH_LR  eret guest
```

Burst handling, in order:

| Step | Mechanism | Why |
| --- | --- | --- |
| 1. Exit | `HCR.IMO` → EL2, never guest EL1 | host can pick a different VCPU |
| 2. Pin + IPI | inject never waits for a migrate | [`interrupt.md`](interrupt.md) |
| 3. Exclusive SPI | handler runs on the VCPU's pCPU | no extra hop |
| 4. Steal | READY + IRQ → prio 20 > NORMAL 16 | other VM on SHARE loses the core |
| 5. Hysteresis | RUNNING + more IRQs → stay | storm does not re-boost |
| 6. Short slice | 100 µs (`kIPITimeSlice`) | after EOI, quantum ends, other VM runs |
| 7. LR fill | up to 16 `ICH_LR` per enter | batch the burst |
| 8. Leftover | `ICH_HCR.UIE` maintenance exit | inject the rest when LRs drain |
| 9. vtimer steal | replace a PENDING LR with vtimer | avoid resume livelock if LRs full |
| 10. Overload exempt | IRQ / PREEMPT never drop to 14 | latency beats fairness for 100 µs |
| 11. Occupancy | SHARE still preempts; PAUSE does not help the paused VM | occupancy is slow (8 ms) |

Do **not** hold `SCHED_MODE_IRQ` across a period refill: Policy2
restores `GetSchedPriority()` on `period_tick`. Policy1 relies on
the next `AddSchedHint` seeing no IRQ bits and dropping to
NORMAL.

LR full (`elrs == 0`): `gich_maybe_interrupt` returns without
Pop. If vtimer is pending it steals a PENDING slot
(`gich_prefer_vtimer_interrupt`). Remaining vectors stay in the
bitmap; UIE fires when the guest EOIs enough LRs empty.

Level-triggered storms are an inject/HW-bit bug, not a scheduler
bug — see [`interrupt.md`](interrupt.md) Pitfalls. The scheduler
still bounds them: 100 µs slice + hysteresis.

Latency recipe: exclusive `bind_pcpus` + exclusive SPI +
occupancy not SHARE + Policy1 so guest FIFO/IRQ is visible.

### Guest RT / spinlock

Two RT stories: host `THREAD_FLAG_REAL_TIME`, and guest FIFO/RR
sampled into a VCPU hint.

Host RT (`thread_set_real_time` / `thread_set_realtime_slice`):

| Call | Effect |
| --- | --- |
| slice `-1` | `THREAD_FLAG_REAL_TIME`; cancel preempt timer |
| slice `0` | clear RT, `thread_yield` |
| slice `> 0` | one-shot slice (capped 1 s), then back to normal |

`thread_is_realtime` is `FLAG_REAL_TIME && base_priority > 16`.
A VCPU thread is **not** permanently RT. `AddSchedHint` raises
`base_priority` and may call `SetRealTimeSlice` (Policy2, while
RUNNING and not period-start) for a short window. After the hint
mode drops, it is a normal CFS-like thread again.

Guest FIFO/RR → `STATUS_RT_RUNNING` → `SCHED_MODE_RT` (prio 18,
200 µs). Guest CFS stays NORMAL (16). Guest spinlock /
`preempt_count` → `PREEMPT_DISABLE` (same prio band, 200 µs) so
the holder is not preempted mid-lock by the other VM on a SHARE
core.

Switch order when several bits are set: IRQ wins over
preempt-disable wins over RT. A FIFO thread in `handle_irq` is
IRQ, not RT.

### Overload

```text
 period 5 ms, this VCPU budget 2 ms
 runtime 2.1 ms, ex_remain < 50 µs
    mode ≤ NORMAL  →  OVERLOAD  prio 14  slice = budget  RESCHEDULE
    other VM at 16  →  wins the core
    mode ≥ PREEMPT_DISABLE  →  overload ignored, finish IRQ/lock
```

Next `period_tick` zeros `last_run_time_`. The dropped VCPU
returns to whatever `AddSchedHint` says this period. Overload
with leftover group budget stays at the current mode: the core
had idle, no need to punish.

### Occupancy steal

Slow path. `ctl->policy` change, or `8_8_DYNAMIC` watermarks,
runs at most every 8 ms (`kMinSchedInterval`). It writes shm and
injects `sched_irq`; it does **not** call `AddSchedHint`.

```text
 sys util < 40%     static split (SOS 0,1 + SHARE 4; UOS 0,3,5,6 + SHARE 4)
 one VM > 40%, other < 40%
    FindLowUtilVCPU(idle VM)  skip 0x9d
    idle VM PAUSE that pCPU, busy VM RUN
 both > 75% and max-util imbalance > 204
    FindFitUtilVCPU  move one core toward the poorer VM
 pCPU 7                  CalcBigCorePolicy  (UOS big-core 75% / 500 ms)
```

A burst IRQ on a just-PAUSEd VCPU still injects (bitmap + IPI).
The guest must unpause to handle it; occupancy is not a
lost-IRQ path if `sched_irq` / wakeup IRQ lists are right
([`interrupt.md`](interrupt.md) `CheckSkipWakeup`).

### VM_CALL / VMM trap

`vmexit_handler` returns `ZX_ERR_NEXT` for a packet (GICD MMIO,
VHM, SGI via VMM, …). Resume leaves the loop, `SetVMCallMode`
sets prio 24, then the **same** thread is in EL0 VMM. Comment in
`vcpu.cpp`: userspace mutexes (GIC, VHM) would otherwise block
other VCPUs.

Next `zx_vcpu_resume`: hysteresis drops `VM_CALL` toward IRQ /
PREEMPT / RT / NORMAL from the new status.

### Hint policy 0 / 1 / 2

`VcpuGroup::SetSchedPolicy` swaps the Resume hooks:

| Policy | Period timer | `SchedVcpuResumePre` | Guest peek |
| --- | --- | --- | --- |
| 0 DEFAULT | nop | nop | no |
| 1 | 5 ms refill | `AddSchedHint` | `preempt_count` + `policy` |
| 2 | 5 ms refill | `AddSchedHint2` | no; RT slice while RUNNING |

Occupancy (`NONE` / `3_5_0` / `8_8_DYNAMIC` / …) is independent.
Both apply.

## nebula_sched

Guest Linux module. Crook never parks a VCPU thread. It writes
who *may* sit on a core into shared DRAM and rings a doorbell
(`sched_irq`). **`nebula_sched` is the actuator**: it pauses
that guest CPU. The same page also carries util, a recipe so
Policy1 can peek guest `current`, and a per-pCPU RT bit so two
FIFO tasks do not land on one SHARE core.

SOS = VM 1, UOS = VM 2, tbox = VM 3. One layout, three readers.

### Principle

Three contracts, one shared page. None of them replaces guest
CFS.

```text
  1. Occupancy     slow, ≥ 8 ms
     host writes RUN / PAUSE / SHARE / OFFLINE per (VM, vCPU)
     doorbell on that VM's VCPU 0
     guest pauses the Linux CPU
     host VCPU thread stays READY — it just has nothing to run

  2. Hint feed     every VM-exit (Policy1)
     guest once shows Crook where current->policy and
     preempt_count live in guest RAM
     Crook peeks, then raises the **host** VCPU thread
     (IRQ 20 / RT 18 / spinlock 18 / …)

  3. Cross-VM RT   guest-local, on context switch
     this VM publishes "FIFO is running on this pCPU"
     the other VM's RT wakeup skips that CPU
```

PAUSE is cooperative. If the guest ignores the doorbell and
still enters, Crook will run it.

Two clocks, easy to mix:

| | Occupancy | Hint |
| --- | --- | --- |
| Question | *may* this VM sit on the core? | *which* VCPU wins **now**? |
| Rate | ≥ 8 ms + doorbell | every VM-exit |
| Writes | guest CPU parked / running | host thread prio / slice |
| SHARE | both unpaused | IRQ 20 steals the core |
| PAUSE | guest stops scheduling here | hint still applies if it `eret`s |

Two RT stories, also easy to mix:

| | Host hint | Guest RT mask |
| --- | --- | --- |
| Sees | guest FIFO/RR on this VCPU | same, published to shm |
| Does | this VCPU thread → prio 18 / 200 µs | other VM will not *place* FIFO here |
| SHARE | steals the core for 200 µs | tries not to collide in the first place |

### Diagrams

#### N1. Three planes

Host decides occupancy and hint. Guest parks CPUs and publishes
RT. Shared DRAM is the only wire.

```text
        SOS  VM 1              UOS  VM 2              tbox  VM 3
     ┌──────────────┐       ┌──────────────┐       ┌──────────────┐
     │ nebula_sched │       │ nebula_sched │       │ nebula_sched │
     │ park CPU     │       │ park / unplug│       │ RUN only     │
     │ publish RT   │       │ publish RT   │       │ publish RT   │
     └──────┬───────┘       └──────┬───────┘       └──────┬───────┘
            │                      │                      │
            │   doorbell (sched_irq into VCPU 0)          │
            └──────────┬───────────┴──────────┬───────────┘
                       ▼                      ▼
              ┌─────────────────────────────────────┐
              │  shared DRAM                        │
              │  who may run  ·  util  ·  RT bit    │
              └──────────────────▲──────────────────┘
                                 │
              ┌──────────────────┴──────────────────┐
              │  Crook                              │
              │  occupancy table  (≥ 8 ms)          │
              │  hint prio / slice  (every exit)    │
              └─────────────────────────────────────┘
```

#### N2. Shared page

```text
 ┌──────────────────────────────────────────────────────────┐
 │  util per pCPU          host writes, 8_8_DYNAMIC reads   │
 │  util per vCPU          SOS row / UOS row                │
 │                                                          │
 │  occupancy[VM][vCPU]    RUN  PAUSE  SHARE  OFFLINE       │
 │                         host writes, guest applies       │
 │                                                          │
 │  layout enum            NONE / 3_5_0 / 8_8_DYNAMIC / …   │
 │  suspend[VM]            1 → host freezes occupancy       │
 │                                                          │
 │  bind  vCPU → pCPU      pin table                        │
 │  RT bit[VM][pCPU]       1 = that VM is FIFO here         │
 └──────────────────────────────────────────────────────────┘

  host →  util, occupancy, bind
  guest → layout enum, RT bit, suspend
  doorbell → guest re-reads occupancy[this VM]
```

#### N3. Occupancy apply

Host picks the table. Guest parks. Doorbell is the only
notification.

```text
  someone writes the layout enum
        │
        ▼
  next Resume, if ≥ 8 ms
        │
        ├── a VM is suspending ──────────────► skip
        ├── table unchanged (static layout) ─► skip
        │
        ▼
  fill occupancy[SOS / UOS / tbox][0..7]
        │
        ▼  row changed?
  write shm, ring that VM's doorbell
        │
        ▼
  guest  for each of its CPUs
        RUN / SHARE  →  unpause, schedule here
        PAUSE        →  pause, CFS/RT skip this CPU
        OFFLINE      →  SOS: unpause
                        UOS: unplug
                        tbox: PAUSE is a nop
        │
        ▼
  host VCPU thread still READY
  guest just has no work on that CPU
```

Park keeps at least two CPUs of a VM. Occupancy tables are
built so a VM is never emptied. tbox is not the idle side of
`8_8_DYNAMIC`.

#### N4. Policy1 peek

Without this, Crook cannot see guest FIFO or a held spinlock.
Guest tells the map once; Crook peeks on every later exit.

```text
  boot (once)
    guest ── "here is current, policy, preempt_count" ──► Crook
    Crook maps guest DRAM read-only, latches ready

  every VM-exit after that
    guest current
         │
         ├── policy FIFO/RR ──────────► RT
         ├── preempt_count in IRQ ────► IRQ
         ├── preempt_count in lock ───► spinlock
         └── none of the above ───────► NORMAL
         │
         ▼
    raise / drop this VCPU's host prio + slice
    (hysteresis: READY only raises, RUNNING only drops)
```

Wrong map → Crook reads `-1` → Policy1 behaves like Policy2
(budget only, no guest FIFO / lock).

#### N5. Cross-VM RT mask

Guest-side placement. Host hint (prio 18) is a different path.

```text
  SOS context switch
        current is FIFO?  ──yes──► RT bit[SOS][this pCPU] = 1
                              no──► 0

  UOS RT wakeup
        start from online CPUs
        for each pCPU where SOS RT bit is 1:  drop it
        place UOS FIFO on what remains

  SHARE pCPU 0, SOS FIFO running
        UOS RT wakeup  ──►  CPU 0 gone  ──►  UOS FIFO elsewhere

  exclusive occupancy already avoids this
  the mask is for SHARE
```

Bind must exist before the bit is written. Unbound → the bit
lands on pCPU 0, a false SHARE collision.

#### N6. Two timescales on one SHARE core

Occupancy says both may sit. Hint says who sits *now*.

```text
  pCPU 0  SHARE     period 5 ms      occupancy ≥ 8 ms

  occupancy     R  R  R  R  R  R  R  R     both RUN, no park
  hint          16 16 16 20 20 16 16 16    IRQ steal, then drop

  time →
  SOS   [ NORMAL 16 ████ ][ IRQ 20 ██ 100µs ][ 16 ████ ]
  UOS   [ READY          ][  wait           ][ 16 ████ ]
                         ▲ doorbell not involved
                         ▲ hint stole the core
```

`8_8_DYNAMIC` is the slow path that *does* ring the doorbell:

```text
  time →                    ≥ 8 ms
  util  SOS 80%  UOS 30%
           │
           ▼
  occupancy  UOS CPU 5  RUN ──► PAUSE
             SOS CPU 5  SHARE/PAUSE ──► RUN
           │
           ▼  doorbell UOS VCPU 0
  UOS parks CPU 5
  SOS has pCPU 5 to itself

  UOS SPI for vCPU 5 still injects
  guest must unpause to handle it
  occupancy is not a lost-IRQ path
```

#### N7. Suspend

One VM going to sleep freezes occupancy for **everyone**.

```text
  suspend this VM
    mark suspend[this] = 1     host occupancy loop bails
    mask the doorbell
    RUN every CPU              do not sleep parked

  resume
    re-read occupancy, park/unpark
    suspend[this] = 0
    unmask the doorbell
```

#### N8. Who configures what

Two knobs, two owners. Both apply.

```text
  boot (VMM / lua)
    period, budget, hint prio/slice
    hint algorithm  0 / 1 / 2
    doorbell SPI, VCPU → pCPU pin

  runtime (guest)
    occupancy layout enum     NONE / 3_5_0 / 8_8_DYNAMIC / …
    (the shm byte, not lua)
```

### Contracts that break silently

- No module, no doorbell → host writes occupancy and nobody
  parks. SHARE cores still time-slice on hint prio.
- One VM suspending → occupancy frozen for all VMs.
- tbox PAUSE is a nop. Do not PAUSE tbox and expect silence.
- Park will not leave a VM with fewer than two CPUs.
- SOS OFFLINE unpauses; UOS OFFLINE unplugs. Not the same.
- Bind first, then RT bits. Unbound bits hit pCPU 0.

## Sequence diagrams

Timing versions of **Diagrams** 4–7. Same events, more timestamps.

### Shared-core Resume (no IRQ)

```text
 pCPU 0  occupancy SHARE     SOS VCPU 16     UOS VCPU 16
         period 5 ms         budget 2 ms     budget 2 ms

 t0  SOS Resume
     Policy1: both NORMAL, SOS remaining slice
     eret SOS ──────────────────────────────── SOS guest EL1h
 t1  quantum / WFI / trap
     EL2 exit, need_sched_
 t2  Policy1: SOS runtime 2.1 ms, no spare
     SOS OVERLOAD 14, UOS 16  RESCHEDULE
     thread CS  SOS → UOS
 t3  UOS Resume  eret UOS ──────────────────── UOS guest EL1h
 t4  period_tick  ResetPeriodRuntime
     both back to NORMAL next hint
```

### Burst IRQ for the other VM

SHARE core, UOS running, SOS owns the SPI.

```text
 time →
 UOS thread   [==== guest EL1h =================]  exit   READY
 SOS thread    READY                               IRQ20  [==== guest IRQ ===] 100µs
 device SPI ──────────────────────────────────────●
 host GIC                                         │
                                                  ▼
                                           VcpuIrqHandler
                                           SOS.PhysicalInterrupt
                                           bitmap + IPI pCPU 0
                                           UOS HCR.IMO → EL2
                                           UOS Resume need_sched_
                                           Policy1 walk group:
                                             UOS RUNNING + (maybe no SOS IRQ bits)
                                             SOS READY  + IRQ_PENDING
                                               AddSchedHint: raise IRQ 20 / 100 µs
                                           thread_reschedule
                                           CS UOS → SOS
                                           SOS gich_maybe_interrupt
                                           eret SOS  guest handle_irq  EOI
                                           100 µs slice / no more IRQ bits
                                           drop NORMAL, UOS runs
```

### Burst IRQ for the current VM (storm)

UOS already running, many SPIs for UOS.

```text
 UOS guest EL1h
    SPI 1  HCR.IMO  EL2 exit
    PhysicalInterrupt  bitmap {1}
    AddSchedHint: RUNNING + IRQ → hysteresis (stay NORMAL unless
                  already IRQ / VM_CALL)
    gich_maybe_interrupt  LR[0]=SPI1
    eret
 UOS handle_irq  EOI
    SPI 2..N pending while still in guest
    SPI 2  EL2 exit
    bitmap {2..N}
    still RUNNING → no re-boost
    fill LR[0..k]  leftover → ICH_HCR.UIE
    eret  guest handles a batch
    LRs drain  maintenance IRQ  EL2 exit
    inject the rest
    100 µs only if this VCPU entered IRQ mode (typically after a
    steal). Otherwise NORMAL slice = remaining budget.
```

LR full + vtimer:

```text
 16 LRs valid, vtimer pending in bitmap
    gich_maybe_interrupt  elrs==0
    TryPop(vtimer)  steal a PENDING LR
    UIE for the rest
 without the steal: resume immediately exits (vtimer PENDING in
 CNTV), LRs still full, livelock
```

### Guest spinlock vs other VM IRQ

```text
 SOS guest  preempt_count != 0     PREEMPT_DISABLE 18 / 200 µs
 UOS        SPI pending            READY → IRQ 20 / 100 µs
            20 > 18  →  SOS preempted mid-lock
 Policy1 exists so Crook can see the lock; it still loses to IRQ.
 Exclusive occupancy (not SHARE) is the way to keep the lock.
```

### Occupancy 8_8_DYNAMIC steal

```text
 userspace  sched_control.policy = 8_8_DYNAMIC
 Vcpu::Resume  ApplyVcpuPolicy  (if > 8 ms, TryAcquire kSchedLock)
   sys 80%, SOS util 80%, UOS 30%
   FindLowUtilVCPU(UOS, ceil 100, skip 0x9d, prefer 0x70)
     → pCPU 5
   kVcpuPolicy[UOS][5] = PAUSE
   kVcpuPolicy[SOS][5] = RUN
   update_vm_policy → shm
   Interrupt(UOS vcpu0, sched_irq)     nebula_sched parks CPU 5
   Interrupt(SOS vcpu0, sched_irq)     SOS applies RUN on CPU 5
 SOS VCPU on pCPU 5 gets the core to itself
 next burst IRQ for UOS vCPU 5: still injected; guest must unpause
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

### Priority inheritance (host mutex, not VCPU hint)

```text
 B (prio 24)  mutex_acquire  held by A (prio 16)
   A.inherited = 24, requeue / keep running
 A  mutex_release
   wake B; A.inherited = -1 if last mutex
```

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
 UOS    R  .  .  R  .  R  R  R
 tbox   .  .  .  .  .  .  .  .
```

`R` = RUN. Empty = PAUSE/OFFLINE. Shared cores (pCPU 0 here) run
SOS and UOS VCPU threads back-to-back; IRQ/RT hints decide who
preempts. Exclusive cores skip that fight.

Latency-sensitive passthrough: exclusive VCPU bind + exclusive SPI
bind + that pCPU not SHARE. Same advice as interrupt.md.

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
| `nebula_sched` | Guest module: parks CPUs, feeds Policy1, publishes RT. |
| occupancy shm | Shared DRAM: util, RUN/PAUSE/SHARE/OFFLINE, RT bits. |
| `sched_irq` | Doorbell into VCPU 0: guest, re-read occupancy. |
| Hint policy | Policy0/1/2: how Resume maps guest status → host prio. |
| Mode hysteresis | RUNNING: only drop; READY: only raise. Burst-IRQ contract. |
| `need_sched_` | Previous VM-exit asked Policy1/2 to recompute hints. |
| `ex_remain` | Group leftover after budgets and overruns. |
| `bind_pcpus` | Lua array: VCPU index → pCPU. |
| PI | Kernel mutex donates waiter `effec` to holder. Not futex. |
| `inherited_priority` | One slot. `-1` = off. Cleared at `mutexes_held == 0`. |
| Host RT | `THREAD_FLAG_REAL_TIME`: no quantum timer, no ±1 boost. |
| Guest RT hint | Guest FIFO/RR → `SCHED_MODE_RT` on the VCPU thread. |
| Thread CS | EL1: callee-saved + TTBR0. `arch_context_switch`. |
| VCPU world switch | EL2: `El2State` GPRs/sysregs, `VTTBR` on/off, `eret`. |
| `El2State` | Guest + host register file for one VCPU. |
| UIE | `ICH_HCR` underflow: maintenance exit when LRs drain. |

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
| `GetSchedStatus` / `AddSchedHint` | `zircon/kernel/arch/arm64/hypervisor/vcpu.cpp` |
| `gich_maybe_interrupt` / UIE / vtimer steal | same, `gich_prefer_vtimer_interrupt` |
| `SetVMCallMode` | same, on `ZX_ERR_NEXT` packet |
| Occupancy shm | `zircon/kernel/kernel/pelt.c` `update_vm_policy` |
| `VcpuGroup` / Policy1/2 / `8_8_DYNAMIC` | `zircon/third_party/lib/thyper/vcpu_group.cpp` |
| Policy enums | `zircon/kernel/include/kernel/sched.h` |
| Bind table | `zircon/kernel/arch/arm64/hypervisor/guest.cpp` `SetCpuAffinity` |
| Lua knobs | `garnet/bin/guest/guest_config.h` |
| Apply lua → ioctl | `garnet/bin/guest/vmm.cc` |
| Set period/budget/prio | `zircon/third_party/lib/thyper/board_mtk_comm.cpp` |

---

Everyday's a Hustle.
