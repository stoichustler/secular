# GPU

**After this:** you can say why GRANT, STOP, or a timeout happened.
**First read:** Principle, state machine, Config (product numbers).
**Next:** [`tee.md`](tee.md).

Savior shares one physical **Mali GPU** between SOS (Yocto) and UOS
(Android / ALPS). The product path is Arm **SWA**: software
virtualization, no Mali Access Window / Resource Group, no hardware
partition. The hypervisor is the **Arbiter**. Guests talk to it
through **SMC** (Comms) and four **notify IRQs** (GRANT / STOP / LOST
/ RCVR). Ownership is exclusive: only the granted VM has GPU MMIO in
stage-2 and Mali IRQs dispatched onto its VCPUs.

`zx_gpu_scheduler_create` refuses `BOOTMODE_PHYP`,
`BOOTMODE_QEMU_AHYP`, and `BOOTMODE_OKL4_AHYP`. Later `zx_gpu_*`
require `BOOTMODE_AHYP`.

This tree **is** the mt8678 3OS GPU SWA product. QEMU lua does
not enable SWA; product numbers live in
`yocto/prebuilt/hypervisor/grt/auto8678p1_64_sos_indep/sos_mt8678.lua`.

Read [`README.md`](README.md) for the product map,
[`vmm.md`](vmm.md) for SOS / UOS lifecycle,
[`memory.md`](memory.md) for `MapDevice` / stage-2,
[`interrupt.md`](interrupt.md) for `PhysicalInterrupt` vs virtual
inject, [`scheduler.md`](scheduler.md) for VCPU pin.

Arm's model (Access Manager, access window, arbiter, yield) is
`Arm Mali GPU 5th Generation and Later Virtualization Guide`
112001_0100_01_en. Savior implements the **software** time-slice
of that model: the hypervisor **is** the Arbiter, and stage-2
map/unmap stands in for a hardware access window.

This note is the map of SWA: why it exists, the three isolation
planes, Arm core concepts, Savior objects, SMC/IRQ protocol,
state machine, grant / stop / recovery, and MMIO map.

## Framework

```text
 SOS (Yocto, kernel VMID 1)          UOS (Android, kernel VMID 2)
  2D/3D app                           2D/3D app
  libmali.so                          libmali.so
  mali_kbase + mali_arb               mali_kbase + mali_arb
  DTS: mali + arbiter (mali,sw-arbiter)
              │ SMC  FC_ID_GPU_ARBITER (x1=0x3)
              │ x2=cmd  x3=arg0  x4=arg1
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Guest EL1  SMC  →  Savior EL2 vmexit  handle_smc_instruction   │
 │  SMC_FC_NBL_VHM_REQ / FC_ID_GPU_ARBITER                        │
 │  GpuSchedulerAccessor::HandleSmc                               │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ GpuScheduler  (kernel object, singleton via Accessor)          │
 │  GpuScheduleStateMachine   current owner + pending requester   │
 │  GpuSchedBackend           MapGpu / UnmapGpu / timers / inject │
 │  GpuSchedIntrHandler       Mali IRQ → owner VCPU (HW LR)       │
 │  VmInfo[MAX_VM=3]          vcpus, regions, cmdbuf, timers      │
 └────────────────────────────┬───────────────────────────────────┘
                              │ GRANT/STOP/LOST/RCVR  virtual IRQ
                              │ Mali JOB/MMU/GPU/EVT/PWR  physical
                              ▼
                         Mali GPU
```

Userspace VMM is only the **bootstrap**. Runtime arbitration is in
the kernel:

```text
 SOS Vmm
  GpuSchedulerServiceImpl::createGpuSched
    zx_gpu_scheduler_create / set_vmo / set_timeslice / set_mode
    FIDL  machina.GpuScheduleService
    DTS  /arbiter  compatible = "mali,sw-arbiter"
 UOS Vmm
  GpuSchedulerClient::prepareGpuSched
    GetSchedHandle + set_vmo + set_timeslice
    add_arbiter_dts
 Both
  zx_gpu_scheduler_add_vcpu   (affinity VCPU first in the vector)
```

Kernel VMID vs FIDL user vmid is the same table as [`vmm.md`](vmm.md):
SOS 1, ALPS 2, tbox 3. Debug dumps print `vmid=1` Yocto, `vmid=2`
Android. **tbox does not join SWA** (no `/arbiter`, no `mali_arb`).

## Principle (SWA)

The SoC has **one** Mali. SOS (IVI / Yocto) and UOS (Android) both
need 2D/3D. There is no Mali Access Manager, no Access Window
hardware, no Resource Group, no shader-core partition. Concurrent
owners would mean two kernels programming the same MMIO and
sharing the same IRQ wires. That is undefined.

SWA's answer: **exclusive ownership over time**, enforced by the
hypervisor.

```text
  what a guest needs to "have the GPU"
     1. CPU MMIO   stage-2 PTE for GPU regs     AXI-subordinate
     2. IRQs       Mali SPIs on its VCPUs       PHYSICAL, HW LR
     3. DMA        SMMU STE → that guest's S2   AXI-manager
```

Savior punches (1) and retargets (2) on every GRANT / STOP.
(3) is **not** retuned at GRANT: both VMs' StreamIDs must already
be attached to **their own** stage-2. Isolation of GPU DMA is
"the other guest's RAM is not in this S2", not "SID changes
owner". A guest without MMIO cannot submit; a guest with MMIO
but the other's SID would DMA into the wrong IPA — so STE setup
at boot is part of SWA correctness ([`memory.md`](memory.md)).

