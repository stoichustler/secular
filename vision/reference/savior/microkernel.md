# Microkernel

**After this:** you can say what the Nebula kernel does, what it
refuses to do, and how a `Guest` is the same kind of thing as a
`Process`. **First read:** Principle + diagrams 1–6, then 11–19.
**Next:** [`objects.md`](objects.md) · [`fidl.md`](fidl.md).

Source name is **Nebula** (`nbl`, `gz_nebula`). Product name of
the HEE image is Savior. The host is **Zircon**: a microkernel,
not Linux. The three Linux guests are payloads. This note is that
split — objects, handles, syscalls, userspace servers — and how
the hypervisor plugs into it.

Read [`README.md`](README.md) for the five hypervisor facts,
[`bootstrap.md`](bootstrap.md) for userboot, [`cpu.md`](cpu.md)
for `svc` vs `HVC #4`, [`vmm.md`](vmm.md) for the Vmm process.

The other notes assume this page: “userspace owns policy, kernel
owns the `eret`” is a **microkernel rule**, not a hypervisor
slogan.

## Principle

1. **The kernel is small on purpose.** It owns processor time,
   address spaces, interrupts, signaling, and (on this tree)
   guest enter/exit. It does **not** own filesystems, TCP,
   virtio, lua, ACRN `HC_*` decode, or GICD MMIO. Those are EL0
   processes talking through handles.

2. **Userspace never holds a kernel pointer.** A handle is a
   32-bit integer in **that** process’s table: `{Dispatcher*,
   rights, process}`. The same integer in another process is a
   different object or invalid. Rights live on the handle, not
   on the object.

3. **Last handle close destroys the object.** `Guest` /
   `Vcpu` die with the Vmm process. There is no kernel daemon
   that keeps a VM alive after `file://sos` exits.

4. **A syscall is `svc` on the host.** Guest `smc` is an
   intercept on the **same** VCPU thread (`HVC #4` / `eret`).
   Those two paths do not share a vector table. Guest Linux
   `open` is not a Zircon syscall.

5. **Three fabrics, one kernel.** Host EL0 ↔ EL0 is a Channel
   (FIDL). Kernel → Vmm is a Port packet (`zx_vcpu_resume`
   return, or `GUEST_BELL` on a trap port). Guest → host is
   an SMC / abort the kernel either handles or posts. Do not
   mix them.

## Framework

```text
 EL0   userspace servers                         (policy, emulate, FIDL)
         userboot → devmgr → appmgr → sysmgr
           GuestManager          catalog, relaunch, GRT WDT
           Vmm  file://sos       lua, VGIC MMIO, virtio, VHM BE
           Vmm  file://uos       VhmDevice, GPU client
           Vmm  tbox             console
           DDK drivers           UART, disk, …  (host devices)
              │  zx_*  (vDSO, svc)     handles + rights
              ▼
 EL1h  Zircon microkernel                        (mechanism)
         Dispatcher objects
           Job / Process / Thread / VMO / VMAR
           Channel / Port / Event / Interrupt
           Guest / Vcpu / GpuScheduler
         LK scheduler, PMM, GIC, EL2 trampoline
              │  HVC #4 / eret
              ▼
 EL1h  guest Linux   SOS / UOS / tbox            (payloads)
```

Linux is the **guest**. Zircon is the **host**. There is no
Linux hypervisor module.

## Diagrams

### 1. Linux monolith vs Nebula microkernel

Same SoC, opposite split. Virtio and VHM live in EL0 **because**
the kernel will not.

```text
 Linux (not this host)                 Nebula / Zircon (this host)
 ┌──────────────────────────┐          ┌──────────────────────────┐
 │  kernel                  │          │  EL0  Vmm, virtio, VGIC  │
 │   VFS, TCP, virtio,      │          │       lua, VHM, WDT      │
 │   sched, mm, irq, kvm?   │          │  ─ ─ ─ zx_* / FIDL ─ ─ ─ │
 │                          │          │  EL1  objects, sched,    │
 │  user  qemu / crosvm     │          │       mm, irq, eret      │
 └──────────────────────────┘          └──────────────────────────┘
```

