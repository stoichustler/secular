# GPU

Crook shares one physical **Mali GPU** between SOS (Yocto) and UOS
(Android / ALPS). The product path is Arm **SWA**: software
virtualization, no Mali Access Window / Resource Group, no hardware
partition. The hypervisor is the **Arbiter**. Guests talk to it
through **SMC** (Comms) and four **notify IRQs** (GRANT / STOP / LOST
/ RCVR). Ownership is exclusive: only the granted VM has GPU MMIO in
stage-2 and Mali IRQs dispatched onto its VCPUs.

QEMU virt has a separate **virtio-gpu** 2D scanout. That is a display
device, not SWA. SWA syscalls refuse `BOOTMODE_PHYP`,
`BOOTMODE_QEMU_AHYP`, and `BOOTMODE_OKL4_AHYP`; they only run on
`BOOTMODE_AHYP`.

Read [`vmm.md`](vmm.md) for SOS / UOS lifecycle,
[`memory.md`](memory.md) for `MapDevice` / stage-2,
[`interrupt.md`](interrupt.md) for `PhysicalInterrupt` vs virtual
inject, [`scheduler.md`](scheduler.md) for VCPU pin.

This note is the map of SWA: objects, message protocol, state
machine, grant / stop / recovery flows, IRQ dispatch, MMIO map, and
the QEMU virtio-gpu path.

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
 │ Guest EL1  SMC  →  Crook EL2 vmexit  handle_smc_instruction    │
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
Android.

## Arm GPU virt methods

SWA is the one Crook implements. The other two are Mali hardware
features the SoC would need; they are not in this tree.

| Method | Hardware | Who owns the GPU | Crook |
| --- | --- | --- | --- |
| **SWA** | none | one VM at a time; hypervisor maps MMIO + IRQs | yes |
| **HWA** | Access Window per VM (addr / message buffer / IRQ) + Resource Group | Arbiter still picks the window | no |
| **Hardware separation** | Partition = independent GPU | no time-share | no |

SWA cost is **context switch**: unmap previous guest, map next, inject
GRANT. Mali itself is not partitioned.

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

Command buffer: 4 KiB VMO mapped into the guest at IPA
`0x88000b000` (`kShareMemBase`) and into the kernel as
`GpuSchedCmdType*`. Guest DTS node `/arbiter` `reg` points at that
IPA. The SMC payload is the command id in `x2`; the buffer holds
trace / AEE side data, not the command itself.

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
| `DESTROY` | 0x07 | sys / revoke | drop VM (UOS reboot uses this path) |
| `DUMP` | 0x08 | VM → Arbiter | dump history |
| `TIMERSET` | 0x09 | VM → Arbiter | reset a timer period (internal) |
| `ASSIGN` | 0x11 | SOS only | SIMPLE mode: force grant |

`REQUEST` / `STOPPED` `arg0` bits (when `WITH_GPU_SCHEDULER_OPTIONS`):

| bit | REQUEST | STOPPED |
| --- | --- | --- |
| 0 | — | `STOPPED_REQUIRE_ENABLE` request again after stop |
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
| 0 | GRANT | 4320 | you may use the GPU; MMIO is mapped |
| 1 | STOP | 4321 | stop using the GPU; reply `STOPPED` |
| 2 | LOST | — | GPU was taken; current code injects a VCPU **exception** `0xABCE1235` instead of the LOST IRQ (`#if 0` around the IRQ inject) |
| 3 | RCVR | 4323 | run GPU recovery; reply `RECOVERED`. Registered as a **fast interrupt** on add-vcpu |

### Mali IRQs  (physical, `VcpuDispatcher::PhysicalInterrupt`, HW LR)

| index | name | DTS interrupt-names |
| --- | --- | --- |
| 0 | JOB | `MALI-JOB` |
| 1 | MMU | `MALI-MMU` |
| 2 | GPU | `MALI-GPU` |
| 3 | EVT | `MALI-EVT` / config `MALI-EVENT` |
| 4 | PWR | `MALI-PWR` |

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