**Why the hypervisor is the arbiter.** Arm's reference
(`panthor_arbitration.ko`) talks to an Access Manager through a
Resource Group page mapped into a host VM. Savior has no such
page. The only thing that can open or close the "window" is
`MapDevice` / `UnMapDevice` and `register_int_handler`. Those
are kernel operations, so `GpuScheduler` lives in Savior.

**Cooperative, then forced.** The owner must STOP (drain or
quiesce) before the next GRANT. That is Arm yield. If it
ignores STOP until `yield_timeout`, Savior **does not** reset
Mali in hardware (there is no Access Manager reset). It
unmaps, injects Lost/RCVR, and hopes the next kbase init
cleans the GPU. A badly behaved guest costs the other VM
work. SWA **reduces** interference; it is not hard isolation.

**What the guest thinks.** Each VM has a full `mali_kbase` plus
`mali_arb`. The VMM injects `/arbiter` (`compatible =
"mali,sw-arbiter"`) into that guest's DTB. From the driver, the
GPU appears, disappears, and comes back: GRANT ≈ power-on /
window-open, STOP ≈ must idle, LOST ≈ device-lost. There is no
second GPU and no shared kbase context.

**Who may request.** SOS and UOS. tbox is a third VM on the
same cores; it has no Mali path. SIMPLE mode (`ASSIGN`) is SOS
only: force grant, no GRANT IRQ — a bring-up / single-owner
bypass of the handshake.

**Three IRQ planes, one owner.**

| Plane | Type | Who receives |
| --- | --- | --- |
| Notify GRANT/STOP/LOST/RCVR | virtual | the VM being told to start/stop/recover |
| Mali JOB/MMU/GPU/EVT/PWR | physical, HW bit | **current owner only** |
| Host GIC | remaining SoC | not SWA |

While VM A is Granted, VM B's `mali_kbase` must not see Mali
SPIs. `EnableGpuIrq` overwrites the dispatch set; `DisableGpuIrq`
installs a dummy that EOIs without injecting.

**Switch cost.** Unmap PTEs + TLBI + remap + GRANT IRQ + guest
kbase re-program. HWA keeps the guest map stable and only
flips the Access Manager; SWA pays the remap every time. That
is why slices are milliseconds (`request_timeout` / SCHEDULE),
not microseconds.

**NORMAL vs SIMPLE.**

```text
 NORMAL   REQUEST → GRANT IRQ → ACTIVE → (slice) STOP → STOPPED → …
 SIMPLE   SOS ASSIGN → EnableGpu, Granted, no IRQ
          no UOS handshake; one owner
```

Product 3OS is NORMAL. SIMPLE is for a board that has not
brought up `mali_arb` on the second VM yet.

## Core concepts (Arm)

From 112001_0100_01_en ch. 2–4. The basic product is a **shared
single-partition GPU**: one execution domain, several guests,
time-sliced. AE parts add **partitions** (hardware-isolated
domains). Savior is the first family.

```text
 Arm HWA (Access Manager in the GPU)
 ┌──────── Host / privileged ────────┐   ┌──── Guest VM ────┐
 │  Driver instance     Arbiter      │   │  Driver instance │
 │  (may also be a guest)            │   │                  │
 └───────┬──────────────┬────────────┘   └────────┬─────────┘
         │              │  Resource Group         │
         ▼              ▼  (arbiter messages)     ▼
      Access Window 0              Access Window 1
         │  open ──► GPU subinstance ◄── closed   │
         └──────────── Partition ─────────────────┘
                              GPU
```

Savior has **no** Access Manager, no access-window aperture, no
Resource Group. The hypervisor both isolates and arbitrates:

```text
 Savior SWA
 ┌──────── SOS ────────┐   ┌──────── UOS ────────┐
 │ mali_kbase+mali_arb │   │ mali_kbase+mali_arb │
 └──────────┬──────────┘   └──────────┬──────────┘
            │ SMC REQUEST/IDLE/STOPPED│
            ▼                         ▼
      Savior EL1  GpuScheduler  (Arbiter + isolation)
            │ GRANT/STOP/RCVR  virtual IRQ
            │ MapGpu / UnmapGpu  stage-2
            ▼
      one Mali  (GPU subinstance)
```

### VM, guest, driver instance

The software domain that gets the GPU is a **VM / guest**. Each
guest that uses the GPU has its **own driver instance**
(`mali_kbase` + `mali_arb` in Savior). That instance:

- programs the GPU control interface the guest can see
- loads firmware into the GPU subinstance (when the SoC has it)
- handles IRQs and faults on its assigned interface
- yields / restarts when the arbiter asks
- submits userspace work

Control and data paths of two driver instances stay isolated.
They never share a kbase context.

### GPU subinstance

The **execution hardware** a guest drives while it is the owner.
From the guest it *is* the GPU. The platform moves that ownership
between software domains; only one guest connects to a given
subinstance at a time (unless partitions exist).

### Access window

Arm: the **guest-visible control path** for one driver instance —
MMIO/control aperture plus a stable mapping that can stay in the
guest aspace while the Access Manager opens or closes the window.

- **open** — guest talks to the GPU subinstance
- **closed** — guest keeps a little window-control visibility, not
  the rest of the GPU. Using the closed part is invalid access.

The window is an ownership boundary, not just an address range.
HWA keeps the guest map stable across switches; the Access
Manager flips which window is connected.

Savior SWA has no such hardware. **Open** = `EnableGpu` (stage-2
PTE present, Mali IRQs routed). **Closed** = `DisableGpu`
(unmap, dummy IRQ). Each GRANT remaps.

### Time slicing

Several guests share one subinstance **over time**, not by
splitting shader cores.

