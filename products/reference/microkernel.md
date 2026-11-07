# Microkernel

Secular is a Zircon-microkernel console system. The kernel is the
privileged core; drivers, the filesystem, the component tree, and the
serial shell run in user space. This note is the map of that split:
what a microkernel is for, which pieces must stay in the kernel, how
Zircon (and therefore Secular) instantiates that design, how that
differs from a Linux monolithic kernel, and **how the security model
is built on handles, rights, jobs, and userspace capability routing**.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read [`bootstrap.md`](zircon/bootstrap.md) for the boot path,
[`cpu.md`](zircon/cpu.md) for jobs / processes / threads,
[`memory.md`](zircon/memory.md) for VMOs / VMARs,
[`ipc.md`](zircon/ipc.md) for channels and handles,
[`interrupt.md`](zircon/interrupt.md) for GIC / exceptions,
[`virtualization.md`](virtualization.md) for the in-kernel hypervisor.
Fuchsia-layer security (sandboxing, capability routing, namespaces)
is Pagano, Verderame, and Merlo, *Understanding Fuchsia Security*
(2021), [`pdf/understanding-fuchsia-security.pdf`](pdf/understanding-fuchsia-security.pdf).
That paper describes a full Fuchsia stack. This image is **bringup**:
the kernel mechanisms are here; Session Manager, Hub, Dart/Web
runners, FAR package Merkle, and a userspace VFS are not.

Start with **Design core**, then **Security** (framework + diagrams).

## Design core

A **monolithic kernel** puts almost all privileged work in one address
space: scheduling, virtual memory, VFS, drivers, networking, IPC. A
**microkernel** keeps in privileged mode only the mechanisms that
*must* run with the MMU and interrupt controller under their control.
Policy, protocol, and device code live in user processes.

### Framework

```text
┌─────────────────────────────────────────────────────────────────┐
│ User space  (EL0)                                               │
│   drivers, filesystems, network, init / component manager,      │
│   shells, VMM device model, applications                        │
│                                                                 │
│   talk through kernel objects (handles) and shared memory       │
└────────────────────────────┬────────────────────────────────────┘
                             │ syscall / exception / IRQ forward
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│ Microkernel  (EL1, one address space, small TCB)                │
│                                                                 │
│   threads + scheduler                                           │
│   address spaces (page tables, maps)                            │
│   IPC  (messages, signals, wait)                                │
│   interrupt dispatch  (ack, then wake a user driver)            │
│   capabilities  (who may touch which object, with which rights) │
│                                                                 │
│   not in the kernel:  POSIX, VFS, TCP, GPU, UART policy, …      │
└─────────────────────────────────────────────────────────────────┘
```

Four questions decide whether something belongs in the kernel:

| Question | Kernel if |
| --- | --- |
| Does it program the MMU / interrupt controller / CPU mode? | yes |
| Does a bug here take down every process? | only if there is no user-space alternative |
| Does it encode *policy* (which file, which driver, which name)? | **no** — that is user space |
| Can it be a process that talks over IPC? | then it should be |

The usual goals of that split:

- **Least privilege.** A UART driver cannot rewrite another process's
  page tables. A crashed driver is a process crash, not a kernel panic.
- **Small TCB.** The trusted computing base is the kernel plus the few
  user processes that hold powerful handles (root job, resource
  objects, bootstrap channel).
- **Explicit communication.** Processes do not share a global kernel
  namespace. They receive **capabilities** and talk through them.
- **Replaceable policy.** Init, the driver host, the filesystem, and
  the shell can change without a kernel ABI break.

Zircon is a *pragmatic* microkernel, not an L4-pure one. It still owns
VMOs, ports, profiles, clocks, and a hypervisor. It does **not** own a
VFS, a POSIX process model, or in-kernel device drivers.

Security is the same split, asked a different way: the kernel answers
"may this handle do this operation on this object?"; userspace answers
"who is allowed to hold that handle at all?".

## Core components

Every microkernel, including Zircon, is the same five mechanisms with
different names.

### Framework

```text
                    ┌──────────── jobs ────────────┐
                    │  Process                     │
                    │    Thread  ──► Scheduler     │
                    │    VMAR    ──► page tables   │
                    │    handles ──► objects       │
                    └──────────────┬───────────────┘
                                   │
         ┌─────────────┬───────────┼───────────┬──────────────┐
         ▼             ▼           ▼           ▼              ▼
      Thread        Address      IPC        Interrupt     Capability
      + sched       space                   dispatch      (handle)
         │             │           │           │              │
         ▼             ▼           ▼           ▼              ▼
    WaitQueue      VMO / VMAR   Channel    IRQ → Port    rights bits
    Fair/Deadline  aspace       Socket     user driver   per handle
                                FIFO
                                Port / Event
```