| type | default (lua / spec) | Fired |
| --- | --- | --- |
| SCHEDULE | SOS/UOS 16 ms | owner has had the GPU long enough; STOP |
| STOPPING | UOS 5 s (SOS from config) | guest ignored STOP; SOS → recovery, else Lost |
| RECOVERY | config | recovery hung; Lost |
| PREPARING | config | guest got GRANT but no ACTIVE; log only |

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

## Flows

### 1. Create (SOS first)

```text
 lua / JSON  setArbiterGpuEnable / Region / Irqs / SchedTime / SchedMode / SchedVcpu
 Vmm::prepareGpuSchedService
    read DTS reserved-memory  compatible = "mediatek,sw-arbiter-aee"
    GET_IRQ("GRANT"|"STOP"|"LOST"|"RCVR"|"MALI-JOB"|…)
    GET_SOS_TIMER_TIMEOUT / GET_UOS_TIMER_TIMEOUT
    gpu_regions  (paddr, size, flag)*
 GpuSchedulerServiceImpl::createGpuSched
    VMO 4K  map guest IPA 0x88000b000
    optional physical AEE VMO
    zx_gpu_scheduler_create          GpuSchedDispatcher
    zx_gpu_scheduler_set_vmo         kernel maps cmdbuf
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
      empty list → VCPU 4 is the affinity VCPU (legacy)
      RegisterFastInterrupt(RCVR)
      first VCPU  InitVmInfo: stage-2 GPU regions + AddVm
```

UOS:

```text
 GpuSchedulerClient::prepareGpuSched
    FIDL GetSchedHandle / Irq / Reg / Time / Rsv / AeeInfo
    map same IPA 0x88000b000, set_vmo, set_timeslice (UOS timers)
    tbox (user vmid 1) skips AEE VMO
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
   Crook GIC  registered handler dispatch_irq
   GpuSchedIntrHandler::DispatchIrq
     pick_running_vcpu  STATE_INIT | RUNNING | SUSPEND
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
| cmdbuf | IPA `0x88000b000`, 4 KiB, uncached | `GpuSchedCmdType`; debug latency stamps |
| AEE | DTS `mediatek,sw-arbiter-aee` / `mali,sw-arbiter-aee` | KE/HWT dump `SYS_SWA_AEE_RAW` |
| GPUEB | `mediatek,me_GPUEB_SHARED` | Android ready magic, written on first ACTIVE |

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

Empty `vcpus` falls back to VCPU 4 as the affinity CPU. Affinity
VCPUs are inserted at the **front** of `VmInfo::vcpus`, so IRQ
inject prefers them (`pick_running_vcpu` walks in order).

Syscalls (AHYP only):

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

## QEMU virtio-gpu (not SWA)

QEMU guests get `machina::VirtioGpu`: virtio id GPU, queues
CONTROLQ + CURSORQ, 2D only. Commands:
`GET_DISPLAY_INFO`, `RESOURCE_CREATE_2D`, `RESOURCE_UNREF`,
`SET_SCANOUT`, `RESOURCE_ATTACH_BACKING`, `TRANSFER_TO_HOST_2D`,
`RESOURCE_FLUSH`, cursor update/move.

UOS DTS can add `nbl,virtio_gpu_irq` / vsock nodes for a **paravirt
display** path. That does not time-share Mali. `virtgpu_dbg` in the
hypervisor is a debug hook on `FC_ID_INJECT_HW_IRQ`, not the
arbiter.

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
| FIDL / client | `garnet/lib/machina/fidl/gpu_scheduler.fidl` `gpu_scheduler.cc` |
| Config | `garnet/bin/guest/guest_config.h` `ArbiterGpuSpec` |
| VCPU pick | `garnet/lib/machina/gpu_scheduler_vcpu_selection.h` |
| QEMU 2D GPU | `garnet/lib/machina/virtio_gpu.cc` |

---

Everyday's a Hustle.
