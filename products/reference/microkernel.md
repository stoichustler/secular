# Microkernel

Secular is a Zircon-microkernel console system. The kernel is the
privileged core; drivers, the filesystem, the component tree, and the
serial shell run in user space. This note is the map of that split:
what a microkernel is for, which pieces must stay in the kernel, how
Zircon (and therefore Secular) instantiates that design, and how that
differs from a Linux monolithic kernel.

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
| **Namespace** | Global VFS, mount, netns, pidns | No kernel path namespace; names live in component_manager / FIDL |
| **File I/O** | `open` / `read` / `write` / `ioctl` on inodes | Channel / Socket / FIFO / Stream / VMO; no in-kernel filesystem |
| **IPC** | pipes, sockets, signals, futex, netlink | Channel (bytes+handles), Socket, FIFO, Port, Event, futex |
| **Shared memory** | `mmap` of files / anon / memfd | VMO + `zx_vmar_map` |
| **Drivers** | `.ko` in kernel AS | EL0 processes; Interrupt + MMIO VMO + Resource |
| **IRQ** | `request_irq` → kernel handler | Kernel acks GIC, signals `InterruptDispatcher`; driver waits in user space |
| **Init** | pid 1 (`init` / systemd) | userboot → `component_manager --boot` |
| **Syscalls** | Large POSIX + Linux set | Small Zircon set via vDSO |
| **Signals** | POSIX signals | Exceptions + channel / port; no `kill(2)` semantics in the kernel |
| **Failure domain** | Driver bug → oops / panic | Driver bug → process death; kernel continues |
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