On this tree VHM and MSI are EL0 (`ZX_PKT_GUEST_VCPU_VHM_REQ`).
That is the microkernel choice: `venus-hee.mk` has **no**
`WITH_VHM_IN_KERNEL`.

### 2. Two exception paths (host syscall ≠ guest intercept)

```text
 Host EL0  (Vmm thread)                    Guest EL1h  (same thread)
    zx_vcpu_resume()                          smc / wfi / abort
         │ svc                                     │ HCR.TSC / TWI
         ▼                                         ▼
    VBAR_EL1  exceptions.S                    VBAR_EL2  el2.S
         sys_vcpu_resume                           el2_guest_exit
         Vcpu::Resume                              HCR=RW, VTTBR=0
         HVC #4 ──────────────────────────────►    eret host EL1
              guest runs
              intercept ──────────────────────►    packet or ZX_OK
         eret EL0t  (syscall returns)
```

`svc` is how **userspace asks the kernel**. `HVC #4` is how
**that same thread enters the guest**. [`cpu.md`](cpu.md).

### 3. Object, Dispatcher, handle, rights

Every resource the Vmm touches is this shape.

```text
  kernel heap
    GuestDispatcher  ──►  Guest  (VTTBR, traps, vcpus_[])
    VcpuDispatcher   ──►  Vcpu   (thread_t, El2State, ICH_LR)
    VmObjectDispatcher ─► VMO
    PortDispatcher   ──►  port   (trap BELL)

  Vmm process handle table
    h1  GuestDispatcher*     RIGHT_WRITE
    h2  VcpuDispatcher*      RIGHT_EXECUTE | SIGNAL | READ | WRITE
    h3  VMAR                 guest physical aspace
    h4  Port                 GUEST_BELL waiter

  syscall
    sys_vcpu_resume(h2)
       GetDispatcherWithRights(h2, ZX_RIGHT_EXECUTE)
       wrong type / missing right  →  error, no eret
```

A second handle to the same `Guest` can have fewer rights
(`zx_handle_duplicate`). The object does not store “who may
resume”.

### 4. Job / Process / Thread  ≅  Guest / Vcpu

Same nesting, different payload.

```text
 Job          (policy: who may create what)
   Process    (one aspace, one handle table)
     Thread   (one LK thread_t, runs EL0)

 Guest        (one stage-2 aspace, one trap map, one VMID)
   Vcpu       (one LK thread_t with THREAD_FLAG_VCPU, eret EL1h)
```

`zx_process_create` + `zx_thread_start` is how sysmgr launches
Vmm. `zx_guest_create_new` + `zx_vcpu_create` is how Vmm
launches SOS. Closing the process closes every handle; closing
the last `Guest` handle tears down stage-2.

### 5. Handles one Vmm holds

```text
 file://sos process
    Guest            zx_guest_create_new
    guest VMAR       S2 maps (setVmem)
    Vcpu[0..n]       zx_vcpu_create, pin
    Port             zx_guest_set_trap  BELL
    Interrupt        passthrough SPI bind
    Channel          FIDL to GuestManager
    Channel          FIDL VhmService (UOS Vmm is the client)
    VMO              occupancy shm, ioreq, cvm-lock
    GpuScheduler     SoC only
```

UOS Vmm is a **different** process, different handle table,
different `Guest`. SOS cannot `zx_vcpu_resume` a UOS VCPU:
it has no handle.

### 6. Syscall path (vDSO)

Zircon syscalls are C functions in a kernel-provided vDSO
(`libzircon`), mapped into every process next to userboot.

