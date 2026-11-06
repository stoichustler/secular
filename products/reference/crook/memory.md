# Memory

Crook is a type-1 hypervisor. The host MMU is EL1 stage-1 (`TTBR0` /
`TTBR1`). Each guest has a **stage-2** table (`VTTBR_EL2`) that maps
IPA (guest PA) to host PA. Device MMIO is either identity-mapped in
that table or **unmapped** so a guest access traps to the VMM.

Read [`bootstrap.md`](bootstrap.md) for the EL / MMU-on sequence,
[`interrupt.md`](interrupt.md) for VGIC traps on GICD/GICR,
[`scheduler.md`](scheduler.md) for VCPU pin.

This note is the map of that path: the VM objects and how they nest,
how the three aspaces are built, how stage-2 is walked, how lua
`setVmem` becomes a PTE, how a passthrough device's **CPU MMIO**,
**IRQ**, and **DMA** (SMMU) are wired, how SMPU holes stay out of
stage-2, which registers tag the walk, how TLBs are invalidated, and
how permissions are enforced.

## Framework

Three aspaces. CPU does two walks (guest S1 then host S2). DMA does
the same S2 on the SMMU, optionally with a guest S1 in front.

```text
 Guest EL1h
   VA  ── stage-1 (guest TTBR0/TTBR1) ─────► IPA
                                              │
                                              ▼  CPU stage-2  VTTBR_EL2
 Device DMA                                   │
   IOVA ── SMMU S1 (guest CD) ──► IPA         │
                 │                            │
                 ▼  SMMU S2 (same TTB)        ▼
 Host EL1                                     PA
   user VA ── TTBR0 ──► PA                    │
   kernel VA / physmap ── TTBR1 ──► PA        ▼
                                            SMPU (interconnect firewall)
 EL2 (only while the VCPU is in guest)        │
   VTTBR, VTCR, HCR.VM, TLBI IPAS2 / VMALLS12 ▼
                                            DRAM / device
```

```text
 ┌────────────────────────────────────────────────────────────────┐
 │ Host kernel  VmAspace TYPE_KERNEL                              │
 │  TTBR1  KERNEL_BASE + physmap                                  │
 │  PMM / VMO / VMAR                                              │
 └────────────────────────────┬───────────────────────────────────┘
                              │  zx_guest_create_new → guest VMAR
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ GuestPhysicalAddressSpace  VmAspace TYPE_GUEST_PHYS            │
 │  IPA 0 … 2^36   tables are stage-2 PTEs (S2AP, MemAttr)        │
 │  VMID in VTTBR[63:48]                                          │
 └────────────────────────────┬───────────────────────────────────┘
                              │  lua VmemSpec + traps
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ VMM  (machina Guest::Init)                                     │
 │  kCached / kDevice  physical VMO, GPA = HPA                    │
 │  kDemand            anonymous VMO (fault in)                   │
 │  kMpu               skipped, then UnmapPhysicalMemory          │
 │  zx_guest_set_trap  UnmapRange  →  MMIO to EL2                 │
 │  vSMMU  traps SMMU MMIO, nests guest S1 onto host S2           │
 └────────────────────────────────────────────────────────────────┘
```

| Aspace | Hardware table | VA / IPA range | Who installs |
| --- | --- | --- | --- |
| Kernel | `TTBR1_EL1` | `KERNEL_ASPACE_BASE` … +512 GiB physmap; `KERNEL_BASE` = −4 GiB | `start.S`, then `vm_init` |
| User process | `TTBR0_EL1` + ASID | `[0x1000000, +0xfffffe000000)` | `ProcessDispatcher`, context switch |
| Guest IPA | `VTTBR_EL2` + VMID | `[0, 2^36)` (64 GiB) | `GuestPhysicalAddressSpace` |
| EL2 itself | `TTBR0_EL2` | EL2 vectors / stack | `el2_hvc_on` once per CPU |
| Device DMA | SMMU STE → S2 TTB (same as `VTTBR`) | IOVA or IPA | `ArmSmmuv3` + vSMMU |

Host never walks `VTTBR`. Guest stage-1 is the guest kernel's problem.
Crook only owns stage-2.

## Objects

Memory is three layers. Userspace holds **handles**. Each handle is a
**Dispatcher** with rights. The dispatcher owns (or shares) a VM-core
object that actually has pages or page tables.

```text
 EL0  zx::vmo / zx::vmar / zx::guest / zx::bti / zx::iommu / zx::pmt
         │  handle table  (rights: READ WRITE EXECUTE MAP …)
         ▼
 EL1  VmObjectDispatcher / VmAddressRegionDispatcher / GuestDispatcher
      IommuDispatcher / BusTransactionInitiatorDispatcher
      PinnedMemoryTokenDispatcher
         │  RefPtr
         ▼
 VM   VmObject  VmAspace  VmAddressRegion  VmMapping
      GuestPhysicalAddressSpace  TrapMap  ArmArchVmAspace
      Iommu (ArmSmmuv3)  vm_page_t
         │
         ▼
 HW   PMM arenas  TTBR0/1  VTTBR  SMMU STRTAB
```

A mapping is always **VA/IPA range → offset in a VMO**. The VMO is
the bytes; the mapping is one view. The same VMO can sit in a process
TTBR0, the kernel physmap-adjacent VMAR, and a guest stage-2 aspace
at once.