```text
 time →
 SOS AW   ████ grant ████ yield ░░░░░░░░░░░░░░░░░ ████
 UOS AW   ░░░░░░░░░░░░░░░░░░░░░ ████ grant ████ yield
 GPU      ──── SOS ──────────── ──── UOS ────────────
```

1. Each guest has (or, in SWA, *is given*) a control path.
2. Only one path is connected to the subinstance.
3. Software decides when ownership moves.
4. The current owner must **stop** before the next owner starts.

Arm HWA then: request yield → **reset GPU** → open next window →
resume. Reset is architectural: it drops transient state so the
next guest cannot see the last one's GPU guts.

Trade-off: better utilization of one GPU; switching cost and
sensitivity to a badly behaved guest or a bad policy. Arbitration
**reduces** interference; it does **not** give hard isolation.
Hard isolation needs partitions.

### Yield and handover

A **yield** is the current owner giving up the subinstance.

Arm preferred path (firmware, Access Manager):

```text
 arbiter  YIELD_NOW or YIELD_IDLE
 firmware  quiesce, save, YIELD_DONE     (async; not CPU-bound)
 Access Manager  close window, reset subinstance
 arbiter  OPEN_WINDOW
 Access Manager  open, RAM clear
 next driver  power up, boot or warm-resume firmware
```

`YIELD_NOW` = stop as soon as possible. `YIELD_IDLE` = drain
current queues first. Yields are **cooperative**. If they fail,
the platform forces recovery → lost work / device-lost.

Arm also keeps a **legacy software stop**: `ARB_VM_GPU_STOP` →
guest `VM_ARB_GPU_STOPPED` → arbiter closes the window. Savior's
STOP IRQ + SMC `STOPPED` is this family, not the CSF
`YIELD_DONE` path.

### Firmware

Part of the **current** execution environment, not a shared
service. It schedules, quiesces, yields; it is booted or
warm-resumed when a window opens. Different guests may use
different images. Savior's guest kbase still talks to Mali
firmware **while granted**; the hypervisor does not run it.

### Arbiter

Software that picks **which** control path may use the
subinstance and **when** to switch.

Responsibilities:

- request yield from the current owner
- select the next window / VM
- timeout and recovery if handover does not finish

Placement (Arm): host kernel module, a dedicated VM, the
hypervisor, or a safety island. Reference is
`panthor_arbitration.ko` in the host / a VM, round-robin,
`request_timeout` / `yield_timeout`. Policy is replaceable
(priority, VSync, safety). Savior puts the arbiter **in the
hypervisor** (`GpuScheduler`) because there is no Access
Manager messaging page to map into a host VM.

Fairness vs policy: the arbiter implements **platform policy**.
Tune for latency, throughput, or predictability. Still
time-sharing the same shaders.

### Hypervisor vs Access Manager

| Who | Owns |
| --- | --- |
| Access Manager (GPU HW) | which window is open; reset; guest vs control IRQs |
| Arbiter | scheduling policy, yield request, next owner |
| Hypervisor | stage-2 of guest-visible regs, SMMU for GPU DMA, IRQ routing, security |

Arm: arbiter **can** sit outside the hypervisor; isolation stays
with EL2 / SMMU. Savior: one object does both, because SWA has
nothing else to open a window with except `MapDevice`.

AXI-subordinate = CPU MMIO into GPU regs (stage-2). AXI-manager
= GPU DMA out (SMMU StreamID). Arm encodes StreamID as access-
window index + protected bit. Savior uses the SID table in
[`memory.md`](memory.md); grant does not retune STE, so **both**
VMs' SIDs must already be attached to **their** stage-2, and
only the granted VM has CPU MMIO.

### Partition vs time slice

Independent knobs:

| | Time slice | Partition (AE) |
| --- | --- | --- |
| What | share one subinstance over time | concurrent isolated domains |
| Isolation | policy + reset | hardware |
| Savior | yes (SWA) | no |

Hybrid (Arm, not Savior): one partition for a safety guest, IVI
guests time-slice another.

## Virt methods

| Method | Hardware | Who owns the GPU | Savior |
| --- | --- | --- | --- |
| **SWA** | none | one VM at a time; hypervisor maps MMIO + IRQs | **yes** |
| **HWA** | Access Window + Resource Group + Access Manager | Arbiter picks the window; map stays | no |
| **Hardware separation** | Partition = independent GPU | concurrent, no time-share required | no |

SWA cost is **context switch**: unmap previous guest, map next,
inject GRANT. Mali itself is not partitioned. That is the remap
HWA's access window is designed to avoid.

### Arm protocol vs Savior SMC

Arm arbiter↔VM (v3) sits on the access-window message path.
Savior has no that path; SMC + virtual IRQ is the whole channel.

| Arm | Savior |
| --- | --- |
| `VM_ARB_GPU_REQUEST` | SMC `REQUEST` |
| window open / `WINDOW_OPENED` | `EnableGpu` + GRANT IRQ |
| `YIELD_NOW` / `YIELD_IDLE` | STOP IRQ (always "now"); guest IDLE is voluntary yield |
| `YIELD_DONE` (firmware) | — (not used) |
| `ARB_VM_GPU_STOP` / `VM_ARB_GPU_STOPPED` | STOP IRQ / SMC `STOPPED` |
| `WINDOW_CLOSED` | `DisableGpu` UnmapGpu |
| GPU_LOST | exception `0xABCE1235` or RCVR on SOS |
| `request_timeout` | SCHEDULE timer |
| `yield_timeout` | STOPPING timer |
| `ARB_VM_INIT` / `VM_ARB_INIT` | `zx_gpu_scheduler_add_vcpu` + DTS `/arbiter` |

