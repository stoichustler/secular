# Memory

Crook is a type-1 hypervisor. The host MMU is EL1 stage-1 (`TTBR0` /
`TTBR1`). Each guest has a **stage-2** table (`VTTBR_EL2`) that maps
IPA (guest PA) to host PA. Device MMIO is either identity-mapped in
that table or **unmapped** so a guest access traps to the VMM.

Read [`bootstrap.md`](bootstrap.md) for the EL / MMU-on sequence,
[`interrupt.md`](interrupt.md) for VGIC traps on GICD/GICR,
[`scheduler.md`](scheduler.md) for VCPU pin,
[`gpu.md`](gpu.md) for `MapDevice` / SWA grant,
[`vmm.md`](vmm.md) for guest lifecycle.

This note is the map of that path: how Zircon **organizes pages**
(arena `vm_page_t` array, VMO page list, VMAR mappings), the VM
objects and how they nest, how physical pages are born and die, how
the three aspaces are built, how stage-1 and stage-2 are walked, how
lua `setVmem` becomes a PTE,
how a passthrough device's **CPU MMIO**, **IRQ**, and **DMA** (SMMU)
are wired, how GPU grant punches S2 in and out, how SMPU holes stay
out of stage-2, which registers tag the walk, how TLBs are
invalidated, and how permissions are enforced.

Start with **Layout** and **Diagrams**, then the sections they index.

Each later section is meant to be read as **picture → what it means →
where in source**. If a section has no poster in **Diagrams**, it
has its own ASCII figure next to the implementation.

## Contents

