---
name: zircon-interrupt
description: >
  Use when changing GICv3, IRQ handlers, page faults, exception vectors, or guest vGIC injection on the Secular console image.
---

# Zircon Interrupt: GICv3, SYNC, and vGIC

Stop when the handler has EOI'd, a fault has filled or killed, or a vINTID sits in an ICH LR.

## When to use

* Editing `arm64_irq`, GICD/GICR, `vmm_page_fault_handler`, or ICH list registers.
* A missing timer tick, UART IRQ, or guest vINTID.

## When not to use

* The thread that the IRQ preempts — `cpu.md`.
* The PTE the fault must install — `memory.md`.

> [!NOTE]
> Console image: Zircon kernel + linux-arm64-boot-shim + BOOTFS + serial
> console + `zxsh`. The kernel is an EL2 VHE host.

## 1. GIC

### Framework

Host GICv3 on the left (physical IRQs). Guest vGIC on the right (virtual
IRQs in ICH list registers; GICD/GICR emulated in `bin/vmm`).

```text
┌────────────────────────────────┐    ┌────────────────────────────────┐
│ Host  EL2 VHE                  │    │ Guest  Linux / RTOS VCPU       │
│                                │    │                                │
│  VBAR_EL1  arm64_el1_exception │    │  ICC_*_EL1  virtual CPU ifc    │
│   0x200 kernel IRQ  (SP_ELx)   │    │   hardware; not trapped        │
│   0x400 user   IRQ  (lower EL) │    │                                │
│                                │    │  GICD  IPA 0x800000000         │
│  GICD  0x08000000  shared SPI  │    │  GICR  IPA 0x800010000         │
│  GICR  0x080A0000  per-CPU     │    │   MMIO trap → gic_distributor  │
│  ICC_*_EL1  IAR1 / EOIR1       │    │                                │
│                                │    │  EL2 ICH_*  list registers     │
│  SGI 0 GENERIC                 │    │   ICH_LR  vINTID Pending/Active│
│      1 RESCHEDULE              │    │   ICH_HCR.En  ICH_VMCR         │
│      2 INTERRUPT (vmexit)      │    │                                │
│      3 HALT                    │    │                                │
└────────────────────────────────┘    └────────────────────────────────┘
         physical IRQ                         virtual IRQ
         GICD/GICR/ICC                        ICH_LR  +  emulated GICD
```

### Flow

Host acknowledge path, then guest inject path.

```text
 IRQ taken (DAIF.I set)
           │
           ▼
┌─────────────────────┐
│ exceptions.S        │  push iframe, restore x20
└──────────┬──────────┘
           ▼
┌─────────────────────┐
│ arm64_irq           │  int_handler_start
└──────────┬──────────┘
           ▼
┌─────────────────────┐
│ ICC_IAR1_EL1        │  ack, Pending → Active
└──────────┬──────────┘
           │
     INTID class
      │    │    │
    SGI   PPI  SPI
      │    │    │
      ▼    ▼    ▼
   mp_mbx  timer  UART / …
      │    │    │
      └────┴────┘
           ▼
┌─────────────────────┐
│ ICC_EOIR1_EL1       │  drop Active
└──────────┬──────────┘
           ▼
 int_handler_finish → Preempt()? → eret

 guest str [GICD] ── trap ─► VMM ── Vcpu::Interrupt ─► ICH_LR = PENDING|vINTID
         │
         ▼
    eret to guest
         │
         ▼
    mrs ICC_IAR1  (hardware, no hypercall)  LR → ACTIVE
         │
         ▼
    msr ICC_EOIR1                           LR empty
```

### Exception model (how an IRQ reaches C)

AArch64 takes exceptions through `VBAR_EL1`. The table is 2 KiB, 16 slots of
128 bytes (`zircon/kernel/lib/arch/arm64/include/lib/arch/arm64/exception-asm.h`).
Four contexts × four types:

```text
offset   context              type
0x000    current EL, SP_EL0   SYNC / IRQ / FIQ / SError
0x200    current EL, SP_ELx   SYNC / IRQ / FIQ / SError     ← kernel
0x400    lower EL, AArch64    SYNC / IRQ / FIQ / SError     ← user
0x600    lower EL, AArch32    (unused here)
```

Secular always uses `SPSel=1` (`SP_EL1`) in the kernel, so kernel IRQs land
in the `0x200` group. User IRQs land in `0x400`. FIQ is not used as a
separate class: GICv3 Group 1 IRQs are taken as IRQ.

`arm64_cpu_early_init` (`zircon/kernel/arch/arm64/arch.cc`) writes
`VBAR_EL1 = arm64_el1_exception`. The IRQ slot saves an `iframe_t` and calls
`arm64_irq`.

DAIF masks the four classes. `arch_disable_ints` is `msr daifset, #6` (IRQ
and SError). Exception entry already has IRQs masked; the handler must not
re-enable them until the GIC has been acknowledged, and it leaves them
masked across `platform_irq`.

SYNC (page fault, BRK, undef, …) uses the same table, other slots. That
path is **Exception** below. Syscalls (`svc`) also take the EL0 SYNC
slot, then peel off in assembly before C; they are [`cpu.md`](cpu.md).

```text
IRQ taken (DAIF.I set by hardware)
  exceptions.S   push iframe, restore x20 from current Thread
  arm64_irq      Spectre check if from EL0
                 int_handler_start   (blocking disallowed)
                 platform_irq        → GICv3 handle_irq
                 int_handler_finish  (preempt pending?)
                 Thread::Current::Preempt() if needed
                 pending signals if from EL0
  eret
```

### Physical GICv3 on QEMU virt

QEMU virt puts a GICv3 at:

| Physical | Size | Role |
| --- | --- | --- |
| `0x08000000` | `0x10000` | GICD distributor (shared) |
| `0x080A0000` | `0x20000` × N | GICR redistributor, one per CPU |