Guest-side Arm AW states INIT / READY / GPU_REQUEST /
GPU_GRANTED / GPU_STOP / GPU_STOPPED line up with Savior
Undefined / Idle / Pending / Granted / Stopping / Idle.

### Principle: one owner

```text
                  at most one current_vmidx_
                  at most one current_req_vmidx_
         Idle ──REQUEST──► Granted ──STOP──► Stopping ──STOPPED──► Idle
                    ▲                              │
                    └──── Grant Pending ◄──────────┘
```

No guest programs Mali unless Granted. Pending never has MMIO.
That is Arm "only one access window open" in software.

## Objects

```text
 GpuSchedDispatcher          Zircon dispatcher, ZX_DEFAULT_GPU_SCHED_RIGHTS
        │ owns
        ▼
 GpuScheduler                GpuSchedBackend
        │                    EnableGpu / DisableGpu / NotifyVmReady
        │                    InjectGrant/Stop/Lost/Recovery
        │                    StartTimer / StopTimer
        ├── GpuScheduleStateMachine
        │     vms_[3]  current_vmidx_  current_req_vmidx_
        ├── VmInfo[3]
        │     vmid, vcpus[], mapping_regions[], cmd*, timers, state
        ├── gpu_irq_[5]      JOB MMU GPU EVT PWR
        ├── notify_irq_[4]   GRANT STOP LOST RCVR
        └── sched_timers_[4] SCHEDULE STOPPING RECOVERY PREPARING

 GpuSchedulerAccessor        singleton; SMC entry; Guard around HandleSmc
 GpuSchedIntrHandler         singleton; dispatch Mali IRQ to owner VCPUs
```

`GpuScheduler::Create` takes IRQ numbers and GPU regions as
`(paddr, size, flag)` triples. Flag `ARB_GPU_REGION_PIN` (0x1) keeps
the region mapped even after `DisableGpu`.

Command buffer: `zx::vmo::create(4K)` (paged), `SPECIFIC` into the
guest at IPA `0x88000b000` (`kShareMemBase`, default **CACHED**),
then `zx_gpu_scheduler_set_vmo` maps the **same VMO** into the
kernel **UNCACHED** (`GpuSchedCmdType*`). That split is alias B
([`shm.md`](shm.md)). Guest DTS `/arbiter` `reg` points at the IPA.
SMC command id is in `x2`; the buffer is trace / AEE, not the
command. IPA sits inside SOS identity `ram`; SPECIFIC succeeds
only if that 4K is already a hole.

## Message protocol

Guest → hypervisor is SMC. Hypervisor → guest is a virtual IRQ.
Mali hardware IRQs are a third plane: they only go to the **current
owner**.

### SMC  (`ARB_CMD_GPU_*`, low 8 bits of `x2`)

| cmd | id | Direction | Meaning |
| --- | --- | --- | --- |
| `ACTIVE` | 0x01 | VM → Arbiter | GPU is in use; cancel PREPARING, maybe start SCHEDULE |
| `IDLE` | 0x02 | VM → Arbiter | owner yields; if someone is Pending, STOP current |
| `REQUEST` | 0x03 | VM → Arbiter | want the GPU |
| `STOPPED` | 0x04 | VM → Arbiter | finished STOP; switch is legal |
| `RECOVERED` | 0x05 | VM → Arbiter | recovery done; grant Pending |
| `TRACE` | 0x06 | VM → Arbiter | debug (internal) |
| `DESTROY` | 0x07 | **not an SMC** | `zx_gpu_scheduler_destroy_vm`; `HandleSmc` has no DESTROY case |
| `DUMP` | 0x08 | VM → Arbiter | dump history |
| `TIMERSET` | 0x09 | VM → Arbiter | reset a timer period (internal) |
| `ASSIGN` | 0x11 | SOS only | SIMPLE mode: force grant |

`WITH_GPU_SCHEDULER_OPTIONS` is **false** on this HEE (`venus-hee.mk`).
`TRACE` / `TIMERSET` SMCs are `#if WITH_GPU_SCHEDULER_INTERNAL`
(also false). `STOPPED_REQUIRE_ENABLE` (STOPPED bit 0) is **always**
parsed.

`REQUEST` / `STOPPED` extra bits (only when OPTIONS is true):

| bit | REQUEST | STOPPED |
| --- | --- | --- |
| 0 | — | `STOPPED_REQUIRE_ENABLE` (always parsed) |
| 1 | `REQUEST_TIMESET_ENABLE` use `arg1` as deadline | same for the follow-up request |
| 2 | `REQUEST_SCHEDULE_DIRECT` skip waiting for ACTIVE | same |

SMC trap:

```text
 guest  smc
   x0 = SMC_FC_NBL_VHM_REQ
   x1 = FC_ID_GPU_ARBITER   (0x3)
   x2 = ARB_CMD_GPU_*
   x3 = arg0
   x4 = arg1
        │
        ▼
 vmexit  handle_smc_instruction
   handle_gpu_arbiter_cmd
   GpuSchedulerAccessor::HandleSmc
     NORMAL  →  GpuScheduler::HandleSmc
     SIMPLE  →  HandleSimpleSmc  (SOS ASSIGN only)
   x0 = zx_status_t
```

`HandleSmc` takes the scheduler spinlock with interrupts off, then
`RunPendingTimerTasks` **outside** the lock. Map/unmap also drop the
lock (they walk the guest aspace). Timer callbacks re-enter the
same pattern.

### Notify IRQs  (virtual, `VcpuDispatcher::Interrupt`)