### Core tree

```text
                    VmAspace                    (TYPE_USER | KERNEL | GUEST_PHYS)
                        │
                   Root VmAddressRegion         contiguous VA/IPA span
                   /         |         \
              child VMAR   VmMapping   gap
                               │
                               │  object_offset_, arch_mmu_flags_
                               ▼
                            VmObject
                     /         |          \
            Paged          Physical     PhysicalPaged
         (demand RAM)    (fixed HPA)   (sparse HPA list)
                               │
                               ▼
                           vm_page_t  ← PMM arena
```

`VmAddressRegionOrMapping` is the tagged union. VMARs nest; mappings
are leaves. Children do not overlap. `WAVLTree` keyed by `base()`.

| Object | Owns | Notes |
| --- | --- | --- |
| `VmAspace` | root VMAR + `ArmArchVmAspace` | One hardware table. `Destroy()` to break the VMAR cycle. |
| `ArmArchVmAspace` | top-level PTE page, ASID or VMID | S1 vs S2 attrs chosen by `ARCH_ASPACE_FLAG_GUEST`. |
| `VmAddressRegion` | child VMARs / mappings | `CAN_MAP_{READ,WRITE,EXECUTE,SPECIFIC}`. |
| `VmMapping` | `RefPtr<VmObject>`, offset, MMU flags | `MapRange` writes PTEs. `Protect` / `Unmap` may split. |
| `VmObject` | size, cache policy, children | Not necessarily mapped. |
| `VmObjectPaged` | `vm_page_t` list | Fault / `CommitRange` / `CommitRangeContiguous`. |
| `VmObjectPhysical` | `[paddr, paddr+size)` | MMIO and identity RAM. No PMM alloc. |
| `VmObjectPhysicalPaged` | HPA per page | Non-contiguous passthrough RAM. |
| `vm_page_t` | 32 B, `state` + `map_count` | FREE / OBJECT / WIRED. Pin count for BTI. |
| PMM | arenas of DRAM | `pmm_alloc_page`, `pmm_alloc_contiguous`, `pmm_alloc_range`. |

`zx_vmar_map` path:

```text
zx_vmar_map(vmar, flags, offset, vmo, vmo_off, len)
  →  VmAddressRegionDispatcher::Map
  →  parent VMAR  CreateVmMapping(SPECIFIC?, vmo, flags)
  →  VmMapping::MapRange  (optional commit)
  →  ArmArchVmAspace::Map  S1 or S2 PTE
```

### Handles and dispatchers

| Handle (`zx_obj_type`) | Dispatcher | Core object |
| --- | --- | --- |
| `ZX_OBJ_TYPE_VMO` | `VmObjectDispatcher` | `VmObject` |
| `ZX_OBJ_TYPE_VMAR` | `VmAddressRegionDispatcher` | `VmAddressRegion` |
| `ZX_OBJ_TYPE_PROCESS` | `ProcessDispatcher` | `VmAspace TYPE_USER` |
| `ZX_OBJ_TYPE_GUEST` | `GuestDispatcher` | `Guest` → GPAS + `TrapMap` |
| `ZX_OBJ_TYPE_IOMMU` | `IommuDispatcher` | `Iommu` / `ArmSmmuv3` |
| `ZX_OBJ_TYPE_BTI` | `BusTransactionInitiatorDispatcher` | SID on that IOMMU |
| `ZX_OBJ_TYPE_PMT` | `PinnedMemoryTokenDispatcher` | pinned VMO range for DMA |

`ProcessDispatcher::aspace_` is the user TTBR0 aspace. Its root VMAR
handle is `PA_VMAR_ROOT` at process start. Kernel aspace is a
singleton (`VmAspace::kernel_aspace()`), never a handle.

### Guest objects

```text
 GuestDispatcher
      │
      ▼
    Guest  (arch/arm64/hypervisor)
      ├─ GuestPhysicalAddressSpace
      │     paspace_  VmAspace TYPE_GUEST_PHYS
      │     guest_phys_mem_  optional backing VMO
      │     CreateGuestPtr → kernel VMAR mapping of a GPA slice
      ├─ TrapMap   WAVL of Trap (MEM / BELL)
      └─ Vcpu[]    each Resume loads VTTBR from paspace_.arch_aspace()
```

`zx_guest_create_new` also returns a **guest VMAR** handle: that is
the root VMAR of `TYPE_GUEST_PHYS`. VMM `zx_vmar_map` into it **is**
the stage-2 fill. `zx_guest_set_trap` unmaps a range from that VMAR
and inserts a `Trap`.

`GuestPtr` is not a handle. Kernel-only: map guest RAM into the
**kernel** aspace so EL1 can read a guest structure, then
`mapping_->Destroy()`.

### VMM objects (EL0)

```text
 machina::Guest
   guest_     zx_handle_t     GuestDispatcher
   vmar_      zx_handle_t     guest root VMAR  (stage-2)
   phys_mem_  PhysMem         host VA over the RAM VMO  (is_physmem spec)
   mappings_  IoMapping[]     each SetTrap  MEM or BELL
   vsmmus_    Smmuv3[]        vSMMU + zx::iommu + zx::bti per SID
   vcpus_     Vcpu[]
```