| Component | Job | Zircon object / code |
| --- | --- | --- |
| **Thread + scheduler** | Run, block, preempt, migrate | `Thread`, `ThreadDispatcher`, Fair / Deadline ([`cpu.md`](zircon/cpu.md)) |
| **Address space** | Isolate memory; share pages on purpose | `ProcessDispatcher`, `VmAddressRegionDispatcher`, `VmObjectDispatcher` ([`memory.md`](zircon/memory.md)) |
| **IPC** | Move bytes, signals, and capabilities | Channel, Socket, FIFO, Event, Port, futex ([`ipc.md`](zircon/ipc.md)) |
| **Interrupt dispatch** | Hardware IRQ → kernel ack → user wait | `InterruptDispatcher`, GICv3 ([`interrupt.md`](zircon/interrupt.md)) |
| **Capability** | Name an object with rights, transfer it | Handle table: `(object, rights)` per process |
| **Time** | Timers, clocks, deadlines | `TimerDispatcher`, `ClockDispatcher`, per-CPU `TimerQueue` |
| **Resources** | Gate MMIO / IRQ / SMC / hypervisor | `ResourceDispatcher` |
| **Virtualization** | Guest GPA + VCPU (optional) | `GuestDispatcher`, `VcpuDispatcher` ([`virtualization.md`](virtualization.md)) |

User space then builds the rest on those objects:

| Userspace piece | Role on Secular |
| --- | --- |
| **userboot** | First process. Unpacks BOOTFS, starts `component_manager`. |
| **vDSO** | Syscall surface mapped into every process. |
| **component_manager** | Process tree, capabilities, CML. Not a kernel job. |
| **driver framework** | UART / board / virtio as EL0 processes. |
| **console + zxsh** | Serial shell. |
| **VMM** | Device model for Linux / RTOS guests. Kernel only does EL2 + GPA. |

That last row is the KVM-style split: the microkernel holds the
hypervisor *mechanism*; the VMM is an ordinary user process.

## Secular (Zircon / Fuchsia bringup)

Secular is Fuchsia **bringup** renamed. It is not a second kernel. The
design core is Zircon's: kernel objects, handles, rights, and a
userspace component tree.

### Framework

```text
 QEMU aarch64
   ATF → linux-arm64-boot-shim → physboot → vmzircon (EL1)
                                              │
                                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Zircon kernel                                                  │
 │   Job → Process → Thread                                       │
 │   VMAR / VMO                                                   │
 │   Channel / Socket / FIFO / Port / Event                       │
 │   Interrupt, Resource, Guest / Vcpu                            │
 │   handle tables + rights                                       │
 └────────────────────────────┬───────────────────────────────────┘
                              │ userboot (first process)
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ BOOTFS  user space                                             │
 │   component_manager --boot                                     │
 │     driver_manager / board + UART drivers                      │
 │     console-launcher → console → zxsh                          │
 │     guest_manager / vmm  (optional guests)                     │
 └────────────────────────────────────────────────────────────────┘
```

### Design core

**1. Everything a process can name is a kernel object.**
There is no kernel path string, no `inode`, no `uid` check inside
`zx_channel_write`. The process holds a handle. The handle carries
**rights** (`READ`, `WRITE`, `MAP`, `TRANSFER`, …). Drop the handle,
lose the right.

**2. Handles are process-local capabilities.**
A handle value is not a pointer and is not valid in another process
until it is **transferred** (usually as a channel message). That is
how userboot hands `component_manager` the bootstrap channel, and how
FIDL hands a VMO or an interrupt to a driver.

**3. Jobs contain processes contain threads.**
The root job is the capability to create everything else. Killing a
job kills the subtree. There is no `fork`. A new process is
`zx_process_create` + map a VMAR + `zx_process_start` with a thread
and an entry stack.

**4. IPC is messages plus shared memory.**
Channels carry discrete messages (bytes + handles) — FIDL, bootstrap,
component protocols. VMOs carry pages. Drivers typically: channel for
control, VMO or FIFO for bulk.

**5. Policy is userspace.**
`component_manager` decides who gets which capability. The driver host
decides which process binds a fragment. The kernel does not know what
a "console" or a "block device" is — only Interrupt, VMO, Channel,
Resource.

**6. Drivers are processes.**
A bug in the QEMU UART driver is an EL0 crash. The kernel still owns
the GIC and the page tables. Compare Linux: a bug in an in-tree
driver is a kernel oops.

### Flow

Capability passing, not "open a path":

```text
 userboot
   unpack BOOTFS
   zx_process_create(component_manager)
   write bootstrap Channel  (handles: VMOs, job, resource, …)
        │
        ▼
 component_manager
   start driver_manager, console-launcher, …
   each child gets only the handles its CML allows
        │
        ▼
 UART driver
   Resource  → map MMIO VMO
   Interrupt → zx_interrupt_wait
   Channel   → console
        │
        ▼
 zxsh   read/write the console channel  (never /dev/tty in the kernel)
```

Syscall entry is the vDSO (`svc` from EL0, [`cpu.md`](zircon/cpu.md)).
There is no `ioctl` multiplexer on a file descriptor.

## Linux vs Secular

Linux is a **monolithic POSIX kernel**. Secular is a **Zircon
microkernel + userspace bringup**. Same hardware (QEMU virt, GICv3,
EL1), different privilege split.

