# Savior hypervisor handbook

Savior is a **type-1 hypervisor** on a **Zircon microkernel**
(HEE / BL33, source name Nebula). It runs three Linux guests —
SOS (Yocto), UOS/ALPS (Android), tbox. TEE is BL32 at S-EL1,
**not** a Savior VM. Linux is the payload. Zircon is the host.

This directory is the learning handbook for **this tree**
(`grpower/workspace/nebula` in `mt8678-3os-gpu-swa`): principle,
then the picture, then the code. Source identifiers still say
`nebula` / `nbl` / `gz_nebula`; keep them so `grep` matches.

**After this page:** you can place every later note on one
picture (three worlds, one VCPU thread, two walks, two injects)
and you know QEMU numbers are not SoC numbers. The host split
(kernel objects vs EL0 servers) is [`microkernel.md`](microkernel.md).

## How to use

| Mode | What to open |
| --- | --- |
| First afternoon | This page → [`microkernel.md`](microkernel.md) diagrams 1–6 → [`objects.md`](objects.md) Guest/Vcpu → [`fidl.md`](fidl.md) GuestLifecycle → [`system.md`](system.md) → F1, F2, F5, F9 |
| A log / a hang | [How to read a failure](#how-to-read-a-failure) → one flow in [`flows.md`](flows.md) → that plane’s **Diagrams** |
| A symbol | that note’s **Code** table, or [`glossary.md`](glossary.md) |

Each later note is one plane. Same shape every time:
**Principle** → **Framework** → **Diagrams** → objects / flows →
**Pitfalls** → **Terminology** → **Code**.

Diagrams that quote `0x08000000` or SPI 254 are **QEMU**. Board
debug: **Two configs** and `sos_mt8678.lua`.

## Principle

Five facts every later note assumes.

**1. Three worlds, one DRAM.** ATF carves TEE / HEE / REE from
`/reserved-memory`. Savior is HEE. Guests are REE.

```text
 EL3   ATF  BL31     PSCI, SPD, SMC
 EL2   trampoline    el2.S  (VBAR_EL2, identity TTBR0_EL2, HVC #4)
 EL1h  Savior host   arm64_elX_to_el1 at _start; VTTBR=0
 EL2   guest enter   HVC #4 → el2_hvc_resume, VM, VTTBR, eret EL1h
 EL1h  guest         SOS / UOS / tbox, stage-2 under them
 EL0   Savior user    guest_manager, Vmm, sysmgr
 S-EL1 TEE           BL32
```

**2. A VCPU is a pinned host thread.** One LK scheduler. A guest
CPU is `thread_t` with `THREAD_FLAG_VCPU`. Entering the guest is
**not** a thread switch: `zx_vcpu_resume` → `HVC #4` → `eret` on
the **same** thread. SOS → UOS on one core is a context switch
between two VCPU threads.

**3. Two walks to one PA.** Guest VA → IPA (guest TTBR) then IPA →
PA (`VTTBR_EL2`). Host user → `TTBR0`; kernel / physmap →
`TTBR1`. DMA uses the same stage-2 on the SMMU. Product RAM and
passthrough MMIO are identity (GPA = HPA).

**4. Two inject types, one list-register tail.** A host-GIC SPI is
`PHYSICAL` (HW bit in `ICH_LR`). Virtio, VHM, SGI, LPI, GPU
notify are `VIRTUAL`. Guest EOI of a HW LR deactivates the
**physical** INTID. Wrong bit is a storm or a lost IRQ.

**5. Userspace owns policy, kernel owns the `eret`.** That is
the microkernel rule ([`microkernel.md`](microkernel.md)). Lua,
virtio, VGIC MMIO, catalog, watchdog, VHM FE/BE live in
guest_manager / machina. Stage-2, ICH_LR, PSCI, GPU grant live
in the kernel. A guest `SMC` is an intercept. ACRN `HC_*`
bounce to SOS EL0 (`ZX_PKT_GUEST_VCPU_VHM_REQ`). Host EL0
talks `zx_*` (handles); it never holds a kernel pointer.

AHYP (`kernel.bootmode=ahyp`) is the product path.

**Why those five (invariants, not slogans).** Host EL1h keeps
Zircon syscalls and the LK scheduler in one world; EL2 only
switches `VTTBR`/`HCR`. A guest CPU that was a second scheduler
would hide FIFO/IRQ from the host — occupancy + hint peek exist
**because** the host only sees a `thread_t`. Identity S2 exists
so passthrough DMA/CPU share one PA. HW vs VIRTUAL exists so a
guest EOI cannot deactivate the wrong physical INTID. VHM in
EL0 exists because `venus-hee.mk` has no in-kernel ACRN decode.

## Framework

```text
 QEMU virt / SoC
   ATF  BL1 → BL2 → BL31          EL3
   FIP  BL32 TEE / BL33 HEE / REE kernel / REE rootfs
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ Savior kernel   Zircon / LK                                    │
 │  host EL1h  (arm64_elX_to_el1), HVC into el2.S                 │
 │  PMM / VMO / VMAR / Guest + Vcpu + VTTBR / VGIC ICH_LR         │
 │  GpuScheduler  (SWA arbiter, AHYP)                             │
 │  VHM/MSI: kernel posts VHM_REQ; SOS/UOS Vmm emulate            │
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

| Guest | User vmid | Kernel VMID | Who starts it |
| --- | --- | --- | --- |
| SOS | −1 | 1 | catalog `auto_start` |
| UOS / ALPS | 0 | 2 | SOS VHM `HC_CREATE_VM` / `file://uos` |
| tbox | 1 | 3 | `Launch(1)` or delayed auto_start |

Words used everywhere: [`glossary.md`](glossary.md).

## Two configs

The **kernel and VMM C++** are one. Numbers come from lua + catalog.

| | QEMU (`BOOTMODE_AHYP` on virt) | SoC 3OS (mt8678 product image) |
| --- | --- | --- |
| Lua in git | `garnet/bin/guest/sos/qemu/{sos,tbox,product}.lua` | `yocto/prebuilt/hypervisor/grt/auto8678p1_64_sos_indep/` |
| SOS CPUs | 4 | 8, `setBindcpus(0..7)` |
| GICD / GICR | `0x08000000` / `0x080A0000` | `0x0c400000` / `0x0c440000` |
| SOS IO window | 5-arg `setVmem("IO", 0, 1GiB, kCached)` | **none**; per-device 6-arg `product_config:setVmem` |
| VHM kick | SPI 254 | SPI 287 |
| TIPC / vtee | 245 / 513 | 286 / 174 |
| GPU SWA | off | on: `0x48000000/0x480000`, GRANT 4316–4319, Mali 636–640, VCPU 4 |
| vSMMU | DummyIommu | SOC only (`0x14900000`, IRQ 863) |
| Occupancy | guest shm default `NONE` | periods 7 ms, budgets 2 ms, `setSchedIrq(283)` |
| tbox | 4 CPUs, QEMU GIC | 8 CPUs, bind `{3,1,2,0,4,5,6,7}` |

Board image loads `/system/data/{sos,product}.lua`.

## Learning path

First pass, in this order. After the first afternoon the notes
are independent.

| # | Note | After this you can | First read |
| --- | --- | --- | --- |
| 1 | This page | Place every later note on one picture | Principle, Two configs |
| 2 | [`microkernel.md`](microkernel.md) | Say what the kernel does vs EL0, and why `Guest` is a handle | Principle, diagrams 1–6 |
| 3 | [`objects.md`](objects.md) | Name Guest / Vcpu / trap / rights and the creating syscall | Principle, Guest + Vcpu tables |
| 4 | [`fidl.md`](fidl.md) | Say why launch is FIDL and list every machina interface | Principle, diagrams 1–5, GuestLifecycle |
| 5 | [`system.md`](system.md) | Point at every process, shm, doorbell, SMC class | runtime picture, comms table, SMC |
| 6 | [`flows.md`](flows.md) | Walk a log F1–F18 without opening another file | F1, F2, F5, F9, log table |
| 7 | [`bootstrap.md`](bootstrap.md) | Name the EL/MMU switch that failed | B1–B3, nodes (7)–(13) |
| 8 | [`cpu.md`](cpu.md) | Explain one `zx_vcpu_resume` and one guest `SMC` | Diagrams 1–5, EL2 |
| 8b | [`exceptions.md`](exceptions.md) | Say guest inject vs host exception port vs kernel die | Principle, diagrams 1–4 |
| 9 | [`memory.md`](memory.md) | Draw guest VA → PA and say why a DABT happened | Layout, Diagrams 1–6 |
| 9b | [`shm.md`](shm.md) | Classify memory stomping: overlap / alias / missing barrier | Principle, diagrams 1–4, usage table |
| 10 | [`interrupt.md`](interrupt.md) | Say why the HW bit is on or off | Diagrams 1–5 |
| 11 | [`scheduler.md`](scheduler.md) | Say why SOS stole a SHARE core | Principle, then Diagrams 1–6 |
| 12 | [`vmm.md`](vmm.md) | Say why a guest relaunched or did not | lifecycle + RESET |
| 13 | [`pm.md`](pm.md) | Say why idle entered ATF | Framework + diagrams 1–4 |
| 14 | [`devices.md`](devices.md) | Classify an IPA as map / trap / hole | Diagrams 1–7 |
| 15 | [`vhm.md`](vhm.md) | Say why UOS hung on a BAR | Principle + diagrams 1–6 |
| 16 | [`gpu.md`](gpu.md) | Say why GRANT, STOP, or timeout | state machine, F9, Config |
| 17 | [`tee.md`](tee.md) | Say why an SMC never reached TEE | Diagrams 1–5 |
| 18 | [`guest.md`](guest.md) | Name the Linux module that is the actuator | occupancy + diagrams 1–5 |
| 19 | [`security.md`](security.md) | Name what S2 actually stops | Principle + diagrams 1–4 |
| 20 | [`logging.md`](logging.md) | Say why `SYS_NEBULA_RAMCONSOLE_DUMP_T` is missing | dump table |
| — | [`glossary.md`](glossary.md) | Look up a word without leaving the handbook | as needed |

## This tree

`venus-hee.mk`: `WITH_CROSS_VM_SCHED`, `WITH_VIRT_SERROR`, GPU
scheduler. **No** `WITH_VHE_SUPPORT`, `WITH_VHM_IN_KERNEL`,
`WITH_MSI_IN_KERNEL`. Do not look for `el2_vhe.S`, `vhm.cpp`,
`msi_router.cpp`, or `HyperPmManager`.

| Topic | Fact |
| --- | --- |
| Host EL | `arm64_elX_to_el1` → EL1h |
| Guest enter / exit | `el2.S` `HVC #4` / `eret` host EL1 |
| VHM / MSI | EL0 (`ZX_PKT_GUEST_VCPU_VHM_REQ`) |
| Host PM | 2-state `hyper_suspend_state` + `SuspendDispatcher` |

## Source map

| Tree | What lives there |
| --- | --- |
| `arm-trusted-firmware/` | BL1 / BL2 / BL31, SPD, PSCI |
| `zircon/kernel/arch/arm64/start.S` | `_start`, drop to EL1h |
| `zircon/kernel/arch/arm64/hypervisor/` | VCPU, `el2.S`, vmexit, VGIC, GPU SWA, TEE |
| `zircon/kernel/vm/` | PMM, VMO, VMAR |
| `zircon/kernel/kernel/sched.c` | LK run queue |
| `zircon/kernel/syscalls/hypervisor.cpp` | `zx_guest_*`, `zx_vcpu_*` |
| `zircon/kernel/object/` | GuestDispatcher, VcpuDispatcher, handles |
| `zircon/system/public/zircon/rights.h` | default Guest / Vcpu rights |
| `garnet/bin/guest_manager/` | catalog proto, Launch / Stop |
| `garnet/bin/guest/` | Vmm, `guest_config.cc` lua `set*` |
| `garnet/lib/machina/` | Guest, VGIC, virtio, VHM, watchdog |
| `garnet/lib/machina/guest.cc` | `HandleNebulaHyperCall` (`0x80`/`0xA0`/`0xB0`) |
| `garnet/lib/machina/fidl/` | Vhm / GPU / ipc FIDL |
| `garnet/public/lib/virtualization/fidl/` | GuestLifecycle / GuestManager |

## How to read a failure

| Symptom | Start here |
| --- | --- |
| Never reaches `lk_main` | F1, [`bootstrap.md`](bootstrap.md) (1)–(8) |
| `sysmgr` never launches SOS | [`bootstrap.md`](bootstrap.md) (9)–(12), [`vmm.md`](vmm.md) |
| Guest `eret` then abort | [`cpu.md`](cpu.md), [`memory.md`](memory.md), [`exceptions.md`](exceptions.md) |
| Vmm crash / host user fault | [`exceptions.md`](exceptions.md) exception ports |
| Savior kernel panic / ATF WDT | [`exceptions.md`](exceptions.md) `exception_die` |
| IRQ storm / lost SPI | [`interrupt.md`](interrupt.md) HW bit |
| SOS / UOS fight for a core | [`scheduler.md`](scheduler.md) |
| Guest reboot loops | [`vmm.md`](vmm.md) RESET vs RESET2 |
| UOS never appears | [`vhm.md`](vhm.md) `HC_START_VM` |
| GPU black / grant timeout | [`gpu.md`](gpu.md), F9 |
| Guest SMC to TEE hangs | [`tee.md`](tee.md) |
| Host sleep / idle ATF / GPUEB off | [`pm.md`](pm.md) |
| Occupancy doorbell ignored | F15, [`guest.md`](guest.md), [`scheduler.md`](scheduler.md) |
| Watchdog relaunch / SoC WDT | F16, [`vmm.md`](vmm.md) |
| Lock stuck after VM death | F17, [`guest.md`](guest.md) |
| ipc-mbox peer silent | F18, [`system.md`](system.md) |
| Stage-2 hole / SMPU | [`security.md`](security.md), [`memory.md`](memory.md) |
| Memory stomping / cache / shm stale | [`shm.md`](shm.md) |
| `map guest physical memory -4` | [`shm.md`](shm.md) overlap, `gz_nebula` |
| No `SYS_NEBULA_RAMCONSOLE_DUMP_T` | [`logging.md`](logging.md) |
| SPI / GIC / GPU numbers ≠ QEMU posters | **Two configs**, `sos_mt8678.lua` |
| Don’t know which plane | [`system.md`](system.md) communication table |
| Don’t know kernel vs EL0 | [`microkernel.md`](microkernel.md) diagram 10 |
| Which handle / syscall | [`objects.md`](objects.md) |
| Launch / VHM FIDL stuck | [`fidl.md`](fidl.md) GuestLifecycle / VhmService |
| Unknown `HC_*` / watchdog kick | [`system.md`](system.md) `HC_ID` / guest-call |

## Boot modes

QEMU Savior **is** `BOOTMODE_AHYP`, not `BOOTMODE_QEMU_AHYP`.

| Mode | How it is selected | Savior role |
| --- | --- | --- |
| `BOOTMODE_AHYP` | DTB `/active-hyp` | Type-1 hypervisor. Product path |
| `BOOTMODE_TEE` | BL31 SPSR EL1h | No guest_manager |
| `BOOTMODE_PHYP` | default if `/active-hyp` absent | `zx_guest_create` + `SetupPHyp` |
| `BOOTMODE_QEMU_AHYP` | DTB `/okl-hyp` | OKL, not this VCPU path |
| `BOOTMODE_OKL4_AHYP` | `/okl-hyp` + `ree-cpu-num==0` | same family as QEMU_AHYP |

## Coverage

AHYP product path only (SOS / UOS / tbox). Not a dump of
`zircon/kernel`. Self-score against **this tree’s AHYP 3OS
path** (can you debug a hang from a log using only this
directory + `grep` on the cited files). Not a percentage of
every C++ line in `zircon/`.

| Plane | % | Still thin |
| --- | --- | --- |
| README / path / two configs | 92 | — |
| [`microkernel.md`](microkernel.md) | 93 | futex / exception ports |
| [`objects.md`](objects.md) | 85 | TrapMap code, BTI/PMT |
| [`fidl.md`](fidl.md) | 85 | full wire-format spec, generated bindings |
| [`system.md`](system.md) | 90 | SharedIrq bitmaps |
| [`flows.md`](flows.md) | 88 | F14 FF-A share path |
| [`bootstrap.md`](bootstrap.md) | 90 | BL2 kedump internals |
| [`cpu.md`](cpu.md) | 90 | `el2.S` insn-level, FP/SVE lazy |
| [`exceptions.md`](exceptions.md) | 90 | SError virtualization detail |
| [`memory.md`](memory.md) | 92 | every SMMU CMD, SoC mblock dump |
| [`shm.md`](shm.md) | 92 | product hole at `0x88000b000` |
| [`interrupt.md`](interrupt.md) | 92 | ITS table RAM layout |
| [`scheduler.md`](scheduler.md) | 90 | per-board knob recipes |
| [`vmm.md`](vmm.md) | 93 | — |
| [`pm.md`](pm.md) | 88 | DDK driver list |
| [`devices.md`](devices.md) | 86 | every product.lua `setVmem` line |
| [`vhm.md`](vhm.md) | 90 | — |
| [`gpu.md`](gpu.md) | 90 | GPUEB firmware internals |
| [`tee.md`](tee.md) | 86 | Trusty TA set, SP UUID |
| [`guest.md`](guest.md) | 90 | — |
| [`security.md`](security.md) | 82 | ATF SMPU register dump |
| [`logging.md`](logging.md) | 90 | — |
| [`glossary.md`](glossary.md) | 85 | — |
| **Handbook (AHYP 3OS)** | **91** | see Out |

**Out (0%, on purpose).** x86 hypervisor; OKL microvisor;
hardware Mali Access Window; virtio-gpu; full guest
`nebula_sched` / `mali_kbase_arbiter_swa.c` internals (contract
in [`guest.md`](guest.md) GRANT/STOP kwork, source in Yocto/ALPS);
FIDL 3;
`HyperPmManager` / in-kernel VHM (not this `venus-hee.mk`).

---

Next: [`microkernel.md`](microkernel.md) · [`objects.md`](objects.md) · [`fidl.md`](fidl.md) · [`glossary.md`](glossary.md)

Everyday's a Hustle.