```text
 Vmm  zx_vcpu_resume(vcpu, &packet)
    libzircon.so  (vDSO, user RX, kernel-owned pages)
       svc
          │
          ▼
    VBAR_EL1  iframe
    arm64_syscall
       ProcessDispatcher::GetDispatcherWithRights
       sys_vcpu_resume
          VcpuDispatcher::Resume
             Vcpu::Resume          (cpu.md F2)
    eret EL0t
       ZX_OK + packet     →  Vmm HandlePacket
       ZX_ERR_CANCELED    →  REBOOT_REQUIRED
       ZX_ERR_UNAVAILABLE →  SHUTDOWN
```

There is no `int 0x80` Linux ABI on the host. Guest Linux has
its own `svc` into **guest** `VBAR_EL1`, under stage-2.

### 7. Kernel posts a packet (it will not emulate)

Microkernel rule: GICD / virtio / VHM BAR are userspace.
The kernel only classifies the abort.

```text
 guest  str  GICD IPA
    EL2 DABT
    traps.FindTrap  GUEST_TRAP_MEM
    fill zx_port_packet_t
       type = ZX_PKT_TYPE_GUEST_MEM
    return ZX_ERR_NEXT from Resume
         │
         ▼
 Vmm  zx_vcpu_resume returns ZX_OK + packet
    HandleMem  →  GicDistributor / virtio / VhmDevice
    emulate, maybe WriteState
    loop resume
```

| Packet `type` | Who waits | Typical |
| --- | --- | --- |
| `GUEST_MEM` | VCPU thread (`zx_vcpu_resume`) | VGIC, virtio, VHM SYNC |
| `GUEST_BELL` | trap port waiter | VHM BELL, posted write |
| `GUEST_VCPU` | VCPU thread | STARTUP, VHM_REQ, STOP, ON/OFF |
| `GUEST_IO` | (x86) | not this ARM tree |

`GUEST_VCPU` subtypes: `INTERRUPT`, `STARTUP`, `VHM_REQ`,
`OFF`, `ON`, `KILLED`, `STOP`. ACRN `HC_*` is `VHM_REQ`:
kernel copies GPRs, SOS Vmm decodes `x1`.

### 8. FIDL is not a syscall

GuestManager never calls `zx_vcpu_resume`. It talks to the
Vmm process over a Channel.

```text
 GuestManager process                  Vmm process
    lifecycle.Create  ──── channel ────►  VmmController::Create
    lifecycle.Run     ──── channel ────►  StartPrimaryVcpu
                                              zx_vcpu_resume  (svc)
    lifecycle.Stop    ──── channel ────►  teardown, close handles
```

FIDL is EL0 ↔ EL0. The kernel only moves bytes and handles
on that Channel. Policy (relaunch, 1 s delay, catalog) is
GuestManager. [`vmm.md`](vmm.md).

### 9. Last handle close

```text
 GuestManager  Kill / Vmm exit
    process dies
       close every handle in its table
          last GuestDispatcher handle
             ~GuestDispatcher
                Guest destroy
                   VTTBR pages gone
                   Vcpu threads gone
                   traps gone
```

A SOS reboot is: that process exits, GuestManager waits 1 s,
`CreateApplication("file://sos")` again, **new** handles, new
`Guest`. Not a kernel “reset VM” ioctl. [`vmm.md`](vmm.md) F12.

### 10. Who handles this event?

```text
 event
   ├─ host EL0 needs a kernel object
   │     syscall  zx_*                 (this page diagram 6)
   ├─ host EL0 needs another process
   │     Channel / FIDL                (diagram 8)
   ├─ guest instruction / MMIO / IRQ
   │     EL2 intercept
   │        kernel can finish?  WFI, PSCI, SGI, demand fault, GPU SMC
   │           ZX_OK, loop Resume
   │        Vmm must emulate?  GICD, virtio, VHM BAR, HC_*
   │           packet, return to syscall
   │        TEE?
   │           TeeSmcHandler, still kernel, not a packet
   ├─ host EL0 fault
   │     exception port walk  ([`exceptions.md`](exceptions.md))
   ├─ host EL1 unhandled
   │     exception_die → ATF WDT
   └─ guest Linux syscall / S1 abort S2 allows
         guest VBAR_EL1, Savior never sees it
```