### Where the work runs

```text
 Linux                                      Secular / Zircon
 ┌──────────────────────────────┐           ┌──────────────────────────────┐
 │ kernel  (one address space)  │           │ kernel                       │
 │  sched, mm, VFS, net,        │           │  sched, mm, IPC, IRQ,        │
 │  drivers, namespaces,        │           │  objects + handles           │
 │  kvm, cgroups, LSM, …        │           └──────────────┬───────────────┘
 └──────────────┬───────────────┘                          │
                │ syscall                                  │ syscall
                ▼                                          ▼
 ┌──────────────────────────────┐           ┌──────────────────────────────┐
 │ user  processes              │           │ user  processes              │
 │  glibc, init, apps           │           │  userboot, component_manager │
 │  (drivers are *not* here)    │           │  drivers, console, zxsh, VMM │
 └──────────────────────────────┘           └──────────────────────────────┘
```

### Comparison

| | Linux | Secular (Zircon) |
| --- | --- | --- |
| **Kernel type** | Monolithic | Microkernel (pragmatic) |
| **Privilege** | Kernel does VFS, net, drivers | Kernel does objects; policy and drivers in EL0 |
| **Process model** | `task_struct`; `fork` / `exec` / `clone` | Job → Process → Thread; `zx_process_create` (no fork) |
| **Identity** | uid/gid, capabilities, LSM | Handle + rights; no kernel uid |
| **Ambient authority** | Yes: a uid can `open("/etc/passwd")` | **No:** you cannot name an object you do not hold |
| **Namespace** | Global VFS, mount, netns, pidns | No kernel path namespace; names live in component_manager / FIDL |
| **File I/O** | `open` / `read` / `write` / `ioctl` on inodes | Channel / Socket / FIFO / Stream / VMO; no in-kernel filesystem |
| **IPC** | pipes, sockets, signals, futex, netlink | Channel (bytes+handles), Socket, FIFO, Port, Event, futex |
| **Shared memory** | `mmap` of files / anon / memfd | VMO + `zx_vmar_map` |
| **Drivers** | `.ko` in kernel AS | EL0 processes; Interrupt + MMIO VMO + Resource |
| **IRQ** | `request_irq` → kernel handler | Kernel acks GIC, signals `InterruptDispatcher`; driver waits in user space |
| **Init** | pid 1 (`init` / systemd) | userboot → `component_manager --boot` |
| **Syscalls** | Large POSIX + Linux set | Small Zircon set via vDSO; `ELR` must be an approved stub |
| **Signals** | POSIX signals | Exceptions + channel / port; no `kill(2)` semantics in the kernel |
| **Failure domain** | Driver bug → oops / panic | Driver bug → process death; kernel continues |
| **Privilege escalation** | uid 0, setuid, LSM bypass | Steal a powerful handle, or compromise kernel / `component_manager` |
| **Hypervisor** | KVM in-kernel + QEMU/crosvm in user space | Same split: `zx_guest` / `zx_vcpu` in kernel, `bin/vmm` in user space |
| **Guest** | Linux as host, anything as guest | Zircon as host; Linux / RTOS as guests on this image |
| **ABI to apps** | POSIX / Linux uapi | Zircon syscalls + FIDL; POSIX would be a user runtime (not in this product) |

### What that means on this tree

- A UART hang on Secular is a **userspace** driver stuck on
  `zx_interrupt_wait` or a channel, not a kernel IRQ thread — unless
  the GIC path itself is wrong ([`interrupt.md`](zircon/interrupt.md)).
- There is no `/proc`, no `task_struct` dump, no `dmesg` in the Linux
  sense. Debug is kcounters, the debuglog, handle tables, and
  userspace inspect.
- Linux-as-**guest** still looks like Linux *inside* the VM. The host
  that traps MMIO is Zircon + `bin/vmm`, not a Linux KVM module
  ([`virtualization.md`](virtualization.md)).
- Do not grow Secular toward a Linux ABI in the kernel. If a POSIX
  layer is needed, it is a user process, not a Zircon object.

### When to pick which picture

| You are looking at | Think |
| --- | --- |
| `zircon/kernel/**` | Microkernel: threads, VM, objects, GIC |
| `userspace/devices/**`, `userspace/sys/**` | Policy and drivers (Linux would have put this in-tree in `drivers/`) |
| `userspace/hypervisor/**` | Userspace VMM (Linux: QEMU); traps handled in Zircon EL2 (Linux: KVM) |
| A Linux **guest** Image | Monolithic guest on top of the microkernel host |
| `make run` serial | `zxsh` over a userspace console stack, not `getty` on a kernel TTY |

## Security

Fuchsia (and therefore Secular) is a **capability-based** OS. Isolation
is the default: a newly created process has no kernel objects, no
memory, and no right to create more until someone **transfers**
handles into it. There is no kernel uid, no global `/`, and no
"root user" that can open any path.

Two layers, not one:

| Layer | Token | Who enforces | What it answers |
| --- | --- | --- | --- |
| **Kernel** | Handle `(object, rights)` | Zircon | May this process invoke this syscall on this object? |
| **Component** | CML capability (`protocol`, `directory`, `service`, …) | `component_manager` | May this component *obtain* a handle / channel to that resource? |

The kernel never sees a component URL. `component_manager` never walks
another process's page tables. A hole in either layer is a hole in
the system: steal a handle, and rights apply; compromise
`component_manager`, and you can mint routing.

This image is bringup. Kernel isolation, handles, jobs, vDSO, and
`component_manager --boot` are here. The paper's Session Manager,
Hub, Dart/Web runners, and FAR Merkle package signing are **not**.

### Framework

Three questions, three mechanisms:

```text
 1. Who may run privileged code / see kernel memory?
        hardware:  EL0 vs EL1, TTBR0 vs TTBR1, AP / UXN / PXN
 2. Who may name a kernel object, and which operations?
        kernel:    handle table + rights bits + job policy
 3. Who is allowed to hold that handle in the first place?
        userspace: bootstrap channel, CML use / offer / expose
```

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ Component  (CML, FIDL)                                                   │
│   use / offer / expose   →  component_manager routes a channel           │
│   namespace is per-component; no global root                             │
└────────────────────────────┬─────────────────────────────────────────────┘
                             │ transfers handles (Channel messages)
                             ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Process  (EL0)                                                           │
│   handle table:  h → (Dispatcher*, rights)                               │
│   one VmAspace (TTBR0)   vDSO mapped RO+X                                │
│   cannot name objects it does not hold                                   │
└────────────────────────────┬─────────────────────────────────────────────┘
                             │ svc only from vDSO stubs
                             ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Zircon kernel  (EL1)                                                     │
│   check ELR ∈ vDSO                                                       │
│   lookup handle; require rights; enforce JobPolicy                       │
│   objects: Job, Process, Thread, VMO, VMAR, Channel, Resource, IRQ, …    │
│   copy user memory through UserInPtr (do not treat user VA as &T)        │
└────────────────────────────┬─────────────────────────────────────────────┘
                             │ MMU / GIC / PSCI
                             ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Hardware  (QEMU virt PE)                                                 │
│   EL0 cannot fetch kernel (UXN on kernel text)                           │
│   EL1 must not execute user (PXN on user text)                           │
│   TTBR1 global; TTBR0 swapped per process                                │
└──────────────────────────────────────────────────────────────────────────┘
```

The TCB is the kernel plus whoever holds the **root job**, the
**system Resource**, and the **bootstrap** ends. On this image that
is userboot, then `component_manager`. A UART driver is **not** TCB.

### Diagrams

Eight posters. Isolation stack, the handle as a capability, how a
process starts empty, how jobs constrain what handles can create,
how CML routes a protocol, how a driver is sandboxed, what a
compromise buys, and how that differs from Linux.

#### 1. Isolation stack

Hardware, kernel objects, then component routing. Each layer is
blind to the one above.

```text
  zxsh  ──console channel──►  console  ──FIDL──►  UART driver
    │                            │                      │
    │         component_manager routes these channels   │
    │         (use / offer / expose in CML)             │
    ▼                            ▼                      ▼
  process A                   process B              process C
  handles: console            handles: uart,         handles:
           stdin/out                   irq, mmio              Resource,
                                                              Interrupt, VMO
                    │                      │
                    └──────────┬───────────┘
                               ▼
                    Zircon objects (koids)
                    Channel / Interrupt / VMO / Resource
                               │
                               ▼
                    EL1  page tables, GIC, scheduler
```

`zxsh` has a console channel. It does **not** have the UART MMIO
VMO or the GIC SPI. The driver has those, and nothing else it was
not offered.

#### 2. A handle is a capability

Not a path, not a uid, not a pointer. Process-local integer naming
`(object, rights)`. Invalid in every other process until transferred.

```text
 process A                         kernel                        process B
 ┌──────────────────┐              ┌────────────┐                ┌──────────────────┐
 │ handle table     │              │ Dispatcher │                │ handle table     │
 │  h=3  READ|WRITE │── same koid ─│  koid K    │─ same koid ──► │  h=7  READ       │
 │  h=4  TRANSFER   │              │  Channel   │                │                  │
 └──────────────────┘              └────────────┘                └──────────────────┘
     local int                          ▲                              local int
     not a pointer                      │
                                 Channel write *moves*
                                 the handle (A loses it)
                                 B gets a new local value
                                 rights ⊆ sender's rights