`PhysMem` is a **host** mapping (`zx_vmar_map` into the VMM process)
of the same VMO that is mapped into the guest VMAR. VMM pokes guest
RAM via `phys_mem_.as<T>(gpa - phys_base)` without a hypercall.
`UnmapPhysicalMemory` drops both the host view and the guest S2 PTE.

### DMA objects

```text
 IommuDispatcher ──► ArmSmmuv3
                       ├─ s2_pt_phys_ / vmid_   from SetGuest
                       ├─ AttachedDevice(sid)   STE S2_TRANS
                       └─ Stage1Hwpt            nested guest STE words

 zx::bti  (sid)  ──► BusTransactionInitiatorDispatcher
                       AttachBti(sid) on create
                       Pin VMO  →  PinnedMemoryTokenDispatcher (PMT)
```

For passthrough, Crook rarely pins host VMOs into the SMMU (that is
the dummy-IOMMU / host-driver path). Guest DMA uses the STE's S2 TTB
instead of a PMT. PMT still exists for host devices that DMA into
kernel/user VMOs.

### Who points at whom

```text
 ProcessDispatcher ──aspace──► TYPE_USER VmAspace ──TTBR0──► user PTEs
                                      │
                                 mappings ──► VmObjectPaged ──► vm_page_t
                                                     ▲
 GuestDispatcher ──Guest──GPAS── TYPE_GUEST_PHYS     │  same VMO possible
                         │         VmAspace          │
                         │            │              │
                         │       mappings ──► VmObjectPhysical ── HPA
                         │            │
                         │            ▼
                         │      ArmArchVmAspace ──VTTBR──► S2 PTEs
                         │            ▲
                         │            │ S2TTB
 ArmSmmuv3 ──SetGuest────┘            │
     STE[sid] ────────────────────────┘

 VMM process  TYPE_USER aspace
     PhysMem mapping ──► same RAM VMO as guest S2
```

One physical page can be: in a PMM arena (FREE), WIRED into kernel,
in a paged VMO (`map_count` ≥ 1), identity in a Physical VMO, and
visible as IPA via S2 **without** a second `vm_page_t`. Physical VMOs
do not take pages from PMM; they name HPA that already exist.

## Host MMU

### Physmap and kernel

After (8) in [`bootstrap.md`](bootstrap.md):

```text
paddr_to_physmap(PA) = 0xffff000000000000 + PA     KERNEL_ASPACE_BASE
KERNEL_BASE          = 0xffffffff00000000          kernel link VA
```

`arm64_kernel_translation_table` is the TTBR1 root. Early maps are
RWX; `vm_init` then `ProtectRegion`s:

| Region | Flags |
| --- | --- |
| `.text` | R-X |
| `.rodata` | R-- |
| `.data` / `.bss` | RW- |

Physmap is reserved so user VMARs cannot steal those VAs. `TCR.EPD0`
keeps TTBR0 off until a user thread is scheduled
(`MMU_TCR_FLAGS_USER`).

### User

`VmAspace::TYPE_USER`: one top-level TTBR0 table, unique ASID.
Context switch writes `TTBR0_EL1 = (ASID << 48) | tt_phys` and
clears `TCR.EPD0`. Idle / kernel threads put `EPD0` back.

The kernel never maps guest IPA into TTBR1 except via a temporary
`GuestPtr` (see **Objects**). PMM arenas of DRAM; WIRED pages are not
freeable.

## Stage-2

### Create

`zx_guest_create_new` → `Guest::Create` →
`GuestPhysicalAddressSpace::Create(vmid)`:

- `VmAspace::TYPE_GUEST_PHYS`, base 0, size `1 << 36`
- `arch_aspace.Init(..., ARCH_ASPACE_FLAG_GUEST)`
- allocate and zero a top-level table; **no ASID allocator** — the
  8-bit VMID is stored in `asid_`
- `VTTBR = (VMID << 48) | tt_phys` (`arm64_vttbr`)

AHYP QEMU path: VMM maps each `VmemSpec` into the guest VMAR
(`ZX_VM_FLAG_SPECIFIC | MAP_CONTIGUOUS`). That `zx_vmar_map` **is**
the stage-2 fill.

PHyp leftover (`setup_stage2_mapping`): identity 1 GiB peripherals
at IPA 0 plus a DRAM window, both `VmObjectPhysical`. Crook AHYP
does not use that helper; lua `setVmem` does the same job.

### Walk

4 KiB granule, start level **1** (`VTCR.SL0 = 1`), `T0SZ = 28`
(`64 − 36`). IPA 36 bits → PA up to the platform PARANGE (folded
into `VTCR.PS` from `ID_AA64MMFR0_EL1`).

```text
 Guest VA
    │  guest TTBR0/1   (guest-owned)
    ▼
 IPA  (FAR_EL2 low 12 bits | HPFAR_EL2 << 8)
    │  VTTBR_EL2, VTCR_EL2, HCR.VM=1
    ▼
 PA
```

`HCR.PTW`: a stage-2 fault taken **during a guest table walk** still
traps to EL2 (permission / translation on the IPA of a PTE). Without
it those faults would be reported as guest S1 faults.

On `Vcpu::Resume`:

```text
vttbr = arm64_vttbr(VMID, stage2_root)
EL2  switch_to_guest:  msr vttbr_el2, vttbr
eret guest
… intercept …
switch_to_host:        msr vttbr_el2, xzr
```

