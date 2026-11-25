# Objects

**After this:** you can name every kernel object a Vmm holds, the
syscall that creates it, and what closing the last handle does.
**First read:** Principle + diagrams 1–4 + Guest / Vcpu tables.
**Next:** [`fidl.md`](fidl.md). Prev: [`microkernel.md`](microkernel.md).

Zircon is object-based. Hypervisor state is not a global “KVM
module”. It is `GuestDispatcher` / `VcpuDispatcher` /
`GpuSchedDispatcher` sitting in a process handle table, same as
`Process` / `Thread` / `VMO`. This note is those objects, their
rights, and the syscalls that act on them.

Read [`microkernel.md`](microkernel.md) for handle/rights,
[`cpu.md`](cpu.md) for Resume, [`memory.md`](memory.md) for the
guest VMAR, [`interrupt.md`](interrupt.md) for passthrough,
[`gpu.md`](gpu.md) for SWA, [`vmm.md`](vmm.md) for process
lifetime.

## Principle

1. **A Dispatcher is the kernel object.** Userspace sees a
   32-bit handle. Syscalls look up type + rights in **this**
   process’s table. Wrong type or missing right → error, no
   side effect.

2. **`Guest` is the VM; `Vcpu` is one CPU of that VM.**
   `zx_guest_create_new` returns two handles: Guest and a
   **guest VMAR** (stage-2 aspace). `zx_vcpu_create` takes the
   Guest handle. The VCPU thread is an LK `thread_t` with
   `THREAD_FLAG_VCPU`.

3. **Default rights are the session.** Guest:
   `ZX_RIGHTS_BASIC | WRITE`. Vcpu:
   `BASIC | IO | EXECUTE | SIGNAL`. Resume needs `EXECUTE`.
   Interrupt needs `SIGNAL`. Trap needs Guest `WRITE`.

4. **Last handle close is teardown.** There is no kernel “VM
   table” that outlives the Vmm process. `~GuestDispatcher`
   drops `Guest` → VTTBR, traps, VCPUs gone.

5. **Product create is `zx_guest_create_new` on AHYP.** It
   refuses PHYP / QEMU_AHYP / OKL4_AHYP (not TEE).
   `zx_guest_create` is the PHYP path (physmem VMO + `SetupPHyp`);
   it does **not** refuse AHYP.

## Framework

```text
 hypervisor Resource                 (who may create a Guest)
        │ zx_guest_create_new
        ▼
 GuestDispatcher  ──►  Guest
    gpas_     GuestPhysicalAddressSpace   VTTBR
    traps_    TrapMap                     BELL / MEM holes
    vcpus_[]  Vcpu*
    vmid_     1 / 2 / 3
    user_vmid −1 / 0 / 1
        │ zx_vcpu_create
        ▼
 VcpuDispatcher  ──►  Vcpu
    thread_      THREAD_FLAG_VCPU, pinned pCPU
    el2_state_   GPRs + sysregs
    gich_state_  bitmap + ICH_LR
    guest_       back-ref

 GpuSchedDispatcher  ──►  GpuScheduler     (SoC)
 InterruptEventDispatcher                  passthrough SPI
 PortDispatcher                            GUEST_BELL
 VmObjectDispatcher                        RAM, shm, ioreq
 VmAddressRegionDispatcher                 guest VMAR (S2)
 ChannelDispatcher                         FIDL (fidl.md)
 WakeupSourceDispatcher                    idle ATF gate
```

## Diagrams

### 1. Create graph (AHYP)

```text
 Vmm  (needs hypervisor resource handle)
    zx_guest_create_new(resource, 0, user_vmid, &guest, &vmar)
       validate ZX_RSRC_KIND_HYPERVISOR
       Guest::Create
          Alloc VMID, empty gpas_ + traps_
       GuestDispatcher + VmAddressRegionDispatcher(RootVmar)
    MapRange / physical VMO onto guest VMAR     (setVmem)
    zx_guest_set_trap(guest, BELL|MEM, ipa, len, port, key)
    zx_vcpu_create(guest, hcr_ignored, entry, &vcpu)
       pin bind_cpus[id], El2State, THREAD_FLAG_VCPU
    zx_vcpu_resume(vcpu, &packet)
```

### 2. Handle table of `file://sos`