### 11. Who may create a Guest (Job + Resource)

Not “any process with a Vmm binary”. Two capabilities:

```text
 Job     PA_JOB_DEFAULT at process start
    may zx_process_create children (CreateApplication)
    does **not** by itself allow zx_guest_create_new

 Resource  ZX_RSRC_KIND_HYPERVISOR
    sysinfo  IOCTL_SYSINFO_GET_HYPERVISOR_RESOURCE
       zx_resource_create(root, KIND_HYPERVISOR, 0, 0)
    Guest::Init  ioctl → handle
    sys_guest_create_new  validate_resource(that handle)
```

Root resource (`GET_ROOT_RESOURCE`) is duplicated with
**TRANSFER only** — enough to mint a hypervisor resource or
create a physical VMO, not a blank cheque. No hypervisor
resource → no Guest. Job only decides **who may spawn the Vmm
process** that then asks sysinfo.

### 12. Why Channel cannot `DUPLICATE`

`ZX_DEFAULT_CHANNEL_RIGHTS` = `ZX_RIGHTS_BASIC` **minus**
`ZX_RIGHT_DUPLICATE`, plus IO / SIGNAL / SIGNAL_PEER.
**TRANSFER stays.** You can **move** an end into another
process (FIDL `request<T>`, processargs). You cannot mint a
second handle to the same end.

Two readers on one stream would race (bytes/handles delivered
once). Same as a Unix pipe. Interrupt handles also drop
DUPLICATE (one bind). Port drops **WAIT** (see 13), not
DUPLICATE.

### 13. Port vs `zx_object_wait_one`

| Wait | Syscall | This tree |
| --- | --- | --- |
| One object, signal bits | `zx_object_wait_one` | Event (`ZX_USER_SIGNAL_0` watchdog / ioreq); process `ZX_TASK_TERMINATED` |
| Queue of packets | `zx_port_wait` | BELL trap port (`trapbell-handler-N`) |
| VCPU intercept | `zx_vcpu_resume` **returns** the packet | MEM / VHM_REQ / STARTUP — **not** a port |

Port default rights **strip `ZX_RIGHT_WAIT`**, so
`zx_object_wait_one(port)` is the wrong API even if you hold
the handle. `zx_port_wait` uses IO. MEM traps must not be
bound to the BELL port: Resume would return `ZX_OK` with no
packet and the VCPU thread would spin.

### 14. processargs (what a newborn process gets)

Message in a Channel at start (`zx_proc_args`: protocol,
argv, environ, handle-info array). `zx_get_startup_handle(PA_HND(type, arg))`.

| Type | Meaning | Vmm |
| --- | --- | --- |
| `PA_PROC_SELF` / `PA_THREAD_SELF` | self | libc |
| `PA_JOB_DEFAULT` | job for children | CreateApplication |
| `PA_VMAR_ROOT` / `PA_VMAR_LOADED` | aspace | maps |
| `PA_VMO_VDSO` | vDSO image (dup, RX, not W) | already mapped; needed to spawn children |
| `PA_VMO_STACK` / `PA_VMO_EXECUTABLE` | stack / ELF | loader |
| `PA_SVC_LOADER` | loader channel | dynamic linker |
| `PA_NS_DIR` | namespace | `/svc`, `/dev` |
| `PA_FDIO_*` | stdio / fd table | libfdio |
| `PA_DIRECTORY_REQUEST` | `/svc` server | outgoing services |
| `PA_RESOURCE` | (devmgr) root-ish | not GuestLifecycle |
| `PA_USER0` | **GuestLifecycle server end** | `VmmController` bind |
| `PA_USER1` / `PA_USER2` | spare | unused here |

vDSO is mapped **by the creator** before entry; `PA_VMO_VDSO`
is so this process can map it into **its** children. Guest
Linux never sees these handles.