Host EL1 always runs with `VTTBR = 0`.

### PTE (stage-2, not MAIR index)

`mmu_flags_to_s2_pte_attr`:

| Host flag | S2 bits |
| --- | --- |
| `CACHED` | MemAttr `0xf` (Normal WB), inner shareable |
| `UNCACHED_DEVICE` | MemAttr `0x1` (Device-nGnRE) |
| `PERM_WRITE` | `S2AP = RW` (otherwise RO) |
| no `PERM_EXECUTE` | `XN` |

S1 uses `MAIR_EL1` AttrIndx. S2 encodes memory type **in the PTE**
(`S2 MemAttr[3:0]`). Combining S1+S2 follows ARM DDI 0487 D4.5.4:
device wins over normal; RO in either stage is RO.

Block maps (1 GiB / 2 MiB) are used when the VMO is contiguous.

## Device memory

### Lua

SOS (`sos.lua`):

```text
sos_config:setVmem("IO", 0x00000000, 0x40000000, 1, 0)
```

`policy = 1` is `kCached`, `is_physmem = 0`. GPA = HPA = 0, size 1
GiB: identity map of the QEMU virt MMIO window (GIC, UART, …). RAM
comes from the leftover DRAM allocator, also identity.

tbox uses `setVmemAuto("grt,tbox-reserved", …)`: the allocator
returns a reserved-memory node; GPA = HPA, cached.

| `MemoryPolicy` | VMO | Cache | Typical |
| --- | --- | --- | --- |
| `kDemand` (0) | anonymous | cached | unused on QEMU SOS |
| `kCached` (1) | `zx_vmo_create_physical` | `ZX_CACHE_POLICY_CACHED` | RAM, SOS IO window |
| `kDevice` (2) | physical | `UNCACHED_DEVICE` | true device MMIO on SoC |
| `kMpu` (3) | skipped | — | TEE / MPU-protected |

`Guest::Init` maps each spec:

```text
zx_vmo_create_physical(hpa, size)
vmo.set_cache_policy(cached | device)
zx_vmar_map(guest_vmar, SPECIFIC|CONTIGUOUS|RWX, gpa_base, vmo)
```

That mapping walks `ArmArchVmAspace` with `ARCH_ASPACE_FLAG_GUEST` →
S2 PTEs.

### Traps vs maps

A trap **removes** the S2 mapping so the next access aborts to EL2:

```text
zx_guest_set_trap(MEM|BELL, addr, len)
  Guest::SetTrap
    gpas->UnmapRange(page-aligned)
    traps_.InsertTrap(...)
```

GICD/GICR, virtio MMIO, VHM windows, pl011 (if emulated) all take
this path. The 1 GiB SOS IO map is the default; traps punch holes.

| Trap | On abort | Who handles |
| --- | --- | --- |
| `ZX_GUEST_TRAP_MEM` | `ZX_PKT_TYPE_GUEST_MEM` back to the VCPU thread | `IoHandler::Read/Write` (VGIC, virtio, …) |
| `ZX_GUEST_TRAP_BELL` | `ZX_PKT_TYPE_GUEST_BELL` on a port (async) | doorbell / kick |

`MMIO_SYNC` = MEM. `MMIO_BELL` = BELL.

### Abort → VMM

```text
 Guest load/store  IPA in a hole
         │
         ▼  EL2  ESR.EC = data abort  HPFAR = IPA[47:12]
         │
         ▼  handle_data_abort
              IPA = (HPFAR<<8) | (FAR & 0xfff)
         │
         ├─ trap BELL  →  Queue packet, guest PC advanced
         ├─ trap MEM   →  ZX_ERR_NEXT, VCPU thread emulates, writes GPRs
         └─ no trap    →  vmm_guest_page_fault_handler
                            demand-fill S2, or inject_data_abort_exception
```

Instruction abort (guest fetch of unmapped IPA) only tries the
page-fault fill; it does not look at traps.

## Device passthrough

A guest that **owns** a real device needs three things to match.
Missing any one of them is a half-assign: MMIO works but DMA writes
the host, or DMA works but IRQs never arrive.

```text
                    ┌──────── guest ────────┐
                    │  driver  MMIO / DMA   │
                    └───────────┬───────────┘
          CPU load/store        │         DMA read/write
                │               │               │
                ▼               │               ▼
     S2 identity PTE            │     SMMU STE (SID)
     (kDevice / kCached)        │        │
                │               │        ├─ S2_TRANS     IPA ──S2──► PA
                │               │        └─ S12_TRANS    IOVA──S1──►IPA──S2──►PA
                ▼               │               │
           device regs          │               ▼
                │               │            DRAM
                ▼               │
     IRQ passthrough  ──────────┘
     (InterruptEventDispatcher, ICH_LR HW=1)
```

| Leg | What Crook does | If skipped |
| --- | --- | --- |
| CPU MMIO | `VmemSpec` identity S2, **no** trap on that window | guest abort, or VMM emulates |
| IRQ | `ZX_INTERRUPT_PASSTHROUGH` + `bind_vcpu` ([`interrupt.md`](interrupt.md)) | host eats the SPI |
| DMA | SMMU STE for the device **SID**, S2 TTB = this guest's GPAS | DMA uses host IPA, or SMMU abort |

