# Interrupt

Crook is a type-1 hypervisor. The host GIC is GICv3. Guests see a
**VGIC**: emulated GICD/GICR MMIO plus the GICv3 virtual CPU interface
(`ICH_*_EL2`). Physical IRQs can be **passed through** into a VCPU, or
the VMM can **inject** a virtual IRQ / MSI / LPI that never existed on
the wire.

Read [`bootstrap.md`](bootstrap.md) for the EL2 / VCPU `eret`,
[`memory.md`](memory.md) for stage-2, [`scheduler.md`](scheduler.md)
for VCPU pin / IPI wakeup.

This note is the map of that path: how a SPI/PPI/SGI/MSI is configured,
how it reaches a VCPU bitmap, how `ICH_LR<n>_EL2` is filled, and which
policy (passthrough vs virtual, exclusive vs broadcast, HW bit) applies.

## Framework

Three planes share one GICv3.

```text
 Device / PCI MSI / guest IPI
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Host GICv3   GICD 0x08000000  GICR 0x080A0000 (QEMU virt)      │
 │  SPI / PPI pending on Distributor / Redistributor              │
 │  CPU interface → EL2 while a VCPU is running (HCR.IMO/FMO)     │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Crook kernel EL1 / EL2                                         │
 │  arm_gicv3  IRQ handler                                        │
 │  InterruptEventDispatcher  (passthrough)                       │
 │  Vcpu::Interrupt → GichState bitmap  PHYSICAL | VIRTUAL        │
 │  gich_maybe_interrupt → ICH_LR<n>_EL2  then eret guest         │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ VMM  (guest_manager / machina)   EL0                           │
 │  GicDistributor  traps GICD/GICR                               │
 │  GicIts          traps ITS, resolves MSI → LPI                 │
 │  VhmService / VhmDevice   FE↔BE notify                         │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
   Guest EL1h   VGIC driver  (Linux GIC / virtio / ACRN)
```

| Plane | What it owns |
| --- | --- |
| Hardware GICv3 | Pending/active bits, routing, MSI write to ITS (if the SoC has one) |
| Host kernel | IRQ handler, `InterruptEventDispatcher`, VCPU bitmap, `ICH_LR` |
| VMM | Which SPIs are passthrough, GICD/GICR/ITS emulation, VHM/MSI |
| Guest | VGIC driver, EOI, `ICC_SGI1R_EL1`, virtio/MSI programming |

Two bits decide how a vector is injected:

| Bit / type | Meaning |
| --- | --- |
| `InterruptType::PHYSICAL` | Came from the host GIC. LR gets `ICH_LR_HARDWARE`. Guest EOI deactivates the **physical** INTID. |
| `InterruptType::VIRTUAL` | Software inject. No HW bit. Guest EOI only clears the LR. |

Wrong HW bit is the usual lost-IRQ / IRQ-storm bug (see **Pitfalls**).

### IRQ classes

```text
INTID
  0–15     SGI     IPI. Guest write ICC_SGI1R_EL1 is trapped.
 16–31     PPI     per-CPU (timer, …). Bound exclusively to one VCPU.
 32–1019   SPI     shared. Lua lists which ones SOS/tbox own.
4096–5119  eSPI    extended SPI, same passthrough path.
 ≥8192     LPI     MSI via emulated ITS. No physical GIC pending.
```

QEMU SOS (`sos.lua`) identity-maps host GIC MMIO and forwards SPIs
`36–251` and `253–512`, plus PPI 15. tbox uses the same GICD/GICR
base but does **not** take the SOS SPI range.

## Configuration

### Lua → VMM

`guest_manager` loads `sos.lua` / `tbox.lua`. VGIC fields become a
`VgicSpec` (`main_uos.cc` `vgic_build_from`):

```text
sos_config:setVgicPaddr(0x8000000, 0x80a0000)   GICD, GICR
sos_config:setVgicPercpuIrq(15)                 PPI passthrough
sos_config:setVgicIrq(i)                        SPI passthrough  (36–251, 253–512)
sos_config:setVhmIrq(254)
sos_config:setVsockIrqs(...)
sos_config:setVteeNotifierIrq(513)
```