```text
 SOS Vmm process
    Guest            WRITE          S2 + traps + VMID 1
    VMAR             BASIC          guest physical maps
    Vcpu[0..n]       BASIC|IO|EXECUTE|SIGNAL  (no READ)
    Port             WRITE          BELL waiter
    Interrupt[]      IO             passthrough SPI
    VMO[]            MAP/RW         RAM, occupancy, ioreq, cvm-lock
    Channel          IO             GuestLifecycle, VhmService, …
    GpuScheduler     (SoC)          SWA
    Resource         HYPERVISOR     create only
```

UOS Vmm is a **different** table. No shared `Vcpu` handle →
ioreq + kick SPI ([`vhm.md`](vhm.md)).

### 3. Rights check (one syscall)

```text
 sys_vcpu_resume(handle, packet)
    ProcessDispatcher::GetCurrent()
    GetDispatcherWithRights(handle, ZX_RIGHT_EXECUTE, &vcpu)
       missing handle          ZX_ERR_BAD_HANDLE
       not VcpuDispatcher      ZX_ERR_WRONG_TYPE
       no EXECUTE              ZX_ERR_ACCESS_DENIED
    VcpuDispatcher::Resume
```

Duplicate the handle without `EXECUTE` and Resume is gone;
the object is unchanged.

### 4. Lifetime

```text
 create syscall  →  Dispatcher refcount = 1 (the handle)
 duplicate       →  refcount++
 kernel pointer  →  refcount++  (e.g. Vcpu holds GuestDispatcher)
 close handle    →  refcount--
 last ref 0      →  destructor
    ~VcpuDispatcher   Stop thread, FreeVpid
    ~GuestDispatcher  ~Guest: unmap S2, drop traps
    ~GpuSchedDispatcher  revoke maps
```

Vmm process death closes every handle in one shot.

## Guest

Kernel: `Guest` (arch) + `GuestDispatcher` (object).
User handle type `ZX_OBJ_TYPE_GUEST`.
Default rights: `ZX_RIGHTS_BASIC | ZX_RIGHT_WRITE`.

| Field | Role |
| --- | --- |
| `gpas_` | stage-2 aspace; `VTTBR` root |
| `traps_` | IPA holes: BELL / MEM / IO |
| `vmid_` | kernel 1/2/3 |
| `user_vmid_` | FIDL −1/0/1 |
| `vcpus_[]` | up to `VM_MAX_VCPU_NUM` |
| `cpu_binding_table_` | VCPU → pCPU |
| `guest_state_` | `GUEST_PM_*` |
| `sched_irq_` / `cvm_lock_irq_` / `smc_irq_` | virtual doorbells |
| wakeup IRQ list | s2idle `CheckSkipWakeup` |

User vmid → kernel VMID: `user + 2` (SOS −1→1).

### Syscalls on a Guest handle (`WRITE` unless noted)

| Call | Effect |
| --- | --- |
| `zx_guest_create_new` | AHYP: Guest + guest VMAR. Needs hypervisor **resource** |
| `zx_guest_create` | PHYP only: physmem VMO + `SetupPHyp` |
| `zx_guest_set_trap` | unmap IPA, record kind + optional Port |
| `zx_guest_remove_trap` | undo |
| `zx_guest_set_cpu_affinity` | bind table |
| `zx_guest_set_user_vmid` | −1/0/1; tbox first time `init_mtk_spm_map` |
| `zx_guest_set_wakeup_irqs` | s2idle list; `0xdeadbeef` cut_point required |
| `zx_guest_set_smc_irq` | VIRTUAL doorbell for VM ringbuf; not TEE complete |
| `zx_guest_set_cvm_lock_irq` / `_vmo` | [`guest.md`](guest.md) |
| `zx_guest_tee_vm_create` / `_destroy` | [`tee.md`](tee.md) |
| `zx_guest_set_sched_id` | occupancy row (lua `setSchedId`) |

Trap kinds (this tree; no `VHM_*`):

| `kind` | Abort becomes | Who waits |
| --- | --- | --- |
| `ZX_GUEST_TRAP_BELL` (0) | `ZX_PKT_TYPE_GUEST_BELL` | trap **port** |
| `ZX_GUEST_TRAP_MEM` (1) | `ZX_PKT_TYPE_GUEST_MEM` | `zx_vcpu_resume` |
| `ZX_GUEST_TRAP_IO` (2) | x86 IO; ARM unused | — |