| index | name | Typical SPI (debug) | Meaning |
| --- | --- | --- | --- |
| 0 | GRANT | 4316 (product) | you may use the GPU; MMIO is mapped |
| 1 | STOP | 4317 | stop using the GPU; reply `STOPPED` |
| 2 | LOST | 4318 listed; IRQ unused | GPU was taken; current code injects a VCPU **exception** `0xABCE1235` instead of the LOST IRQ (`#if 0` around the IRQ inject) |
| 3 | RCVR | 4319 | run GPU recovery; reply `RECOVERED`. Registered as a **fast interrupt** on add-vcpu |

### Mali IRQs  (physical, `VcpuDispatcher::PhysicalInterrupt`, HW LR)

| index | name | DTS interrupt-names |
| --- | --- | --- |
| 0 | JOB | `MALI-JOB` (product SPI 638) |
| 1 | MMU | `MALI-MMU` (637) |
| 2 | GPU | `MALI-GPU` (636) |
| 3 | EVT | `MALI-EVT` / config `MALI-EVENT` (639) |
| 4 | PWR | `MALI-PWR` (640) |

`DispatchIrq` always uses `force_physical = true` so the ICH_LR HW
bit stays tied to the physical GIC lifecycle. See
[`interrupt.md`](interrupt.md).

## State machine

`GpuVmState` per VM. Invariants: at most one `current_vmidx_`
(owner), at most one `current_req_vmidx_` (Pending). Owner states
are Granted / Yield / Stopping / Recovery.

```text
                    AddVm
                      │
                      ▼
                 Undefined
                      │
                      ▼
                    Idle ◄───────────────────────────────────────────┐
                      │ REQUEST, no owner                            │
                      │ GrantVm: EnableGpu + GRANT + PREPARING timer │
                      ▼                                              │
                  Granted                                            │
                   /  | \                                            │
         ACTIVE   /   |  \ IDLE, no Pending                          │
  (cancel PREP,  /    |   \                                          │
   maybe SCHED) /     |    ▼                                         │
               /      |   Yield                                      │
              /       |     │ IDLE + Pending, or SCHED timeout       │
             /        |     │ StopCurrent: STOP + STOPPING timer     │
            /         |     ▼                                        │
           /          | Stopping                                     │
          /           |  │ STOPPED                                   │
         /            |  │ DisableGpu, Idle, Grant Pending           │
        /             |  │                                           │
       /              |  │ STOP timeout                              │
      /               |  ├─ SOS  → Recovery (RCVR IRQ + RECOVERY tmr)│
     /                |  └─ other → Lost (exception), then Idle      │
    /                 │                                              │
   /   other VM REQUEST while Granted                                │
  /    requester → Pending; if owner ACTIVE/direct, start SCHED      │
 /                                                                   │
 │  Recovery  RECOVERED → Idle + Grant Pending                       │
 │            RECOVERY timeout → Lost                                │
 │  RemoveVm / DESTROY / UOS reboot  → Idle (recovery grant to SOS)  │
 └───────────────────────────────────────────────────────────────────┘
```

| Event | From | To | Backend |
| --- | --- | --- | --- |
| `REQUEST`, no owner | Idle | Granted | `EnableGpu` + GRANT + PREPARING |
| `REQUEST`, owner exists | Idle | Pending | maybe start SCHED on owner |
| `ACTIVE` | Granted | Granted | stop PREPARING; if Pending and not `schedule_direct`, start SCHED |
| `ACTIVE` | Yield | Granted | bump virtual period |
| `IDLE`, no Pending | Granted | Yield | stop SCHED |
| `IDLE` + Pending | Granted / Yield | Stopping | STOP + STOPPING timer |
| SCHED timeout | Granted | Stopping | same |
| `STOPPED` | Stopping | Idle, then grant Pending | `DisableGpu`; GRANT next |
| `STOPPED` + `require` and no Pending | Stopping | Granted | GRANT again + PREPARING |
| STOP timeout, SOS | Stopping | Recovery | RCVR |
| STOP timeout, not SOS | Stopping | (Lost) Idle | exception |
| `RECOVERED` | Recovery | Idle, grant Pending | stop RECOVERY timer, `DisableGpu` |
| PREPARING timeout | Granted | (stay) | anomaly log only |

`AssignDirect` (SIMPLE / `ASSIGN`) grants without the REQUEST
handshake: `EnableGpu` and set Granted, **no GRANT IRQ**. SOS uses
this when `sched_mode = ARB_SCHED_MODE_SIMPLE`.

### Timers

| type | default (lua / spec) | Arm analogue | Fired |
| --- | --- | --- | --- |
| SCHEDULE | SOS/UOS 16 ms | `request_timeout` | owner has had the GPU long enough; STOP |
| STOPPING | UOS 5 s (SOS from config) | `yield_timeout` | guest ignored STOP; SOS → recovery, else Lost |
| RECOVERY | config | GPU_LOST handling | recovery hung; Lost |
| PREPARING | config | — | guest got GRANT but no ACTIVE; log only |

Cap is `kGpuSchedMaxTimerPeriod` = 60 s. Policy:

| `GpuSchedPolicy` | SCHEDULE period |
| --- | --- |
| Default | configured slice |
| None | 0 → never start SCHED (owner keeps GPU until IDLE/STOPPED) |
| Perf | 2 ms … 20 ms, stride 1 µs, from a 0…100 virtual period |

`UpdateVirtualPeriod(+1)` on ACTIVE / SCHED timeout, `(-1)` on IDLE
into Yield. Perf maps that onto `schedule_period`. `MaybeUpdateSchedulePeriod`
runs after a successful STOP.

`ScheduleTimerPeriod` is `min(owner.schedule_period, requester.deadline)`.
Deadline comes from `REQUEST` `arg1` when timeset is set.

