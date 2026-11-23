# Savior hypervisor handbook

Savior's host is a **Zircon microkernel used as a type-1
hypervisor**. The product name for that host image is **Savior**
(HEE / BL33). Guests are ordinary Linux: SOS (Yocto), UOS/ALPS
(Android), tbox. This directory is the handbook: **principle first,
then the framework, then the code**.

Written against **this tree** (`workspace/nebula`). A second tree
is used as the 3OS / Mali SWA reference:

```text
this tree     mt8668 / Savior     VHE + in-kernel VHM + MSI
reference     mt8678-3os-gpu-swa non-VHE EL2 trampoline, VHM in EL0
```

Same guests, same `GpuScheduler`, same `guest_manager`. The EL2
enter/exit and the VHM hot path differ. Notes call out the split
where it matters. Implementation paths are from **this tree**
unless marked “mt8678”.

The product name is **Savior** (was Crook / Nebula in older
trees). Source paths and macros in this checkout still say
`nebula` / `nbl` / `gz_nebula` / `nebula_sched` / `SYS_NEBULA_*`;
the handbook keeps those identifiers so they match `grep`.

Start here, then follow the **Learning path**. Each later note is
one plane: picture → what it means → where in source.

## Principle

Five facts every later note assumes. They are the hypervisor, not
an extra layer on top of Zircon.

**1. Three worlds, one DRAM.** ATF carves TEE / HEE / REE from
`/reserved-memory` before any of them runs. Savior is HEE. Guests
are REE. TEE is S-EL1 and is **not** a Savior VM.

```text
 EL3   ATF  BL31     PSCI, SPD, SMC
 EL2   Savior host    this tree: VHE HCR.E2H|TGE, VTTBR=0
                     (kernel uses EL1 names)
 EL2   guest enter   TGE=0, VM, VTTBR   el2_vhe.S (this) / el2.S (mt8678)
 EL1h  guest         SOS / UOS / tbox, stage-2 under them
 EL0   Savior user    guest_manager, Vmm, sysmgr
 S-EL1 TEE           BL32
```

**2. A VCPU is a pinned host thread.** One LK scheduler. A guest
CPU is `thread_t` with `THREAD_FLAG_VCPU` and affinity of one
pCPU. Guest tasks never enter `run_queue`. Entering the guest is
**not** a thread switch: `zx_vcpu_resume` → `arm64_el2_resume` →
`eret` on the **same** thread. SOS → UOS on one core is an ordinary
context switch between two VCPU threads.

**3. Two walks to one PA.** Guest VA → IPA (guest TTBR) then IPA →
PA (`VTTBR_EL2`). Host user VA → `TTBR0`; kernel / physmap →
`TTBR1`. DMA uses the same stage-2 on the SMMU. Product RAM and
passthrough MMIO are identity (GPA = HPA).

**4. Two inject types, one list-register tail.** A host-GIC SPI is
`PHYSICAL` and the LR gets the HW bit. Virtio, VHM, SGI, LPI, GPU
notify are `VIRTUAL`. Guest EOI of a HW LR deactivates the
**physical** INTID. Wrong bit is a storm or a lost IRQ.

**5. Userspace owns policy, kernel owns the `eret`.** Lua, virtio,
VGIC MMIO, catalog, watchdog live in guest_manager / machina.
Stage-2, ICH_LR, PSCI, GPU grant, and (this tree) VHM MMIO live in
the kernel. A guest `SMC` is an intercept, not a trip to EL3,
until Savior decides to forward it.

AHYP (`kernel.bootmode=ahyp`) is the product path. PHYP /
QEMU_AHYP / OKL4_AHYP are other boot modes; GPU SWA and
`zx_guest_create_new` refuse them.

## Framework