Lua `setVmem(..., kDevice)` is the MMIO leg. `setVgicIrq` is the IRQ
leg. `setVsmmu(paddr, irq, sids)` is the DMA leg: those SIDs are
attached to this VM's stage-2.

Two DMA styles:

**Identity DMA (S2 only).** Guest programs the device with IPA = HPA.
Host STE `CFG = S2_TRANS`, `S2TTB` = guest stage-2 root, `S2VMID` =
guest VMID. The SMMU walk is the same IPA→PA walk as the CPU
`VTTBR`. Used when the guest does not turn on its IOMMU for that
device.

**IOVA DMA (nested S1+S2).** Guest programs vSMMU (IOVA→IPA in a
context descriptor) **and** the device with IOVAs. Host STE
`CFG = S12_TRANS`: first the **guest** CD (S1), then the **host**
S2 TTB. Guest never sees the real STRTAB.

QEMU virt has no SMMU; the DMA leg is a no-op there. SoC boards
(MTK: MM / APU / SOC / GPU SMMU) need all three legs.

```text
 lua  setVmem(device window, kDevice)     // CPU
      setVgicIrq(spi)                     // IRQ
      setVsmmu(smmu_base, irq, {sid…})    // DMA SIDs
         │
         ▼
 Guest::Init  physical VMO + vmar_map     S2 PTE
 GicDistributor  PASSTHROUGH bind_vcpu
 Smmuv3::Init
   zx_iommu_create(ARM_SMMUV3, type)
   zx_iommu_set_guest(iommu, guest)       remember S2 root + VMID
   trap SMMU MMIO  (vSMMU)
   for sid: zx_bti_create(iommu, sid)
            AttachBti → smmu_attach_dev   STE S2_TRANS
```

A device whose MMIO stays in the 1 GiB SOS IO window **and** is not
trapped is already CPU-passthrough. GICD/GICR/virtio/vSMMU are the
exceptions: they punch trap holes.

## SMMU (IOMMU)

ARM SMMUv3 sits on the DMA path. Stream ID (SID) selects a Stream
Table Entry (STE). The STE says abort, bypass, S1, S2, or nested
S1+S2.

Crook splits it in two:

| Plane | Object | Role |
| --- | --- | --- |
| Host kernel | `ArmSmmuv3` (`smmuv3.cpp`) | real STRTAB / CMDQ / EVTQ, STE programming |
| VMM | `machina::Smmuv3` | **vSMMU**: trap guest SMMU MMIO, consume guest CMDQ, nest STEs |

Four SoC instances, selected by MMIO base (`GetSmmuType`):

```text
MM_SMMU   0x30800000
APU_SMMU  0x4C000000
SOC_SMMU  0x14900000
GPU_SMMU  0x48600000
```

SOS `vmm.cc` builds one `Smmuv3` per `VsmmuSpec`. UOS has the same
path behind `#if 0` in `main_uos.cc` (not on by default).

### Host attach (S2 only)

```text
zx_iommu_set_guest
  ArmSmmuv3::SetGuest
    s2_pt_phys_ = guest GPAS tt_phys
    vmid_       = guest aspace asid_ (VMID)

zx_bti_create(iommu, sid)
  ArmSmmuv3::AttachBti(sid)
    smmu_attach_dev(smmu, vmid, s2_pt_phys, sid)
```

`smmu_attach_dev` writes the hardware STE (8×64-bit), then
`smmu_sync_ste` (CMDQ `CFGI_STE` + `CMD_SYNC`):

```text
STE[0]  V | CFG = S2_TRANS
STE[1]  SHCFG = INCOMING
STE[2]  VTCR (PS, TG 4K, T0SZ=28, SL0=1, WBWA, inner SH)
        | S2VMID | S2R | S2AA64
STE[3]  S2TTB = guest stage-2 root  (same PA as VTTBR.BADDR)
```

SMMU S2 parameters match CPU `VTCR_EL2` (`get_default_s2_cfg`: 36-bit
IPA, 4K, SL0=1). DMA IPA then walks the **same** tables as the CPU.

### vSMMU (guest programs IOMMU)

Guest SMMU registers are **not** the real ones. `Smmuv3::Init` traps
`spec.paddr` (`kSmmuv3Size = 0x80000`). Guest CMDQ lives in guest
RAM; the VMM reads it on doorbell / MMIO cons/prod updates.

```text
 Guest  STRTAB / CD in guest RAM
 Guest  CMDQ  CFGI_STE / CFGI_CD / TLBI_NH_* / CMD_SYNC
         │
         ▼  MMIO trap  Smmuv3::Write
         ▼  cmdq_consume
              CFGI_STE       config_ste  →  nest or unnest
              CFGI_CD        queue to host HwptInvalidate
              TLBI_NH_*      zx_iommu_hwpt_invalidate  (host CMDQ)
              CMD_SYNC       inject vSMMU IRQ
         │
 Host EVTQ  →  OnNewEvent  →  guest EVTQ + SignalInterrupt
```

`config_ste` reads the **guest** STE at that SID:

- not valid, or S1 disabled → `uninstall_nested_ste` → host STE back
  to `S2_TRANS` only
- S1 enabled → copy allowed fields (`V, S1Fmt, S1CTXPTR, S1CDMax`,
  `S1DSS/CIR/COR/CSH, S1STALLD`) into `iommu_hwpt_arm_smmuv3`,
  `install_nested_ste` → `AllocHwpt` + `AttachHwpt`

