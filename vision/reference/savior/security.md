# Security

**After this:** you can name what stage-2 actually stops.
**First read:** Principle + diagrams 1–4. **Next:** [`logging.md`](logging.md).

Savior's isolation is **stage-2 plus traps plus an interconnect
firewall**. The **host** is a Zircon microkernel
([`microkernel.md`](microkernel.md)); that does not isolate
guest from guest — S2 does. Guests are EL1h. They never run at
EL2. This note is what is actually enforced, what is only
convention (lua), and where a guest can still hurt another.

Read [`memory.md`](memory.md) for S2 / SMPU / SMMU,
[`cpu.md`](cpu.md) for `HCR` traps, [`interrupt.md`](interrupt.md)
for HW-bit / exclusive SPI, [`gpu.md`](gpu.md) for grant,
[`tee.md`](tee.md) for SMC tagging, [`devices.md`](devices.md)
for identity windows.

## Principle

1. **Stage-2 is the CPU and DMA boundary.** A GPA with no S2 PTE
   cannot be fetched or stored by that guest, and (with SMMU S2)
   cannot be DMAed. Identity maps are still isolation: the
   *other* guest's RAM is simply not in this table.

2. **HCR is the instruction boundary.** `TSC` (SMC), `TWI` (WFI),
   `IMO`/`FMO` (physical IRQ/FIQ to EL2), `TID3` (ID regs), `VM`
   (S2 on). Guests cannot program `VTTBR` or `HCR`. They cannot
   `eret` to EL2.

3. **Lua is not a security monitor.** SOS `setVmem("IO", 0, 1GiB)`
   maps a gigabyte of MMIO because the product needs it. Exclusive
   SPIs and later unmaps are the real narrowing. A missing
   `setVgicIrq` does not unmap the device.

4. **TEE trusts Savior's VMID, not the guest.** Forwarded SMC
   overwrites `x7`. GPU grant is exclusive in software: two
   `MapDevice`s at once is a bug in `GpuScheduler`, not a hardware
   Access Window.

5. **EL0 cannot skip the kernel.** Every `zx_guest_*` /
   `zx_vcpu_*` checks handle rights ([`cpu.md`](cpu.md)
   Syscalls). Trap kinds are only `BELL` / `MEM` / `IO`; unknown
   kind is `ZX_ERR_INVALID_ARGS`. There are no `VHM_*` trap kinds.
   GPU syscalls refuse PHYP / QEMU_AHYP / OKL4_AHYP. The VMM is
   still a privileged process: isolation from a **guest** is S2;
   isolation from a **buggy VMM** is not this note.

## Framework

```text
 Guest EL1h
    VA ── guest S1 ──► IPA
                       │
                       ▼  VTTBR  S2AP + MemAttr
                      PA ──► SMPU (SoC interconnect) ──► DRAM / device
 Device DMA
    IOVA ── SMMU S1? ──► IPA ── same S2 TTB ──► PA ──► SMPU

 HCR.TSC / TWI / IMO / TID3     trap to Savior
 ICH_LR HW bit                  guest EOI vs physical INTID
 GpuScheduler MapGpu            only current_vmidx_ has Mali S2
 TeeSmcHandler x7=vmid          cannot forge client id
```

No guest EL2: `HCR.RW` (AArch64 EL1). Host is EL1h; EL2 is the
trampoline only. Guest `hvc` is a no-op (`x0 = -1`).

## Diagrams

### 1. Four layers (what actually stops a guest)

```text
 Guest EL1h
    │  instruction / load / store
    ▼
 [1] HCR traps
       TSC  smc → Savior, not EL3
       TWI  wfi → Savior
       IMO  physical IRQ → EL2, not guest
       TID3 ID regs sanitized
    │  still a guest load to some IPA
    ▼
 [2] Stage-2  (VTTBR)
       no PTE        →  DABT / IABT
       S2AP denies   →  abort
       identity PTE  →  that PA
    │
    ▼
 [3] SMMU  (DMA only)
       STE S2TTB = same VTTBR
       StreamID missing → SoC bypass or abort (board)
    │
    ▼
 [4] SMPU  (SoC interconnect, QEMU has none)
       kMpu windows left out of S2 on purpose
```

Lua decides **which** IPA is in [2]. It does not implement [1]
or [4].

### 2. Two walks + DMA (same S2)

```text
 Guest CPU                         Device DMA
 VA ── S1 (guest TTBR) ── IPA      IOVA ── SMMU S1? ── IPA
                           │                              │
                           └──────── VTTBR S2 ────────────┘
                                          │
                                          ▼
                                         PA
                                          │
                                          ▼  SMPU
                                       DRAM / MMIO
```

A hole in S2 is a hole for both legs. GPU ungrant unmaps Mali
MMIO for the CPU; it does **not** retune the SMMU STE (boot-time).

SMPU is **after** PA on the SoC interconnect (ATF/TEE program it;
Savior EL1 cannot). Product `setVmem` uses `kCached`/`kDevice`,
not `kMpu`. Holes are punched from DTB reserved-memory names
(`unmap_mpu_mem_region`). SOS must not `setVmemAuto("gz_nebula")`
(overlaps RAM identity). QEMU has no SMPU. Detail:
[`memory.md`](memory.md) **SMPU**.

### 3. HCR trap path

`Vcpu::Create` ignores the syscall `hcr` argument. This is the
product mask:

```text
 guest  smc / wfi / mrs ID / physical IRQ
    HCR bit set?
       TSC  →  handle_smc_instruction
       TWI  →  handle_wfi          Wait on GichState
       TID3 →  handle_tid3_reg     sanitized ID_AA64*
       IMO  →  EL2 IRQ vector      Resume swallows, loop
    not set (TWE, TVM)
       WFE / VM sysregs run in guest
```