The boot-shim reads the DTB and appends a
`ZBI_KERNEL_DRIVER_ARM_GIC_V3` item (`zbi_dcfg_arm_gic_v3_driver_t`):

| Field | QEMU virt |
| --- | --- |
| `mmio_phys` | `0x08000000` |
| `gicd_offset` | `0` |
| `gicr_offset` | `0x100000` (`0x080A0000`) |
| `gicr_stride` | `0x20000` |
| `ipi_base` | `0` (SGIs 0–3) |

`ArmGicInitEarly` (`zircon/kernel/dev/interrupt/gic/v3/arm_gicv3.rs`)
records those bases. After the VM is up, `ArmGicInitPostVm` maps them
uncached-device through the kernel aspace. Per-CPU
`init_percpu_early` programs the **CPU interface** via system registers
(`ICC_*_EL1`), not a memory-mapped GICC (that is GICv2).

Three hardware pieces:

```text
GICD   shared MMIO     SPI routing, enables, priorities
GICR   per-CPU MMIO    SGI/PPI enables, WAKER, LPI pending
ICC    per-CPU sysreg  IAR / EOIR / PMR / IGRPEN1 / SGI1R
```

`ICC_SRE_EL1.SRE = 1` selects the system-register CPU interface.

### INTID space

| INTID | Class | Scope |
| --- | --- | --- |
| 0–15 | SGI | software; IPI |
| 16–31 | PPI | per-CPU (timer, maintenance) |
| 32–1019 | SPI | shared (UART, virtio, …) |
| 1020–1023 | special | spurious; do not EOI |
| 8192+ | LPI | ITS; unused on the console image |

`MAX_INTERRUPTS` is 1024. Handlers live in the pdev table
(`zircon/kernel/dev/pdev/interrupt/interrupt.rs`).

SGI 0–3 with `ipi_base = 0` are the four MP IPIs
(`zircon/kernel/include/kernel/mp.h`):

| `mp_ipi` | SGI | Meaning |
| --- | --- | --- |
| `GENERIC` | 0 | `mp_sync_exec` cross-call |
| `RESCHEDULE` | 1 | remote `Scheduler::Reschedule` |
| `INTERRUPT` | 2 | hypervisor: force a vmexit |
| `HALT` | 3 | panic / halt other CPUs |

`send_ipi` writes `ICC_SGI1R_EL1` with the target affinity list.

### IRQ acknowledge path

GICv3 Group 1, EL1:

```text
ICC_IAR1_EL1     read  → INTID, state Pending → Active
handler(INTID)
ICC_EOIR1_EL1    write INTID  → drop Active
```

`handle_irq` (`arm_gicv3.rs`):

1. `vector = ICC_IAR1_EL1 & 0xffffff`.
2. If `1020…1023`, return (no EOI).
3. If `vector >= 32`, count an external IRQ.
4. If LPI (`>= 8192`), look up the ITS handler table; else
   `pdev_invoke_int_if_present(vector)`.
5. `ICC_EOIR1_EL1 = vector`.

Priority mask `ICC_PMR_EL1 = 0xff` (accept all). `ICC_IGRPEN1_EL1 = 1`
enables Group 1. Priority is 8 bits in hardware; the GIC may implement
fewer levels (low bits RAZ/WI).

Level-triggered SPIs stay pending while the line is high: the handler must
clear the device, then EOI, or the IRQ immediately retakes. Edge-triggered
latches until EOI. `configure_interrupt` programs that in GICD/GICR
`ICFGR`.

Mask/unmask of SGI/PPI is **this CPU only** (GICR). SPI mask is global
(GICD) but affinity (`set_interrupt_affinity`) picks the target PE.

### Who calls `register_int_handler`

On the console image the interesting sources are:

| Vector | Source |
| --- | --- |
| SGI 0–3 | IPI mailbox (`mp_mbx_*_irq`) |
| PPI (generic timer) | `zircon/kernel/dev/timer/` → scheduler tick |
| SPI (PL011) | kernel serial, if the UART is IRQ-driven |
| SPI (virtio, …) | not in the console ZBI |

`register_int_handler` takes a lock that is also held during dispatch, so
register/unregister synchronizes with in-flight IRQs.

## 2. Exception

IRQ is **GIC** above. This section is everything else that takes
`VBAR_EL1`: synchronous exceptions (page fault, BRK, undef, alignment)
and SError. Same iframe, same `SP_EL1`, different ESR class. Syscalls
share the EL0 SYNC vector but never reach this C path
([`cpu.md`](cpu.md)).

### Framework

One vector table, four ARM types. Only SYNC is decoded in C here.

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ VBAR_EL1  arm64_el1_exception                                            │
│                                                                          │
│   0x200  current EL, SP_EL1     SYNC / IRQ / SError     ← kernel         │
│   0x400  lower EL, AArch64      SYNC / IRQ / SError     ← user           │
│                                                                          │
│   iframe_t on SP_EL1     ESR_EL1     FAR_EL1 (aborts, PC align, WP)      │
└────────────┬─────────────────────────────────────────────────────────────┘
             │  SYNC, not svc
             ▼
