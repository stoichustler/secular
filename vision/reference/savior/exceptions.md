# Exceptions

**After this:** you can say whether a fault is a guest inject, a
host exception-port packet, or a kernel panic into ATF.
**First read:** Principle + diagrams 1–4.
**Prev:** [`cpu.md`](cpu.md). **Next:** [`memory.md`](memory.md).

Three mechanisms share the English word “exception”. They do
**not** share a vector table or a handler.

| World | Vector | Who runs |
| --- | --- | --- |
| Guest intercept | `VBAR_EL2` `el2.S` | Savior, then maybe inject or packet |
| Guest taken fault | guest `VBAR_EL1+0x200` | **guest** Linux, after Savior injects |
| Host user fault | `VBAR_EL1` `exceptions.S` | exception port (crashlogger) |
| Host kernel fault | same `VBAR_EL1` | `exception_die` → ATF WDT |

Guest Linux `svc` / page-fault in its own S1 never hits Savior
if S2 allows the IPA. [`cpu.md`](cpu.md) intercepts;
[`microkernel.md`](microkernel.md) diagram 2.

## Principle

1. **EL2 sees only what HCR traps.** SMC, WFI, S2 abort, TID3.
   A guest EL0 data abort that S1 + S2 both allow is **guest**
   `VBAR_EL1`, not Savior.

2. **Inject is rewriting guest EL1 sysregs, not a packet.**
   `inject_data_abort_exception` sets `ESR_EL1` / `FAR_EL1` /
   `ELR_EL1` / `SPSR` (DAIF masked) and points `ELR_EL2` at
   `VBAR_EL1+0x200`. Next `eret` the **guest** takes the abort.

3. **Host user faults go to exception ports, in order.**
   debugger → thread → process → job…root → system. No port
   (or `ZX_RESUME_TRY_NEXT` exhausted) → kill the process.
   This is **not** used for guest Linux crashes.

4. **Kernel unhandled sync exception is fatal.**
   `exception_die` → `notify_exception_to_atf` (`PSCI64_PSCI_WDT_REBOOT`
   vendor panic) → BL2 kedump. Vmm `HandleMem` failure must
   **inject** a guest DABT instead of taking this path.

## Framework

```text
 Guest EL1  ldr  unmapped IPA
    HCR.VM  EL2  DABT
    handle_data_abort
       trap MEM?     packet, Vmm emulate or inject
       no trap?      vmm_guest_page_fault  or inject DABT
         │
         ▼  inject_data_abort_exception
    next eret  guest VBAR_EL1+0x200   (guest handler)

 Guest EL1  smc  illegal VHM FID (wrong x7)
    handle_smc_instruction  next_pc first
    inject_instruction_abort_exception
         │
         ▼  FAR/ELR_EL1 = instruction **after** smc

 Host EL0  bad ptr
    VBAR_EL1  arm64_sync_exception
    try_dispatch_user_exception
       ExceptionPortIterator
       packet on bound port, thread paused
       zx_task_resume(EXCEPTION) or TRY_NEXT or kill

 Host EL1  bug in Savior
    exception_die
    PSCI WDT_REBOOT  ATF  BL2 kedump
    then platform_halt (does not return)
```

## Diagrams

### 1. Guest intercept vs guest taken abort

```text
 guest store
    S2 PTE missing, no trap
       inject DABT
       ESR_EL1 = (0x25<<26)|0x10     EC=Data Abort, same-EL
       FAR_EL1 = IPA
       ELR_EL1 = faulting PC (saved elr_el2)
       SPSR    DAIF masked
       ELR_EL2 = VBAR_EL1 + 0x200    same-EL offset
    eret  →  guest Linux sync handler

 guest smc  VHM/virtgpu-dbg FID, x7 secure_id != 1
    next_pc first, then inject IABT
       ESR_EL1 = (0x21<<26)|0x10     EC=Instruction Abort
       FAR_EL1 = elr_el2             (already advanced past smc)
```

`zx_vcpu_inject_exception(vcpu, far)` is the Vmm-callable DABT
path (`ZX_RIGHT_WRITE`). Unhandled `HandleMem` uses it so the
**guest** sees a fault, the VCPU thread does not die.

`force_inject_data_abort` (debug cmd) skips demand-fault fill
and always injects.

### 2. Host user exception ports

`zx_task_bind_exception_port(task, port, key, flags)`.
`ZX_EXCEPTION_PORT_DEBUGGER` binds the process debugger slot.

Search order (`exception.cpp` `ExceptionPortIterator`):