```

Rights shrink on `zx_handle_duplicate` and on write with a
disposition. `ZX_RIGHT_TRANSFER` is required to put a handle in a
channel. Without `ZX_RIGHT_WAIT` you cannot `zx_object_wait_*`.
Default **channel** rights omit `ZX_RIGHT_DUPLICATE`: an endpoint is
not forked by dup; it is moved.

An attacker who reads `h=3` from A's address space still cannot use
it in B: B's table has different integers, and the kernel looks up
by `(process, handle value)`.

#### 3. Rights bits

Fine-grained operations on the *same* object, per handle.

```text
                 ┌─────────────────────────────────────────┐
                 │ ChannelDispatcher  koid 0x42            │
                 └──────────────┬──────────────────────────┘
            ┌───────────────────┼───────────────────┐
            ▼                   ▼                   ▼
     process A            process B            process C
     h=5                  h=9                  (no handle)
     READ|WRITE|WAIT      READ|WAIT            cannot name it
     TRANSFER             no TRANSFER
                          no WRITE
            │                   │
            │ write messages    │ read only
            │ can send the      │ cannot send the
            │ handle onward     │ handle onward
            ▼                   ▼
         allowed             allowed             ZX_ERR_BAD_HANDLE
```

| Right | Typical meaning |
| --- | --- |
| `DUPLICATE` | `zx_handle_duplicate`; new handle, rights ⊆ old |
| `TRANSFER` | put this handle in a channel message |
| `READ` / `WRITE` | object I/O (channel bytes, VMO contents, …) |
| `EXECUTE` | map executable / run a VCPU |
| `MAP` | `zx_vmar_map` this VMO |
| `WAIT` | `zx_object_wait_*` |
| `INSPECT` | `zx_object_get_info` |
| `MANAGE_JOB` / `MANAGE_PROCESS` / `MANAGE_THREAD` | create children, start, kill |
| `SET_POLICY` / `GET_POLICY` | `zx_job_set_policy` |
| `DESTROY` | close / kill this object |
| `SIGNAL` / `SIGNAL_PEER` | `zx_object_signal` |
| `SAME_RIGHTS` | dup/transfer without shrinking |

Holding a VMO with `READ` but not `WRITE` is a read-only view of
the same pages another process writes. That is how crashlog and
kcounter VMOs are handed to userboot (`userabi.rs` strips
`ZX_RIGHT_WRITE`).

#### 4. Sandbox: a process starts empty

`zx_process_create` yields a process with an empty handle table, a
root VMAR with no mappings, and no threads. It cannot allocate,
map, or talk until the creator installs handles and maps a binary.

```text
 zx_process_create(parent_job)
        │
        ▼
 ┌─────────────────────────────────────────┐
 │ ProcessDispatcher                       │
 │   handle table   empty                  │
 │   VmAspace       empty TTBR0            │
 │   threads        none                   │
 │   rights to the world: none             │
 └─────────────────────────────────────────┘
        │
        │ creator maps ELF + vDSO + stack into the VMAR
        │ creator transfers handles over a channel
        │ zx_process_start(entry, arg0 = bootstrap channel)
        ▼
 process runs — and still only sees what it was given
