# Interrupt

Secular boots a Zircon kernel on `qemu-system-aarch64` (virt, arm64). The
kernel runs at EL1. Device IRQs, timers, and IPIs arrive through a **GICv3**.
A guest (Linux or RTOS) sees a **vGIC**: the same GICv3 virtualization
extensions at EL2, plus a distributor the userspace VMM emulates. This note
is the map of that interrupt path.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read `cpu.md` for iframe / DAIF / `x20`, and `memory.md` for the MMU. Guest
entry is `virtualization.md` plus
`userspace/hypervisor/README.md`.

## GIC

### Framework

Host GICv3 on the left (physical IRQs). Guest vGIC on the right (virtual
IRQs in ICH list registers; GICD/GICR emulated in `bin/vmm`).

```text
┌────────────────────────────────┐    ┌────────────────────────────────┐
│ Host  kernel EL1               │    │ Guest  Linux / RTOS VCPU       │
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

## vGIC

A guest does not program the host GICD. It sees a virtual GICv3:

```text
guest EL1
  ICC_*_EL1          virtual CPU interface (hardware, GICv3 VE)
  GICD / GICR MMIO   emulated by userspace VMM (traps)
        │
        ▼  zx_guest_set_trap  (MMIO)
EL2  ICH_*           list registers, VMCR, HCR
        │  inject vIRQ into the virtual CPU interface
        ▼
host EL1  VMM thread  (bin/vmm) + kernel Vcpu
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

## Where in the tree

Read in this order.

### Host GIC

| File | Why |
| --- | --- |
| `zircon/kernel/lib/arch/arm64/include/lib/arch/arm64/exception-asm.h` | VBAR layout |
| `zircon/kernel/arch/arm64/exceptions.S` | IRQ slot → iframe → `arm64_irq` |
| `zircon/kernel/arch/arm64/exceptions_c.cc` | `arm64_irq` → `platform_irq` → preempt |
| `zircon/kernel/arch/arm64/include/arch/arm64/interrupt.h` | DAIF |
| `zircon/kernel/dev/interrupt/include/dev/interrupt.h` | mask / handler API |
| `zircon/kernel/dev/pdev/interrupt/interrupt.rs` | `platform_irq`, IPI, registration |
| `zircon/kernel/dev/interrupt/gic/v3/arm_gicv3.rs` | GICD/GICR/ICC, `handle_irq` |
| `zircon/kernel/dev/interrupt/gic/v3/regs.rs` | `ICC_IAR1` / `EOIR1` / `SGI1R` |
| `zircon/kernel/dev/interrupt/gic/common/include/dev/interrupt/arm_gic_common.h` | SGI/PPI/SPI bases |
| `zircon/kernel/include/kernel/mp.h` | four IPI kinds |
| `zircon/kernel/kernel/interrupt.rs` | `int_handler_start` / `finish` |
| `zircon/kernel/phys/lib/boot-shim/` | DTB → ZBI GIC item |

### vGIC

| File | Why |
| --- | --- |
| `zircon/kernel/arch/arm64/include/arch/arm64/hypervisor/gic/gicv3.h` | LR / VMCR / VTR bits |
| `zircon/kernel/arch/arm64/hypervisor/gic/gicv3.cc` | ICH ops registered with the GIC layer |
| `zircon/kernel/arch/arm64/hypervisor/gic/el2.S` | EL2 read/write of ICH_* |
| `zircon/kernel/arch/arm64/hypervisor/vcpu.cc` | `gich_maybe_interrupt`, VCPU create |
| `zircon/kernel/arch/arm64/hypervisor/el2.S` | world switch; save/restore ICH |
| `zircon/kernel/dev/interrupt/gic/common/arm_gic_hw_interface.cc` | `gic_get_lr_from_vector` |
| `userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.cc` | guest GICD/GICR MMIO |
| `userspace/hypervisor/bin/vmm/arch/arm64/gic_distributor.h` | guest IPA layout |
| `userspace/hypervisor/bin/vmm/io.h` | `kDevicePhysBase = 0x800000000` |

Suggested first pass: **VBAR IRQ slot → `arm64_irq` → `ICC_IAR1` → UART or
timer handler → `ICC_EOIR1`**. Then **`gich_maybe_interrupt` + LR format +
`gic_distributor.cc`**. That is enough to see a host timer tick, and to see
a guest `vmm launch` inject vINTID 0 (SGI) or a virtio SPI.

---

Everyday's a Hustle.