```text
 QEMU virt / SoC
   ATF  BL1 → BL2 → BL31          EL3
   FIP  BL32 TEE / BL33 HEE / REE kernel / REE rootfs
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Savior kernel   Zircon / LK                                     │
 │  this tree: VHE EL2 (looks like EL1)                           │
 │  mt8678:    host EL1h, HVC into el2.S                          │
 │  PMM / VMO / VMAR / Guest + Vcpu + VTTBR / VGIC ICH_LR         │
 │  GpuScheduler  (SWA arbiter, both trees, AHYP only)            │
 │  this tree: in-kernel Vhm + MsiRouter                          │
 └───────────────┬──────────────────────────────┬─────────────────┘
                 │ zx_*                         │ SMC / IRQ / abort
                 ▼                              ▼
 ┌───────────────────────────────┐    guest EL1h  (stage-2)
 │ guest_manager  (sysmgr)       │      SOS   kernel VMID 1
 │  one Vmm process per guest    │      UOS   kernel VMID 2
 │  machina: traps, VGIC, virtio │      tbox  kernel VMID 3
 │  SOS Vmm: VhmService  (FE/BE) │
 └───────────────────────────────┘
```

| Plane | Who | What it decides |
| --- | --- | --- |
| ATF EL3 | BL31 | PSCI to the SoC, SPD into TEE, first `ERET` into Savior |
| Host kernel | Zircon | threads, pages, stage-2, VCPU bitmap, PSCI, SWA, (this) VHM |
| Guest enter/exit | `el2_vhe.S` / `el2.S` | `El2State`, `VTTBR`, `HCR`, `eret` guest |
| VMM EL0 | guest_manager + machina | catalog, lua, virtio, VGIC MMIO, watchdog, VHM protocol |
| Guest EL1h | Linux | its TTBR, GIC driver, PSCI, `nebula_sched`, mali_arb |

User vmid (FIDL / lua) vs kernel VMID:

| Guest | User vmid | Kernel VMID | Who starts it |
| --- | --- | --- | --- |
| SOS | −1 | 1 | catalog `auto_start` |
| UOS / ALPS | 0 | 2 | SOS VHM `HC_CREATE_VM` / `file://uos` |
| tbox | 1 | 3 | `Launch(1)` or delayed auto_start |

## Two trees

| Topic | This tree (Savior / mt8668) | mt8678-3os-gpu-swa |
| --- | --- | --- |
| HEE flags | `venus-hee.mk`: `WITH_VHE_SUPPORT`, `WITH_VHM_IN_KERNEL`, `WITH_MSI_IN_KERNEL` | same file: no those three |
| Host EL | stay EL2, `HCR.E2H\|TGE` (`arm64_elX_to_el2`) | `eret` EL1h (`arm64_elX_to_el1`) |
| Guest enter | `el2_vhe.S` `b el2_hvc_resume` | `el2.S` `HVC #4` |
| Guest exit | share `VBAR_EL1` if `VTTBR≠0`, `br lr` | `VBAR_EL2` exec table, `eret` host |
| VHM MMIO | kernel `Vhm` (`vhm.cpp`) SYNC/BELL | SOS Vmm emulates, kernel only posts `VHM_REQ` |
| MSI | kernel `MsiRouter` | userspace `HC_INJECT_MSI` |
| GPU SWA | `GpuScheduler` in both | same objects, this reference tree is the 3OS SWA product |
| Cross-VM sched | `WITH_CROSS_VM_SCHED` both | both |
| Host PM | `HyperPmManager` if DTS `/features/pm` | short legacy `hyp_power.cpp` |

Read [`cpu.md`](cpu.md) for the VHE vs `HVC` split, [`vhm.md`](vhm.md)
for the kernel vs EL0 split, [`gpu.md`](gpu.md) for SWA (shared).

## Learning path

First pass, in this order. Each note is independent after that.

1. This page — three worlds, VCPU thread, two walks, two injects
2. [`flows.md`](flows.md) — F1–F14 end-to-end pictures
3. [`bootstrap.md`](bootstrap.md) — reset → first guest `eret`
4. [`cpu.md`](cpu.md) — `Resume`, intercepts, PSCI, idle, SMP
5. [`memory.md`](memory.md) — PMM / VMO / stage-2 / SMMU
6. [`interrupt.md`](interrupt.md) — GICv3, VGIC, HW bit
7. [`scheduler.md`](scheduler.md) — one run queue, `VcpuGroup`
8. [`vmm.md`](vmm.md) — catalog, relaunch, STR, watchdog
9. [`pm.md`](pm.md) — host suspend gate, `HyperPmManager`
10. [`devices.md`](devices.md) — lua maps, virtio vs passthrough, vSMMU
11. [`vhm.md`](vhm.md) — SOS creates UOS, ioreq page
12. [`gpu.md`](gpu.md) — Mali SWA, one owner
13. [`tee.md`](tee.md) — guest SMC → TEE / FFA / TIPC
14. [`guest.md`](guest.md) — Linux modules Savior depends on
15. [`security.md`](security.md) — isolation that is actually enforced
16. [`logging.md`](logging.md) — ramconsole vs fwlog, BL2 kedump