```

userboot is the first exception that proves the rule. The kernel's
`userboot_init` (`zircon/kernel/lib/userabi/userabi.rs`) **is** the
creator: it builds the process, maps the PIE, and writes two
bootstrap messages (process-self handles, then system capabilities:
ZBI VMO, vDSO, root job, resource, log, …). After that, every other
process on the image is started by a userspace creator
(`component_manager`) the same way.

A component does not always equal one Zircon process (runners can
co-locate). Sandboxing is still at the component: the runner only
hands it the capabilities in the manifest.

#### 5. Job tree: policy and kill domain

Handles cannot express "this process must never create a channel".
Jobs can. Policy is set only while the job is empty, then inherited.

```text
 root JobDispatcher          ← userboot holds this handle
   policy, exception walk, kill subtree
        │
        ├── Job  (component_manager's children)
        │     ZX_POL_NEW_PROCESS  DENY?   no more fork-style spawn
        │     ZX_POL_VMAR_WX      DENY    no W|X mappings
        │     ZX_POL_NEW_CHANNEL  ALLOW
        │        │
        │        └── Process  UART driver
        │              handle table ⊆ what CM offered
        │
        └── Process  userboot → then component_manager
```

| Condition | Meaning |
| --- | --- |
| `ZX_POL_BAD_HANDLE` | use of a closed / forged handle value |
| `ZX_POL_WRONG_OBJECT` | right object type required, wrong one passed |
| `ZX_POL_VMAR_WX` | mapping that is both writable and executable |
| `ZX_POL_NEW_ANY` / `NEW_VMO` / `NEW_CHANNEL` / … | creating that object kind |
| `ZX_POL_NEW_PROCESS` | `zx_process_create` |
| `ZX_POL_AMBIENT_MARK_VMO_EXEC` | marking a VMO executable without a sealed exec path |

Actions: `ALLOW`, `DENY`, `ALLOW_EXCEPTION`, `DENY_EXCEPTION`,
`KILL`. Relative policy (`ZX_JOB_POL_RELATIVE`) fills holes left by
the parent; absolute (`ZX_JOB_POL_ABSOLUTE`) must not conflict.

Killing the job kills every child. That is the containment for a
compromised component: drop its job, the subtree is gone, its
handles close, its channels assert `PEER_CLOSED`.

#### 6. Component capability routing

Kernel handles are the tokens. CML says who may **receive** them.
`use` / `offer` / `expose` build a provider–consumer path on the
component instance tree. Routing only succeeds if the target is in
the tree **and** every hop offers or exposes the capability.

```text
                    A  component_manager  (parent)
                   / \
          (2) expose  (3) offer protocol P to C
                 /     \
                B       C
               /       / \
        (1) expose    D   E  (4) offered P
               P                  E uses P
              /
             F  (server of protocol P)

 E asks for P. CM walks F → B → A → C → E.
 All four hops declare the route. Channel is created;
 E gets one end, F the other. No other component sees P.
```

Fuchsia names seven capability kinds: **Directory, Event, Protocol,
Resolver, Runner, Service, Storage**. On this console image the
ones that matter are **protocol / service** (FIDL to the UART
driver, console, driver_manager) and whatever directory the driver
framework exposes. There is no Session Manager "user experience"
tree and no Hub.

Collections (dynamic children that inherit every capability of the
realm) can over-grant. That is a paper finding, and a reason to
keep realms small.

#### 7. Driver as an untrusted process

The QEMU UART is the running example. Hardware access is a
**Resource** plus an **Interrupt**, not a kernel `.ko`.

```text
 component_manager
   offer:  protocol fuchsia.hardware.serial / device dir
        │
        ▼
 UART driver process
   handles:
     Resource  ZX_RSRC_KIND_MMIO  →  zx_vmo_create_physical / map
     Resource  ZX_RSRC_KIND_IRQ   →  zx_interrupt_create
     Channel   to console / driver_manager
        │
        │  may:  wait the SPI, read/write UART MMIO, talk FIDL
        │  may not:  map another process, program GIC, open "all MMIO"
        ▼
 kernel
   GIC ack  →  InterruptDispatcher  →  zx_interrupt_wait wakes
   PTE for the UART page is in *this* process's TTBR0 only
```

| Kind | Gates |
| --- | --- |
| `ZX_RSRC_KIND_MMIO` | physical address ranges |
| `ZX_RSRC_KIND_IRQ` | SPI / PPI numbers |
| `ZX_RSRC_KIND_SMC` | SMC calls |
| `ZX_RSRC_KIND_SYSTEM` | hypervisor, CPU, … |

The root resource is a bootstrap handle. A driver receives a
**subrange**, not the root. That is how MMIO and IRQ are
least-privilege at the kernel, even before CML.

#### 8. What a compromise buys

```text
 compromise zxsh
   →  console channel, whatever the shell had
   →  not UART MMIO, not root job, not other processes' aspaces

 compromise UART driver
   →  that UART, its IRQ, its FIDL clients
   →  not page tables, not GIC distributor, not the kernel heap
   →  process death restarts the driver; kernel keeps running

 compromise component_manager
   →  can route any capability it holds (root job, resources, …)
   →  this is TCB. Same class as pid 1 + udev on Linux.

 compromise Zircon kernel
   →  everything. Small TCB is the point of the split.

 steal a handle value from another process's memory
   →  useless in yours; the table is process-local
```

### Kernel mechanisms

#### Objects, handles, rights

Every kernel function a process can reach is an operation on an
object it holds. The paper's phrasing: objects isolate tasks;
handles bind a process to an object; rights select the syscalls
that handle may invoke.

Consequences:

- Attack surface of a process is **the objects in its table**, not
  the whole kernel ABI.
- Two processes can share an object with different rights (A writes
  the channel, B only reads).
- Duplicate and transfer never grow rights.
- Closing the last handle can destroy the object (`on_zero_handles`).
  Remaining peers see `PEER_CLOSED`.

Handle lookup is `ProcessDispatcher::handle_table()` →
`get_dispatcher_with_rights::<T>(handle, required_rights)`. Wrong
type is `ZX_ERR_WRONG_TYPE` (and maybe `ZX_POL_WRONG_OBJECT`).
Missing rights is `ZX_ERR_ACCESS_DENIED`.

#### vDSO: the only `svc` sites

Userspace does not `svc` from arbitrary code. Every syscall is a
vDSO stub. The kernel checks that `ELR_EL1` at the `svc` lies in
that process's vDSO mapping; otherwise `ZX_EXCP_POLICY_CODE_BAD_SYSCALL`
([`cpu.md`](zircon/cpu.md) System calls).

```text
 EL0  zx_channel_write  in libzx / vDSO
        mov x16, #ZX_SYS_channel_write
        svc #0                         ← ELR must be this stub
        dsb nsh / isb / brk 0          ← speculation barrier; kernel skips it
 EL1  valid_pc(ELR − vdso_code) ?
        yes → wrapper → sys_channel_write
        no  → sys_invalid_syscall
```

That is a control-flow integrity gate on the syscall surface: a
ROP gadget that issues `svc` from the heap does not become a
syscall. The vDSO is mapped into every process; it is not a
writable library the process provided.

#### Memory isolation

One `VmAspace` per process (`TTBR0` + ASID). Kernel lives in
`TTBR1`, never swapped. EL0 that issues a kernel VA walks TTBR1
and then **faults** on AP/UXN — it does not get a kernel mapping
([`memory.md`](zircon/memory.md)).

| Rule | How |
| --- | --- |
| User cannot execute kernel | kernel text `UXN` |
| Kernel must not execute user | user text `PXN` |
| User cannot write kernel | kernel PTEs `AP` EL1-only |
| W^X in userspace | `ZX_POL_VMAR_WX` |
| User strings are not `&str` | `UserInPtr` copy; may fault |
| Share pages on purpose | VMO mapped in two VMARs |

A VMAR handle with no `ZX_RIGHT_WRITE` cannot punch writable maps.
A VMO handle without `ZX_RIGHT_MAP` cannot be mapped at all.

#### Kernel hardening

Compiled with Clang. Kernel stacks include a **shadow call stack**
(return addresses off the main stack) and **SafeStack** (locals
that may be overflowed stay off the return stack). A buffer
overflow on a kernel local cannot overwrite the saved `lr` on the
safe / shadow stack. The same toolchain is the default for
userspace C.

Syscall dispatch also clears unused GPRs on entry (speculation)
and requires typed `sys_*` wrappers. User copies go through
`UserInPtr` / `UserOutPtr`, not raw dereference.

#### CPRNG

`zx_cprng_draw` is a kernel syscall (ChaCha20). Entropy is
**not** taken from a user driver — drivers are untrusted. The
kernel mixes hardware RNG and jitter entropy
(`zircon/kernel/lib/crypto/global_prng.cc`) and reseeds itself.
That matches the paper: user-mode drivers must not be the source
of kernel randomness.

### Userspace mechanisms

#### Namespaces

There is no kernel `/`. A "path" is a string in a **component
namespace** that `component_manager` assembled from Directory
capabilities. Object paths (handles, files in that namespace) are
not meaningful outside it. Dot-dot traversal of another
component's directory is denied; crossing namespaces is IPC with
an access check, not `open("../other")`.

This image has no userspace VFS product. The console talks a
channel, not `/dev/tty`. The principle still holds: names are not
ambient.

#### FIDL as the ICC surface

Components do not share address spaces. They speak FIDL over a
Zircon channel ([`ipc.md`](zircon/ipc.md)). The protocol is the
security boundary: a client with the channel can call what the
FIDL file exposes, and nothing else. Generated bindings in Rust /
C++ / Go are the same bytes on the wire.

Handles in a FIDL message are kernel capability transfer. That is
how a driver receives an Interrupt or a VMO: not by pathname, by
message.

#### Package integrity (Fuchsia, not this image)

Full Fuchsia signs packages with a Merkle tree in `meta.far`.
`component_manager` recomputes the tree before starting a
component. That detects bit-flips and unsigned edits; it does
**not** stop an attacker who re-signs a malicious package with a
key the device trusts (the paper's repackaging note).

Secular boots a **ZBI + BOOTFS**. Integrity on this path is the
boot chain (QEMU/ATF → shim → physboot → `vmzircon`) and optional
BOOTFS CRC (`userboot.crc`). There is no package server and no
FAR Merkle step on `make run`. Do not describe this console as
an app-store OS.

### TCB and attack surface

```text
 always TCB
   Zircon kernel (sched, VM, IPC, GIC, handle tables, hypervisor)
   vDSO contents
   userboot  (first process; holds root job + resources)
   component_manager --boot  (routes every later capability)

 not TCB on this image
   UART / board / virtio drivers
   console, console-launcher, zxsh
   optional VMM  (a crashed guest or VMM is a process)

 not present (full Fuchsia TCB / surface, omitted here)
   Session Manager, Hub, Dart/Web runners
   netstack, pkg-cache, software delivery
   Starnix (Linux ABI in user space)
```

Small kernel TCB is the microkernel bet. It **moves** trust into
`component_manager`. A bug in capability routing, or a realm that
offers too much to a collection, is as serious as a Linux LSM
hole. Keep CML offers tight; do not hand the root job or the
root resource to a driver.

### Flow

End-to-end, one serial character, with the security checks named:

```text
 zxsh  write(console_channel, bytes)
        │  handle must have ZX_RIGHT_WRITE
        ▼
 console process  FIDL / bytes
        │  only has the UART protocol channel, not MMIO
        ▼
 UART driver
        │  Resource (MMIO subrange) + Interrupt
        │  map UART page in *this* TTBR0
        ▼
 EL1  GIC SPI  →  InterruptDispatcher  →  driver zx_interrupt_wait
        MMIO store goes to QEMU UART; kernel did not parse the byte
```

Boot-time minting of the first capabilities:

```text
 kernel userboot_init
   ProcessDispatcher::create(root job, "userboot")     empty sandbox
   map userboot PIE + vDSO
   bootstrap channel:
     msg1  process-self: log, process, root VMAR, thread, loaded VMAR
     msg2  system: ZBI, vDSO, crashlog, kcounters, root job, resource, …
   zx_process_start(userboot, bootstrap_handle)
        │
        ▼
 userboot  unpack BOOTFS, start component_manager the same way
        │  CM gets a job, VMOs, resource — not every kernel object
        ▼
 component_manager  CML routes  →  driver_manager, console, zxsh
```

### What this product does not include

| Fuchsia (paper) | On Secular console |
| --- | --- |
| Session Manager, user "session" | no; `console-launcher` is the interactive path |
| Hub | no |
| Dart AOT / Web runners | no; ELF only (`zxsh`, `component_manager`, drivers) |
| MemFS / Blobfs / MinFS / FVM | no in-kernel FS; no userspace FS product |
| FAR Merkle package verify | BOOTFS in the ZBI; optional CRC |
| Realm collections / many apps | bringup component tree only |
| Starnix | out of scope |

The **kernel** security story (handles, rights, jobs, vDSO, VMAR,
resources, driver-as-process) is the same as Fuchsia's. The
**product** security story is smaller: fewer TCB processes, no
app install path, serial console as the human interface.

## Where in the tree

| File | Why |
| --- | --- |
| [`zircon/system/public/zircon/rights.h`](../../zircon/system/public/zircon/rights.h) | `ZX_RIGHT_*`, default rights per object |
| [`zircon/system/public/zircon/syscalls/policy.h`](../../zircon/system/public/zircon/syscalls/policy.h) | `ZX_POL_*` job policy |
| [`zircon/system/public/zircon/syscalls/resource.h`](../../zircon/system/public/zircon/syscalls/resource.h) | `ZX_RSRC_KIND_*` |
| [`zircon/kernel/lib/userabi/userabi.rs`](../../zircon/kernel/lib/userabi/userabi.rs) | first process; bootstrap handles |
| [`zircon/kernel/lib/userabi/vdso/`](../../zircon/kernel/lib/userabi/vdso/) | the only approved `svc` stubs |
| [`zircon/kernel/object/job_policy.cc`](../../zircon/kernel/object/job_policy.cc) | inherit / enforce policy |
| [`zircon/kernel/lib/syscalls/job.rs`](../../zircon/kernel/lib/syscalls/job.rs) | `sys_job_create`, `sys_job_set_policy` |
| [`zircon/kernel/object/channel_dispatcher.rs`](../../zircon/kernel/object/channel_dispatcher.rs) | capability transfer |
| [`zircon/kernel/lib/crypto/global_prng.cc`](../../zircon/kernel/lib/crypto/global_prng.cc) | kernel CPRNG, no driver entropy |
| [`userspace/sys/`](../../userspace/sys/) | `component_manager` (userspace TCB) |
| [`pdf/understanding-fuchsia-security.pdf`](pdf/understanding-fuchsia-security.pdf) | Fuchsia security survey (2021) |

Suggested first pass: **handle + rights** in [`ipc.md`](zircon/ipc.md) →
**empty process + job policy** in [`cpu.md`](zircon/cpu.md) →
**bootstrap messages** in [`userabi.rs`](../../zircon/kernel/lib/userabi/userabi.rs) →
**vDSO ELR check** in [`cpu.md`](zircon/cpu.md) System calls →
**UART as Resource + Interrupt** in [`interrupt.md`](zircon/interrupt.md) →
CML `use`/`offer`/`expose` on the console components. That is enough
to see why `zxsh` cannot program the GIC.

## Terminology

| Term | Meaning in this note |
| --- | --- |
| Microkernel | Privileged code is mechanisms (sched, VM, IPC, IRQ, objects). Policy and drivers are processes. |
| TCB | Trusted computing base: kernel + userboot + `component_manager` + vDSO. Not the UART driver. |
| Kernel object | Dispatcher with a koid: Job, Process, Thread, VMO, Channel, Resource, … |
| Handle | Process-local `(object, rights)`. The kernel capability. |
| Rights | Bitset of operations that handle may perform. Shrink-only on dup/transfer. |
| Capability (kernel) | The handle. Possession is authorization. |
| Capability (component) | CML declaration (`protocol`, `directory`, …) that `component_manager` routes into a handle. |
| Routing | `use` / `offer` / `expose` path on the component instance tree. |
| Namespace | Per-component name tree. No kernel `/`. |
| Ambient authority | Ability to name a resource by global path/uid. Zircon does not have it. |
| Sandbox | New process (or component) starts with no privileges; only transferred handles apply. |
| Job policy | Extra constraints handles cannot express (`ZX_POL_VMAR_WX`, `ZX_POL_NEW_*`). |
| Resource | Kernel object that gates MMIO / IRQ / SMC / system. |
| vDSO | Kernel-provided ELF with the only legal `svc` sites. |
| `UserInPtr` | User VA; copy, do not dereference as a Rust/C++ reference. |
| SafeStack / ShadowCallStack | Clang hardening: overflowable locals and return addresses on separate stacks. |
| Least privilege | Each process holds the minimum handles and rights for its job. |
| Failure domain | Driver crash = process death. Kernel crash = whole system. |