Swimlane: [`flows.md`](flows.md) F9.

## Flows

Arm time-slice examples (guide 4.3.7) in Savior terms.

```text
 no other requester (Arm "No Yield")
   UOS REQUEST → GRANT → ACTIVE
   SCHED never starts (no Pending)
   UOS IDLE → Yield; GPU idle until next REQUEST

 other VM waiting (Arm "Normal / Early Yield")
   SOS Granted+ACTIVE, UOS REQUEST → UOS Pending
   SCHED timeout (request_timeout) or SOS IDLE
     STOP IRQ  (Arm ARB_VM_GPU_STOP / YIELD_NOW)
     STOPPING timer  (yield_timeout)
   SOS STOPPED → UnmapGpu → Grant UOS

 yield fails (Arm "Failure to Yield" / GPU_LOST)
   STOPPING timeout
     SOS → RCVR IRQ + Recovery timer
     not SOS → Lost exception, then grant Pending
```

```text
 SOS mali_arb          Savior GpuScheduler           UOS mali_arb
      │                      │                           │
      │ Granted, ACTIVE      │                           │
      │                      │◄──── SMC REQUEST ─────────┤
      │                      │  UOS Pending              │
      │                      │  start SCHED              │
      │◄── STOP IRQ ─────────┤  SCHED fire               │
      │  drain GPU           │                           │
      ├──── SMC STOPPED ────►│  DisableGpu               │
      │  Idle                │  EnableGpu UOS            │
      │                      ├──────── GRANT IRQ ───────►│
      │                      │                           │ map MMIO, ACTIVE
```

### 1. Create (SOS first)

```text
 lua / JSON  setArbiterGpuEnable / Region / Irqs / SchedTime / SchedMode / SchedVcpu
 Vmm::prepareGpuSchedService
    read DTS reserved-memory  compatible = "mediatek,sw-arbiter-aee"
    GET_IRQ("GRANT"|"STOP"|"LOST"|"RCVR"|"MALI-JOB"|…)
    GET_SOS_TIMER_TIMEOUT / GET_UOS_TIMER_TIMEOUT
    gpu_regions  (paddr, size, flag)*
 GpuSchedulerServiceImpl::createGpuSched
    paged VMO 4K  SPECIFIC guest IPA 0x88000b000  (CACHED)
    optional physical AEE VMO
    zx_gpu_scheduler_create          GpuSchedDispatcher
    zx_gpu_scheduler_set_vmo         kernel UNCACHED map of same VMO
    zx_gpu_scheduler_set_aee_vmo
    zx_gpu_scheduler_set_mode
    zx_gpu_scheduler_set_timeslice   SOS timers
    PublishService  FIDL GpuScheduleService
    DTS /arbiter  mali,sw-arbiter  + interrupt-names
    optional /arbiter-aee  mali,sw-arbiter-aee
    GPUEB  compatible = "mediatek,me_GPUEB_SHARED"  → set_gpueb_shmem_vmo
 StartVcpu
    addVcpu(vcpu, affinity)
      affinity = id in arbiter-gpu-vcpus
      empty list → VCPU 0
      product lua sets VCPU 4 (`setArbiterGpuSchedVcpu(4)`)
      RegisterFastInterrupt(RCVR)
      first VCPU  InitVmInfo: stage-2 GPU regions + AddVm
```

UOS:

```text
 GpuSchedulerClient::prepareGpuSched
    FIDL GetSchedHandle / Irq / Reg / Time / Rsv / AeeInfo
    map same IPA 0x88000b000, set_vmo, set_timeslice (UOS timers)
    tbox never calls this client (SOS VMM only creates the
    scheduler if user vmid == SOS)
 add_arbiter_dts   same mali,sw-arbiter node
 addVcpu
```

`InitVmInfo` builds `VmObjectPhysical` per GPU region, cache policy
`UNCACHED_DEVICE`, `CreateVmMapping` at the **same IPA as paddr**
(identity in the guest GPA). If the range was already mapped
(passthrough) or `ARB_GPU_REGION_PIN`, it `MapDevice` immediately;
otherwise the PTE stays unmapped until GRANT.

### 2. REQUEST → GRANT (idle GPU)

```text
 UOS mali_arb  SMC REQUEST
   HandleRequest
     no current_vmidx_  →  GrantVm
       EnableGpu
         MapGpu     AddressSpace::MapDevice  (drop spinlock)
         EnableGpuIrq
           SetDispatchIrqCpu(owner vcpus)
           register_int_handler(mali irq, dispatch_irq)
           unmask
       state = Granted, current_vmidx_ = UOS
       InjectGrant   virtual IRQ GRANT
       StartTimer PREPARING
 UOS  GRANT IRQ  →  mali_arb  programs GPU
 UOS  SMC ACTIVE
   stop PREPARING
   NotifyVmReady: if ALPS, write GPUEB magic 0x4147414D454D4E4F
                  at offset SWA_GPUEB_MAGIC_OFFSET
```

### 3. Preempt (SOS owns, UOS REQUEST)

```text
 SOS  Granted + ACTIVE
 UOS  SMC REQUEST
   UOS → Pending, current_req_vmidx_ = UOS
   ScheduleCurrentOwnerForPendingRequester
     owner Granted and (ACTIVE already or schedule_direct)
       StartTimer SCHEDULE  period = min(SOS slice, UOS deadline)
 SCHED timeout
   StopCurrent
     SOS → Stopping
     InjectStop  STOP IRQ
     StartTimer STOPPING
 SOS mali_arb  drains GPU, SMC STOPPED
   ReleaseCurrent  SOS → Idle, UnmapGpu, dummy Mali handlers
   GrantPendingRequester  → GrantVm(UOS)  as in (2)
```