`GicDistributor::Init` then:

1. `PassThroughInterrupts(irqs)` for every listed SPI.
2. Map guest GICD/GICR at those GPAs (`TrapType::MMIO_SYNC`) so guest
   GIC register access is emulated, not a raw stage-2 map of the host
   GIC (the 0–1 GiB IO window is identity, but GICD/GICR traps win).
3. One `GicRedistributor` per VCPU, each with the PPI list.
4. Optional `GicIts::Init` if `vgic.has_its`.

### Passthrough object

For each SPI/PPI:

```text
zx::interrupt::create(root_resource)
interrupt.bind(0, resource, vector, ZX_INTERRUPT_PASSTHROUGH)
```

`ZX_INTERRUPT_PASSTHROUGH` →
`InterruptEventDispatcher::PassThrough`:

- `register_int_handler(vector, VcpuIrqHandler, this)`
- `interrupt_reset(vector)` so a leftover Active cannot block the VM
- left **masked**; guest GICD ISENABLE / VMM `unmask` enables it later

`RegisterVcpu` / `GicRedistributor::BindVcpu` then
`interrupt.bind_vcpu(vcpu, exclusive)`:

- exclusive SPI: `interrupt_bind_cpu(vector, vcpu.bind_cpu_num())` and
  a one-element `vcpus_[]`
- PPI: always exclusive to that VCPU
- non-exclusive: the dispatcher keeps every VCPU and indexes by
  `arch_curr_cpu_num() % vcpus_.size()`

Guest writes to GICD IENABLE / ICFG / IROUTE are emulated and turn
into `unmask_interrupt` / `configure_interrupt` / `BindVcpuExclusively`.

### VCPU ICH state

On `Vcpu` create (`vcpu.cpp`):

```text
hcr = VM|PTW|FMO|IMO|FB|BSU_IS|TWI|TSC|RW|APK|API|ATA|TID3
ich_state.num_lrs = ICH_VTR.LRs          // up to 16
ich_state.elrsr   = all LRs empty
GichState.interrupt_tracker  two-level bitmap, 65535 vectors
```

`HCR.IMO` / `HCR.FMO`: while the guest runs, physical IRQ/FIQ take
the EL2 vector, not guest EL1. That is why a passthrough SPI still
hits the **host** GIC driver first.

## Physical interrupt (passthrough)

Device SPI/PPI that the guest owns. HW bit set.

```text
 Device asserts SPI/PPI
         │
         ▼  (1) GICv3 Distributor / Redistributor pending
         │
         ▼  (2) CPU interface
         │
         ▼  (3) if VCPU running: HCR.IMO → EL2, VM exit
                else: host EL1 IRQ
         │
         ▼  (4) arm_gicv3  registered handler
         │
         ▼  (5) InterruptEventDispatcher::VcpuIrqHandler
                pick vcpus_[cpu % n]; if that VCPU is PWROFF, vcpu0
         │
         ▼  (6) VcpuDispatcher::PhysicalInterrupt(vector)
                GichState.Track(vector, PHYSICAL)
                Signal; IPI the pinned pCPU if the VCPU thread is running
         │
         ▼  (7) Vcpu::Resume  gich_maybe_interrupt
                Pop bitmap → empty ICH_LR
                LR = PENDING | GROUP1 | VIRTUAL_ID | HARDWARE | PHYSICAL_ID
                prio 0 if SGI/PPI, 0x10 if SPI
         │
         ▼  (8) gic_write_gich_state  (ICH_HCR.En, maybe UIE)
         │
         ▼  (9) eret guest EL1h
         │
         ▼  (10) guest VGIC: virtual IRQ, Linux handle_irq
         │
         ▼  (11) guest EOI  (ICC_EOIR / DIR)
                HW=1 → GIC deactivates the **physical** INTID
```

`VcpuIrqHandler` returns `IRQ_EOI_PRIORITY_DROP`, not
`IRQ_EOI_DEACTIVATE`. The host must **not** deactivate: the LR HW bit
is what lets the guest EOI retire the physical interrupt.