## Documents

| Note | Plane |
| --- | --- |
| [`flows.md`](flows.md) | F1–F14 numbered end-to-end flowcharts |
| [`bootstrap.md`](bootstrap.md) | Reset → ATF → shim → `lk_main` → sysmgr → first `eret` |
| [`cpu.md`](cpu.md) | VCPU, EL2 enter/exit, intercepts, idle, SMP, PSCI |
| [`memory.md`](memory.md) | PMM / VMO / VMAR, TTBR0/1, stage-2, SMMU, MapDevice |
| [`interrupt.md`](interrupt.md) | GICv3, VGIC, passthrough vs inject, ICH_LR |
| [`scheduler.md`](scheduler.md) | LK `run_queue`, VCPU pin, `VcpuGroup`, `nebula_sched` |
| [`vmm.md`](vmm.md) | GuestManager, Vmm lifecycle, reboot, STR, watchdog |
| [`pm.md`](pm.md) | Host idle gate, DTS profile, `HyperPmManager` vs legacy |
| [`devices.md`](devices.md) | lua `setVmem` / virtio / passthrough / vSMMU |
| [`vhm.md`](vhm.md) | ACRN-like FE/BE, ioreq, SOS→UOS |
| [`gpu.md`](gpu.md) | Mali SWA arbiter (AHYP): exclusive time-slice |
| [`tee.md`](tee.md) | SMC forward, FFA, TIPC, `SOS_STATE` |
| [`guest.md`](guest.md) | Guest Linux: occupancy, ACRN HSM, vtee, mali_arb |
| [`security.md`](security.md) | Stage-2, HCR traps, SMPU, MapDevice, no guest EL2 |
| [`logging.md`](logging.md) | ramconsole / fwlog, BL2 kedump, `SYS_NEBULA_*` |

## Coverage

This handbook is the **AHYP Savior product path** (Zircon as
type-1 hypervisor, SOS / UOS / tbox). It is **not** a dump of
every file under `zircon/kernel`.

**In (product).** Boot, VHE EL2, VCPU/`Resume`/intercepts, PSCI,
stage-2 / SMMU, GICv3 VGIC, LK + `VcpuGroup`, GuestManager, host
PM, lua devices / virtio / vSMMU, VHM, Mali SWA, TEE/FF-A, guest
Linux actuators, isolation, ramconsole.

**In (this tree extras).** In-kernel VHM/MSI, `HyperPmManager`,
`MsiRouter`, `cross_vm_lock`, compile flags in `venus-hee.mk`.

**Satellite, documented where they live.**

| Direction | Where |
| --- | --- |
| Full `zx_guest_*` / `zx_vcpu_*` / `zx_gpu_*` | [`cpu.md`](cpu.md) Syscalls |
| First/last guest `El2CpuState` / `arm64_el2_on` | [`cpu.md`](cpu.md) EL2 |
| Cross-VM lock (shared DRAM + IRQ) | [`guest.md`](guest.md) |
| GICv2 / x86 / OKL4 microvisor | out of product; see below |
| SVE (`WITH_SVE_SUPPORT=false` here) | [`cpu.md`](cpu.md) EL2 |
| `WITH_VIRT_SERROR` (`HCR.AMO`) | [`cpu.md`](cpu.md) HCR |
| PHYP `zx_guest_create` | [`cpu.md`](cpu.md) Syscalls, boot modes |

**Out of this handbook.**