┌────────────────────────────────┐     ┌───────────────────────────────────┐
│ arm64_sync_exception           │     │ if not resolved                   │
│  switch ESR.EC                 │────►│  user: dispatch_user_exception    │
│   abort → page-fault handler   │     │    debug → thread → process → job │
│   BRK   → SW breakpoint        │     │  kernel: exception_die → halt     │
│   undef → undefined insn       │     └───────────────────────────────────┘
│   …                            │
└────────────────────────────────┘
```

### Flow

```text
 EL0 / EL1  fault / BRK / undef
        │
        ▼
 exceptions.S     iframe, restore x20
        │
        │  EL0 + ESR_EC_SVC64  ──►  syscall (cpu.md), not this path
        ▼
 arm64_sync_exception(iframe, flags, ESR)
        │
        ▼
 ┌──────────────────────────────────────────┐
 │ ESR.EC                                   │
 │  Instruction / Data Abort  → page fault  │
 │  BRK / BKPT                → SW break    │
 │  HW breakpoint / step / WP → debugger    │
 │  PC alignment              → unaligned   │
 │  Unknown / MOPS            → undef       │
 │  FP/SIMD trap              → panic today │
 │  else                      → GENERAL     │
 └──────────────────┬───────────────────────┘
                    │
          resolved? (ZX_OK)
           │ yes          │ no
           ▼              ▼
        eret         user exception walk
                     debug → thread → process → jobs
                       │ handled          │ not
                       ▼                  ▼
                     eret            kill process / thread
```

From EL0, after C returns, `arch_iframe_process_pending_signals` runs
before `eret` (kill / suspend sampled with IRQs off).

### Four ARM types

AArch64 names four exception **types**. DAIF masks three of them (D is
debug). The VBAR has a slot for each type in each context.

| Type | When | This kernel |
| --- | --- | --- |
| SYNC | `svc`, abort, BRK, undef, alignment, system insn trap | `arm64_sync_exception` (svc peeled in asm) |
| IRQ | GICv3 Group 1 | `arm64_irq` (**GIC**) |
| FIQ | Group 0 | unused; Group 1 IRQs are IRQ |
| SError | async abort (impl-defined) | `arm64_serror_exception`: count + ktrace, not fatal |

An unused / out-of-order VBAR slot is `arm64_invalid_exception` →
`platform_halt`. AArch32 lower-EL slots panic unless restricted mode
(`cpu.md`).

### Registers at entry

| Register | Role |
| --- | --- |
| `ESR_EL1` | syndrome. `EC` bits [31:26] pick the class; `ISS` bits [24:0] are class-specific; `IL` is instruction length |
| `FAR_EL1` | faulting VA for instruction/data abort, PC alignment, watchpoint |
| `ELR_EL1` | interrupted PC (in the iframe as `elr`) |
| `SPSR_EL1` | interrupted PSTATE (in the iframe as `spsr`) |
| `SP_EL0` | user stack (iframe `usp` when from EL0) |

`exception_sets_far` is true only for the abort / PC-align / watchpoint
classes. Other SYNC exceptions leave `FAR` stale; the dispatcher then
passes 0.

### `arm64_sync_exception` classes

[`exceptions_c.cc`](../../../zircon/kernel/arch/arm64/exceptions_c.cc)
switches on `ESR.EC`. Kernel-origin (same EL) of most of these is a
bug: `exception_die` → print ESR/FAR/iframe → `platform_halt`.

| `ESR.EC` | From | Handler | Userspace type |
| --- | --- | --- | --- |
| `kSvc64` / `kSvc32` | `svc` | must not reach C | — (assembly) |
| `kInstructionAbortLowerEl` / `SameEl` | I-side MMU | `arm64_instruction_abort_handler` | `ZX_EXCP_FATAL_PAGE_FAULT` if unresolved |
| `kDataAbortLowerEl` / `SameEl` | D-side MMU / align | `arm64_data_abort_handler` | fatal page fault, or `ZX_EXCP_UNALIGNED_ACCESS` |
| `kBrk` / `kBkpt` | `BRK` / AArch32 `BKPT` | `arm64_brk_handler` | `ZX_EXCP_SW_BREAKPOINT` |
| `kBreakpointLowerEl` / `SameEl` | HW breakpoint | `arm64_hw_breakpoint_exception_handler` | `ZX_EXCP_HW_BREAKPOINT` |
| `kStepLowerEl` / `SameEl` | software step | `arm64_step_handler` | `ZX_EXCP_HW_BREAKPOINT` |
| `kWatchpointLowerEl` / `SameEl` | HW watchpoint | `arm64_watchpoint_exception_handler` | `ZX_EXCP_HW_BREAKPOINT` (`FAR` = hit address) |
| `kPcAlignment` | PC not aligned | `arm64_pc_alignment_fault_handler` | `ZX_EXCP_UNALIGNED_ACCESS` |
| `kUnknown` | mostly undef insn | `arm64_unknown_handler` | `ZX_EXCP_UNDEFINED_INSTRUCTION` |
| `kMops` | FEAT_MOPS PC/PSTATE mismatch | `arm64_mops_handler` | `ZX_EXCP_UNDEFINED_INSTRUCTION` |
| `kFp` | FP/SIMD/SVE trap | `arm64_fpu_handler` | **panic** (not enabled as a user path) |
| other | — | `ZX_EXCP_GENERAL`, then die if unhandled | `ZX_EXCP_GENERAL` |

Synthetic exceptions (`ZX_EXCP_THREAD_STARTING` / `EXITING` /
`POLICY_ERROR`) are **not** hardware. They are posted from thread start,
exit, or a policy check (`cpu.md` job policy, bad syscall). Same user
walk.

### Why they happen

Hardware takes a **SYNC** exception on the instruction that caused it
(`ELR` = that PC, or the next PC for `svc`/`BRK`). **IRQ** / **SError**
are asynchronous: they can land between any two instructions.

| Kind | Why it is taken |
| --- | --- |
| IRQ | A GICv3 INTID became pending and `DAIF.I` is clear: timer PPI, UART SPI, IPI SGI. **GIC**. |
| FIQ | Group 0. Not used; this product delivers Group 1 as IRQ. |
| SError | Async abort the CPU could not report synchronously (impl-defined; RAS, poisoned line). QEMU virt almost never injects one. Counted, not fatal. |
| `svc` | User executed `svc #0` (vDSO stub). Not a fault. [`cpu.md`](cpu.md). |
| Instruction abort | I-side MMU walk failed while **fetching** the instruction at `FAR`. |
| Data abort | D-side MMU walk failed, or an alignment check failed, on a **load/store** (or cache op) at `FAR`. |
| `BRK` / `BKPT` | The instruction itself is a breakpoint. Someone planted it. |
| HW breakpoint | `PC` matches a programmed `DBGBVR` and MDSCR lets it fire. |
| Watchpoint | A load/store address matches a programmed `DBGWVR`. |
| Software step | Debugger set `MDSCR.SS`; the **next** instruction after a step request completed. |
| PC alignment | A branch / `eret` / exception return set `PC` not 4-byte aligned (AArch64). |
| Unknown | The encoding is not a valid A64 instruction in this state (`UDF`, reserved, wrong width). |
| MOPS | A `CPY*` / `SET*` (FEAT_MOPS) ran with PSTATE not matching the copy-restart protocol. |
| FP/SIMD trap | `CPACR_EL1` is trapping FP/SIMD/SVE. This kernel panics; it is not a lazy-enable path. |
| Policy / start / exit | Software posted a synthetic exception. No `ESR` from the CPU. |