Level-triggered lines stay pending until the device deasserts. The
dispatcher can `configure_interrupt(LEVEL, …)` from GICD ICFG.

**Bind / affinity.** `SetSPIVcpuMask` plus `BindVcpuExclusively`
pins an SPI to one VCPU (and that VCPU's pCPU). Latency-sensitive
devices use this. Broadcast (`GICD_IROUTER.IRM`) keeps all VCPUs in
`vcpus_[]`.

## Virtual interrupt

No host GIC pending. Used for virtio, VHM doorbells, guest IPI, LPI.

```text
 VMM or kernel  Vcpu::Interrupt(vector)          // default VIRTUAL
         │
         ▼  GichState.Track(vector, VIRTUAL)
            Signal + IPI pCPU
         │
         ▼  gich_maybe_interrupt
            LR = PENDING | GROUP1 | VIRTUAL_ID     // no HARDWARE
         │
         ▼  eret guest → guest VGIC → EOI clears LR only
```

Userspace injects with `zx_vcpu_interrupt(vcpu, vector)` →
`sys_vcpu_interrupt` → `VcpuDispatcher::Interrupt`. If the host GIC
already has a handler on that vector (`pdev_has_int_handler`), the
dispatcher promotes it to PHYSICAL.

`GicDistributor::Interrupt` / `Guest::SignalInterrupt` walk the
current CPU mask and call `vcpu->Interrupt` (virtual) for each
target. SPI stops after the first hit; SGI can fan out.

## IPI (guest SGI)

```text
 Guest  ipi_send_mask
         │
         ▼  write ICC_SGI1R_EL1     (HCR.TSC / sysreg trap)
         │
         ▼  EL2  handle_sgi
              decode aff / target_list / int_id / IRM
         │
         ├─ CROSS_VM_SCHED:  uvcpu->Interrupt(sgi.int_id)  in kernel
         │
         └─ else:  ZX_PKT_GUEST_VCPU_INTERRUPT  to VMM
                   Vcpu::HandleVcpu → Guest::SignalInterrupt(mask, vector)
         │
         ▼  same VIRTUAL inject as above
```

`ICC_SGI1R_EL1` is write-only. The guest never programs a physical
SGI through the host GIC.

## MSI / ITS / LPI

GICv3 MSI is a memory write the ITS translates to an LPI (≥ 8192).
Crook emulates the ITS; there is no host LPI for virtio.

```text
 Guest programs ITS (MAPD/MAPC/MAPTI) + unmasks LPI in PROP table
         │
         ▼  GicIts command queue  (trapped MMIO)
         │
 Backend / VHM  HC_INJECT_MSI  (SMC)  or virtio MSI-X doorbell
         │
         ▼  VhmServiceImpl::inject_msi(vmid, guest_paddr)
              acrn_msi_entry { msi_addr = devid, msi_data = event }
         │
         ▼  VmContext::send_interrupt  VHM_INTR_INJECT on the channel
         │
         ▼  VhmDevice
              evt_id == -1  →  GicDistributor::Interrupt(dev_id)   SPI virq
              else          →  GicIts::InjectMsi(devid, event)
                                ResolveLPI → SignalLpiInterrupt(vcpu, lpi)
         │
         ▼  zx_vcpu_interrupt  VIRTUAL  (LPI INTID)
```

Virtio PCI MSI-X (`msix_config.cc`) calls `TriggerMsixInterrupt` →
`pci_->Interrupt(index)` → the same distributor / ITS path.

`GicIts::InjectMsi` refuses a disabled LPI (`LPI_PROP_ENABLE` in the
guest PROP table). Target VCPU comes from the collection mapping.

## VHM FE ↔ BE

SOS (backend) and tbox/UOS (frontend) notify each other two ways.
Same inject tail (`ICH_LR`); different front door.

### virq (no host GIC)

**FE → BE** (guest write, usually virtio PCI):

```text
 FE  write virtio/PCI register
   →  stage-2 / MMIO trap  EL2
   →  VM exit  VhmDevice
   →  VhmService (SOS)
   →  Vcpu handle  zx_vcpu_interrupt
   →  ICH_LR  VIRTUAL
   →  BE Linux / ACRN handler
```

**BE → FE** (SOS hypercall):

```text
 BE  SMC  HC_INJECT_MSI
   →  EL2  vmexit  ZX_PKT_GUEST_VCPU_VHM_REQ
   →  VhmServiceImpl::inject_msi
   →  VHM_INTR_INJECT  to UOS VhmDevice
   →  GicDistributor / GicIts
   →  ICH_LR  VIRTUAL
   →  FE handler
```

### hwirq (host GIC pending)

Both directions: `SMC_FC_NBL_VHM_REQ` from the guest. EL2 sets the
**physical** interrupt pending (`interrupt_set_pending`). From there
the path is the passthrough SPI path (handler →
`PhysicalInterrupt` → LR with HW=1). Use this when the notify must
look like a real SPI to the other VM (and must survive the HW-bit
rules).

## Inject into a VCPU (common tail)

Every strategy ends here. `Vcpu::Resume` holds `virq_lock_`:

```text
timer_maybe_interrupt
gich_maybe_interrupt(gich_state, ich_state)
  elrs = ICH empty slots
  if elrs == 0:
      if vtimer pending: steal a PENDING LR   (avoid resume livelock)
      else: ICH_HCR.UIE  (maintenance IRQ when LRs drain)
      return
  while elrs && Pop(vector):
      if vector already ACTIVE in an LR:  PENDING_AND_ACTIVE
      else: fill empty LR
      hw = (type == PHYSICAL)
      prio = (vector < 32) ? 0 : 0x10
      lr = state | GROUP1 | PRIORITY | VIRTUAL_ID
           | [HARDWARE | PHYSICAL_ID] if hw
gic_write_gich_state(ich_state, HCR.En [| UIE])
running = true
EL2 restore  ICH_LR*  VTTBR  guest sysregs
eret  guest EL1h
```

After VM exit, `TrackAllListRegisters` remembers which INTIDs still
sit in LRs so a second inject of the same vector becomes
PENDING_AND_ACTIVE instead of a duplicate LR.

Bitmap (`interrupt_tracker.h`): 1024 groups × 64 vectors. Each
vector occupies two bits (`INACTIVE` / `VIRTUAL` / `PHYSICAL`).
`Pop` takes the lowest group, then the lowest vector in that group.

If the VCPU thread is on another CPU, `InterruptCpu` sends a host
IPI to that pCPU so `Resume` notices the new pending bit.

## Policies

| Policy | When | Mechanism |
| --- | --- | --- |
| Passthrough SPI/PPI | Device the guest owns | `ZX_INTERRUPT_PASSTHROUGH` + LR HW=1 |
| Virtual SPI | virtio / VHM doorbell | `zx_vcpu_interrupt`, HW=0 |
| Exclusive bind | latency, no sharing | `bind_vcpu(..., true)` + `interrupt_bind_cpu` |
| Broadcast | IROUTER.IRM / shared SPI | all VCPUs in `vcpus_[]` |
| PPI exclusive | timer, per-CPU | redistributor `BindVcpu` exclusive |
| SGI in-kernel | `WITH_CROSS_VM_SCHED` | `handle_sgi` → `Vcpu::Interrupt` |
| SGI via VMM | otherwise | `ZX_PKT_GUEST_VCPU_INTERRUPT` |
| MSI/LPI | virtio MSI-X, VHM MSI | emulated ITS, INTID ≥ 8192 |
| Shared IRQ | one SPI, several VMs | `SharedIrq` bitmask, then virq per guest |
| Level vs edge | GICD ICFG | `configure_interrupt`; level needs device deassert |
| Pin pCPU | interrupt latency | VCPU affinity + `BindPcpu` |
| UIE | more pending than LRs | maintenance VM-exit when LRs empty |

Latency-sensitive: passthrough + exclusive bind + pin. That is also
what avoids an extra VMM round-trip.

## Pitfalls

From the product IRQ issues; all are HW-bit or pending-state bugs.

**Storm (level).** EOI the GIC without deasserting the line. The SPI
re-pends immediately. `irq_monitor` (`trace_irq_monitor`) prints when
a vector is stuck.

**Storm (HW bit on inject).** Injecting a passthrough SPI with
`VIRTUAL` (HW=0) leaves the physical GIC Active. Next edge never
fires, or a level line storms. Modem-wake on SOS hit this.

**Lost IRQ.** Guest `no_suspend` / wakeup IRQ list wrong, so
`CheckSkipWakeup` drops the inject. Or LR HW=0 on a physical SPI:
guest EOI does not `DIR` the physical INTID → stuck Active → silent
loss of further IRQs.

**PENDING_AND_ACTIVE.** Re-inject while the LR is still ACTIVE. The
code does this on purpose; a wrong HW bit here also wedges the
physical Active bit.

**LR full.** Sixteen LRs. If all are valid and vtimer is pending,
`gich_prefer_vtimer_interrupt` steals a PENDING slot. Without that,
resume immediately VM-exits forever.

**Debug.**

```text
irq_monitor          host storm
/proc/interrupts     guest counts
NBL_IRQ / TID_IRQ_DELAY_INJECT / TAG_IRQ_INJECT
WITH_INTR_PEINDING_DEBUG   bitmap → LR state machine
interrupt_dump_regs(vector)
```

## Terminology

| Term | Meaning here |
| --- | --- |
| GICv3 | Host interrupt controller. GICD + per-CPU GICR + CPU interface. |
| VGIC | Guest view: emulated GICD/GICR/ITS + `ICH_*_EL2` list registers. |
| SPI / PPI / SGI / LPI | Shared / per-CPU / IPI / MSI-translated INTIDs. |
| Passthrough | Host GIC handler delivers that INTID into a VCPU as PHYSICAL. |
| Virtual IRQ | Software inject. No host GIC pending. |
| `ICH_LR` | EL2 list register. Pending/Active + INTID + optional HW bit. |
| HW bit | `ICH_LR_HARDWARE`. Guest EOI deactivates the physical INTID. |
| ITS | Interrupt Translation Service. DeviceID+EventID → LPI. Emulated. |
| MSI | Memory write that the ITS (or VHM) turns into an LPI/SPI inject. |
| VHM | ACRN-compatible FE/BE channel between SOS and UOS. |
| `HCR.IMO` | Physical IRQs while the guest runs go to EL2, not guest EL1. |

## Code

| Step | File |
| --- | --- |
| Host GICv3 | `zircon/kernel/dev/interrupt/arm_gic/v3/arm_gicv3.c` |
| Passthrough dispatcher | `zircon/kernel/object/interrupt_event_dispatcher.cpp` |
| `zx_interrupt_bind` / `bind_vcpu` | `zircon/kernel/syscalls/ddk.cpp` |
| Bitmap | `zircon/kernel/lib/hypervisor/include/hypervisor/interrupt_tracker.h` |
| `Vcpu::Interrupt` / `gich_maybe_interrupt` | `zircon/kernel/arch/arm64/hypervisor/vcpu.cpp` |
| `ICH_LR` encode | `zircon/kernel/arch/arm64/hypervisor/gic/gicv3.cpp` |
| EL2 ICH access | `zircon/kernel/arch/arm64/hypervisor/gic/el2.S` |
| SGI trap | `zircon/kernel/arch/arm64/hypervisor/vmexit.cpp` `handle_sgi` |
| `zx_vcpu_interrupt` | `zircon/kernel/syscalls/hypervisor.cpp` |
| Lua VGIC | `garnet/bin/guest/sos/qemu/sos.lua` |
| GICD/GICR emulate + passthrough | `garnet/lib/machina/arch/arm64/gic_distributor.cc` |
| ITS / MSI | `garnet/lib/machina/arch/arm64/gic_its.cc` |
| VHM MSI | `garnet/lib/machina/vhm_service_impl.cc` `inject_msi` |
| VHM device inject | `garnet/lib/machina/vhm_device.cc` `VHM_INTR_INJECT` |
| irq_monitor | `zircon/kernel/dev/interrupt/arm_gic/common/irq_monitor.cpp` |

---

Everyday's a Hustle.