| Bit | Guest cannot |
| --- | --- |
| `VM` | skip stage-2 |
| `PTW` | walk a table the S2 would reject, silently |
| `IMO` / `FMO` | take a physical IRQ/FIQ in EL1 |
| `TWI` | `wfi` on the physical core |
| `TSC` | `smc` to EL3 |
| `TID3` | read raw `ID_AA64*` (sanitized) |
| `AMO` | (HEE) physical SError in EL1 if `WITH_VIRT_SERROR` |
| `RW` | AArch32 EL1 |

Not set: `TWE` (WFE), `TVM` (VM sysregs after boot). Guest
`SCTLR`/`TTBR` live in `El2State` and are switched on enter.

### 4. What a guest can still do

Isolation is per-table, not “the other VM is invisible in
hardware”.

```text
 SOS identity 0–1 GiB IO (QEMU)
    SOS CPU can program any device in that window
    unless a later trap / kMpu / GPU ungrant punched a hole

 Passthrough SPI bound to this VM
    guest EOI of a HW LR deactivates the **physical** INTID
    wrong HW bit = storm or a stuck Active

 Shared pages  (occupancy, ioreq, ipc-mbox, cvm-lock, GPU cmdbuf)
    both sides agreed; S2 maps them on purpose

 Forged x7 on a TEE SMC
    harmless; Savior overwrites
 Forged x0 on a HYP FID
    validate_hypervcall + inject IABT

 Two MapDevice at once
    GpuScheduler bug, not a hardware window
```

## What S2 actually does

| Mapping | Who may use it | Who must not |
| --- | --- | --- |
| SOS RAM identity | SOS | UOS / tbox tables omit it |
| tbox reserved | tbox | SOS `nbl,guest-memory` hole |
| SOS 0–1 GiB IO | SOS CPU | UOS unless VHM maps a BAR |
| Mali MMIO | granted VM only | the other VM is unmapped |
| `kMpu` | nobody in S2 | TEE / SMPU owns the PA |
| trap hole | nobody until VMM fills | guest DABT |

S2AP is R/W/X as lua `ZX_VM` flags. A guest cannot raise them.
Instruction abort on an XN IPA is `handle_instruction_abort`.

SMPU is **after** S2, on the SoC. QEMU has none. A `kMpu` window
is left out of S2 so even a buggy map cannot program that PA;
DMA to it is still denied by the firewall if an STE were wrong.

## IRQ isolation

- Passthrough SPI is bound to **one** VM
  (`ZX_INTERRUPT_PASSTHROUGH` / `interrupt_bind_cpu`).
  Injecting it `VIRTUAL` into another VM is asserted against
  (`Vcpu::Interrupt`).
- Broadcast vs exclusive: lua. A broadcast SPI in two LRs with
  HW=1 is a storm.
- Guest SGI is trapped `ICC_SGI1R` and becomes a virtual inject
  inside that Guest, never a host SGI.

## GPU

SWA is **time-slice exclusive**. Only `current_vmidx_` has Mali
MMIO in S2 and Mali IRQs as PHYSICAL. The other VM's driver
instance is Idle / Stopping. There is no hardware Access Window
on this product. Two grants is a state-machine bug.

## SMC / TEE

Illegal Savior HYP parameters inject an **instruction abort**
into the guest, they are not forwarded. PSCI `SYSTEM_RESET` from
**any** guest (including SOS) is canceled after
`ramconsole_snapshot_take` (no SoC reset). See
[`logging.md`](logging.md).

## What this is not

- Not a formally verified separation kernel.
- Not hardware GPU partition (no Mali Resource Group).
- Not “SOS cannot DMA to tbox” unless the SMMU STE + S2 + SMPU
  are all programmed. Identity SOS IO is a large window.
- Not protection against a guest that is **given** a passthrough
  device with a broken IOMMU path.

## Pitfalls

- **Identity IO + missing trap.** Guest programs a block Savior
  thought was private.
- **SMMU bypass.** Device with StreamID not in the STE still
  hits PA if the SoC allows bypass; SMPU is the last gate.
- **HW bit on a virtual inject.** Physical INTID stuck Active.
- **Forged `x7`.** Harmless; Savior overwrites. Forged `x0` on a
  HYP FID is not: validate_hypervcall + inject abort.
- **`zx_vcpu_create` hcr.** Ignored. Do not think a VMM can
  drop `TSC`.

## Terminology

| Term | Meaning here |
| --- | --- |
| Isolation | S2 + HCR + IRQ bind + (SoC) SMPU |
| Identity | GPA = HPA in that guest's S2 |
| Hole | IPA with no PTE (trap or `kMpu`) |
| Grant | Exclusive `MapDevice` for Mali |
| Client id | kernel VMID in forwarded SMC `x7` |

## Code

| Step | File |
| --- | --- |
| S2 PTE / S2AP | `zircon/kernel/arch/arm64/mmu.cpp`, [`memory.md`](memory.md) |
| HCR mask | `vcpu.cpp` `Vcpu::Create` |
| TID3 sanitize | `vmexit.cpp` `handle_tid3_reg` |
| SMC validate | `vmexit.cpp` `validate_hypervcall` |
| IRQ bind | `interrupt_event_dispatcher.cpp`, `Vcpu::Interrupt` |
| GPU map | `gpu_sched_backend_adapter.cpp` |
| TEE x7 | `tee/tee.cpp` `TeeSmcHandler` |
| SMPU | [`memory.md`](memory.md) SMPU |

---

Prev: [`guest.md`](guest.md) · Next: [`logging.md`](logging.md) · [`README.md`](README.md)

Everyday's a Hustle.