### 15. Job policy and OOM importance

**Policy** (`zx_job_set_policy`, topic `ZX_JOB_POL_BASIC`):
`RELATIVE` fills holes from the parent; `ABSOLUTE` must match
or the syscall fails. Conditions (`policy.h`): `BAD_HANDLE`,
`WRONG_OBJECT`, `VMAR_WX`, `NEW_VMO` / `NEW_CHANNEL` / …
`NEW_PROCESS`, plus this tree `ZX_POL_TRUSTY` /
`CALL_TRUSTY_API`. Actions: `ALLOW`, `DENY`, `EXCEPTION`,
`KILL` (implies DENY). A process inherits the cookie at
`ProcessDispatcher` create (`policy_(job_->GetPolicy())`).
`QueryPolicy(ZX_POL_NEW_CHANNEL)` gates `zx_channel_create`.

This product does **not** deny Vmm creating channels or guests
via job policy; the hypervisor **resource** is the gate
(diagram 11). Policy is how a parent *could* sandbox a child.

**OOM importance** (`ZX_JOB_IMPORTANCE_MIN=0` …
`MAX=255`, or `INHERITED=-1`). Root job is MAX. A child
defaults INHERITED and is inserted on a global list **just
less important** than its youngest sibling (older, closer to
root = more important). `oom_lowmem` walks
`ForEachJobByImportance` (least first) and `Kill()`s the first
job that still has **running** process descendants. Vmm sits
under sysmgr, near the root — killed late. There is no
“immortal” bit (TODO in `glue.cpp`).

### 16. Handle table (what the 32-bit integer is)

```text
 kernel arena   256K Handle slots
    base_value = index | (generation << shift)
    generation ++ on reuse of the slot

 process
    handle_rand_   PRNG, bit31 and bits 1:0 clear
    user zx_handle_t = handle_rand_ XOR ((base_value << 1) | 1)
       always odd, never 0  (ZX_HANDLE_INVALID = 0)

 lookup
    handle_id = (value XOR mixer) >> 1
    Handle::FromU32  →  arena + index, check generation
    process_id_ must match this process
```

Close: erase from the process list, `HandleOwner` destructor
`Delete`s the slot (generation left in free memory for next
alloc). A stale user integer fails generation or `process_id`.
`Dup` is a new slot, new generation, possibly fewer rights.
Table is a doubly-linked list under `handle_table_lock_`, not
a hash of the integer.

### 17. vDSO ABI

`syscalls.abigen` lists every `zx_*`. The generator emits
`libzircon` stubs (user RX) and kernel `sys_*` dispatch.
Creator maps the vDSO VMO into the new aspace **before** entry
(`vdso_code_mapping_`); userspace cannot remap it writable.
`zx_vcpu_resume` is one row:

```text
 syscall vcpu_resume (vcpu: handle) returns (status, packet OUT);
```

User: C ABI in the vDSO → `svc` with that syscall number.
Kernel: `exceptions.S` → `arm64_syscall` →
`sys_vcpu_resume`. The kernel does **not** export a table at a
stable numeric ABI for apps to `svc` by hand; the contract is
the vDSO text. Guest Linux has its own syscall ABI under
stage-2 and never calls these numbers.

### 18. DDK (host drivers, not guest)

Host devices are EL0 processes. They talk to the kernel with
**DDK syscalls** (`syscalls/ddk.cpp`), not FIDL, for IRQ/IOMMU
/physical VMO:

```text
 zx_interrupt_create(root_resource)     InterruptEventDispatcher
 zx_interrupt_bind(handle, slot, vector, flags)
 zx_interrupt_wait(handle) → slots      event_wait on the dispatcher
 zx_interrupt_bind_vcpu(interrupt, vcpu, exclusive)
    passthrough SPI → PhysicalInterrupt  (HW LR)
 zx_vmo_create_physical(root, paddr, size)
 zx_iommu_* / zx_bti_*                  SMMU
```