```text
 1. process debugger port
 2. thread port
 3. process port
 4. job port, then parent jobs up to root
 5. system port   (crashlogger binds this)
```

Packet type `ZX_PKT_TYPE_EXCEPTION(*)`:
`GENERAL`, `FATAL_PAGE_FAULT`, `UNDEFINED_INSTRUCTION`,
`SW_BREAKPOINT`, `HW_BREAKPOINT`, `UNALIGNED_ACCESS`,
synthetic `THREAD_STARTING` / `EXITING` (debugger only),
`POLICY_ERROR` (job policy + `ACTION_EXCEPTION`).

ARM report: `esr`, `far`. Thread is paused until
`zx_task_resume(ZX_RESUME_EXCEPTION)` or
`ZX_RESUME_EXCEPTION|ZX_RESUME_TRY_NEXT`.

crashlogger: `zx_task_bind_exception_port(subject, eport, …)`
on the **system** port (`PA_USER0` task + port in processargs
for the crashlogger process itself).

A **guest** panic never hits these ports. Guest crash is guest
Linux + maybe occupancy/watchdog / PSCI RESET.

### 3. Host kernel die

```text
 arm64_sync_exception
    same-EL unhandled  →  exception_die
    lower-EL no port   →  exception_die
 notify_exception_to_atf
    smc PSCI64_PSCI_WDT_REBOOT
    type = vendor | NEBULA_PANIC_EXCEPTION | MTK_DOMAIN_AEE
 BL2 kedump  SYS_NEBULA_RAMCONSOLE_DUMP_T
```

[`logging.md`](logging.md): this is the **whole-SoC** dump path.
SOS `SYSTEM_RESET` canceled in-hypervisor does **not** run this.

### 4. IRQ is not this chapter

Physical IRQ while a VCPU is in guest: EL2 IRQ vector,
`ZX_ERR_NEXT`, swallow, loop ([`interrupt.md`](interrupt.md)).
Not `inject_*_abort`. Virtual IRQ is `ICH_LR`, not `ESR_EL1`.

SError: `HCR.AMO` if `WITH_VIRT_SERROR` (this `venus-hee.mk`
has it). Physical SError to EL2, not guest EL1, unless
virtualized.

## Guest abort inject (fields)

| | DABT | IABT |
| --- | --- | --- |
| Function | `inject_data_abort_exception` | `inject_instruction_abort_exception` |
| `ESR_EL1.EC` | `0x25` | `0x21` |
| ISS FSC | `0x10` (sync external abort) | same |
| `FAR_EL1` | guest IPA (not next_pc) | `elr_el2` after `next_pc` (past `smc`) |
| Vector | `VBAR_EL1+0x200` | same (current EL, SP0 unused: **same-EL** offset) |

AArch64 same-EL sync offset is `+0x200` from `VBAR_EL1`. Guest
must have programmed `VBAR_EL1` before this can land.

## Pitfalls

- **Vmm crash vs guest DABT.** Unhandled BAR: inject, do not
  let the VCPU thread take a host fault (that kills Vmm →
  GuestManager relaunch).
- **Exception port on Guest handle.** There is none. Bind
  ports on **host** thread/process/job.
- **Guest `brk`.** If not trapped, guest debug; host `BRK` in
  EL0 is `ZX_EXCP_SW_BREAKPOINT` on the Vmm thread.
- **`+0x200` with VBAR=0.** Inject to IPA 0x200 — guest reset
  vector if they never set VBAR.

## Terminology

| Term | Meaning here |
| --- | --- |
| Intercept | EL2 took HCR/S2; Savior handles |
| Inject | Savior writes guest `ESR/FAR/ELR`, next `eret` guest takes it |
| Exception port | Host Zircon: port packet, thread paused |
| `exception_die` | Host kernel fatal → ATF WDT |

## Code

| Step | File |
| --- | --- |
| Guest DABT/IABT inject | `vmexit.cpp` `inject_*_abort_exception` |
| `zx_vcpu_inject_exception` | `vcpu.cpp` `Vcpu::InjectException` |
| Host sync | `exceptions.S`, `exceptions_c.cpp` |
| Port walk | `kernel/object/exception.cpp` |
| Bind | `zx_task_bind_exception_port` `exception.h` |
| Kernel panic → ATF | `notify_exception_to_atf` |
| crashlogger | `zircon/system/core/crashlogger/` |

---

Prev: [`cpu.md`](cpu.md) · Next: [`memory.md`](memory.md) · [`microkernel.md`](microkernel.md)

Everyday's a Hustle.