Unknown kind → `ZX_ERR_INVALID_ARGS`. BELL is write-only.
MEM with `ISV=0` → `ZX_ERR_IO_DATA_INTEGRITY`, not a Kick.

## Vcpu

Kernel: `Vcpu` + `VcpuDispatcher`. Type `ZX_OBJ_TYPE_VCPU`.
Default rights: `BASIC | IO | EXECUTE | SIGNAL`.
Create needs Guest `WRITE`. `hcr` argument is **ignored**.

| Field | Role |
| --- | --- |
| `thread_` | pinned `thread_t`, `THREAD_FLAG_VCPU` |
| `el2_state_` | guest GPRs + sysregs; `tpidr_el2` in guest |
| `gich_state_` | pending bitmaps + `ICH_LR` |
| `vcpu_group_` | `WITH_CROSS_VM_SCHED` hint / occupancy |
| power | `INIT` / `RUNNING` / `SUSPEND` / `STANDBY` / `PWROFF` |

### Syscalls on a Vcpu handle

| Call | Rights | Effect |
| --- | --- | --- |
| `zx_vcpu_create` | Guest WRITE | pin, `STATE_RUNNING`, SOS `SOS_STATE` SMC |
| `zx_vcpu_resume` | EXECUTE | enter guest; return packet or error |
| `zx_vcpu_interrupt` | SIGNAL | VIRTUAL inject |
| `zx_vcpu_stop` | SIGNAL | stop loop |
| `zx_vcpu_read_state` | READ | `ZX_VCPU_STATE` → `zx_vcpu_state_t` (x0–x30, sp, cpsr) |
| `zx_vcpu_write_state` | WRITE | same; used to set DTB in x0 |
| `zx_vcpu_inject_exception` | WRITE | pending DABT, next enter |
| `zx_vcpu_on` / `zx_vcpu_off` | EXECUTE | also PSCI CPU_ON/OFF |
| `zx_vcpu_dump_state` | READ | watchdog proto dump |
| `zx_system_suspend` | EXECUTE | STR helper |
| `zx_wakeup_guest_os` | — | `HYPERVISER_GUEST_OS_RESUME` from host |

Resume return (to Vmm):

| `zx_vcpu_resume` | Packet / error | Vmm |
| --- | --- | --- |
| `ZX_OK` + `GUEST_MEM` | MMIO abort | HandleMem |
| `ZX_OK` + `GUEST_VCPU` | STARTUP / VHM_REQ / STOP / ON / OFF | HandleVcpu |
| `ZX_ERR_CANCELED` | PSCI RESET | `REBOOT_REQUIRED` |
| `ZX_ERR_UNAVAILABLE` | PSCI OFF | `SHUTDOWN` |
| `GUEST_BELL` | **not** here | trap port waiter |

`GUEST_VCPU` subtypes: `INTERRUPT` 0, `STARTUP` 1, `VHM_REQ` 2,
`OFF` 3, `ON` 4, `KILLED` 5, `STOP` 6.

Physical IRQ during guest is swallowed inside Resume
(`ZX_ERR_NEXT` from EL2) unless the thread is killed.

## GpuScheduler

Type `ZX_OBJ_TYPE_GPU_SCHEDULER`. One per SoC, created by SOS
Vmm, handle duplicated to UOS via FIDL `GetSchedHandle`.
AHYP only.

| Call | Effect |
| --- | --- |
| `zx_gpu_scheduler_create` | irq arrays + MMIO regions |
| `zx_gpu_scheduler_add_vcpu` | register that VM’s VCPUs; `affinity` = GRANT VCPU |
| `zx_gpu_scheduler_set_vmo` | cmdbuf VMO per VM |
| `zx_gpu_scheduler_set_aee_vmo` / `_gpueb_shmem_vmo` | debug / magic |
| `zx_gpu_scheduler_set_timeslice` | PREPARING / SCHED / STOPPING periods |
| `zx_gpu_scheduler_set_mode` | NORMAL / SIMPLE |
| `zx_gpu_scheduler_destroy_vm` | on `HC_DESTROY_VM` |

Guest protocol is SMC `FC_ID_GPU_ARBITER`, not these syscalls.
[`gpu.md`](gpu.md).