- `zircon/kernel/arch/x86/hypervisor/` — not Savior
- `BOOTMODE_QEMU_AHYP` / `OKL4_AHYP` / `lib/microvisor` — OKL, not VCPU
- `gicv2.cpp` — compiled; QEMU virt / SoC Savior is GICv3
- Hardware Mali Access Window / Resource Group — not implemented
- virtio-gpu / `virtgpu_dbg` — display paravirt, not SWA; not documented here
- Yocto/ALPS `nebula_sched` / `mali_arb` **.c** — contract only ([`guest.md`](guest.md))
- Board lua besides QEMU — same APIs, different numbers

## Boot modes

`arch_bootargs.bootmode` from the HEE DTB / Image header.

| Mode | How it is selected | Savior role |
| --- | --- | --- |
| `BOOTMODE_AHYP` | DTB `/active-hyp` | Type-1 hypervisor. Product path |
| `BOOTMODE_TEE` | BL31 SPSR EL1h | No guest_manager |
| `BOOTMODE_PHYP` | default if `/active-hyp` absent | `zx_guest_create` + `SetupPHyp` |
| `BOOTMODE_QEMU_AHYP` | DTB `/okl-hyp` | OKL hypervisor, not this VCPU path |
| `BOOTMODE_OKL4_AHYP` | `/okl-hyp` + `ree-cpu-num==0` | same family as QEMU_AHYP |

QEMU Savior **is** `BOOTMODE_AHYP`, not `BOOTMODE_QEMU_AHYP`.

## Source map

| Tree | What lives there |
| --- | --- |
| `arm-trusted-firmware/` | BL1 / BL2 / BL31, SPD, PSCI |
| `zircon/kernel/arch/arm64/start.S` | `_start`, VHE stay / non-VHE drop |
| `zircon/kernel/arch/arm64/hypervisor/` | VCPU, EL2, vmexit, VGIC, GPU SWA, VHM, TEE/FFA |
| `el2_vhe.S` | this tree: guest enter/exit |
| `el2.S` | mt8678 / TEE: `HVC` trampoline |
| `zircon/kernel/vm/` | PMM, VMO, VMAR |
| `zircon/kernel/kernel/sched.c` | LK run queue |
| `zircon/kernel/object/` | `GuestDispatcher`, `VcpuDispatcher` |
| `zircon/kernel/syscalls/hypervisor.cpp` | `zx_guest_*`, `zx_vcpu_*` |
| `garnet/bin/guest_manager/` | catalog, Launch / Stop |
| `garnet/bin/guest/` | Vmm, lua, bootloader proto |
| `garnet/lib/machina/` | Guest, VCPU loop, VGIC, virtio, VHM, watchdog |

Lua (QEMU): `garnet/bin/guest/sos/qemu/{product,sos,tbox}.lua`.
Catalog: `garnet/bin/guest_manager/data/guest_manager_qemu.pb.txt`.

## How to read a failure

| Symptom | Start here |
| --- | --- |
| Never reaches `lk_main` | [`flows.md`](flows.md) F1, [`bootstrap.md`](bootstrap.md) (1)–(8) |
| `sysmgr` never launches SOS | [`bootstrap.md`](bootstrap.md) (9)–(12), [`vmm.md`](vmm.md) |
| Guest `eret` then abort | [`cpu.md`](cpu.md), [`memory.md`](memory.md) |
| IRQ storm / lost SPI | [`interrupt.md`](interrupt.md) HW bit |
| SOS / UOS fight for a core | [`scheduler.md`](scheduler.md) |
| Guest reboot loops | [`vmm.md`](vmm.md) RESET vs RESET2 |
| UOS never appears | [`vhm.md`](vhm.md) `HC_START_VM` |
| GPU black / grant timeout | [`gpu.md`](gpu.md) SWA, [`flows.md`](flows.md) F9 |
| Guest SMC to TEE hangs | [`tee.md`](tee.md) |
| Host sleep / idle ATF / GPUEB off | [`pm.md`](pm.md) |
| Occupancy doorbell ignored | [`guest.md`](guest.md), [`scheduler.md`](scheduler.md) |
| Stage-2 hole / SMPU | [`security.md`](security.md), [`memory.md`](memory.md) |
| No `SYS_NEBULA_RAMCONSOLE_DUMP_T` | [`logging.md`](logging.md) |

---

Everyday's a Hustle.