If SOS is already **Yield** (IDLE with no Pending, then UOS REQUEST):
`ScheduleCurrentOwnerForPendingRequester` calls `StopCurrent`
immediately — no SCHED wait.

If SOS sends `IDLE` while UOS is Pending: same `StopCurrent`.

`STOPPED` with `require=1` and no Pending: stay Granted, GRANT
again, PREPARING. Used when the guest stopped only to flush, then
wants the GPU back.

### 4. Hang / recovery

```text
 STOPPING timer fires
   current is SOS?  HandleStopTimeout(recovery=true)
     RcvrVm  Recovery + RCVR IRQ + RECOVERY timer
     SOS mali_kbase reset, SMC RECOVERED
       ReleaseCurrent, GrantPendingRequester
   else
     LostVm  InjectException(0xABCE1235) on a running VCPU
 RECOVERY timer fires
   LostVm
 UOS destroy / reboot
   RevokeGpu  RemoveVm(vmid, recovery_vmid = SOS)
     SOS may be granted as fallback
```

Android reboot while Yocto holds a GRANT: STOP timeout on Android
goes Lost; if Android was owner, SOS can recover.

### 5. Mali IRQ while granted

```text
 Mali  JOB/MMU/GPU/EVT/PWR  physical SPI
   Savior GIC  registered handler dispatch_irq
   GpuSchedIntrHandler::DispatchIrq
     pick_running_vcpu  STATE_INIT | RUNNING
     PhysicalInterrupt(vector)   HW bit in ICH_LR
   guest mali_kbase
 DisableGpuIrq  (on ReleaseCurrent)
   mask, reset, dummy_irq  (log + EOI deactivate)
   ClearDispatchIrqVcpu
```

Notify IRQs pick a running VCPU from **that VM's** `vcpus` vector
(affinity VCPU is index 0). Mali IRQs use the handler's current
dispatch set, which `EnableGpuIrq` overwrites to the new owner.

## Resources

### GPU MMIO

`MapGpu` / `UnmapGpu` walk `VmInfo::mapping_regions`. PIN regions
are never unmapped. Map/unmap go through
`Guest::AddressSpace()->MapDevice` / `UnMapDevice` — stage-2 PTEs
for device memory, then TLB invalidate. See [`memory.md`](memory.md).

Enable/Disable drop the scheduler spinlock and may re-enable IRQs
around the aspace walk (`ints_enabled_before_lock_`).

### Command / AEE / GPUEB

| Buffer | Where | Use |
| --- | --- | --- |
| cmdbuf | IPA `0x88000b000`, 4 KiB | guest map CACHED; kernel `SetVmo` UNCACHED ([`shm.md`](shm.md)) |
| AEE | DTS `mediatek,sw-arbiter-aee` / `mali,sw-arbiter-aee` | KE/HWT dump `SYS_SWA_AEE_RAW` |
| GPUEB | `mediatek,me_GPUEB_SHARED` | Android ready magic, written on first ALPS ACTIVE |

GPUEB (`gpu_sched_shm.h`): VMO from DTS `reg`, mapped cached.
Magic at **byte offset `0x3FC0`** (`SWA_GPUEB_BASE_ADDR =
0x4000-0x40`), 8 bytes `0x4147414D454D4E4F`.
`NotifyVmReady` writes it **only for ALPS** (`USER_VMID_ALPS`),
on the first successful ACTIVE after grant. SOS ACTIVE does not
write. Idle ATF while a guest still runs can power-gate GPUEB
and lose this page — that is why `is_all_vm_suspend()` is in
the idle loop ([`pm.md`](pm.md)).

AEE layout: 64-byte header, then two VM blocks (SOS index 0, ALPS
index 1). Header words `[offset, size]` per VM. Each block is a
ring of `TrackerItemType` `{timestamp, value, arg, type}`. Parse:

```text
python3 ./swa_tool/parse_swa_aee.py SYS_SWA_AEE_RAW
```

History also dumps from `nblruncmd -t k arbiter vmdump` into Yocto
kmsg. EventType is `SMC` / `IRQ` / `SYS`. IRQ EventValue is the
vector; SMC EventValue is the command name.

## Config

`ArbiterGpuSpec` (lua / JSON via `GuestConfig`):

| field | meaning |
| --- | --- |
| `enable` | create the scheduler |
| `regions` | `(paddr, size, flag)*` GPU MMIO |
| `irqs` | name → GIC intid (`GRANT`, `STOP`, `LOST`, `RCVR`, `MALI-JOB`, …) |
| `sos_timer` / `uos_timer` | `schedule` / `stopping` / `recovery` / `preparing` ns |
| `sched_mode` | 0 NORMAL, 1 SIMPLE |
| `vcpus` | which VCPU ids are GPU-affinity (`arbiter-gpu-vcpus`) |
| `is_reserved_mem` | also publish `mali,arbiter_res_mem` in DTS |

Empty `vcpus` falls back to **VCPU 0**. Product lua sets
`setArbiterGpuSchedVcpu(4)` so affinity is 4. Affinity VCPUs are
inserted at the **front** of `VmInfo::vcpus`, so IRQ inject prefers
them (`pick_running_vcpu` walks INIT|RUNNING in order).

Product (`sos_mt8678.lua`):

```text
 enable           true
 region           paddr 0x48000000  size 0x480000  flag 0
 GRANT/STOP/LOST/RCVR    4316 / 4317 / 4318 / 4319
 MALI-GPU/MMU/JOB/EVENT/PWR   636 / 637 / 638 / 639 / 640
 affinity VCPU    4
```