#### Page faults (instruction / data abort)

The MMU is walking TTBR0 (user) or TTBR1 (kernel). It stops and takes
an abort when the walk cannot complete the access. `DFSC` is the
reason; `WnR` is write vs read; instruction vs data is which abort
class.

| What the program did | DFSC | Outcome on the console image |
| --- | --- | --- |
| First load/store/fetch in a mapped VMO (page not yet in the L3) | translation | **Recoverable.** VM faults the VMO, installs the PTE, `eret`, insn retries. |
| Touch a VA with no VMAR mapping (null, wild pointer, past the stack VMO) | translation | Not found → `ZX_EXCP_FATAL_PAGE_FAULT` → process killed (no crashsvc). |
| Access a page whose Access Flag is clear | access flag | **Recoverable.** `AccessedFault` sets AF; no new page. |
| Store to a read-only mapping (`ZX_VM_PERM_READ` only) | permission | Fatal page fault unless a handler (or a pager) upgrades it. |
| Fetch from a non-executable mapping (data page, missing `X`) | permission (I-abort) | Fatal page fault. |
| EL0 load/store of a kernel VA (`TTBR1`, `VA[55]=1`) | translation / permission | User FAR is not user-accessible → `ZX_ERR_NOT_FOUND` → fatal. |
| Kernel deref of a bad kernel pointer | translation (SameEl) | **Panic.** Kernel does not demand-page TTBR1. |
| Kernel `copy_from/to_user` of a bad **user** pointer | translation / permission, with `data_fault_resume` | Copy returns `ZX_ERR_INVALID_ARGS`; syscall fails. Not a panic. |
| Unaligned exclusive / device access, or `SCTLR.A` alignment check | alignment (`0b100001`) | `ZX_EXCP_UNALIGNED_ACCESS`, not a VM fault. |
| External abort / ECC / tag check on that access | other DFSC | User: fatal exception. Kernel: `exception_die`. |

Demand fill is the common **good** case: `zx_vmar_map` created a
mapping, but the physical page is populated on first touch. A null
dereference is the common **bad** case: no mapping, same DFSC class,
different VM result.

Instruction abort in the kernel is always treated as a bug (bad `ELR`
or executing unmapped kernel text). Data abort in the kernel is a bug
unless it is that user-copy.

#### Breakpoints and debug

| Exception | Who caused it | Typical user |
| --- | --- | --- |
| `BRK #<imm>` | Compiler / sanitizer / `__builtin_debugtrap` / a debugger writing a breakpoint insn. The vDSO `svc` stub has a `brk 0` after the speculation barrier that **must not** execute; the kernel advances `ELR` past it on syscall return. | `ZX_EXCP_SW_BREAKPOINT` |
| HW breakpoint | `zx_thread_write_state` programmed `DBGBVR`/`DBGBCR`; `MDSCR.MDE` on. Fires when `PC` equals that VA. | debugger |
| Watchpoint | Same, `DBGWVR`/`DBGWCR`. Fires on a load and/or store to that VA (`FAR` says which). | debugger |
| Software step | Debugger set `MDSCR.SS` and resumed. The following instruction completes and traps. | debugger |

Without a debugger these do not occur on the console boot path. A stray
`BRK` in user code still takes the user walk and kills the process.

#### Undefined, alignment, FP, MOPS

| Exception | Why |
| --- | --- |
| Unknown / undef | Executed a bit pattern that is not an instruction at this EL/width: `UDF`, garbage after a bad jump, AArch32 insn in A64, or a trapped system insn the kernel does not emulate. |
| PC alignment | `PC[1:0] != 00`. AArch64 instructions are 4 bytes. A corrupted function pointer or a bad `eret` target does this. |
| Data alignment | Access size/address the memory type does not allow (Device, unaligned `STXR`/`LDXR`, or alignment checking). Ordinary unaligned integer loads on Normal memory do **not** fault. |
| MOPS | FEAT_MOPS memcpy/memset insns keep restart state in PSTATE. If `PC` and that state disagree (signal, migrate, or a jump into the middle of a sequence), the CPU traps. |
| FP/SIMD | `CPACR_EL1.FPEN` is trapping those insns. The handler panics even from EL0; user FP is expected to be enabled, not lazily trapped. |

#### Synthetic (no CPU syndrome)

Posted by kernel C++, then the same user walk.

| Type | Why |
| --- | --- |
| `ZX_EXCP_THREAD_STARTING` | First (or any) thread is about to `eret` to EL0. Lets a job/process debugger see the initial regs. |
| `ZX_EXCP_THREAD_EXITING` | Thread is dying; debugger can still read regs. |
| `ZX_EXCP_PROCESS_STARTING` | First thread of a process, for a job debugger. |
| `ZX_EXCP_POLICY_ERROR` | A syscall violated job policy: `ZX_POL_NEW_*`, WX VMAR, bad handle, wrong object type, or a `svc` whose PC was **not** in the vDSO (`ZX_EXCP_POLICY_CODE_BAD_SYSCALL`). |
| `ZX_EXCP_USER` | `zx_thread_raise_exception` from userspace. |