## El2State / IchState / GichState / TrapMap

Not Zircon handles. They live **inside** `Vcpu` / `Guest`.

**`El2State`** — one page, `tpidr_el2` while in guest
(`el2_state.h`, `sizeof <= PAGE_SIZE`):

```text
 resume / passive_mode
 guest_state
    x[31]  fp  system_state (TTBR/SCTLR/VBAR/ELR/SPSR/VMPIDR…)
    cntv_ctl / cntv_cval
    esr_el2, far_el2, hpfar_el2, cntvoff_el2
 host_state     x18–x30 + host sysregs
 ich_state      num_lrs, vmcr, elrsr, apr[][], lr[16]
```

**`GichState`** — software pending, not the LR file:

```text
 interrupt_tracker_   PHYSICAL | VIRTUAL bitmaps (to 65535)
 Wait / Signal        WFI and s2idle
 timer / vtimer_timer CNTV oneshot; cancel in destructor
 lr_reg_nums_         from ICH_VTR (≤ 16)
```

`Wait` returns immediately if any LR is in use
(`IsUsingListRegister`).

**`TrapMap`** — IPA → `{kind, port, key}`. `zx_guest_set_trap`
inserts and `UnmapRange`s that IPA in `gpas_`. Find on DABT
([`cpu.md`](cpu.md) F4).

## Resource

`ZX_OBJ_TYPE_RESOURCE`. Two ioctls on `/dev/misc/sysinfo`:

| Ioctl | What you get |
| --- | --- |
| `GET_ROOT_RESOURCE` | duplicate of kernel root, **TRANSFER only** |
| `GET_HYPERVISOR_RESOURCE` | `zx_resource_create(root, ZX_RSRC_KIND_HYPERVISOR)` |

`Guest::Init` uses **hypervisor** resource for
`zx_guest_create_new` / GPU create (`validate_resource`).
Physical VMOs (`zx_vmo_create_physical`) use the root resource.
Job (`PA_JOB_DEFAULT`) only allows spawning processes, not
creating a Guest. [`microkernel.md`](microkernel.md) diagram 11.

## Guest VMAR / VMO

`zx_guest_create_new` returns a VMAR whose maps are **stage-2
PTEs**, not host user maps. `zx_vmar_map` of a physical VMO
with `ZX_VM_FLAG_SPECIFIC` at GPA is identity (GPA = HPA) for
product RAM and passthrough MMIO.

| Policy (lua) | Object |
| --- | --- |
| `kCached` / `kDevice` | physical VMO, MapRange |
| `kDemand` | anonymous VMO, fault in on DABT |
| `kMpu` | no map (SMPU hole) |

Shared pages (occupancy, ioreq, cvm-lock, ipc-mbox) are VMOs
mapped into **host** and into **guest S2**. [`memory.md`](memory.md).

## Port

`ZX_OBJ_TYPE_PORT`. Default rights **omit `ZX_RIGHT_WAIT`**:
do not `zx_object_wait_one` a port. Drain with `zx_port_wait`
(IO). `zx_guest_set_trap(..., port, key)` for BELL: abort is
queued on that port, Resume returns `ZX_OK` with **no** packet.
Trapbell-handler-N is `zx_port_wait`. MEM / VHM_REQ return
from `zx_vcpu_resume` on the VCPU thread. [`microkernel.md`](microkernel.md)
diagram 13.

## Interrupt

`ZX_OBJ_TYPE_INTERRUPT`. `zx_interrupt_bind` claims a host SPI.
`zx_interrupt_bind_vcpu(interrupt, vcpu, exclusively)` makes it
passthrough: host handler `PhysicalInterrupt` into that VCPU
(HW bit). Default interrupt rights **cannot DUPLICATE**.
[`interrupt.md`](interrupt.md).

## Channel / Event / Socket

Not hypervisor-specific, but the Vmm is made of them.

| Object | Hypervisor use |
| --- | --- |
| Channel | FIDL ([`fidl.md`](fidl.md)); VHM `_VHM_USE_CHANNEL_` |
| Event | ioreq completion (`VmContext::ioreq_event_[]`) |
| Socket | virtio-console / Inspect `FetchGuestSerial` |
| FIFO | unused on this path |