QEMU lua does **not** call `setArbiterGpuEnable`.

Syscalls: create refuses PHYP / QEMU_AHYP / OKL4_AHYP; later
calls require `BOOTMODE_AHYP`.

```text
zx_gpu_scheduler_create
zx_gpu_scheduler_add_vcpu
zx_gpu_scheduler_set_vmo
zx_gpu_scheduler_set_aee_vmo
zx_gpu_scheduler_set_gpueb_shmem_vmo
zx_gpu_scheduler_set_timeslice
zx_gpu_scheduler_set_mode
zx_gpu_scheduler_destroy_vm     (RevokeGpu)
```

## Terminology

Arm names from 112001_0100_01_en. Savior column is what this tree
actually has.

| Term | Arm meaning | Savior |
| --- | --- | --- |
| VM / guest | software domain that gets the GPU | SOS (VMID 1), UOS/ALPS (2), tbox (3) |
| Driver instance | per-guest GPU kernel driver | `mali_kbase` + `mali_kbase_arbiter_swa.c` |
| GPU subinstance | execution HW the current owner drives | the one Mali |
| Access window | guest-visible MMIO + IRQ; open/closed | **emulated**: stage-2 map = open |
| Access Manager | GPU HW that opens/closes windows, reset | **none**; `GpuSchedBackend` |
| Resource Group | arbiter↔guest message / partition config pages | **none**; SMC + notify IRQs |
| Partition | hardware-isolated execution domain (AE) | **none** |
| Time slicing | share one subinstance over time | SWA; exclusive GRANT |
| Arbiter | who runs next, when to yield | `GpuScheduler` in the host kernel (EL1h) |
| Hypervisor | S2, SMMU, IRQ routing, security | Savior; same object as arbiter |
| Yield | owner stops so another guest can run | STOP IRQ; guest `STOPPED` / `IDLE` |
| `YIELD_NOW` / `YIELD_IDLE` | firmware stop now / after drain | STOP is now; IDLE is guest-voluntary |
| `YIELD_DONE` | firmware finished yield (async) | not used |
| Handover | yield → close → reset → open next | `StopCurrent` → `DisableGpu` → `GrantVm` |
| Reset | drop previous guest's GPU state | implicit in unmap + next kbase init; no AM reset |
| `request_timeout` | how long owner runs before switch | SCHEDULE timer (slice) |
| `yield_timeout` | how long stop may take | STOPPING timer |
| GPU_LOST | forced close, lost work | exception `0xABCE1235` / RCVR |
| StreamID | GPU DMA ID: AW index + protected bit | SMMU SID; not retuned on GRANT |
| AXI-subordinate | CPU MMIO into GPU | stage-2 `MapDevice` |
| AXI-manager | GPU DMA to memory | SMMU STE S2 |
| SWA | software virtualization, remap each grant | **this product** |
| HWA | Access Window hardware | not in this SoC path |
| CSF | command stream frontend (5th gen) | guest firmware while granted |
| panthor | Arm reference Linux driver/arbiter | analogue: kbase + Savior `GpuScheduler` |
| GRANT / STOP / LOST / RCVR | Savior notify IRQs | virtual inject to owner VM |
| `PhysMem` / cmdbuf | IPA `0x88000b000` | paged VMO; guest CACHED, kernel UNCACHED; not an AW page |

## Tests

`zircon/kernel/arch/arm64/hypervisor/gpu_sched/tests` — host-side
unit tests with `FakeGpuBackend`. `make` in that directory; all
cases must PASS.

## Code

| Step | File |
| --- | --- |
| SMC trap | `zircon/kernel/arch/arm64/hypervisor/vmexit.cpp` `FC_ID_GPU_ARBITER` |
| SMC ids | `zircon/trusty/lk/trusty/include/pal/hvcall.h` |
| Scheduler | `zircon/kernel/arch/arm64/hypervisor/gpu_sched/gpu_sched.cpp` |
| State machine | `gpu_sched_state_machine.cpp` |
| Map / unmap / AEE / GPUEB | `gpu_sched_vm_resources.cpp` |
| Mali / notify IRQ | `gpu_sched_interrupts.cpp` |
| Backend / timers | `gpu_sched_backend_adapter.cpp` |
| Types / cmd ids | `zircon/kernel/arch/arm64/include/arch/gpu_sched/` |
| Dispatcher / syscalls | `zircon/kernel/object/gpu_sched_dispatcher.cpp` `zircon/kernel/syscalls/gpu_scheduler.cpp` |
| VMM SOS create | `garnet/bin/guest/vmm.cc` `prepareGpuSchedService` |
| VMM UOS DTS | `garnet/bin/guest/main_uos.cc` `add_arbiter_dts` |
| Product lua | `yocto/prebuilt/hypervisor/grt/auto8678p1_64_sos_indep/sos_mt8678.lua` |
| Syscalls | `zircon/kernel/syscalls/gpu_scheduler.cpp` (not `hypervisor.cpp`) |
| FIDL / client | `garnet/lib/machina/fidl/gpu_scheduler.fidl` `gpu_scheduler.cc` |
| Config | `garnet/bin/guest/guest_config.h` `ArbiterGpuSpec` |
| VCPU pick | `garnet/lib/machina/gpu_scheduler_vcpu_selection.h` |
| Arm model | `Arm Mali GPU 5th Generation and Later Virtualization Guide` 112001_0100_01_en |

---

Prev: [`vhm.md`](vhm.md) · Next: [`tee.md`](tee.md) · [`README.md`](README.md)

Everyday's a Hustle.