- [Framework](#framework)
- [Layout](#layout)
- [Diagrams](#diagrams)
- [Objects](#objects)
- [Pages](#pages)
- [PMM](#pmm)
- [Bootalloc, reserve, heap](#bootalloc-reserve-heap)
- [Host MMU](#host-mmu)
- [VMO](#vmo)
- [Page fault](#page-fault)
- [Stage-2](#stage-2)
- [Device memory](#device-memory)
- [Allocator](#allocator)
- [MapDevice](#mapdevice)
- [Traps vs maps](#traps-vs-maps)
- [Device passthrough](#device-passthrough)
- [SMMU (IOMMU)](#smmu-iommu)
- [SMPU](#smpu)
- [Cache](#cache)
- [ZX_VM flags](#zx_vm-flags)
- [Registers](#registers)
- [TLB](#tlb)
- [Protection](#protection)
- [Flows](#flows)
- [Terminology](#terminology)
- [Code](#code)

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

Software is a stack from DRAM up to a handle. Hardware only ever
sees a page table root and a PTE.

```text
 DRAM / device PA
      │
      ▼
 PMM arena  vm_page_t[]          FREE / OBJECT / WIRED / HEAP / MMU / ALLOC
      │
      ▼
 VmObject  (Paged | Physical | PhysicalPaged)
      │  offset in the VMO
      ▼
 VmMapping  VA/IPA range + arch_mmu_flags
      │  child of
      ▼
 VmAddressRegion  (WAVL, no overlap)
      │  root of
      ▼
 VmAspace  TYPE_USER | KERNEL | GUEST_PHYS
      │  owns
      ▼
 ArmArchVmAspace  TTBR0 / TTBR1 / VTTBR
      │
      ▼
 PTE  (S1 AttrIndx+AP, or S2 MemAttr+S2AP)
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
                              │  lua VmemSpec + traps + MapDevice
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
| Kernel | `TTBR1_EL1` | `KERNEL_ASPACE_BASE` … +256 TiB; physmap first 512 GiB; `KERNEL_BASE` = −4 GiB | `start.S`, then `vm_init` |
| User process | `TTBR0_EL1` + ASID | `[0x1000000, 2^48)` | `ProcessDispatcher`, context switch |
| Guest IPA | `VTTBR_EL2` + VMID | `[0, 2^36)` (64 GiB) | `GuestPhysicalAddressSpace` |
| EL2 itself | `TTBR0_EL2` | EL2 vectors / stack | `el2_hvc_on` once per CPU |
| Device DMA | SMMU STE → S2 TTB (same as `VTTBR`) | IOVA or IPA | `ArmSmmuv3` + vSMMU |

Host never walks `VTTBR`. Guest stage-1 is the guest kernel's problem.
Crook only owns stage-2.

## Layout

Numbers from `zircon/kernel/arch/arm64/rules.mk` and `mmu.h`.

```text
  64-bit VA

  0x0000_0000_0000_0000  ── unused (16 MiB guard)
  0x0000_0000_0100_0000  ── USER_ASPACE_BASE
       TTBR0  user process, ASID-tagged, ASLR
  0x0000_ffff_fe00_0000  ── USER_ASPACE_BASE + USER_ASPACE_SIZE
                             (size = 2^48 − 16 MiB)

  0xffff_0000_0000_0000  ── KERNEL_ASPACE_BASE = PHYSMAP_BASE
       TTBR1  physmap:  paddr_to_physmap(PA) = BASE + PA
                             ARCH_PHYSMAP_SIZE = 512 GiB  (1 << 39)
  0xffff_0080_0000_0000  ── physmap end (typical)

  0xffff_ffff_0000_0000  ── KERNEL_BASE  (−4 GiB)
       kernel .text / .rodata / .data / .bss  (linker script)
  0xffff_ffff_ffff_ffff  ── KERNEL_ASPACE end  (size = 2^48)
```

Guest IPA is **not** a VA. It is the input of stage-2:

```text
  IPA  0                 ── GUEST_PHYSICAL_ASPACE_BASE
       2^36 = 64 GiB     ── GUEST_PHYSICAL_ASPACE_SIZE
                            MMU_GUEST_SIZE_SHIFT = 36
                            VTCR.T0SZ = 64 − 36 = 28
                            VTCR.SL0 = 1  (walk starts at L1)
```

| Constant | Value | Meaning |
| --- | --- | --- |
| `PAGE_SIZE` | 4 KiB | host + guest granule |
| `USER_ASPACE_BASE` | `0x1000000` | 16 MiB guard below user |
| `USER_ASPACE_SIZE` | `0xfffffe000000` | almost 256 TiB |
| `KERNEL_ASPACE_BASE` | `0xffff000000000000` | physmap + kernel |
| `KERNEL_ASPACE_SIZE` | `0x1000000000000` | 256 TiB high half |
| `KERNEL_BASE` | `0xffffffff00000000` | kernel link VA |
| `ARCH_PHYSMAP_SIZE` | 512 GiB | linear map of PA |
| `MMU_GUEST_SIZE_SHIFT` | 36 | IPA width |
| `MMU_USER_SIZE_SHIFT` | 48 | user VA width |
| `MMU_IDENT_SIZE_SHIFT` | 42 | trampoline ident map |

AHYP identity: SOS/tbox/UOS IPA **equals** HPA for every
`kCached` / `kDevice` window. The 64 GiB IPA space is large enough
to cover the SoC PA map Crook hands out.

## Diagrams

Fourteen posters here, then **Pages** for how a 4 KiB granule is
indexed. CPU walk, DMA walk, how lua becomes a PTE, the three legs
of a passthrough device, then PMM, faults, S2 walk, and GPU
`MapDevice`.

### 1. Two walks to the same PA

CPU: guest S1 then Crook S2. DMA: SMMU, same S2 TTB as `VTTBR`.

```text
  guest VA                    device IOVA
      │                            │
      ▼  guest TTBR0/1             ▼  guest CD  (only if nested)
     IPA                          IPA
      │                            │
      │         ┌──────────────────┘
      ▼         ▼
   VTTBR_EL2   SMMU STE  S2TTB = same root, S2VMID = VMID
      │         │
      └────┬────┘
           ▼
          PA  ──►  SMPU  ──►  DRAM / device

  Host EL1 never walks VTTBR (cleared on guest exit).
  EL2 only while the VCPU is in guest: HCR.VM, VTCR, TLBI ipas2*.
```

### 2. Three aspaces

```text
  TTBR1_EL1     kernel + physmap          always on
  TTBR0_EL1     user process + ASID       context switch
  VTTBR_EL2     guest IPA 0 … 2^36        VMID[63:48] | BADDR
  TTBR0_EL2     EL2 vectors / stack       once per CPU
  SMMU STRTAB   STE → S2 TTB              DMA

  ┌─ TYPE_KERNEL ──────────────────────────────┐
  │  TTBR1   KERNEL_BASE / physmap             │
  └──────────────────┬─────────────────────────┘
                     │ zx_guest_create_new
                     ▼
  ┌─ TYPE_GUEST_PHYS  (guest VMAR handle) ─────┐
  │  S2 PTEs   IPA → PA                        │
  │  VMM zx_vmar_map SPECIFIC  ==  stage-2 fill│
  └────────────────────────────────────────────┘
```

### 3. Handle → VMO → PTE

A mapping is always **VA/IPA range → offset in a VMO**. Same VMO
can sit in user TTBR0, kernel, and guest S2 at once.

```text
  EL0   zx::vmo / zx::vmar / zx::guest / zx::bti
           │  handle + rights
           ▼
  EL1   Dispatcher
           │  RefPtr
           ▼
  VM    VmAspace ── root VMAR ── VmMapping ──► VmObject
                                              Paged / Physical
           │                                       │
           ▼                                       ▼
  HW    ArmArchVmAspace  TTBR0/1 or VTTBR     vm_page_t / HPA
```

### 4. `setVmem` → identity S2

Lua does not write page tables. GPA = HPA in the lua path.

```text
  sos.lua  setVmem("IO", 0, 1GiB, kCached, 0)
  allocator append "ram"  is_physmem=1
                 │
                 ▼
  Guest::Init
     kMpu     skip
     kDemand  anonymous VMO          (HPA is PMM, not identity)
     kCached  physical VMO + WB
     kDevice  physical VMO + nGnRE
                 │
                 ▼
     zx_vmar_map(guest_vmar, SPECIFIC|CONTIGUOUS|RWX, gpa)
                 │
                 ▼
     S2 PTE   IPA = gpa_base  →  PA = hpa_base

  first is_physmem  →  PhysMem (VMM host VA over guest RAM)
  IO window keeps is_physmem=0 so PhysMem is DRAM, not MMIO
```

### 5. Map vs trap hole

The 1 GiB SOS IO window is the default identity. Traps punch holes.

```text
  IPA  0                         0x8000000              1 GiB
       [======== kCached IO window (identity) ================]
                 UART ── S2 PTE ──► real device
                 GICD ── hole ──► EL2 abort ──► VMM emulate

  guest ldr / str
       │
       ├─ S2 valid     →  PA  (passthrough)
       ├─ S2 invalid + trap MEM   →  ZX_PKT_GUEST_MEM  IoHandler
       ├─ S2 invalid + trap BELL  →  ZX_PKT_GUEST_BELL  (async)
       └─ S2 invalid, no trap     →  inject_data_abort to guest
```

### 6. Passthrough: three legs

Missing any one is a half-assign.

```text
                    guest driver
              MMIO / DMA / wait-IRQ
           ┌─────────┼─────────┐
           ▼         ▼         ▼
        CPU S2     SPI        SMMU STE
        identity   passthrough  SID → this VM's S2
        kDevice    HW=1 LR      S2_TRANS or S12
           │         │         │
           ▼         ▼         ▼
        regs      ICH_LR      DRAM
                     │
                     └─ bind_vcpu + pin pCPU  (latency)
```

### 7. SMMU: S2 only vs nested S12

```text
  identity DMA                         IOVA DMA
  guest programs IPA=HPA               guest programs vSMMU CD
       │                                    │
       ▼                                    ▼
  STE CFG = S2_TRANS                   STE CFG = S12_TRANS
       │                                    │
       │                               guest CD  IOVA → IPA
       │                                    │
       └────────────┬───────────────────────┘
                    ▼
              host S2TTB + S2VMID     (same as VTTBR)
                    │
                    ▼
                   PA

  Guest never writes host S2TTB / S2VMID.
  QEMU virt: no SMMU; DMA leg is a nop.
```

### 8. `VTTBR` on/off and SMPU

```text
  Vcpu::Resume
     msr vttbr_el2, VMID<<48 | s2_root
     eret guest
     … intercept …
     msr vttbr_el2, xzr          host always VTTBR=0

  kMpu VmemSpec  ── skip in Init ── DTB reserved-memory reg
                                      UnmapPhysicalMemory
                                      S2 hole + PhysMem hole
  CPU load  →  abort to EL2
  DMA to that PA  →  interconnect SMPU deny (even if STE allowed)
```

### 9. PMM page lifecycle

```text
  boot_reserve_add_range     ATF leftover, kernel image, MMIO
  pmm_add_arena              one arena per DRAM window
  boot_reserve_wire          intersecting pages → WIRED

  FREE  ──pmm_alloc_page──►  ALLOC
            │
            ├─ kernel heap          HEAP
            ├─ page table           MMU     (pt_pages_)
            ├─ VmObjectPaged        OBJECT  (map_count, pin_count)
            └─ kernel mapping       WIRED   (not freeable)

  pmm_free / Unmap / Destroy  ──►  FREE  (back on the arena list)

  Physical VMO does **not** take a vm_page_t from PMM.
  It names an HPA that already exists (MMIO or leftover DRAM).
```

### 10. Host page fault

```text
  EL1 data/instr abort
       │
       ▼  vmm_page_fault_handler(va, flags | HW_FAULT)
       ▼  vaddr_to_aspace  →  TYPE_USER or KERNEL
       ▼  root VMAR  WAVL walk  →  VmMapping
       ▼  check AP / UXN vs pf_flags
       ▼  VmObject::GetPageLocked(vmo_offset, pf_flags)
              already in page_list  →  that page
              parent + read         →  share parent's page
              parent + write        →  COW clone, memcpy
              none                  →  pmm_alloc_page, zero
       ▼  ArmArchVmAspace::Map  (read fault drops WRITE bit)
       ▼  return, retry the load/store
```

### 11. Stage-2 walk (36-bit IPA, 4K, SL0=1)

```text
  IPA [35:0]

   35        30 29        21 20        12 11         0
  ┌────────────┬────────────┬────────────┬────────────┐
  │ L1 idx [6] │ L2 idx [9] │ L3 idx [9] │ page off   │
  └─────┬──────┴─────┬──────┴─────┬──────┴────────────┘
        │            │            │
        ▼            ▼            ▼
    VTTBR.BADDR    L1 table     L2 table     L3 table
    64 entries     512          512
        │
        ├─ [1:0]=01 BLOCK  →  1 GiB PA     (ZX_VM_FLAG_MAP_CONTIGUOUS)
        ├─ [1:0]=11 TABLE  →  next level
        └─ [1:0]=00 INVALID → abort to EL2

  L2 BLOCK = 2 MiB.  L3 PAGE = 4 KiB.
```

### 12. GPU `MapDevice` (SWA grant)

Grant does **not** go through lua. The `VmMapping` already exists;
only the S2 PTE is written or cleared.

```text
  GpuScheduler::MapGpu(VmInfo)
     for region in mapping_regions:
        if already mapped: skip
        GPAS::MapDevice(vm_mapping, paddr)
           arch_aspace.MapContiguous(IPA, HPA, flags)
           S2 PTE appears  →  guest can MMIO the Mali
        region.mapped = true

  GpuScheduler::UnmapGpu
     skip ARB_GPU_REGION_PIN
     GPAS::UnMapDevice  →  arch_aspace.Unmap  →  S2 hole

  ProtectDevice(enable=false)  drops S2AP write without unmap.
```

### 13. Boot MMU (trampoline)

```text
  start.S   MMU off, running at load PA
     zero BSS, boot_alloc_init
     TTBR1 = kernel table
        physmap: KERNEL_ASPACE_BASE+PA → PA   DATA flags
        kernel:  KERNEL_BASE → __code_start   RWX (locked later)
     TTBR0 = tt_trampoline
        1 GiB identity block covering this code
     TCR = MMU_TCR_FLAGS_IDENT, MAIR, SCTLR.M=1
     br KERNEL_BASE
     drop trampoline: TCR.EPD0, tlbi vmalle1
     vm_init_preheap → heap_init → vm_init (RX/RO/RW)
```

### 14. S1 PTE vs S2 PTE

S1 uses a **MAIR index**. S2 encodes the memory type **in the PTE**.

```text
  S1 (TTBR0/1)                              S2 (VTTBR)
  63            54 53   11 10  9:8  7:6  5  4:2     1:0
  ┌─┐ … UXN PXN  nG AF  SH   AP  NS AttrIndx  type
                 │              │       │
                 │              │       └─ MAIR_EL1[index]
                 │              └─ 00 priv-RW, 01 user-RW, 10 priv-RO, 11 user-RO
                 └─ nG=1  ASID-tagged (user)

  S2
  63      54:53     10  9:8  7:6   5:2      1:0
  ┌─┐ …   XN        AF  SH   S2AP  MemAttr  type
                 │           │     │
                 │           │     └─ 0xf WB, 0x1 nGnRE, 0x0 nGnRnE, 0x5 NC
                 │           └─ 01 RO, 11 RW   (no user/priv split)
                 └─ 10 = XN
```

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
                    VmAspace                    (TYPE_USER | KERNEL | GUEST_PHYS | LOW_KERNEL)
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

`TYPE_LOW_KERNEL` exists only for SMP bootstrap of very low PA; Crook
AHYP does not use it for guests.

| Object | Owns | Notes |
| --- | --- | --- |
| `VmAspace` | root VMAR + `ArmArchVmAspace` | One hardware table. `Destroy()` to break the VMAR cycle. |
| `ArmArchVmAspace` | top-level PTE page, ASID or VMID | S1 vs S2 attrs chosen by `ARCH_ASPACE_FLAG_GUEST`. |
| `VmAddressRegion` | child VMARs / mappings | `CAN_MAP_{READ,WRITE,EXECUTE,SPECIFIC}`. |
| `VmMapping` | `RefPtr<VmObject>`, offset, MMU flags | `MapRange` writes PTEs. `Protect` / `Unmap` may split. |
| `VmObject` | size, cache policy, children | Not necessarily mapped. |
| `VmObjectPaged` | `vm_page_t` list | Fault / `CommitRange` / `CommitRangeContiguous`. COW via `parent_`. |
| `VmObjectPhysical` | `[paddr, paddr+size)` | MMIO and identity RAM. No PMM alloc. |
| `VmObjectPhysicalPaged` | HPA per page | Non-contiguous passthrough RAM. |
| `vm_page_t` | 32 B, `state` + unused `map_count` | One per DRAM page in the arena array. See **Pages**. |
| PMM | arenas of DRAM | `pmm_alloc_page`, `pmm_alloc_contiguous`, `pmm_alloc_range`. |

`zx_vmar_map` path:

```text
zx_vmar_map(vmar, flags, offset, vmo, vmo_off, len)
  →  VmAddressRegionDispatcher::Map
  →  parent VMAR  CreateVmMapping(SPECIFIC?, vmo, flags)
  →  VmMapping::MapRange  (optional commit)
  →  ArmArchVmAspace::Map  or  MapContiguous
  →  S1 or S2 PTE
```

`Map` takes an array of HPAs (one per page). `MapContiguous` takes a
single base HPA and walks it linearly — that is what identity RAM and
`MapDevice` use, and what lets the arch code emit 1 GiB / 2 MiB
blocks.

### Handles and dispatchers

| Handle (`zx_obj_type`) | Dispatcher | Core object |
| --- | --- | --- |
| `ZX_OBJ_TYPE_VMO` | `VmObjectDispatcher` | `VmObject` |
| `ZX_OBJ_TYPE_VMAR` | `VmAddressRegionDispatcher` | `VmAddressRegion` |
| `ZX_OBJ_TYPE_PROCESS` | `ProcessDispatcher` | `VmAspace TYPE_USER` |
| `ZX_OBJ_TYPE_GUEST` | `GuestDispatcher` | `Guest` → GPAS + `TrapMap` |
| `ZX_OBJ_TYPE_IOMMU` | `IommuDispatcher` | `Iommu` / `ArmSmmuv3` / `DummyIommu` |
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
      ├─ vmid_            kernel VMID  SOS=1 ALPS=2 TBOX=3
      ├─ GuestPhysicalAddressSpace
      │     paspace_  VmAspace TYPE_GUEST_PHYS
      │     guest_phys_mem_  optional backing VMO
      │     CreateGuestPtr → kernel VMAR mapping of a GPA slice
      │     MapDevice / UnMapDevice / ProtectDevice
      ├─ TrapMap   WAVL of Trap (MEM / BELL)
      └─ Vcpu[]    each Resume loads VTTBR from paspace_.arch_aspace()
```

User vmid in FIDL / lua vs kernel VMID (`Guest::kGuestVmidOffset = 2`):

| Guest | User vmid | Kernel VMID |
| --- | --- | --- |
| SOS | −1 | 1 |
| UOS / ALPS | 0 | 2 |
| tbox | 1 | 3 |

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

                 ──► DummyIommu   (no SMMU; Map returns the HPA as IOVA)

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
in a paged VMO (`page_list_` + `mapping_list_`), identity in a
Physical VMO, and visible as IPA via S2 **without** a second
`vm_page_t`. Physical VMOs do not take pages from PMM; they name
HPA that already exist.

How those `vm_page_t` sit in an arena, how a paged VMO finds them,
and how a mapping views them is **Pages**.

## Pages

A Zircon **page** is 4 KiB of DRAM plus a 32-byte `vm_page_t`. The
same granule is indexed three ways. Device MMIO is not in any arena,
so it has no `vm_page_t`. Identity guest RAM on AHYP *does* sit in
an arena, but the Physical VMO never looks at that slot.

```text
  1. Physical   PmmArena.page_array_[PA / 4KiB]     always, for DRAM
  2. Logical    VmObjectPaged.page_list_            only paged VMOs
  3. Virtual    VmMapping in a VMAR WAVL            any VMO, any aspace
```

### Principle

1. **One metadata slot per DRAM page, forever.** `PmmArena::Init`
   carves `page_array_` out of the arena itself (near the top, via
   `boot_reserve_range_search`). Index `i` is PA `base + i*4KiB`.
   `paddr_to_vm_page` / `vm_page_to_paddr` are arithmetic, not a
   hash. Device MMIO has no `vm_page_t`.
2. **The VMO is the bytes; the mapping is a view.** A paged VMO
   holds a sparse tree of `vm_page_t*`. A physical VMO holds only
   `[base, base+size)` and computes `PA = base + offset`. A mapping
   is `VA/IPA range + VMO offset + MMU flags`. The same VMO can
   have many mappings (user TTBR0, kernel, guest S2).
3. **Sparse on purpose.** Creating a VMO allocates **no** pages.
   `page_list_` nodes appear on fault / `CommitRange` / write. A
   1 GiB anonymous VMO with one committed page has one
   `VmPageListNode` of 16 slots, 15 of them null.
4. **Views are tracked on the VMO, not on the page.**
   `vm_page_t.map_count` and `object.obj` / `object.offset` exist
   in the struct but are **unused** in this tree (comments say so).
   Live mappings hang off `VmObject::mapping_list_`. Pinning uses
   `object.pin_count` (5 bits, max 31).
5. **Physical VMO is a name, not a bag of pages.** SOS `"ram"` and
   device MMIO are `VmObjectPhysical`. Lookup never touches PMM.
   AHYP still lets leftover DRAM that *is* in an arena be wrapped
   this way (`sys_vmo_create_physical` skips
   `pmm_arena_intersects`).

### Diagrams

#### P.1 Three indexes of one 4 KiB

```text
  DRAM  4 KiB at PA
           │
           │  index = (PA − arena.base) / 4096
           ▼
  page_array_[i]   vm_page_t   32 B     (PMM, always for DRAM)
           │
           │  only if a paged VMO committed this PA
           ▼
  VmPageListNode.pages_[j]              (logical, sparse)
           │
           │  GetPageLocked → paddr
           ▼
  PTE in TTBR0 / TTBR1 / VTTBR          (virtual, one per mapping)
           │
           └─ same VMO, N mappings  →  N PTEs, one vm_page_t
```

Physical VMO skips the middle box: `PA = vmo.base_ + offset`.

#### P.2 Arena: `page_array_` is the index

```text
  PmmArena  [base, base+size)
  ┌──────────────────────────────────────────────────────────┐
  │  DRAM pages  0 … N−1                                     │
  │                                                          │
  │   page_array_  (WIRED, carved near the top of the arena) │
  │   vm_page_t[N]   N = size / 4096                         │
  │                                                          │
  │   free_list_  ──► FREE pages, intrusive via free.node    │
  └──────────────────────────────────────────────────────────┘

  pa  = arena.base + (page − page_array_) / 32 * 4096
  page = &page_array_[(pa − arena.base) / 4096]

  Init:
    boot_reserve_range_search(base, size, array_bytes)  // high end
    memset array
    pages that back the array → WIRED
    every other page          → FREE, push free_list_
```

Several arenas, priority-sorted. Alloc walks the list. `KMAP`
arenas are already in the physmap (`paddr_to_physmap` works).

#### P.3 `VmPageList`: WAVL of 16-page nodes

A paged VMO does **not** keep a flat `vm_page_t*` per offset.
Offsets are packed 16 to a node (64 KiB of VMO). Nodes live in a
WAVL tree keyed by the node's aligned offset.

```text
  VMO offset  0x123000
    node_offset = ROUNDDOWN(0x123000, 16*4KiB) = 0x120000
    index       = (0x123000 >> 12) % 16        = 3

  page_list_  WAVL<uint64_t, VmPageListNode>
       │
       ├── node 0x00000   pages_[16]   // offsets 0 … 0xF000
       ├── node 0x10000   pages_[16]
       └── node 0x12000   pages_[0..15]
                            [3] = vm_page_t*  →  PA of offset 0x123000
                            others nullptr    →  not committed

  AddPage: find or alloc node, pages_[index] = p
  GetPage: find node, return pages_[index] (or nullptr)
  FreePage: null the slot; if node empty, erase it; pmm_free_page
```

Empty holes cost nothing. Walking `ForEveryPageInRange` starts at
`upper_bound(start)−1` and stops at `lower_bound(end)`.

#### P.4 Mapping: VA/IPA → VMO offset

```text
  VmAspace
    root VMAR  WAVL keyed by base()     children do not overlap
      ├── VMAR
      └── VmMapping  base_, size_, object_offset_, arch_mmu_flags_
            │  RefPtr
            ▼
          VmObject
            mapping_list_  ◄── this VmMapping (intrusive)
            page_list_     or  [base,size) / HPA vector

  va  →  vmo_offset = (va − mapping.base) + mapping.object_offset
  ipa →  same formula  (guest aspace base is 0)

  ActivateLocked
     object_->AddMappingLocked(this)
     parent VMAR subregions_.insert(this)

  Destroy
     UnmapLocked entire range
     RemoveMappingLocked
```

`MapRange` walks the range, `GetPageLocked` each offset, batches up
to 16 HPAs in `VmMappingCoalescer`, then `arch_aspace.Map`.
`commit=true` passes `VMM_PF_FLAG_SW_FAULT` so missing pages are
allocated. `ZX_VM_FLAG_MAP_CONTIGUOUS` uses `MapContiguous` (one
base HPA) instead.

#### P.5 Three VMO shapes

```text
  VmObjectPaged                         VmObjectPhysical
  page_list_  (sparse vm_page_t*)       base_, size_     no pages
  GetPage → tree lookup / fault / COW   GetPage → base + offset
  kDemand, user heap, files             kCached / kDevice identity

  VmObjectPhysicalPaged
  Vector<uint64_t>  one HPA per page
  GetPage → page_list_[offset / 4096]
  sparse passthrough RAM
```

#### P.6 One DRAM page, several views

```text
  vm_page_t  (OBJECT)  PA 0x4_8000_0000
       │
       ▼
  parent VMO  page_list_ [off 0x2000] = this page
       │
       ├── clone (COW, empty slot)  read  →  same page, PTE RO
       │                            write →  new page, memcpy
       └── mapping A  user TTBR0
           mapping B  kernel GuestPtr (temporary)
           mapping C  — none: physical guest RAM is a *different*
                       VMO (Physical), same PA, no vm_page_t slot

  zero_page  (WIRED, one system-wide)
     read fault on empty paged VMO  →  map this PA RO
     write fault                    →  alloc + zero, never write
                                       the shared zero page
```

### Implementation

#### `vm_page_t` (32 bytes, `page.h`)

```text
  flags:8
  state:3     FREE ALLOC WIRED HEAP OBJECT MMU
  map_count   unused in this tree
  union pad[24]
    free.node              FREE / ALLOC lists
    object.offset, obj     unused (comment)
    object.pin_count:5     BTI / CommitRangeContiguous
    object.contiguous_pin  one pin slot spent to hold a run
```

| `state` | Set by | Meaning |
| --- | --- | --- |
| `FREE` | `PmmArena::Init` / `FreePage` | on `free_list_` |
| `ALLOC` | `AllocPage` | just taken, not classified |
| `WIRED` | Init (array itself), `vm_init_preheap`, `CreateFromROData` | never freed |
| `HEAP` | `heap_wrapper.cpp` after `pmm_alloc_kpages` | cmpctmalloc |
| `OBJECT` | `InitializeVmPage` in `VmObjectPaged` | in a `page_list_` |
| `MMU` | x86 page-table alloc | ARM64 `AllocPageTable` uses `pmm_alloc_kpage` and **leaves `ALLOC`** |

`InitializeVmPage`: `ALLOC → OBJECT`, `pin_count = 0`,
`contiguous_pin = 0`.

#### Arena index math (`pmm_arena.h`)

```text
page_belongs_to_arena(page)
  page_array_base ≤ page < page_array_base + N * 32

page_address_from_arena(page)
  ((page − page_array_) / 32) * 4096 + info_.base

address_in_arena(pa)
  base ≤ pa ≤ base + size − 1
```

`pmm_alloc_page`: walk arenas, `list_remove_head` of `free_list_`,
`state = ALLOC`, compute PA from the pointer. `pmm_alloc_range(pa)`:
`AllocSpecific` indexes `page_array_` directly (kernel image, boot).
`pmm_alloc_contiguous`: scan `page_array_` at alignment boundaries
for a run of FREE, then pull each off `free_list_`.

#### Paged VMO (`vm_object_paged.cpp`)

```text
Create(flags, size)          round size, empty page_list_
AddPageLocked(p, offset)     page_list_.AddPage; p already OBJECT
GetPageLocked(offset, flags)
  hit page_list_             return that page
  parent_ + read             return parent's page (shared)
  parent_ + write            alloc, memcpy, AddPage  (COW)
  no parent, read, no flags  return zero_page (RO)
  FAULT_MASK set + write     pmm_alloc_page / free_list, ZeroPage, AddPage
  FAULT_MASK clear           ZX_ERR_NOT_FOUND
CommitRange                  count holes, pmm_alloc_pages, GetPageLocked
                             with SW_FAULT|WRITE using that free_list
DecommitRange                unmap all mapping_list_ views, FreePage
Pin / Unpin                  pin_count++; fail if not committed
CloneCOW                     new VMO, parent_=this, shared lock_,
                             empty page_list_, AddChildLocked
~VmObjectPaged               assert pin_count==0, FreeAllPages
```

Clones share `lock_` with the parent (`VmObject` constructor points
`lock_` at the parent's mutex). `RangeChangeUpdateLocked` walks
`mapping_list_` and unmaps that offset in every aspace so the next
access re-faults (needed after COW replace or decommit).

#### Physical VMO (`vm_object_physical.cpp`)

```text
Create(base, size)           page-aligned, default UNCACHED
GetPageLocked                *_page = nullptr
                             *_pa   = base_ + ROUNDDOWN(offset)
Lookup                       lookup_fn(..., base_ + off) per page
SetMappingCachePolicy        fails if mapping_list_len_ != 0
```

No `vm_page_t`, no pin, no COW, no commit. `MapRange(commit=false)`
still writes PTEs because `GetPageLocked` always succeeds.

#### Mapping the page into hardware (`vm_mapping.cpp`)

```text
MapRange(offset, len, commit)
  for each page:
     GetPageLocked(vmo_off, WRITE | (commit ? SW_FAULT : 0), &pa)
     coalescer.Append(va, pa)     // flush every 16, or on PA break
  coalescer.Flush → arch_aspace.Map(base, phys[], count, flags)

PageFault(va, pf_flags)
  permission vs arch_mmu_flags_
  GetPageLocked
  if already mapped at va:
     same PA → Protect (e.g. upgrade RO → RW after COW)
     other PA → Unmap + MapContiguous
  else MapContiguous(va, new_pa, 1)
  read fault drops PERM_WRITE in the PTE
```

Guest S2 uses the same `MapRange` / `PageFault`. `ARCH_ASPACE_FLAG_GUEST`
only changes how `ArmArchVmAspace` encodes the PTE.

#### Pin (BTI / contiguous)

`Pin` requires every page already committed (`ZX_ERR_NO_MEMORY`
otherwise). `pin_count++` up to 31. `contiguous_pin` spends one
slot so `CommitRangeContiguous` can keep the run. `pmm_free` of an
OBJECT page asserts `pin_count == 0`. Physical VMOs have no pin
path; host DMA into identity RAM does not go through PMT.

## PMM

The physical memory manager owns DRAM that is **not** reserved by
firmware. It does not own device MMIO. Page-array layout is
**Pages**; this section is **who gets a page and how**.

```text
  arena_list_  (priority, low number first)
     ┌─ PmmArena "ram0"  KMAP  base=0x40000000  size=…
     │     page_array_[N]
     │     free_list_ ──► FREE vm_page_t ──► …
     ├─ PmmArena "ram1"  …
     └─ …

  pmm_alloc_page
     lock arena_lock
     for a in arena_list:
        skip if KMAP-only request and arena is not KMAP
        page = a.AllocPage  →  pop free_list_ head, state=ALLOC
        pa   = a.page_address_from_arena(page)
        return page, pa
     fail → nullptr
```

### Arena

Platform early init calls `pmm_add_arena` once per DRAM window
(`pmm_arena_info_t`: name, flags, priority, base, size). The
`PmmArena` object itself is `boot_alloc_mem`. Arenas insert into
`arena_list_` in **priority order**. Alloc always walks from the
front.

`PMM_ARENA_FLAG_KMAP`: already in the physmap, safe for kernel
`pmm_alloc_kpages`. `PMM_ALLOC_FLAG_KMAP` restricts alloc to those
arenas.

`paddr_to_vm_page` / `vm_page_to_paddr` scan arenas (no lock: the
list is frozen after boot). `pmm_arena_intersects(pa, size)` is
true if the range overlaps any arena.

### Alloc paths

```text
  pmm_alloc_page          pop one FREE, any PA
  pmm_alloc_pages(n)      pop n FREE, PA may scatter
  pmm_alloc_range(pa, n)  AllocSpecific: index page_array_[(pa-base)/4K]
                          fail if that slot is not FREE
  pmm_alloc_contiguous    scan page_array_ at align boundaries
                          for a run of FREE, then pull each off free_list_
  pmm_alloc_kpages        contiguous + KMAP + return physmap VA
  pmm_free                for each page, find owning arena, FreePage
                          assert not already FREE
                          OBJECT pages must have pin_count==0
                          state=FREE, push free_list_
```

| Call | What |
| --- | --- |
| `pmm_alloc_page` | one page, any (or KMAP) arena |
| `pmm_alloc_pages` | N pages, not necessarily contiguous |
| `pmm_alloc_range(pa, n)` | **specific** HPAs (wire kernel, boot) |
| `pmm_alloc_contiguous(n, align)` | physically contiguous run |
| `pmm_alloc_kpages` | contiguous + return physmap VA |
| `pmm_free` / `pmm_free_page` | back to FREE |

AHYP exception: `sys_vmo_create_physical` **skips**
`pmm_arena_intersects` when `BOOTMODE_AHYP`. Leftover DRAM that
still sits in a PMM arena may become a physical VMO (SOS `"ram"`).
Non-AHYP returns `ZX_ERR_ACCESS_DENIED` for that, so physical VMOs
are MMIO-only there.

```text
  Non-AHYP                         AHYP
  physical VMO ∩ arena  DENIED     physical VMO ∩ arena  OK
  (MMIO only)                      (SOS "ram" identity)
```

## Bootalloc, reserve, heap

Three allocators, in time order. Only PMM is used after `vm_init`.

```text
  DRAM
  ├──────── kernel image / boot_alloc bump ────────┤  WIRED later
  ├──── boot_reserve holes (ATF, MMIO, DTB) ───────┤  never FREE
  ├──── page_array_ (arena metadata, high end) ────┤  WIRED in Init
  └──── remaining  →  PMM free_list_ ──────────────┘

  time
    boot_alloc  ──(dies)──►  PMM
    boot_reserve ──wire──►   WIRED
    heap_init    ──uses──►   pmm_alloc_kpages → HEAP
```

```text
  start.S
     boot_alloc_init          bump pointer from kernel load PA
     boot_alloc_mem           PmmArena object, early tables
     boot_reserve_add_range   ATF / kernel / MMIO holes
     pmm_add_arena            consults reserve so metadata misses holes
     boot_reserve_wire        intersecting pages → WIRED

  lk_main
     vm_init_preheap
        mark boot_alloc [start, end) WIRED
        random 0..15 padding pages (KASLR-ish physmap pad)
        zero_page  (shared RO zero for demand-paged reads)
     heap_init                cmpctmalloc, pages → HEAP
     vm_init                  lock kernel RX / RO / RW
```

`boot_alloc_*` is a bump allocator (`boot_alloc_start` …
`boot_alloc_end`). It is **not** a heap. After `boot_reserve_wire`,
those pages are WIRED and never return to PMM.

Kernel heap is **cmpctmalloc**. `heap_page_alloc` calls
`pmm_alloc_kpages` then marks every page `HEAP`. Small allocations
round up to a bucket; free coalesces; a whole OS page that is
entirely free goes back to PMM (one cached empty page is kept to
avoid churn). Large allocations through the heap are not allowed —
those go through `pmm_alloc_kpages` / VMO.

`zero_page` is a single WIRED page of zeros. A read fault on an
anonymous VMO that has never been written can map it RO; the next
write faults and COWs.

## Host MMU

Host EL1 always has TTBR1 on. TTBR0 is a per-process user table.
EL2 has a tiny table of its own. Guest IPA never lives in any of
these — that is `VTTBR`.

```text
                    always                 context switch              guest only
  TTBR1_EL1 ──────── kernel + physmap
  TTBR0_EL1 ──────────────────────────── user + ASID  (EPD0 off)
  TTBR0_EL2  EL2 vectors / stack  (el2_hvc_on, once per CPU)
  VTTBR_EL2  ──────────────────────────────────────────── VMID|S2 root
```

### Physmap and kernel

After (8) in [`bootstrap.md`](bootstrap.md):

```text
paddr_to_physmap(PA) = 0xffff000000000000 + PA     KERNEL_ASPACE_BASE
KERNEL_BASE          = 0xffffffff00000000          kernel link VA

  VA
  0xffff_0000_0000_0000   physmap  PA 0 … 512 GiB     DATA (RW, XN)
       …
  0xffff_ffff_0000_0000   .text    R-X
                          .rodata  R--
                          .data/.bss  RW-
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
(`MMU_TCR_FLAGS_USER`). Kernel aspace is a singleton constructed
as a global (`VmAspace::kernel_aspace()`), `asid_ = 0xffff`.

### Identity trampoline

MMU-on cannot jump from PA to `KERNEL_BASE` in one instruction
while both are unmapped. `start.S` therefore programs **two**
tables at once:

```text
  before br KERNEL_BASE              after TCR.EPD0
  TTBR0  1 GiB identity (PC's PA)    disabled (EPD0)
  TTBR1  physmap + kernel RWX        same, then vm_init locks RX/RO/RW
  TCR    FLAGS_IDENT                 FLAGS_KERNEL
         tlbi vmalle1
```

| Table | Register | Content |
| --- | --- | --- |
| `tt_trampoline` | `TTBR0` | 1 GiB identity block covering the current PC |
| kernel table | `TTBR1` | physmap + kernel at `KERNEL_BASE` |

`TCR = MMU_TCR_FLAGS_IDENT` (TTBR0 ident granule). After `br
KERNEL_BASE`, the kernel sets `TCR.EPD0` (disable TTBR0) and
`tlbi vmalle1`. Secondaries wait on `page_tables_not_ready`.

### User

`VmAspace::TYPE_USER`: one top-level TTBR0 table, unique ASID from
`AsidAllocator` (bitmap, 16-bit, range `[1, 0xfffe]`; `0` unused,
`0xffff` global/kernel). Search starts at `last_+1` and wraps.
User aspaces also enable ASLR (`AslrPrng`).

```text
  AsidAllocator.bitmap_   [0 unused | 1 … 0xfffe user | 0xffff kernel]
  Init(TYPE_USER)  asid.Alloc(&asid_)
                   pmm_alloc_kpage → tt_virt_ / tt_phys_, zeroed
  Destroy          tlbi aside1is, asid.Free, pmm_free top table

  ContextSwitch(from, to)
     if from AUTO cache-prot: PoisonCache
     if to:   ttbr0 = ASID<<48 | tt_phys;  tcr = FLAGS_USER
     else:    tcr = FLAGS_KERNEL   (EPD0, TTBR0 ignored)
```

The kernel never maps guest IPA into TTBR1 except via a temporary
`GuestPtr`. PMM arenas of DRAM; WIRED pages are not freeable.

Guest aspaces **do not** take an ASID from this allocator. The
8-bit VMID is stored in the same `asid_` field via
`arch_set_asid(vmid)` after `VmAspace::Create(TYPE_GUEST_PHYS)`.

### EL2 MMU

`el2_hvc_on` (once per CPU) programs `TTBR0_EL2`, `TCR_EL2`,
`MAIR_EL2 = MMU_MAIR_VAL`, `SCTLR_EL2.M`, then `tlbi alle2`. That
table covers EL2 vectors and the EL2 stack only.

```text
  EL2  TTBR0_EL2     vectors, EL2 stack     always (after hvc_on)
  EL2  VTTBR_EL2     guest IPA → PA         only in switch_to_guest
  EL1  TTBR1         kernel                 always
  EL1  TTBR0         user                   when a user thread runs
```

Guest IPA is **not** in `TTBR0_EL2`. Guest translation is `VTTBR` +
`VTCR` + `HCR.VM`. Host EL1 never `msr`s those; it uses HVC helpers.

## VMO

The VMO is the bytes. A mapping is one view of a slice.

Page-list shape, WAVL fan-out, and how `GetPageLocked` fills a slot
are **Pages**. This section is the VMO types and cache policy.

```text
  zx_vmo_create(size)
       VmObjectPaged     page_list_ empty     kDemand
  zx_vmo_create_physical(pa, size)
       VmObjectPhysical  base_=pa             kCached / kDevice
  zx_vmo_create_physical_paged(hpa[])
       VmObjectPhysicalPaged  Vector<HPA>     sparse passthrough

  all three:
     mapping_list_   every VmMapping of this VMO
     children_list_  COW clones (paged only)
     lock_           clones share the parent's mutex
```

### Paged (`zx_vmo_create`)

`VmObjectPaged` holds a `VmPageList` of `vm_page_t` keyed by offset.
Pages are **not** allocated at create. They appear on:

- page fault (`GetPageLocked`)
- `CommitRange` / `CommitRangeContiguous`
- write (`zx_vmo_write`)

`GetPageLocked` (full tree walk in **Pages**):

```text
  page_list_.GetPage(offset)  hit  →  return
  parent_ exists
     read  fault  →  return parent's page (shared)
     write fault  →  pmm_alloc_page, memcpy from parent, AddPage  (COW)
  no parent / miss
     pmm_alloc_page, zero (or copy from supplied page), AddPage
```

Clone (`zx_vmo_clone` → `CloneCOW`) sets `parent_` +
`parent_offset_`, empty `page_list_`, `AddChildLocked`. SOS RAM is
**not** this path; it is a physical VMO. `kDemand` is this path:
GPA is as requested, HPA is whatever PMM hands out.

```text
  parent VMO     page_list_ [0x2000] = P
  clone VMO      page_list_ empty, parent_offset_
       read  0x2000  →  P  (shared, PTE without WRITE)
       write 0x2000  →  alloc P', memcpy, clone.page_list_[0x2000]=P'
                       RangeChangeUpdate  unmap that offset in every mapping
```

`pin_count` (5 bits) stops the page from being freed or moved while
a BTI PMT holds it. `contiguous_pin` spends one pin slot to keep a
run contiguous after `CommitRangeContiguous`.

### Physical (`zx_vmo_create_physical`)

`VmObjectPhysical::Create(paddr, size)`. No `vm_page_t`. Lookup is
`paddr + offset`. Default cache policy is **UNCACHED**;
`Guest::Init` then `set_cache_policy` to CACHED or DEVICE.

```text
  GetPageLocked(off)  *_page = nullptr
                      *_pa   = base_ + ROUNDDOWN(off)
  SetMappingCachePolicy  fail if mapping_list_len_ != 0
```

AHYP lets this VMO cover leftover DRAM (see **PMM**). That is how
SOS `"ram"` is identity-mapped.

### PhysicalPaged (`zx_vmo_create_physical_paged`)

A list of HPAs, one per page. Used when the window is not a single
contiguous run (sparse passthrough RAM). Lookup is
`page_list_[offset / PAGE_SIZE]`. Size is `vector.size() * 4KiB`.

### Cache policy on the VMO

| `ZX_CACHE_POLICY_*` | `ARCH_MMU_FLAG_*` | S1 AttrIndx | S2 MemAttr |
| --- | --- | --- | --- |
| `CACHED` (0) | `CACHED` | 2 Normal WB | `0xf` WB inner-shareable |
| `UNCACHED` (1) | `UNCACHED` | 0 nGnRnE | `0x0` strongly ordered |
| `UNCACHED_DEVICE` (2) | `UNCACHED_DEVICE` | 1 nGnRE | `0x1` Device-nGnRE |
| `WRITE_COMBINING` (3) | `WRITE_COMBINING` | 3 Normal NC | `0x5` Normal uncached |

`Guest::Init` maps `kDevice` → `ZX_CACHE_POLICY_UNCACHED_DEVICE`,
everything else → `CACHED`.

## Page fault

Two entry points, same `VmAspace::PageFault` → VMAR WAVL →
`VmMapping::PageFault`.

```text
  EL1 abort                         EL2 data/instr abort
  vmm_page_fault_handler            handle_data_abort
       │                                 │
       │                            trap? ──yes──► MEM / BELL  (not a fault)
       │                                 │ no
       ▼                                 ▼
  vaddr_to_aspace(va)               vmm_guest_page_fault_handler(ipa)
       │                                 │
       └────────────┬────────────────────┘
                    ▼
             aspace->PageFault
             root VMAR WAVL → VmMapping
             check AP / UXN vs pf_flags
             GetPageLocked
             Map / Protect / replace PTE
```

`pf_flags` (`fault.h`): WRITE, USER, GUEST, INSTRUCTION,
NOT_PRESENT, HW_FAULT, SW_FAULT.

### Host (`vmm_page_fault_handler`)

Called from the EL1 abort path with `VMM_PF_FLAG_HW_FAULT` plus
write / user / instruction / not-present. `vaddr_to_aspace` picks
kernel vs current user. Permission checks use `arch_mmu_flags_`
against `pf_flags`. Then `GetPageLocked`. A **read** fault maps the
PTE **without** `PERM_WRITE`, so a later store COWs.

```text
  already mapped at va
     same PA, flags differ  →  Protect (RO → RW after COW)
     other PA               →  Unmap + MapContiguous
  not mapped                →  MapContiguous(va, new_pa, 1)
  EXECUTE region on ARM64   →  arch_sync_cache_range  (I-cache)
```

A user fault that returns `ZX_ERR_NOT_FOUND` dumps free-page count
and process PSS (debug). A kernel fault does not — the kernel may
already hold VM locks.

### Guest (`vmm_guest_page_fault_handler`)

`handle_data_abort` / `handle_instruction_abort` call this when
there is **no trap** at that IPA. Flags are
`HW_FAULT | WRITE | INSTRUCTION` (the guest path does not try to
be precise about WnR). `PageFault` on the **guest** aspace demand-
fills an S2 PTE for a `kDemand` VMO. If that fails,
`inject_data_abort_exception`:

```text
  ESR_EL1 = (0x25 << 26) | 0x10     data abort, same EL
  FAR_EL1 = IPA
  SPSR_EL1 |= I/F/A mask
  ELR_EL1 = guest PC
  ELR_EL2 = VBAR_EL1 + 0x200        guest vector, same EL
```

Instruction abort **only** tries the fill; it does not look at
traps. `force_inject_data_abort` (debug) skips the fill.

`HCR.PTW`: a stage-2 fault taken **during a guest table walk**
still traps to EL2. Without it those faults would be reported as
guest S1 faults.

## Stage-2

### Create

```text
  zx_guest_create_new(resource, options, user_vmid)
       Guest::Create
          kernel VMID = user_vmid + 2     SOS 1 / ALPS 2 / TBOX 3
          GPAS::Create(vmid)
             VmAspace::Create(TYPE_GUEST_PHYS, "guest_paspace-N")
                base 0, size 2^36
                arch_aspace.Init(..., ARCH_ASPACE_FLAG_GUEST)
                   pmm_alloc_kpage  top-level S2 table, zeroed
                   asid_ left UNUSED  (no AsidAllocator)
             arch_set_asid(vmid)          VMID into asid_
          returns guest handle + guest VMAR handle

  VTTBR value (not loaded yet)
     [63:48] VMID    [47:0] tt_phys   arm64_vttbr()
```

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

Why SL0=1: `MMU_LX_X(12, 1) = 30`, and 36 > 30, so the top index
shift is 30. Top table has `1 << (36 − 30) = 64` entries. A 4-level
walk starting at L0 would need a 39-bit IPA.

```text
 Guest VA
    │  guest TTBR0/1   (guest-owned)
    ▼
 IPA  (FAR_EL2 low 12 bits | HPFAR_EL2 << 8)
    │  VTTBR_EL2, VTCR_EL2, HCR.VM=1
    ▼
 PA
```

HPFAR holds IPA[47:12]; EL2 asm left-shifts it by 8 before storing
into `GuestState`. Software ORs `FAR_EL2 & 0xfff` to recover the
page offset.

On `Vcpu::Resume`:

```text
vttbr = arm64_vttbr(VMID, stage2_root)
EL2  switch_to_guest:  msr vttbr_el2, vttbr ; isb
eret guest
… intercept …
switch_to_host:        msr vttbr_el2, xzr  ; isb
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
| always | `AF` |

S1 uses `MAIR_EL1` AttrIndx. S2 encodes memory type **in the PTE**
(`S2 MemAttr[3:0]`). Combining S1+S2 follows ARM DDI 0487 D4.5.4:
device wins over normal; RO in either stage is RO.

```text
  MapContiguous  aligned 1 GiB / 2 MiB  →  L1/L2 BLOCK
  Unmap / Protect in the middle of a BLOCK
       SplitLargePage
          alloc next-level table
          fill 512 (or 64) entries pointing at the old PA
          replace BLOCK with TABLE
          then Unmap/Protect the slice
```

Block maps (1 GiB / 2 MiB) are used when the VMO is contiguous
(`MapContiguous` / `ZX_VM_FLAG_MAP_CONTIGUOUS`). `SplitLargePage`
breaks a block into the next level when a later Unmap/Protect hits
the middle of it.

`Query` walks the same tables and translates S2 attrs back to
`ARCH_MMU_FLAG_*` (`s2_pte_attr_to_mmu_flags`).

## Device memory

### `sos_config:setVmem`

Lua does not write page tables. `setVmem` only **appends a
`VmemSpec`**. `Guest::Init` later turns that list into physical VMOs
and `zx_vmar_map` into the guest VMAR, which **is** the stage-2 fill.
AHYP identity: the lua `base_addr` is both GPA and HPA.

#### Signature

LuaBridge binds `GuestConfig` as the global `sos_config`
(`ParseConfigFromLua`). Colon-call passes `self`.

```text
sos_config:setVmem(name, base_addr, size, policy, is_physmem)
```

| Arg | C++ | What it becomes |
| --- | --- | --- |
| `name` | `spec.name` | label. Mapping ignores it. DTB export needs `is_reservedmem` |
| `base_addr` | `hpa_base` **and** `gpa_base` | identity. Lua cannot say GPA ≠ HPA |
| `size` | `spec.size` | byte length; kernel `ROUNDUP_PAGE_SIZE` on the VMO |
| `policy` | `MemoryPolicy` | VMO type + S2 MemAttr (see table below) |
| `is_physmem` | `spec.is_physmem` | first `1` becomes VMM `PhysMem` (host VA over guest RAM) |

`setVmem` also sets `is_reservedmem = false`. No page-alignment
check here (JSON / CLI parsers do reject unaligned base/size).

```text
spec.name       = name
spec.hpa_base   = base_addr
spec.gpa_base   = spec.hpa_base      // forced identity
spec.size       = size
spec.policy     = (MemoryPolicy)policy
spec.is_physmem = is_physmem
vmem_spec_.push_back(spec)
```

| `MemoryPolicy` | VMO | Cache | Typical |
| --- | --- | --- | --- |
| `kDemand` (0) | anonymous `zx_vmo_create` | cached | unused on QEMU SOS; HPA is PMM, **not** identity |
| `kCached` (1) | `zx_vmo_create_physical` | `ZX_CACHE_POLICY_CACHED` | RAM, SOS IO window |
| `kDevice` (2) | physical | `UNCACHED_DEVICE` | true device MMIO on SoC |
| `kMpu` (3) | skipped in `Init` | — | TEE / SMPU; punched out later from DTB `reg` |

#### Call flow

```text
 Vmm::Initialize
    read_product_cfg("/system/data/product.lua")
       LuaBridge  product_config:setVmem / setDeviceTree / setProductSpec
    get_product_path  →  /system/data/<default_cfg>     // sos.lua
    read_guest_cfg
       GuestConfigParser::ParseConfigFromLua
          luaL_newstate + luaL_openlibs
          beginClass<GuestConfig>("sos_config")
             addFunction("setVmem", &GuestConfig::setVmem)
             addFunction("setVmemAuto", …)
          setGlobal(L, cfg, "sos_config")
          luaL_dofile(sos.lua)
             sos_config:setVmem("IO", 0, 0x40000000, 1, 0)
                GuestConfig::setVmem
                   VmemSpec  GPA=HPA=0  size=1GiB  kCached  is_physmem=0
                   vmem_spec_.push_back
    open guest_memory_allocator
       ioctl_get_system_memory / _with_path / by compatible
       append_vmem  name="ram"  GPA=HPA=resp.addr  kCached  is_physmem=1
    for vmem_auto:
       ioctl_get_reserved_memory(search_string = name)
       append_vmem  GPA=HPA=resp.addr  (lua base/size ignored)
    guest_.Init(cfg_.vmem())
       zx_guest_create_new  →  guest handle + guest VMAR  (TYPE_GUEST_PHYS)
       for each VmemSpec:
          kMpu        continue
          kDemand     zx_vmo_create(size)
          kCached /
          kDevice     zx_vmo_create_physical(root, hpa, size)
                      vmo.set_cache_policy(CACHED | UNCACHED_DEVICE)
          zx_vmar_map(guest_vmar,
                      SPECIFIC|CONTIGUOUS|RWX,
                      gpa_base, vmo, 0, size)
             VmAddressRegionDispatcher::Map
             ArmArchVmAspace::MapContiguous   ARCH_ASPACE_FLAG_GUEST
             S2 PTE  IPA=gpa_base → PA=hpa_base
          first is_physmem: PhysMem::Init(vmo, hpa)
             zx_vmar_map  VMM process TTBR0  (host poke of guest RAM)
       FXL_CHECK(phys_mem_.vmo())     // at least one is_physmem required
```

```text
 lua sos.lua                    EL0 VMM
    setVmem  ─────────────────►  GuestConfig.vmem_spec_[]
 allocator ram / vmem_auto  ──►  same vector, appended
 Guest::Init
    zx_guest_create_new  ─────►  empty S2 root (VTTBR.BADDR)
    physical VMO         ─────►  VmObjectPhysical [hpa, hpa+size)
    vmar_map SPECIFIC    ─────►  VmMapping at IPA = gpa
                                      │
                                      ▼
                               ArmArchVmAspace  S2 PTE
                                      │
                                      ▼
                               Vcpu::Resume  msr vttbr_el2
                               guest ldr/str  IPA  →  that PA
```

#### Principle

Crook AHYP is **identity stage-2**. The guest is given real host
physical ranges, not a shadow GPA. `setVmem` is how the VMM names
those ranges.

1. **Lua is a list, not a walk.** Each call is one window. Overlap
   fails later at `zx_vmar_map` (VMAR children do not overlap).
2. **GPA = HPA in the SOS lua path.** `sos_config:setVmem` copies
   `base_addr` into both fields. JSON `ParseVmemsConfig` and CLI
   `parse_vmem` *can* split GPA/HPA. `product_config:setVmem` is
   bound to `setProductVmem` and takes `(name, gpa, hpa, size,
   policy, is_physmem)` — GPA may differ. SOS guest lua cannot.
3. **Physical VMO names HPA that already exists.**
   `zx_vmo_create_physical` → `VmObjectPhysical::Create`. No PMM
   alloc. On AHYP the kernel **skips** `pmm_arena_intersects`, so
   leftover DRAM may be a physical VMO even though it sits in a PMM
   arena. Non-AHYP would `ZX_ERR_ACCESS_DENIED` for that.
4. **`ZX_VM_FLAG_SPECIFIC` is the IPA.** The guest VMAR base is 0,
   size 64 GiB. Mapping at `gpa_base` writes the S2 PTE at that IPA.
   `MAP_CONTIGUOUS` lets the arch code use 1 GiB / 2 MiB blocks
   (physical VMO is contiguous HPA).
5. **`is_physmem` is the VMM's window into guest RAM**, not a
   hardware bit. First spec with the flag is mapped into the VMM
   process (`PhysMem`). Kernel/DTB/bootloader poke
   `phys_mem_.as<T>(gpa - phys_base)` without a hypercall. Device
   windows must keep the flag 0 so `PhysMem` is DRAM, not MMIO.
6. **`kDemand` breaks identity.** Anonymous pages get whatever HPA
   PMM hands out; the guest still sees `gpa_base`. SOS RAM is
   therefore `kCached` physical, not demand-paged.
7. **Traps and `kMpu` punch the window after the map.** The 1 GiB
   SOS IO spec is the default; GICD/GICR/virtio `SetTrap` unmaps
   page-aligned holes. `kMpu` specs are skipped in `Init`, then
   `unmap_mpu_mem_region` looks up DTB reserved-memory `reg` and
   `UnmapPhysicalMemory` (host `PhysMem` + guest S2).

S2 attributes from policy (`mmu_flags_to_s2_pte_attr`):

| Policy | S2 MemAttr | S2AP | Typical XN |
| --- | --- | --- | --- |
| `kCached` / `kDemand` | `0xf` Normal WB, inner SH | RW (`PERM_WRITE`) | off (VMM maps RX) |
| `kDevice` | `0x1` Device-nGnRE | RW | on |

QEMU maps the **whole** 0–1 GiB IO window as `kCached`. That is a
blanket identity of virt MMIO (GIC, UART, …), not "this is RAM".
Real SoC device MMIO should be `kDevice` so CPU and DMA see
Device-nGnRE. GICD/GICR are trapped either way.

#### Parse (QEMU SOS)

```text
sos_config:setVmem("IO", 0x00000000, 0x40000000, 1, 0)
```

| Field | Value | Parse |
| --- | --- | --- |
| `name` | `"IO"` | label only. `AddVmemDeviceTreeNodes` skips it (`is_reservedmem == 0`) |
| `base_addr` | `0` | GPA = HPA = 0. QEMU virt peripherals start here (GICD `0x8000000`) |
| `size` | `0x40000000` | 1 GiB. Covers GIC, UART, virtio MMIO, … |
| `policy` | `1` | `kCached` → physical VMO, Normal WB S2 |
| `is_physmem` | `0` | not `PhysMem`. RAM is the allocator `"ram"` spec appended next |

After lua, `Vmm::Initialize` still appends:

```text
 "ram"   GPA=HPA=leftover DRAM    kCached    is_physmem=1
```

`Guest::Init` therefore maps **two** windows on QEMU SOS: IO at 0
(1 GiB), then DRAM at whatever ATF / `guest_memory_allocator`
returned. `phys_base()` / `phys_size()` walk `vmem_spec_` for the
first `is_physmem` (the ram spec). Bootloader DTB lives in that
window (`dtb.base - phys_base` as a `PhysMem` offset).

tbox (`tbox.lua`) does not call `setVmem`. It calls
`setVmemAuto("vmlog_sink_tbox", "", 0, 0, 1, 0)`: queued on
`vmem_auto_spec_`, resolved by `ioctl_get_reserved_memory` using
`name` as the search string. Lua `base_addr`/`size` are placeholders;
the ioctl fills GPA/HPA/size. Policy 1 = cached, `is_physmem` 0
(tbox RAM still comes from the allocator `"ram"`).

#### Other front doors

Same `VmemSpec`, different parsers:

| Front door | GPA vs HPA | Align check | Notes |
| --- | --- | --- | --- |
| lua `sos_config:setVmem` | forced equal | no | SOS / tbox guest lua |
| lua `product_config:setVmem` | separate args | no | product.lua; C++ `setProductVmem` |
| lua `setVmemAuto` | ioctl result | no | reserved-memory lookup by `name` |
| JSON `ParseVmemsConfig` | `"name,gpa,hpa,size,policy,is_physmem"` | yes | 6 fields; GPA may differ |
| CLI `--vmem` `parse_vmem` | `"gpa,hpa,size,policy,is_physmem"` | yes | no name |
| UOS `vmems_build_from` | proto fields | — | `main_uos.cc`; can set `is_reservedmem` |

`Init` is the same. Lua is the SOS path.

#### After the map

```text
 Guest::Init  S2 identity PTEs
    │
    ├─ CreateMapping(GICD/GICR/virtio/vSMMU)
    │     zx_guest_set_trap  UnmapRange     hole in the 1 GiB IO window
    ├─ unmap_mpu_mem_region
    │     kMpu name → DTB reserved-memory  UnmapPhysicalMemory
    └─ Vcpu::Resume  VTTBR  guest load/store
```

## Allocator

`guest_memory_allocator` is a platform-bus device
(`/dev/sys/platform/00:00:f/guest_memory_allocator`). It does **not**
allocate. It **reads the host FDT** that ATF left in Crook's boot
data and returns `{addr, size}` for the VMM to wrap in a `VmemSpec`.

```text
  ATF  /memory  and  /reserved-memory  in the host DTB
         │
         ▼  pbus_get_fdt
  Allocator  (kernel driver, DdkIoctl)
         │
         │  Vmm::Initialize  open /dev/…/guest_memory_allocator
         ▼
  ioctl_get_system_memory        →  /memory reg          "ram"
  ioctl_get_reserved_memory      →  mblock-*-<name>      setVmemAuto
  ioctl_get_guest_memory         →  compatible
  ioctl_get_guest_memory_with_path
         │
         ▼
  VmemSpec  GPA=HPA=resp.addr  kCached  is_physmem=1 for "ram"
  Guest::Init  physical VMO + vmar_map
```

| ioctl | FDT lookup | Used for |
| --- | --- | --- |
| `IOCTL_GET_SYSTEM_MEMORY` | `/memory` `reg` | leftover DRAM → `"ram"` `is_physmem=1` |
| `IOCTL_GET_RESERVED_MEMORY` | `/reserved-memory` node whose name contains `search_string` (`mblock-N-<name>` or `mblock-<name>`) | `setVmemAuto` |
| `IOCTL_GET_GUEST_MEMORY` | node `compatible` | named guest RAM |
| `IOCTL_GET_GUEST_MEMORY_WITH_PATH` | node path | same, by path |
| `IOCTL_GET_GUEST_DTB_MEMORY` | returns a VMO handle of the DTB | VMM parses DTB |
| `IOCTL_QUERY_PROP_WITH_PATH` | u64 property | misc |

QEMU `/memory` is the DRAM ATF did not keep. Mapping it as
`kCached` physical identity is how SOS sees RAM at the same PA the
host PMM also tracks — allowed only because AHYP skips
`pmm_arena_intersects`.

## MapDevice

Lua `setVmem` is the **static** fill. GPU SWA needs to **toggle**
Mali MMIO in a guest's S2 at grant / stop time without tearing down
the `VmMapping`. That is `GuestPhysicalAddressSpace::MapDevice` /
`UnMapDevice` / `ProtectDevice`.

```text
  setVmem (once)              MapDevice (grant / stop)
  create VMO + VmMapping      mapping already exists
  MapContiguous whole window  MapContiguous / Unmap that window
  IPA reserved forever        PTE appears / disappears
                              PIN regions never Unmap
```

The `VmMapping` is created once (bootstrap `set_vmo` / region
list). Grant only writes PTEs:

```text
MapDevice(mapping, paddr)
  arch_aspace.MapContiguous(mapping->base(), paddr,
                            mapping->size()/PAGE_SIZE,
                            mapping->arch_mmu_flags())

UnMapDevice(mapping)
  arch_aspace.Unmap(base, size/PAGE_SIZE)

ProtectDevice(mapping, enable)
  flags = mapping->arch_mmu_flags()
  if !enable: flags &= ~PERM_WRITE
  arch_aspace.Protect(base, size/PAGE_SIZE, flags)
```

`GpuScheduler::MapGpu` / `UnmapGpu` walk `VmInfo.mapping_regions`.
Already-mapped regions are skipped. `ARB_GPU_REGION_PIN` regions
are never unmapped (shared command buffers). See [`gpu.md`](gpu.md).

This is the same S2 table `setVmem` filled. Grant does not retune
STE, so **both** CPU S2 and SMMU S2 see the new PTEs (SMMU still
needs a TLBI; the GPU path issues that with the CPU `tlbi ipas2*`).

## Traps vs maps

A trap **removes** the S2 mapping so the next access aborts to EL2:

```text
  IPA window after setVmem          after SetTrap(GICD)
  [======== valid S2 PTE ========]  [===] hole [========]
                                      ▲
                                      │  UnmapRange page-aligned
                                      │  TrapMap WAVL insert (addr, len, kind, port, key)

  guest str
     S2 valid   →  PA
     S2 invalid + Trap MEM   →  packet, VCPU thread emulates
     S2 invalid + Trap BELL  →  port packet, next_pc in EL2
     S2 invalid, no trap     →  PageFault fill or inject abort
```

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

`MMIO_SYNC` = MEM. `MMIO_BELL` = BELL. BELL rejects reads
(`ZX_ERR_NOT_SUPPORTED`). MEM requires `data_abort.valid` (ISV);
otherwise `ZX_ERR_IO_DATA_INTEGRITY`. MEM returns `ZX_ERR_NEXT` so
the VCPU thread emulates and writes GPRs; BELL queues and
`next_pc`s in EL2.

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
Poster: **Diagrams** 6.
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
| Host fallback | `DummyIommu` | no hardware; `Map` returns the VMO's HPA as the "IOVA" |

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

### Dummy IOMMU

No SMMU. `DummyIommu::Map` looks up the VMO page and returns that
**HPA as the IOVA**, length one page (paged) or the whole range
(physical). Host drivers that DMA through a BTI on QEMU take this
path. Guest passthrough DMA does **not**.

```text
  zx_bti_pin(dummy_iommu, vmo, off, len)
       DummyIommu::Map
          vmo->Lookup(off) → pa
          *vaddr = pa          // IOVA := HPA
          paged:     *mapped_len = PAGE_SIZE
          physical:  *mapped_len = ROUNDUP(size)
       PMT holds the pin

  Device DMA of that "IOVA" is a physical access.
  No STE, no CMDQ, no IOTLB.
```

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

## Cache

Two problems: **what type the PTE claims**, and **keeping the
caches coherent** when that type changes or when software writes
through a different alias.

```text
  same PA
     host physmap     Normal WB   (TTBR1)
     guest S2 kCached Normal WB   OK  (same type)
     guest S2 kDevice nGnRE       BUG (alias)

  S1 AttrIndx  ×  S2 MemAttr   (ARM D4.5.4)
     Device anywhere  →  Device
     else             →  Normal, inner-shareable as programmed
```

**Type.** Host physmap is always Normal WB. A guest S2 `kCached`
window of the same PA is fine (same type). A guest S2 `kDevice`
window of a PA that is also in the physmap is a programming error
(aliasing). lua `kDevice` vs `kCached` is how the VMM picks one.

**Maintenance.** `arch_clean_cache_range` / `arch_invalidate_*`
(`cache-ops.S`, `dc cvac` / `ivac` / `civac`). VMO
`op_range(ZX_VMO_OP_CACHE_*)` walks committed pages via physmap and
runs those. After zeroing a non-cached paged page, ARM64 does
`arch_clean_invalidate_cache_range` so stale WB lines cannot
resurrect zeros as dirty.

```text
  zx_vmo_op_range(CACHE_CLEAN / INVALIDATE / CLEAN_INVALIDATE / SYNC)
       VmObjectPaged::CacheOp
          for each committed page:
             va = paddr_to_physmap(pa)
             dc cvac / ivac / civac
  ContextSwitch  if AUTO: PoisonCache  (Nebula extra)
```

`ArmArchVmAspace::PoisonCache` is a Nebula extra: if
`ARCH_CACHE_PROT_FLAG_AUTO` is set, context-switch poisons the
outgoing user aspace so stale lines cannot be speculated.

S2 MemAttr and S1 AttrIndx combining: if either stage says Device,
the result is Device. Inner-shareable is what Crook programs for
Normal so all CPUs snoop.

## ZX_VM flags

`zx_vmar_map` / `zx_vmar_allocate`:

```text
  zx_vmar_map(vmar, flags, offset, vmo, vmo_off, len)
       │
       ├─ SPECIFIC            mapping at vmar.base + offset   (guest IPA)
       ├─ MAP_RANGE           MapRange(..., commit=true)
       ├─ MAP_CONTIGUOUS      MapContiguous, 1G/2M blocks
       └─ PERM_R/W/X          arch_mmu_flags_ → S1 AP or S2 S2AP/XN

  zx_vmar_allocate            child VMAR
       CAN_MAP_*              what Map inside may ask for
       COMPACT                pack near existing children (ASLR still on)
```

| Flag | Bit | Meaning |
| --- | --- | --- |
| `PERM_READ` / `WRITE` / `EXECUTE` | 0/1/2 | S1 AP / UXN, or S2 S2AP / XN |
| `COMPACT` | 3 | pack child VMARs |
| `SPECIFIC` | 4 | map at this offset (guest IPA) |
| `SPECIFIC_OVERWRITE` | 5 | replace an existing mapping |
| `CAN_MAP_{SPECIFIC,READ,WRITE,EXECUTE}` | 6–9 | VMAR create: what children may do |
| `MAP_RANGE` | 10 | commit / fault-in the whole range now |
| `MAP_CONTIGUOUS` | 11 | `MapContiguous` → block PTEs |

Guest `Init` always uses `PERM_RWX | SPECIFIC | MAP_CONTIGUOUS`.
That is why SOS identity RAM is RWX at stage-2 (no W^X). Without
`SPECIFIC` the kernel would pick a free IPA and identity would
break. Without `MAP_CONTIGUOUS` the arch code would emit 4 KiB
leaves instead of 1 GiB / 2 MiB blocks.

## Registers

Who programs what. EL2 regs are HVC-only from EL1.

```text
  EL1 host
     TTBR1 / TCR.T1 / MAIR     kernel   (never swapped)
     TTBR0 / TCR.EPD0          user     (ContextSwitch)
  EL2
     TTBR0_EL2 / TCR_EL2       EL2 itself   (el2_hvc_on)
     VTTBR / VTCR / HCR.VM     guest S2     (Resume / switch_to_*)
     FAR / HPFAR / ESR         abort info   (el2.S saves into GuestState)
  SMMU
     STRTAB / CMDQ / EVTQ      host MMIO
     STE CFG / S2TTB / S2VMID  programmed by ArmSmmuv3
```

| Register | Role |
| --- | --- |
| `TTBR1_EL1` | Host kernel + physmap. Never swapped. |
| `TTBR0_EL1` | Host user. ASID[63:48]. `TCR.EPD0` disables it. |
| `TCR_EL1` | T0SZ/T1SZ, TG, IRGN/ORGN, EPD0, AS. `MMU_TCR_FLAGS_{KERNEL,USER,IDENT}` |
| `MAIR_EL1` | Attr0 nGnRnE, Attr1 nGnRE, Attr2 Normal WB, Attr3 Normal NC (write-combine) |
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
| `ESR_EL2` | EC = data/instruction abort, ISS = S1 vs S2, WnR, ISV |
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

```text
  CPU TLB entry tagged by
     S1 user     ASID   (TTBR0[63:48])
     S1 kernel   global ASID 0xffff
     S2          VMID   (VTTBR[63:48])   ipas2* needs VTTBR loaded
     combined    VMID + guest ASID       vmalls12e1is drops both

  Map / Unmap / Protect
     ConsistencyManager queues VA
        < threshold  →  tlbi vae1is / ipas2e1is per page
        ≥ threshold  →  tlbi aside1is / FlushAsid  (not global ASID)
     dtor: DSB, issue, pmm_free dropped table pages

  SMMU IOTLB tagged by SID / ASID / VMID   — CMDQ, not tlbi
```

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
TTBR1 uses the global ASID (`0xffff`).

`ConsistencyManager` (stack object inside Map/Unmap/Protect) queues
per-VA invalidates. Past `kMaxPendingTlbs` (16) on a non-global
aspace it switches to one `FlushAsid` (`tlbi aside1is` or guest
`ipas2` path's VMID flush). Destructor always DSB-then-issue, then
`pmm_free`s page-table pages collected in `to_free_`.

SMMU-v3 has its own STE/CD/IOTLB, tagged by SID / ASID / VMID.
Invalidation is **CMDQ**, not `tlbi`:

| Guest CMD | Host |
| --- | --- |
| `CFGI_STE` / `CFGI_STE_RANGE` | `config_ste` → nest/unnest + `smmu_sync_ste` |
| `CFGI_CD` / `TLBI_NH_*` / `ATC_INV` | `zx_iommu_hwpt_invalidate` |
| `CMD_SYNC` | vSMMU IRQ |
| (S2 PTE change) | host `TLBI_S2_IPA` / `TLBI_S12_VMALL` |

## Protection

Four gates, independently. A guest that passes S2 can still die on
SMPU.

```text
  syscall ZX_RIGHT_MAP / WRITE
       │
       ▼  VMAR CAN_MAP_* + mapping PERM_*
       │
       ▼  S1 AP / UXN     (host user)
          or S2 S2AP / XN (guest)
       │
       ▼  SMPU  PA + master   (interconnect, TEE)
       │
       ▼  DRAM / device
```

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
- GPU ungowned MMIO: `UnMapDevice` S2 hole; PIN regions stay.

**Rights the VMM cannot bypass.** `zx_vmar_map` into the guest VMAR
still goes through `ArmArchVmAspace::Map` → S2 PTE. A guest cannot
map host kernel PA unless the VMM created a VMO for it.

**W^X at stage-2** is whatever the VMM asked for. SOS identity RAM is
RWX in the current `Guest::Init` flags.

**Cache aliasing.** Same PA in host physmap (Normal) and guest S2
(Device) is a programming error; lua `kDevice` vs `kCached` is how
the VMM picks one.

## Flows

Timing versions of **Diagrams** 4–13. Same events, more timestamps.

### Host boot (MMU)

```text
 phys  _start
   arm64_elX_to_el1     VTTBR=0
   boot_alloc_init
   TTBR1 = kernel table, TTBR0 = 1 GiB identity
   SCTLR.M=1
   br KERNEL_BASE
   TCR.EPD0             drop trampoline
   tlbi vmalle1
   vm_init_preheap      WIRED bootalloc, zero_page
   heap_init            HEAP pages
   vm_init              lock kernel RX/RO/RW
```

### Guest create → first `eret`

```text
 zx_guest_create_new
   alloc VMID, TYPE_GUEST_PHYS aspace, empty S2 root
   arch_set_asid(vmid)
 lua VmemSpec[] + allocator ram
   physical VMO + vmar_map SPECIFIC   →  S2 identity PTEs
 CreateMapping(GICD/GICR/virtio)
   zx_guest_set_trap  UnmapRange      →  holes
 Vcpu::Resume
   vttbr = VMID<<48 | s2_root
   HCR.VM|PTW
   msr vttbr_el2
   eret guest EL1h
```

### `setVmem` → S2 PTE

```text
 sos.lua  sos_config:setVmem("IO", 0, 1GiB, kCached, 0)
    LuaBridge  GuestConfig::setVmem
    VmemSpec  GPA=HPA=0  is_physmem=0
 allocator  append "ram"  is_physmem=1
 Guest::Init
    zx_vmo_create_physical(0, 1GiB)     IO window
    zx_vmar_map SPECIFIC gpa=0          S2 [0, 1GiB)
    zx_vmo_create_physical(dram, size)  leftover DRAM
    zx_vmar_map SPECIFIC gpa=dram       S2 identity RAM
    PhysMem  host map of the ram VMO
 trap GICD  UnmapRange                  hole at 0x8000000
 guest ldr UART (in IO, no trap)        S2 cached PTE → PA
 guest str GICD                         S2 invalid → EL2 → VMM
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

### GPU grant / stop

```text
 GRANT
   MapGpu  →  MapDevice  →  MapContiguous  S2 PTE
   Mali IRQs → owner VCPU
 STOP
   UnmapGpu → UnMapDevice → Unmap  S2 hole  (PIN kept)
```

### Host user fault

```text
 EL0 load unmapped VA
   EL1 abort  vmm_page_fault_handler
   VmMapping::PageFault
   GetPageLocked  (alloc or COW)
   Map PTE without WRITE if it was a read
   retry
```

### Page: DRAM → `vm_page_t` → VMO → PTE

```text
 pmm_add_arena
    page_array_[i] for each 4 KiB, FREE except WIRED metadata
 zx_vmo_create(size)                  empty page_list_
 zx_vmar_map(..., vmo, vmo_off, len)
    CreateVmMapping + ActivateLocked  mapping_list_ += mapping
 first store at va
    PageFault → GetPageLocked
       miss → pmm_alloc_page → OBJECT → page_list_.AddPage
    MapContiguous(va, pa)
 destroy mapping
    Unmap PTEs, RemoveMappingLocked
 destroy VMO
    FreeAllPages → pmm_free → FREE
```

## Terminology

| Term | Meaning here |
| --- | --- |
| Physmap | `KERNEL_ASPACE_BASE + PA`. Host kernel's linear map of DRAM. |
| IPA / GPA | Guest physical address. Stage-2 input. Identity for SOS/tbox. |
| Stage-1 | EL1 `TTBR0/1`. Host user/kernel, or the **guest's** own tables. |
| Stage-2 | EL2 `VTTBR`. IPA → PA. Crook-owned. |
| VMID | 8 bits in `VTTBR[63:48]`. Tags S2 TLB. Stored in `asid_` of the guest aspace. Kernel: SOS=1, ALPS=2, TBOX=3. |
| ASID | Host user TTBR0, 16-bit allocator. Not used for guests. |
| `VmemSpec` | Lua mapping: name, GPA/HPA, size, `MemoryPolicy`. |
| `setVmem` | LuaBridge → `GuestConfig::setVmem`. Forces GPA = HPA. |
| `setProductVmem` | Same list, GPA and HPA may differ. |
| `is_physmem` | First spec with this bit becomes VMM `PhysMem`. |
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
| `vm_page_t` | 32 B metadata, one per DRAM page in `PmmArena.page_array_`. |
| `page_array_` | Linear `vm_page_t[N]` for an arena. Index = `(PA − base) / 4KiB`. |
| `VmPageList` | Sparse WAVL of 16-page nodes. Paged VMO's logical index. |
| `mapping_list_` | Live `VmMapping*` on a VMO. `map_count` on the page is unused. |
| `zero_page` | Shared RO zero page for demand-paged reads. |
| `MapDevice` | Write S2 PTEs for an existing mapping (GPU grant). |
| PMM | Physical memory manager. Arenas of DRAM, `vm_page_t`. |
| bootalloc | Bump allocator before PMM is up. |
| `zero_page` | Shared RO zero page for demand-paged reads. |
| Dummy IOMMU | No SMMU; Map returns HPA as IOVA. |

## Code

| Step | File |
| --- | --- |
| MMU on, TTBR1/0, trampoline | `zircon/kernel/arch/arm64/start.S` |
| Early ident / physmap maps | `zircon/kernel/arch/arm64/boot-mmu.cpp` |
| Physmap | `zircon/kernel/vm/include/vm/physmap.h` |
| Layout constants | `zircon/kernel/arch/arm64/rules.mk` |
| Kernel lock-down | `zircon/kernel/vm/vm.cpp` `vm_init` / `vm_init_preheap` |
| Bootalloc | `zircon/kernel/vm/bootalloc.cpp` |
| Boot reserve | `zircon/kernel/vm/bootreserve.cpp` |
| PMM / arenas | `zircon/kernel/vm/pmm.cpp`, `pmm_arena.cpp` |
| Arena `page_array_` / free list | `zircon/kernel/vm/pmm_arena.cpp` `Init` / `AllocPage` |
| `vm_page_t` | `zircon/kernel/vm/include/vm/page.h` |
| `VmPageList` WAVL fan-out 16 | `zircon/kernel/vm/include/vm/vm_page_list.h`, `vm_page_list.cpp` |
| `mapping_list_` | `zircon/kernel/vm/vm_object.cpp` `AddMappingLocked` |
| `MapRange` / coalescer | `zircon/kernel/vm/vm_mapping.cpp` |
| `GetPageLocked` / COW / pin | `zircon/kernel/vm/vm_object_paged.cpp` |
| Physical `GetPageLocked` | `zircon/kernel/vm/vm_object_physical.cpp` |
| Heap | `zircon/kernel/lib/heap/cmpctmalloc/cmpctmalloc.c` |
| Aspace types | `zircon/kernel/vm/vm_aspace.cpp` |
| VMAR / mapping | `zircon/kernel/vm/vm_address_region.cpp`, `vm_mapping.cpp` |
| VMO paged / COW | `zircon/kernel/vm/vm_object_paged.cpp` |
| VMO physical | `zircon/kernel/vm/vm_object_physical.cpp` |
| Host page fault | `zircon/kernel/vm/vmm.cpp` `vmm_page_fault_handler` |
| S1/S2 PTE attrs, TLB, ASID | `zircon/kernel/arch/arm64/mmu.cpp` |
| `VTCR` / `VTTBR` helpers | `zircon/kernel/arch/arm64/include/arch/arm64/mmu.h`, `arch/aspace.h` |
| EL2 VTTBR / TLBI | `zircon/kernel/arch/arm64/hypervisor/el2.S` |
| Guest aspace | `zircon/kernel/lib/hypervisor/guest_physical_address_space.cpp` |
| `MapDevice` / `UnMapDevice` | same |
| `zx_guest_create_new` / trap | `zircon/kernel/syscalls/hypervisor.cpp` |
| Physical VMO AHYP skip | `zircon/kernel/syscalls/ddk.cpp` `sys_vmo_create_physical` |
| `SetTrap` unmap | `zircon/kernel/arch/arm64/hypervisor/guest.cpp` |
| Data abort | `zircon/kernel/arch/arm64/hypervisor/vmexit.cpp` `handle_data_abort` |
| Guest fault fill | `zircon/kernel/lib/hypervisor/fault.cpp` |
| VMID enum | `zircon/kernel/include/kernel/sched.h`, `arch/hypervisor.h` |
| Lua `setVmem` / `setVmemAuto` | `garnet/bin/guest/guest_config.h` |
| LuaBridge `sos_config` | `garnet/bin/guest/guest_config.cc` `ParseConfigFromLua` |
| JSON / CLI vmem parse | same, `ParseVmemsConfig` / `parse_vmem` |
| Allocator ram + auto | `garnet/bin/guest/vmm.cc` `Vmm::Initialize` |
| Allocator driver | `garnet/drivers/misc/guest_memory_allocator/allocator.cc` |
| Map VmemSpec | `garnet/lib/machina/guest.cc` `Guest::Init` |
| `UnmapPhysicalMemory` | same |
| Trap install | `garnet/lib/machina/io.cc` |
| SOS IO window | `garnet/bin/guest/sos/qemu/sos.lua` |
| Host SMMU / STE | `zircon/kernel/dev/iommu/arm_smmuv3/smmuv3.cpp` |
| Dummy IOMMU | `zircon/kernel/dev/iommu/dummy/dummy_iommu.cpp` |
| `smmu_attach_dev` / nested STE | `smmuv3.cpp` `smmu_install_nested_hwpt` |
| vSMMU | `garnet/lib/machina/arch/arm64/smmu_v3.cc` |
| Lua `setVsmmu` | `garnet/bin/guest/guest_config.h` |
| SOS vSMMU create | `garnet/bin/guest/vmm.cc` |
| SMPU unmap | `garnet/bin/guest/vmm.cc` `unmap_mpu_mem_region` |
| GPU MapGpu | `zircon/kernel/arch/arm64/hypervisor/gpu_sched/gpu_sched_vm_resources.cpp` |
| Cache ops | `zircon/kernel/arch/arm64/cache-ops.S` |
| `VmAspace` / VMAR / mapping | `zircon/kernel/vm/include/vm/vm_aspace.h`, `vm_address_region.h` |
| VMO types | `vm_object.h`, `vm_object_paged.h`, `vm_object_physical.h` |
| PMM / `vm_page_t` | `zircon/kernel/vm/include/vm/pmm.h`, `page.h` |
| Dispatchers | `zircon/kernel/object/include/object/vm_*_dispatcher.h` |
| BTI / PMT / IOMMU | `bus_transaction_initiator_dispatcher.h`, `iommu_dispatcher.h` |
| `PhysMem` | `garnet/lib/machina/phys_mem.h` |
| `ZX_VM_FLAG_*` / cache policy | `zircon/system/public/zircon/types.h` |

---

Everyday's a Hustle.