#### IRQ vs SYNC (why both exist)

A page fault **must** be SYNC: the CPU cannot retire the load until the
mapping exists, so it stops on that insn and retries after `eret`. A
timer tick **must** be IRQ: the running insn is fine; the PE just needs
to enter the kernel between instructions. Mixing them (polling for
pages, or taking a page fault as an IRQ) is not how AArch64 works.

### Page fault

The common recoverable SYNC path. **Why** an abort is taken is
**Why they happen** above. This subsection is how the kernel resolves
it. Hardware took an instruction abort (prefetch) or data abort
(load/store). `FAR_EL1` is the VA. `ISS.DFSC` (bits [5:0]) says why;
for data aborts `ISS.WnR` is write-not-read, `ISS.CM` is
cache-maintenance (treated as a read).

#### DFSC (why the MMU aborted)

| DFSC | Name | `vmm_page_fault_handler`? | Typical cause |
| --- | --- | --- | --- |
| `0b0001xx` | Translation, level 0–3 | yes (`VMM_PF_FLAG_NOT_PRESENT`) | no mapping / demand fill |
| `0b0010xx` | Access flag, level 1–3 | yes (`VMM_PF_FLAG_ACCESS`) | AF bit clear; `AccessedFault` |
| `0b0011xx` | Permission, level 1–3 | yes | write to RO, execute NX, user vs kernel |
| `0b100001` | Alignment | no | → `ZX_EXCP_UNALIGNED_ACCESS` |
| other | size / external abort / parity / … | no | user: fatal exception; kernel: die |

The handler only calls the VM for access, translation, and permission
(`(dfsc & 0b001100) != 0 && (dfsc & 0b110000) == 0`).

#### Flags into the VM

| Flag | Set when |
| --- | --- |
| `VMM_PF_FLAG_USER` | abort from EL0 (`EC` even: LowerEl) |
| `VMM_PF_FLAG_INSTRUCTION` | instruction abort |
| `VMM_PF_FLAG_WRITE` | data abort, `WnR=1` and not cache-maintenance |
| `VMM_PF_FLAG_NOT_PRESENT` | translation fault |
| `VMM_PF_FLAG_ACCESS` | access-flag fault |
| `VMM_PF_FLAG_HW_FAULT` | always, in `vmm_page_fault_handler` |

#### Path

```text
 abort (I or D)
        │
        ▼
 FAR_EL1, DFSC, WnR
        │
        ▼
 kernel I-abort?              ──yes──► exception_die
 kernel D-abort, no user-copy ──yes──► exception_die
        │
        ▼
 user-copy capture?   (arch().data_fault_resume)
        │ yes: plant ELR = resume, x0 = status, return
        ▼ no
 IRQs on
 vmm_page_fault_handler(FAR, flags)
   Thread::Current::PageFault
     VmAspace::PageFault
       ACCESS  → AccessedFault (set AF, no new page)
       else    → PageFaultInternal (mapping, VMO, pager)
 IRQs off
        │
     ZX_OK?  ──yes──► eret, retry the insn
        │ no
        ▼
 user?  try_dispatch  ZX_EXCP_FATAL_PAGE_FAULT
        (alignment → ZX_EXCP_UNALIGNED_ACCESS)
        │
     ZX_OK?  ──yes──► handler fixed it; eret
        │
        ▼
 exception_die   (kernel) or kill the process (user, no handler)
```

`vmm_page_fault_handler`
([`vmm.cc`](../../../zircon/kernel/vm/vmm.cc)) rejects a non-user VA
(`ZX_ERR_NOT_FOUND`). Kernel addresses never demand-page: a kernel
translation fault is a bug unless it is a **user-copy** touching a user
VA.

Resolved faults (`ZX_OK`) just `eret` to the same `ELR`. The instruction
retries; the mapping / AF bit is now valid. This is how first-touch of a
mapped VMO, stack growth via a mapped region, and AF updates work on the
console image. There is no anonymous mmap-without-a-VMO: userspace maps
VMOs through a VMAR (`memory.md`).

#### User-copy (kernel touching user memory)

`arch_copy_from_user` / `to_user` set
`Thread.arch().data_fault_resume` to a kernel resume PC (with bits 63/62
encoding whether to run the fault handler first). A data abort on a user
VA then:

1. Optionally still calls `vmm_page_fault_handler` (demand fill).
2. If that cannot fix it, or the copy asked to capture the fault, plants
   `iframe.elr` = resume address and `x0` = `ZX_ERR_INVALID_ARGS` (plus
   flags in the high half when the VM might still handle it).
3. Returns to the copy routine instead of panicking.

A kernel data abort **without** `data_fault_resume` is
`exception_die("data abort in kernel mode")`. Any instruction abort in
the kernel is always fatal.

### User exception walk

When the arch handler cannot fix the fault, it calls
`dispatch_user_exception`
([`exception.cc`](../../../zircon/kernel/object/exception.cc)). The
current `Thread` must have a `ThreadDispatcher` (else `ZX_ERR_BAD_STATE`).
The thread is `Blocked::EXCEPTION` until the walk finishes.

Handlers are tried in order (`ExceptionHandlerIterator`):

```text
 process debug exceptionate     (first chance)
        │
        ▼
 thread exceptionate
        │
        ▼
 process exceptionate
        │
        ▼
 process debug exceptionate     (second chance, if requested)
        │
        ▼
 job exceptionate → parent job → … → root job
```