`WaitForInterrupt` loops: exchange signal bits, optional
mask-after-wait / unmask-before-wait, then `event_wait`. That
is how a UART driver blocks. Hypervisor passthrough **does
not** use that wait: bind_vcpu steals the SPI into the VCPU.
FIDL is still how two EL0 servers talk; DDK is EL0 ↔ kernel
for hardware.

### 19. Channel internals

Two `ChannelDispatcher`s share a `PeerHolder`. Write:

```text
 Write(msg)
    no peer?  PEER_CLOSED, handles returned to caller
    peer_->WriteSelf(msg)
       if a Call waiter has matching txid  →  Deliver to that thread
       else  messages_.push_back
             ZX_CHANNEL_READABLE
```

`Call` = write + wait on this thread’s `MessageWaiter` keyed
by `txid` (FIDL request/response). A second `Call` on the same
thread without finishing the first **kills the process** (vDSO
contract). Close last handle: `on_zero_handles` drops `peer_`;
the other end reads `PEER_CLOSED`. Handles in a
`MessagePacket` are **moved** out of the sender’s table on
write and installed in the receiver’s table on read — that is
why FIDL `GetSchedHandle` is a move.

## What the kernel refuses

| Want | Lives in | How |
| --- | --- | --- |
| Launch / relaunch a VM | GuestManager EL0 | FIDL `Create` / `Run` |
| lua maps, VGIC MMIO, virtio | Vmm EL0 | `zx_guest_set_trap` + HandleMem |
| ACRN `HC_*` decode | SOS Vmm EL0 | packet `VHM_REQ` |
| UOS BAR emulate | SOS Linux + HSM | ioreq + kick SPI |
| Occupancy actuator | guest `nebula_sched` | shm + virtual IRQ |
| Filesystem, TCP | not in HEE kernel | guest Linux, or GzFS vsock BE in SOS Vmm |
| Mali time-slice | **kernel** `GpuScheduler` | exception: exclusive map must be atomic with IRQ bind |

GPU grant is in-kernel because `MapDevice` + PHYSICAL Mali IRQs
must not race a second VM. That is still a small object
(`GpuSchedulerDispatcher`), not a userspace driver walking S2.

## Objects (kernel)

| Dispatcher | User sees | Wraps |
| --- | --- | --- |
| `ProcessDispatcher` | process handle | aspace + handle table |
| `ThreadDispatcher` | thread handle | `thread_t` |
| `VmObjectDispatcher` | VMO | pages |
| `VmAddressRegionDispatcher` | VMAR | host or **guest** aspace |
| `ChannelDispatcher` | channel | FIDL / processargs |
| `PortDispatcher` | port | packets, BELL |
| `InterruptEventDispatcher` | interrupt | host GIC SPI |
| `GuestDispatcher` | guest | `Guest` (S2, traps, VMID) |
| `VcpuDispatcher` | vcpu | `Vcpu` (`thread_t` + EL2) |
| `GpuSchedDispatcher` | gpu scheduler | SWA state machine |
| `ResourceDispatcher` | hypervisor resource | who may `zx_guest_create_new` |

LK (`sched.c`, `thread_t`) is underneath. A Channel does not
wrap an LK object. A VCPU does.

## Flows

### Create a guest (microkernel view)

```text
 sysmgr  CreateApplication("file://sos")
    zx_process_create + handles in processargs
 Vmm  Vmm::Initialize
    zx_guest_create_new(resource)     GuestDispatcher + guest VMAR
    MapRange / set_trap               S2 PTEs and holes
    zx_vcpu_create(guest, entry)      VcpuDispatcher, pin
    zx_vcpu_resume                    svc → HVC #4 → guest EL1h
```

### Kernel-handled vs Vmm-handled resume