Channel default rights **cannot DUPLICATE** (TRANSFER yes):
one stream, one reader. Move an end with FIDL `request<T>` or
processargs. Interrupt handles also drop DUPLICATE.
[`microkernel.md`](microkernel.md) diagram 12.

## Process / Thread / Job

How the Vmm exists at all.

```text
 Job  (root)
   Process  file://sos     handle table above
     Thread  VCPU 0        zx_vcpu_resume loop
     Thread  VCPU 1..n
     Thread  trapbell-handler-N
     Thread  vhm-sos-thread
     Thread  virtio backends
```

`CreateApplication` is `zx_process_create` + `zx_thread_start`
plus a processargs Channel: `PA_JOB_DEFAULT`, VMAR, vDSO VMO,
`PA_USER0` = GuestLifecycle server end. Full type table:
[`microkernel.md`](microkernel.md) diagram 14.
[`bootstrap.md`](bootstrap.md) B5–B7, [`vmm.md`](vmm.md).

## WakeupSource / TIPC

| Object | Role |
| --- | --- |
| `WakeupSourceDispatcher` | `zx_wakeup_source_lock` / `_unlock`; idle ATF only if none locked ([`pm.md`](pm.md)) |
| `TipcPort` / `TipcChan` | HEE Trusty TAs (`zx_tipc_*`). Guest vtee is virtio, not this object ([`tee.md`](tee.md)) |

## IOMMU / BTI

`ZX_OBJ_TYPE_IOMMU` / `BTI` / `PMT`: host SMMU. Guest vSMMU is
**emulated in EL0** (`machina::Smmuv3`); nested STE still uses
the Guest `VTTBR`. QEMU: DummyIommu. [`devices.md`](devices.md).

## Object type ids (kernel)

From `diagnostics.cpp` (`ZX_OBJ_TYPE_LAST == 33`). Hypervisor
ones in **bold**.

| Type | Name |
| --- | --- |
| process / thread / job | tasks |
| vmo / vmar | memory |
| channel / socket / fifo / event / event-pair / port | IPC / wait |
| interrupt / resource / log / timer | drivers |
| **guest / vcpu** | hypervisor |
| iommu / bti / pmt / profile / pci-device | DMA / PCI |
| tipc-port / tipc-chan | HEE Trusty |
| **gpu-scheduler** | SWA (`ZX_OBJ_TYPE_GPU_SCHEDULER`) |

## Pitfalls

- **`zx_vcpu_create` hcr.** Ignored. HCR mask is in `Vcpu::Create`.
- **PHYP vs AHYP create.** Product uses `_new`. `zx_guest_create`
  + physmem VMO is PHYP.
- **BELL port vs MEM packet.** Mixing them on one BAR is SYNC
  hang or lost write.
- **No `Vcpu` handle across processes.** That is why VHM exists.
- **Interrupt cannot duplicate.** Bind once; exclusive vs
  broadcast is the `exclusively` flag, not a second handle.
- **Close Guest while Resume.** VCPU thread is killed; Vmm sees
  stop. GuestManager may relaunch.

## Terminology

| Term | Meaning here |
| --- | --- |
| Dispatcher | C++ kernel object |
| Guest / Vcpu | VM and one CPU of it |
| Guest VMAR | stage-2 aspace handle from `create_new` |
| Resource | capability to create a Guest |
| Trap | unmapped IPA + kind + optional Port |
| Packet | `zx_port_packet_t` from Resume or BELL port |

## Code

| Step | File |
| --- | --- |
| Rights | `zircon/system/public/zircon/rights.h` |
| Trap / VCPU state | `zircon/system/public/zircon/syscalls/hypervisor.h` |
| Syscalls | `zircon/kernel/syscalls/hypervisor.cpp`, `gpu_scheduler.cpp` |
| GuestDispatcher | `zircon/kernel/object/guest_dispatcher.cpp` |
| VcpuDispatcher | `zircon/kernel/object/vcpu_dispatcher.cpp` |
| GpuSchedDispatcher | `zircon/kernel/object/gpu_sched_dispatcher.cpp` |
| Guest / Vcpu | `zircon/kernel/arch/arm64/include/arch/hypervisor.h` |
| Object types | `zircon/kernel/object/diagnostics.cpp` |

---

Prev: [`microkernel.md`](microkernel.md) · Next: [`fidl.md`](fidl.md) · [`README.md`](README.md)

Everyday's a Hustle.