Each `HandleException` sends a `zx_exception_t` on that channel and waits
for the receiver (`ZX_ERR_NEXT` = skip, `ZX_OK` = resume, `ZX_ERR_STOP` =
kill this thread). Restricted mode, if active, is intercepted after the
first debug channel and does not propagate (`cpu.md`).

If some handler resumes (`ZX_OK`), C returns and the iframe `eret`s.
If a handler asks to kill the thread (`ZX_ERR_STOP` /
`ZX_ERR_INTERNAL_INTR_KILLED`), `ExitCurrent`. If nobody handles it,
the **process** is killed (`ZX_TASK_RETCODE_EXCEPTION_KILL`) and the
thread exits. A process critical to the root job dumps extra diagnostics
first (that path often reboots).

On the console image there is usually no debugger and no crashsvc. An
unhandled user page fault in `zxsh` therefore prints `KERN: terminating
process '…'` and kills that process.

### Kernel fatal

`exception_die`: `platform_panic_start`, print format + ESR (`ec` / `il`
/ `iss`) + FAR + iframe, stash crashlog regs, `platform_halt(HALT)`.
IRQs stay off. That is not IdlePowerThread (`cpu.md`).

### Console picture

```text
 zxsh load from a mapped VMO, first touch
    │  translation fault, FAR = code/data VA
    ▼
 data/insn abort, EL0 SYNC
    │
    ▼
 vmm_page_fault_handler  →  VMO supplies the page, map L3
    │  ZX_OK
    ▼
 eret, insn retries, zxsh continues

 zxsh load from an unmapped VA
    │  translation fault, no mapping
    ▼
 vmm_page_fault_handler  →  ZX_ERR_NOT_FOUND
    │
    ▼
 ZX_EXCP_FATAL_PAGE_FAULT  →  no handler
    │
    ▼
 process killed
```

## 3. vGIC

A guest does not program the host GICD. It sees a virtual GICv3:

```text
┌────────────────────────────────────────────────────────────────┐
│ guest EL1                                                      │
│   ICC_*_EL1          virtual CPU interface (hardware)          │
│   GICD / GICR MMIO   emulated by userspace VMM (traps)         │
└────────────────────────────┬───────────────────────────────────┘
                             ▼  zx_guest_set_trap (MMIO)
┌────────────────────────────────────────────────────────────────┐
│ EL2  ICH_*           list registers, VMCR, HCR                 │
│      inject vIRQ into the virtual CPU interface                │
└────────────────────────────┬───────────────────────────────────┘
                             ▼
┌────────────────────────────────────────────────────────────────┐
│ host  VMM thread (bin/vmm) + kernel Vcpu                       │
└────────────────────────────────────────────────────────────────┘
```

`HCR_EL2.IMO/FMO/AMO` route physical IRQ/FIQ/SError to EL2 while a VCPU is
running. A physical host IRQ therefore vmexits; the kernel handles it, then
re-enters the guest. Virtual IRQs for the guest do **not** use that path:
they sit in ICH list registers and are delivered as IRQs at guest EL1.

### ICH (EL2 hypervisor control)

GICv3 virtualization is the ICH register file. The kernel talks to it only
in EL2 (`zircon/kernel/arch/arm64/hypervisor/gic/el2.S`, `gicv3.cc`).

| Register | Role |
| --- | --- |
| `ICH_HCR_EL2` | enable virtual interface (`En`); underflow IRQ (`UIE`) |
| `ICH_VMCR_EL2` | virtual `ICC_PMR` / `IGRPEN1` / `FIQEn` |
| `ICH_VTR_EL2` | how many LRs and priority bits |
| `ICH_LR<n>_EL2` | one in-flight virtual interrupt |
| `ICH_ELRSR_EL2` | which LRs are empty |
| `ICH_MISR_EL2` | maintenance (underflow, …) |

A list register holds:

```text
[63:62]  state     00 inactive, 01 pending, 10 active, 11 pending+active
[60]     Group1
[55:48]  priority
[31:0]   vINTID    (virtual INTID the guest's ICC_IAR returns)
```

Up to 16 LRs on typical QEMU CPU models (`kNumLrs`). SGIs/PPIs are injected
at priority `0`, SPIs at `0x10`, so the guest sees SGIs before SPIs when
priorities would otherwise tie.

`gicv3_get_gicv` returns `ZX_ERR_NOT_FOUND`: there is no GICV MMIO to map
into the guest. The virtual CPU interface is system registers.

On VCPU create (`zircon/kernel/arch/arm64/hypervisor/vcpu.cc`):

```text
ICH_VMCR  ← VENG1 | VFIQEN | VPMR=0xff
ELRSR     ← all LRs empty
HCR_EL2   ← IMO|FMO|AMO | VM | …   (physical IRQs to EL2)
```

On each `zx_vcpu_enter`, `gich_maybe_interrupt` pops pending vINTIDs from
`GichState` into empty LRs. `arm64_el2_gicv3_write_gich_state` loads ICH_*
in EL2 just before `eret` to the guest. On vmexit the LRs are read back so
Pending/Active state is not lost.

If every LR is full and more vIRQs wait, `ICH_HCR.UIE` requests a
**maintenance interrupt** when an LR frees. That is a host PPI; the VCPU
thread is kicked (`mp_interrupt` / IPI `INTERRUPT`) if it is running on
another CPU.

### Userspace distributor

Guest `GICD` / `GICR` writes are ordinary MMIO traps into `bin/vmm`:

| Guest IPA | Size | Emulation |
| --- | --- | --- |
| `0x800000000` | `0x10000` | GICD (`GicDistributor`) |
| `0x800010000` | `0x10000` | GICR RD |
| `0x800020000` | `0x10000` | GICR SGI |

(`kDevicePhysBase` in `userspace/hypervisor/bin/vmm/io.h`; stride
`0x20000`.) The DTB the VMM builds for the guest points here, not at the
host `0x08000000`.