```text
 zx_vcpu_resume
    guest WFI          kernel Wait, ZX_OK, loop, never returns to Vmm
    guest PSCI CPU_ON  missing VCPU → packet STARTUP → Vmm StartVcpu
    guest GICD write   packet MEM → Vmm GicDistributor
    guest HC_CREATE_VM packet VHM_REQ → SOS VhmServiceImpl
    guest GPU REQUEST  kernel GpuScheduler, ZX_OK, loop
```

Swimlanes: [`flows.md`](flows.md) F2, F3, F7, F8, F9.

## Pitfalls

- **“Nebula is Linux.”** Guests are. The host is Zircon. Host
  `ps` / `ls` do not exist; `devmgr` and `guest_manager` do.
- **Guest `svc` is not `zx_*`.** Guest libc talks to guest
  Linux. Only the VCPU thread’s `svc` is a Zircon syscall.
- **FIDL hang ≠ kernel hang.** GuestManager `Bind` timeout is
  a Channel. `zx_vcpu_resume` stuck is an intercept that never
  returns (TEE SMC, SYNC ioreq, occupancy pause with no work).
- **Handle in the wrong process.** SOS Vmm cannot resume UOS.
  Kick SPI and ioreq exist **because** there is no shared
  `Vcpu` handle.
- **Kernel VHM.** This tree does not have it. Looking for
  `vhm.cpp` is the wrong OS.
- **Rights.** `zx_vcpu_resume` needs `EXECUTE`. `zx_vcpu_interrupt`
  needs `SIGNAL`. A duplicated handle without `EXECUTE` cannot
  `eret`.

## Terminology

| Term | Meaning here |
| --- | --- |
| Microkernel | Zircon host: objects + syscalls; policy in EL0 |
| Dispatcher | C++ kernel object (`GuestDispatcher`, …) |
| Handle | Per-process `{object, rights}` integer |
| Rights | What **this** handle may do (`EXECUTE`, `WRITE`, …) |
| vDSO | Kernel-provided `libzircon.so`; `zx_*` entry |
| `PA_USER0` | processargs: GuestLifecycle server end |
| Hypervisor resource | `GET_HYPERVISOR_RESOURCE`; required by `zx_guest_create_new` |
| Packet | `zx_port_packet_t` kernel → Vmm |
| FIDL | EL0 ↔ EL0 on a Channel |
| Payload | Guest Linux. Not a Zircon process |
| Nebula | Source / image name of this Zircon tree |

## Code

| Step | File |
| --- | --- |
| Objects / handles | `zircon/docs/objects.md`, `handles.md`, `concepts.md` |
| Dispatcher base | `zircon/kernel/object/include/object/dispatcher.h` |
| Guest / Vcpu objects | `guest_dispatcher.h`, `vcpu_dispatcher.h` |
| Syscalls | `zircon/kernel/syscalls/hypervisor.cpp` |
| vDSO | `zircon/docs/vdso.md`, `libzircon` |
| Host `svc` | `zircon/kernel/arch/arm64/exceptions.S` |
| Guest enter | `el2.S` `HVC #4`, [`cpu.md`](cpu.md) |
| First EL0 | [`bootstrap.md`](bootstrap.md) B5–B7 |
| Vmm process | [`vmm.md`](vmm.md) |
| processargs | `zircon/system/public/zircon/processargs.h` |
| Rights | `zircon/system/public/zircon/rights.h` |
| sysinfo ioctls | `zircon/system/dev/misc/sysinfo/sysinfo.c` |
| Job policy | `zircon/system/public/zircon/syscalls/policy.h`, `policy_manager.cpp` |
| OOM | `kernel/object/glue.cpp` `oom_lowmem` |
| Handle arena | `kernel/object/handle.cpp` (256K, generation) |
| Channel | `kernel/object/channel_dispatcher.cpp` |
| DDK IRQ | `kernel/syscalls/ddk.cpp` `sys_interrupt_*` |
| abigen | `zircon/system/public/zircon/syscalls.abigen` |

---

Prev: [`README.md`](README.md) · Next: [`objects.md`](objects.md) · [`fidl.md`](fidl.md)

Everyday's a Hustle.