Guest cannot set host `S2TTB` / `S2VMID`; those stay hypervisor-owned.

### Nested walk (S12)

`smmu_install_nested_hwpt`:

```text
host STE[0]  = guest STE[0] (S1 ptr) | CFG = S12_TRANS   (or S2_TRANS if guest asked BYPASS)
host STE[1] |= guest STE[1] (S1 cache / stall)
host STE[2..3] unchanged   (S2VMID, S2TTB)
```

```text
 Device  SID
    │
    ▼  host STRTAB STE
 CFG S12_TRANS
    │
    ├─ S1  guest CD at S1CTXPTR (IOVA → IPA)     guest-owned
    │
    └─ S2  S2TTB + S2VMID + STE VTCR             Crook-owned
           IPA → PA   (same tables as VTTBR)
    │
    ▼
 PA   then SMPU / DRAM
```

`CFG = S2_TRANS` skips the guest CD: IOVA is treated as IPA.

SMMU TLB is tagged by SID / ASID / VMID, not the CPU ASID. Guest
`TLBI_NH_VA` is forwarded (`zx_iommu_hwpt_invalidate`) so the host
CMDQ issues the real `TLBI`. S2 PTE changes use host
`TLBI_S2_IPA` / `TLBI_S12_VMALL` (same moment as CPU `tlbi ipas2*`).

Event queue: a real SMMU fault (bad SID, S2 permission, etc.) is
packaged as `zx_packet_iommu_event_t`; the vSMMU writes it into the
guest EVTQ and injects `combined_irq_`.

## SMPU

SMPU is a **secure memory protection unit** on the interconnect
(SoC firewall, programmed by TEE / ATF, not by Crook EL1). It
decides which masters may hit which PA ranges, independent of CPU
stage-2 and SMMU.

Crook's job is to **not map** those PAs into a guest, so a guest
load does not even reach the firewall.

```text
 lua  VmemSpec  policy = kMpu (3)   name = reserved-memory search string
         │
 Guest::Init
   kMpu entries are skipped  (no physical VMO, no S2 PTE)
         │
 unmap_mpu_mem_region  (vmm.cc)
   open guest DTB
   for each kMpu spec:
     find reserved-memory nodes matching spec.name
     read reg {base,size}
     Guest::UnmapPhysicalMemory(base, size)
       phys_mem_ host mapping gone
       zx_vmar_unmap(guest_vmar)     S2 hole
```

If a window was identity-mapped as part of a larger RAM `VmemSpec`,
this punch-out still wins. Guest access → S2 abort →
`inject_data_abort_exception` (or a trap, if one was installed).

DMA to an SMPU-protected PA is blocked by the **firewall** even if
an STE allowed the translation. Keeping S2 empty is how CPU and DMA
views stay consistent.

QEMU has no SMPU; `kMpu` specs are absent or the DTB lookup is a
no-op (`MPU memory region not found … ignore unmap`).

```text
 Guest CPU                 Device DMA
    │                         │
    ▼ S2                       ▼ SMMU S2
   hole (kMpu unmapped)      IPA→PA  (if SID attached)
    │                         │
    ▼ abort to EL2            ▼
                         interconnect SMPU
                         allow / deny by PA + master
                              │
                              ▼
                            DRAM
```

## Registers