A guest `GICD_ISPENDR` / `ISENABLER` / SGI generate becomes
`Vcpu::Interrupt(vector)` in the kernel: the vINTID is queued, LRs filled
on next enter (or the running VCPU is interrupted so it vmexits and
re-enters with the new LR).

Guest `ICC_IAR1_EL1` / `EOIR1_EL1` are **not** trapped when the virtual
interface is on: they operate on the LRs. That is why a Linux guest can
run its own GICv3 driver with no hypercall per IRQ.

```text
guest:  str  enable, [GICD]     trap → VMM → Vcpu::Interrupt(spi)
host:   LR[i] = PENDING | vINTID
guest:  eret
guest:  mrs  ICC_IAR1_EL1       hardware: vINTID, LR → ACTIVE
guest:  handle, msr ICC_EOIR1   hardware: LR → empty
```

Physical host IRQs (UART, timer) still use the host GIC path above. They
never appear in ICH LRs unless the VMM explicitly injects a matching
vINTID.

### Init order (host)

```text
boot-shim          DTB → ZBI GIC v3 item
ArmGicInitEarly    record GICD/GICR bases, IPI base
VM init            map GICD/GICR device memory
ArmGicInitPostVm   enable GICD, per-CPU GICR, ICC
gicv3_hw_interface_register    ICH ops for the hypervisor
ArmGicInitLate     ITS if present (not required for console)
secondary CPU      init_percpu_early + unmask SGIs
```

QEMU virt is GICv3. GICv2 (`arm_gicv2.rs`, memory-mapped GICC/GICH/GICV)
is in the tree for other boards and is not the console path.

## 4. References

Read in this order.

### Host GIC

| File | Why |
| --- | --- |
| [`zircon/kernel/lib/arch/arm64/include/lib/arch/arm64/exception-asm.h`](../../../zircon/kernel/lib/arch/arm64/include/lib/arch/arm64/exception-asm.h) | VBAR layout |
| [`zircon/kernel/arch/arm64/exceptions.S`](../../../zircon/kernel/arch/arm64/exceptions.S) | IRQ / SYNC slots → iframe |
| [`zircon/kernel/arch/arm64/exceptions_c.cc`](../../../zircon/kernel/arch/arm64/exceptions_c.cc) | `arm64_irq`; `arm64_sync_exception` |
| [`zircon/kernel/arch/arm64/include/arch/arm64/interrupt.h`](../../../zircon/kernel/arch/arm64/include/arch/arm64/interrupt.h) | DAIF |
| [`zircon/kernel/dev/interrupt/include/dev/interrupt.h`](../../../zircon/kernel/dev/interrupt/include/dev/interrupt.h) | mask / handler API |
| [`zircon/kernel/dev/pdev/interrupt/interrupt.rs`](../../../zircon/kernel/dev/pdev/interrupt/interrupt.rs) | `platform_irq`, IPI, registration |
| [`zircon/kernel/dev/interrupt/gic/v3/arm_gicv3.rs`](../../../zircon/kernel/dev/interrupt/gic/v3/arm_gicv3.rs) | GICD/GICR/ICC, `handle_irq` |
| [`zircon/kernel/dev/interrupt/gic/v3/regs.rs`](../../../zircon/kernel/dev/interrupt/gic/v3/regs.rs) | `ICC_IAR1` / `EOIR1` / `SGI1R` |
| [`zircon/kernel/dev/interrupt/gic/common/include/dev/interrupt/arm_gic_common.h`](../../../zircon/kernel/dev/interrupt/gic/common/include/dev/interrupt/arm_gic_common.h) | SGI/PPI/SPI bases |
| [`zircon/kernel/include/kernel/mp.h`](../../../zircon/kernel/include/kernel/mp.h) | four IPI kinds |
| [`zircon/kernel/kernel/interrupt.rs`](../../../zircon/kernel/kernel/interrupt.rs) | `int_handler_start` / `finish` |
| [`zircon/kernel/phys/lib/boot-shim/`](../../../zircon/kernel/phys/lib/boot-shim/) | DTB → ZBI GIC item |

### Exception (SYNC)

| File | Why |
| --- | --- |
| [`zircon/kernel/arch/arm64/exceptions.S`](../../../zircon/kernel/arch/arm64/exceptions.S) | EL0 SYNC: `svc` vs `arm64_sync_exception` |
| [`zircon/kernel/arch/arm64/exceptions_c.cc`](../../../zircon/kernel/arch/arm64/exceptions_c.cc) | ESR switch; abort / BRK / undef; `exception_die` |
| [`zircon/kernel/lib/arch/include/lib/arch/arm64/system.h`](../../../zircon/kernel/lib/arch/include/lib/arch/arm64/system.h) | `ESR.EC` enum |
| [`zircon/kernel/vm/include/vm/fault.h`](../../../zircon/kernel/vm/include/vm/fault.h) | `VMM_PF_FLAG_*` |
| [`zircon/kernel/vm/vmm.cc`](../../../zircon/kernel/vm/vmm.cc) | `vmm_page_fault_handler` |
| [`zircon/kernel/kernel/thread.cc`](../../../zircon/kernel/kernel/thread.cc) | `Thread::Current::PageFault` |
| [`zircon/kernel/vm/vm_aspace.cc`](../../../zircon/kernel/vm/vm_aspace.cc) | `VmAspace::PageFault` / `AccessedFault` |
| [`zircon/kernel/arch/arm64/user-copy/`](../../../zircon/kernel/arch/arm64/user-copy/) | `data_fault_resume` for user-copy |
| [`zircon/kernel/object/exception.cc`](../../../zircon/kernel/object/exception.cc) | `dispatch_user_exception`; handler iterator |
| [`zircon/system/public/zircon/syscalls/exception.h`](../../../zircon/system/public/zircon/syscalls/exception.h) | `ZX_EXCP_*` |

