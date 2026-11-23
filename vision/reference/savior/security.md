# Security

Savior's isolation is **stage-2 plus traps plus an interconnect
firewall**, not a second microkernel. Guests are EL1h. They never
run at EL2. This note is what is actually enforced, what is only
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
   Syscalls). `VHM_*` traps cannot be installed with
   `zx_guest_set_trap` (`ACCESS_DENIED`). GPU syscalls refuse
   non-AHYP. The VMM is still a privileged process: isolation
   from a **guest** is S2; isolation from a **buggy VMM** is
   not this note.

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

No guest EL2: `HCR.RW` (AArch64 EL1), VHE host holds `E2H`.
Guest `hvc` is a no-op (`x0 = -1`).

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

## HCR traps (product mask)

From `Vcpu::Create` (the syscall `hcr` argument is **ignored**):

| Bit | Guest cannot |
| --- | --- |
| `VM` | skip stage-2 |
| `PTW` | walk a table the S2 would reject, silently |
| `IMO` / `FMO` | take a physical IRQ/FIQ in EL1 |
| `TWI` | `wfi` on the physical core |
| `TSC` | `smc` to EL3 |
| `TID3` | read raw `ID_AA64*` (sanitized) |
| `AMO` | (HEE) physical SError in EL1 if `WITH_VIRT_SERROR` |

Not set: `TWE` (WFE), `TVM` (VM sysregs after boot). Guest
`SCTLR`/`TTBR` live in `El2State` and are switched on enter.

## IRQ isolation

- Passthrough SPI is bound to **one** VM (`pdev_get_int_vmid`).
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
into the guest, they are not forwarded. PSCI RESET from a
non-platform guest is canceled (no SoC reset). See
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
| IRQ VMID | `pdev_get_int_vmid`, `Vcpu::Interrupt` |
| GPU map | `gpu_sched_backend_adapter.cpp` |
| TEE x7 | `tee/tee.cpp` `TeeSmcHandler` |
| SMPU | [`memory.md`](memory.md) SMPU |

---

Everyday's a Hustle.