| Register | Role |
| --- | --- |
| `TTBR1_EL1` | Host kernel + physmap. Never swapped. |
| `TTBR0_EL1` | Host user. ASID[63:48]. `TCR.EPD0` disables it. |
| `TCR_EL1` | T0SZ/T1SZ, TG, IRGN/ORGN, EPD0, AS. `MMU_TCR_FLAGS_{KERNEL,USER,IDENT}` |
| `MAIR_EL1` | Attr0 nGnRnE, Attr1 nGnRE, Attr2 Normal WB, Attr3 WT |
| `SCTLR_EL1.M/C/I` | MMU / D-cache / I-cache |
| `VTTBR_EL2` | Stage-2 root. VMID[63:48] \| BADDR. 0 on host. |
| `VTCR_EL2` | `MMU_VTCR_EL2_FLAGS`: TG0 4K, T0SZ=28, SL0=1, IRGN/ORGN WBWA, PS from PARANGE |
| `MAIR_EL2` | Same `MMU_MAIR_VAL` as EL1 (EL2's own tables) |
| `TCR_EL2` / `TTBR0_EL2` / `SCTLR_EL2` | EL2 MMU, programmed in `el2_hvc_on` |
| `HCR_EL2.VM` | Enable stage-2 |
| `HCR_EL2.PTW` | S2 faults on table walks trap to EL2 |
| `HCR_EL2.DC` | (commented out) would force S1 as Normal WB |
| `FAR_EL2` | VA of the abort (low 12 bits of IPA) |
| `HPFAR_EL2` | IPA[47:12] of the abort (shift left 8 in EL2 asm) |
| `ESR_EL2` | EC = data/instruction abort, ISS = S1 vs S2, WnR |
| SMMU `STRTAB_BASE` / `CMDQ_BASE` / `EVTQ_BASE` | Host SMMU queues and stream table |
| STE `CFG` | 0 abort, 4 bypass, 5 S1, 6 S2, 7 S1+S2 |
| STE `S2TTB` / `S2VMID` / STE-`VTCR` | DMA stage-2; same TTB/VMID as CPU `VTTBR` |
| STE `S1CTXPTR` | Guest CD for nested S1 (IOVA→IPA) |
| `SMMU_CR0` | SMMUEN, CMDQEN, EVTQEN |

`VTTBR` / `VTCR` are EL2-only. Host EL1 reaches them through HVC
helpers (`arm64_el2_tlbi_*`), not `msr` from EL1. SMMU registers are
host-physical MMIO (`AllocPhysical`, Device-nGnRE); the guest sees
only the trapped vSMMU window.

## TLB

There is **no software TLB**. The CPU TLBs cache S1, S2, and combined
walks. Crook only **invalidates**.

| Event | Instruction | Where |
| --- | --- | --- |
| MMU on, drop trampoline | `tlbi vmalle1` | `start.S` |
| Host kernel PTE change (global ASID) | `tlbi vaae1is` / `vaale1is` | `FlushTLBEntry` |
| Host user PTE change | `tlbi vae1is` + ASID | same |
| User aspace destroy | `tlbi aside1is` | `ArmArchVmAspace::Destroy` |
| Guest S2 PTE change | `HVC` → `tlbi ipas2e1is` or `ipas2le1is`, then `vmalle1is` | `arm64_el2_tlbi_ipa` |
| Guest aspace destroy | `tlbi vmalls12e1is` | `arm64_el2_tlbi_vmid` |
| EL2 MMU on/off | `tlbi alle2` | `el2_hvc_on` / `off` |
| Return from guest | `tlbi vmalle1` (host S1) | `el2.S` host restore |

`ipas2*` needs `VTTBR` loaded (`switch_to_guest`) so the VMID tags
the invalidate. `vmalls12e1is` drops **both** stages for that VMID.

ASID (host user) and VMID (guest) keep entries from colliding. Kernel
TTBR1 uses the global ASID.

SMMU-v3 has its own STE/CD/IOTLB, tagged by SID / ASID / VMID.
Invalidation is **CMDQ**, not `tlbi`:

| Guest CMD | Host |
| --- | --- |
| `CFGI_STE` / `CFGI_STE_RANGE` | `config_ste` → nest/unnest + `smmu_sync_ste` |
| `CFGI_CD` / `TLBI_NH_*` / `ATC_INV` | `zx_iommu_hwpt_invalidate` |
| `CMD_SYNC` | vSMMU IRQ |
| (S2 PTE change) | host `TLBI_S2_IPA` / `TLBI_S12_VMALL` |

## Protection

**Host.** After `vm_init`, kernel RX / RO / RW is locked. User
mappings are per-process TTBR0 with `AP` + UXN. Syscalls check
`ZX_RIGHT_*` on handles before `vmar_map` / `vmo_write`.

**Guest S2.**

- Default RAM: S2AP RW, Normal WB, XN off if the VMM mapped RX.
- Device physical VMO: Device-nGnRE, typically RW, XN.
- Trap hole: invalid S2 descriptor → abort to EL2, never a host
  kernel mapping.
- Unmapped IPA with no trap: `inject_data_abort_exception` into the
  guest (`ESR_EL1` data abort, `FAR_EL1` = IPA).
- `kMpu` windows are unmapped; SMPU / TEE owns that PA.

**Rights the VMM cannot bypass.** `zx_vmar_map` into the guest VMAR
still goes through `ArmArchVmAspace::Map` → S2 PTE. A guest cannot
map host kernel PA unless the VMM created a VMO for it.

**W^X at stage-2** is whatever the VMM asked for. SOS identity RAM is
RWX in the current `Guest::Init` flags.

**Cache aliasing.** Same PA in host physmap (Normal) and guest S2
(Device) is a programming error; lua `kDevice` vs `kCached` is how
the VMM picks one.

## Flows

### Host boot (MMU)

```text
 phys  _start
   arm64_elX_to_el1     VTTBR=0
   TTBR1 = kernel table, TTBR0 = 1 GiB identity
   SCTLR.M=1
   br KERNEL_BASE
   TCR.EPD0             drop trampoline
   tlbi vmalle1
   vm_init              lock kernel RX/RO/RW
```

### Guest create → first `eret`

```text
 zx_guest_create_new
   alloc VMID, TYPE_GUEST_PHYS aspace, empty S2 root
 lua VmemSpec[]
   physical VMO + vmar_map SPECIFIC   →  S2 identity PTEs
 CreateMapping(GICD/GICR/virtio)
   zx_guest_set_trap  UnmapRange      →  holes
 Vcpu::Resume
   vttbr = VMID<<48 | s2_root
   HCR.VM|PTW
   msr vttbr_el2
   eret guest EL1h
```

### Device MMIO (trapped)

```text
 guest str  GICD
   S2 invalid  →  EL2 data abort
   trap MEM    →  VCPU thread  GicDistributor::Write
   next_pc, eret
```

### Device MMIO (CPU passthrough)

```text
 guest ldr  UART  (in the 1 GiB identity window, no trap)
   S2 cached/device PTE  →  PA  →  real device
```

SOS GIC is **not** this path: GICD/GICR are trapped even though they
sit in the 1 GiB window.

### DMA (SMMU S2 only)

```text
 device SID  STE CFG=S2_TRANS  S2TTB=guest root
   DMA IPA  →  same S2 walk as CPU VTTBR  →  PA
```

### DMA (nested S1+S2)

```text
 guest programs vSMMU CD  (IOVA→IPA)  and STE S1
   CFGI_STE  →  host STE CFG=S12_TRANS
 device SID
   IOVA → guest CD → IPA → host S2TTB → PA
```

### SMPU hole

```text
 kMpu VmemSpec / reserved-memory node
   UnmapPhysicalMemory  →  S2 invalid
 guest access  →  data abort to guest
 DMA to that PA  →  interconnect SMPU deny (even if an STE allowed it)
```

## Terminology

| Term | Meaning here |
| --- | --- |
| Physmap | `KERNEL_ASPACE_BASE + PA`. Host kernel's linear map of DRAM. |
| IPA / GPA | Guest physical address. Stage-2 input. Identity for SOS/tbox. |
| Stage-1 | EL1 `TTBR0/1`. Host user/kernel, or the **guest's** own tables. |
| Stage-2 | EL2 `VTTBR`. IPA → PA. Crook-owned. |
| VMID | 8 bits in `VTTBR[63:48]`. Tags S2 TLB. Stored in `asid_` of the guest aspace. |
| ASID | Host user TTBR0. Not used for guests. |
| `VmemSpec` | Lua mapping: name, GPA/HPA, size, `MemoryPolicy`. |
| Trap hole | Page-aligned IPA with no S2 PTE; abort delivered as MEM/BELL. |
| `GuestPtr` | Temporary host mapping of a guest-RAM slice. |
| S2AP | Stage-2 access perm: RO or RW. No separate user/priv bit. |
| `HCR.PTW` | S2 fault on a table-walk IPA traps to EL2. |
| SID | SMMU Stream ID. Selects the STE. `zx_bti_create(..., sid)`. |
| STE | Stream Table Entry. Abort / bypass / S1 / S2 / S12. |
| vSMMU | Emulated SMMU MMIO in the guest. Real STRTAB stays in the host. |
| Nested S12 | Guest S1 CD then host S2 TTB. IOVA → IPA → PA. |
| SMPU | SoC interconnect firewall. `kMpu` windows stay out of stage-2. |
| Device passthrough | Guest owns MMIO S2 + IRQ + DMA SID together. |
| Dispatcher | Kernel object behind a handle (`VmObjectDispatcher`, …). |
| `VmMapping` | Leaf: VA/IPA range + VMO offset + MMU flags. |
| `PhysMem` | VMM host mapping of the guest RAM VMO. |
| PMT | Pinned VMO range for host-side BTI DMA. |

## Code

| Step | File |
| --- | --- |
| MMU on, TTBR1/0 | `zircon/kernel/arch/arm64/start.S` |
| Physmap | `zircon/kernel/vm/include/vm/physmap.h` |
| Kernel lock-down | `zircon/kernel/vm/vm.cpp` `vm_init` |
| Aspace types | `zircon/kernel/vm/vm_aspace.cpp` |
| S1/S2 PTE attrs, TLB | `zircon/kernel/arch/arm64/mmu.cpp` |
| `VTCR` / `VTTBR` helpers | `zircon/kernel/arch/arm64/include/arch/arm64/mmu.h`, `arch/aspace.h` |
| EL2 VTTBR / TLBI | `zircon/kernel/arch/arm64/hypervisor/el2.S` |
| Guest aspace | `zircon/kernel/lib/hypervisor/guest_physical_address_space.cpp` |
| `zx_guest_create_new` / trap | `zircon/kernel/syscalls/hypervisor.cpp` |
| `SetTrap` unmap | `zircon/kernel/arch/arm64/hypervisor/guest.cpp` |
| Data abort | `zircon/kernel/arch/arm64/hypervisor/vmexit.cpp` `handle_data_abort` |
| Lua `setVmem` | `garnet/bin/guest/guest_config.h` |
| Map VmemSpec | `garnet/lib/machina/guest.cc` `Guest::Init` |
| Trap install | `garnet/lib/machina/io.cc` |
| SOS IO window | `garnet/bin/guest/sos/qemu/sos.lua` |
| Host SMMU / STE | `zircon/kernel/dev/iommu/arm_smmuv3/smmuv3.cpp` |
| `smmu_attach_dev` / nested STE | same, `smmu_install_nested_hwpt` |
| vSMMU | `garnet/lib/machina/arch/arm64/smmu_v3.cc` |
| Lua `setVsmmu` | `garnet/bin/guest/guest_config.h` |
| SOS vSMMU create | `garnet/bin/guest/vmm.cc` |
| SMPU unmap | `garnet/bin/guest/vmm.cc` `unmap_mpu_mem_region` |
| `VmAspace` / VMAR / mapping | `zircon/kernel/vm/include/vm/vm_aspace.h`, `vm_address_region.h` |
| VMO types | `vm_object.h`, `vm_object_paged.h`, `vm_object_physical.h` |
| PMM / `vm_page_t` | `zircon/kernel/vm/include/vm/pmm.h`, `page.h` |
| Dispatchers | `zircon/kernel/object/include/object/vm_*_dispatcher.h` |
| BTI / PMT / IOMMU | `bus_transaction_initiator_dispatcher.h`, `iommu_dispatcher.h` |
| `PhysMem` | `garnet/lib/machina/phys_mem.h` |

---

Everyday's a Hustle.