### vGIC

| File | Why |
| --- | --- |
| [`zircon/kernel/arch/arm64/include/arch/arm64/hypervisor/gic/gicv3.h`](../../../zircon/kernel/arch/arm64/include/arch/arm64/hypervisor/gic/gicv3.h) | LR / VMCR / VTR bits |
| [`zircon/kernel/arch/arm64/hypervisor/gic/gicv3.cc`](../../../zircon/kernel/arch/arm64/hypervisor/gic/gicv3.cc) | ICH ops registered with the GIC layer |
| [`zircon/kernel/arch/arm64/hypervisor/gic/el2.S`](../../../zircon/kernel/arch/arm64/hypervisor/gic/el2.S) | EL2 read/write of ICH_* |
| [`zircon/kernel/arch/arm64/hypervisor/vcpu.cc`](../../../zircon/kernel/arch/arm64/hypervisor/vcpu.cc) | `gich_maybe_interrupt`, VCPU create |
| [`zircon/kernel/arch/arm64/hypervisor/el2.S`](../../../zircon/kernel/arch/arm64/hypervisor/el2.S) | world switch; save/restore ICH |
| [`zircon/kernel/dev/interrupt/gic/common/arm_gic_hw_interface.cc`](../../../zircon/kernel/dev/interrupt/gic/common/arm_gic_hw_interface.cc) | `gic_get_lr_from_vector` |
| [`userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.cc`](../../../userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.cc) | guest GICD/GICR MMIO |
| [`userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.h`](../../../userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.h) | guest IPA layout |
| [`userspace/hypervisor/bin/vmm/io.h`](../../../userspace/hypervisor/bin/vmm/io.h) | `kDevicePhysBase = 0x800000000` |

Suggested first pass: **VBAR IRQ slot → `arm64_irq` → `ICC_IAR1` → UART or
timer handler → `ICC_EOIR1`**. Then **EL0 SYNC abort → DFSC →
`vmm_page_fault_handler` → `eret` or `ZX_EXCP_FATAL_PAGE_FAULT`**. Then
**`gich_maybe_interrupt` + LR format + `gic_distributor.cc`**. That is
enough to see a host timer tick, a first-touch page fill, an unmapped
load kill `zxsh`, and a guest `vmm launch` inject vINTID 0 (SGI) or a
virtio SPI.

## Anti-patterns

* **Forget `ICC_EOIR1_EL1`.** The INTID stays Active.
* **Handle `svc` in `arm64_sync_exception`.** Syscalls peel off in assembly.
* **Program the host GICD from a guest.** Guests see emulated GICD + ICH LRs.

## First-pass checklist

- [ ] VBAR IRQ slot → `arm64_irq` → `ICC_IAR1` → handler → `ICC_EOIR1`
- [ ] EL0 SYNC abort → DFSC → `vmm_page_fault_handler`
- [ ] Guest inject: `gich_maybe_interrupt` + LR

## Related

* [`bootstrap.md`](bootstrap.md)
* [`memory.md`](memory.md)
* [`cpu.md`](cpu.md)
* [`interrupt.md`](interrupt.md)
* [`ipc.md`](ipc.md)
* [`async.md`](async.md)
* [`sync.md`](sync.md)

## 6. Terminology

| Term | Meaning in this note |
| --- | --- |
| Exception | ARM: any entry through `VBAR_EL1` (SYNC / IRQ / FIQ / SError). This section uses it mainly for **SYNC** (faults, BRK). IRQ is **GIC**. |
| SYNC | Synchronous exception. Taken on the faulting instruction: abort, `svc`, `BRK`, undef, alignment. |
| IRQ | Asynchronous interrupt from the GIC. |
| SError | Asynchronous abort; impl-defined. Counted, not treated as fatal here. |
| `VBAR_EL1` | Exception vector base. 16 slots × 128 bytes. |
| iframe | `iframe_t` on `SP_EL1`: GPRs, `ELR`, `SPSR`, user SP. See [`cpu.md`](cpu.md). |
| `ESR_EL1` | Exception Syndrome Register. `EC` selects the class; `ISS` is the class payload. |
| `EC` | Exception Class, `ESR[31:26]`. Instruction abort, data abort, `BRK`, `svc`, … |
| `ISS` | Instruction Specific Syndrome, `ESR[24:0]`. For aborts: DFSC, WnR, CM. |
| `FAR_EL1` | Fault Address Register. VA for abort / PC alignment / watchpoint. |
| DFSC | Data/instruction Fault Status Code, `ISS[5:0]`. Translation / access / permission / alignment. |
| Translation fault | No valid descriptor at that level. `VMM_PF_FLAG_NOT_PRESENT`. Demand fill or fatal. |
| Access-flag fault | Mapping exists but AF is clear. `AccessedFault`, no new page. |
| Permission fault | Mapping exists, access not allowed (RO, NX, EL0 vs EL1). |
| Page fault | Software name for an abort the VM may resolve. `vmm_page_fault_handler`. |
| Fatal page fault | Unresolved abort delivered as `ZX_EXCP_FATAL_PAGE_FAULT`. |
| User-copy | Kernel accessing a user VA (`arch_copy_*_user`). Uses `data_fault_resume` so a bad user pointer does not panic. |
| `data_fault_resume` | Kernel PC (plus capture bits) to return to on a captured data abort. |
| Exceptionate | Channel that receives a `zx_exception_t`. Thread, process, job, and process-debug. |
| Exception walk | debug → thread → process → (second-chance debug) → jobs up to root. |
| `ZX_EXCP_*` | Userspace exception type in [`exception.h`](../../../zircon/system/public/zircon/syscalls/exception.h). Hardware or synthetic (`POLICY_ERROR`, thread start/exit). |
| `exception_die` | Kernel fatal: print ESR/FAR/iframe, halt. Not a user exception. |

