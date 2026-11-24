# Shared memory, cache, barriers

**After this:** you can classify **memory stomping** as IPA overlap,
cache alias, or a missing barrier. **First read:** Principle +
diagrams 1–4 + the usage table. **Next:** [`memory.md`](memory.md)
Cache / SMPU.

**Memory stomping** is the English used here for 踩内存: an
unintended overwrite or incoherent view of a physical address.
Not a heap `memset` past the end of a buffer (that is heap
stomping / overflow). Subtypes in this tree:

| English | What happened |
| --- | --- |
| IPA overlap | two `SPECIFIC` maps in one guest VMAR |
| cache alias | same PA, different MemAttr (WB vs Device) |
| missing order | payload not visible before the doorbell |

Related words: **trampling** / **clobber** (same informal sense);
**overlapping mapping**; **cache aliasing**. Use the subtype in
logs; use “memory stomping” when the class is not yet known.

This tree’s guests and Savior share DRAM **by mapping the same
HPA** into several aspaces. ARM does not snoop Device vs Normal
aliases. A second `ZX_VM_FLAG_SPECIFIC` on an already-mapped IPA
is `ZX_ERR_NO_MEMORY` (-4), not a silent overwrite.

Read [`memory.md`](memory.md) for S2 MemAttr, [`vhm.md`](vhm.md)
ioreq, [`scheduler.md`](scheduler.md) occupancy,
[`guest.md`](guest.md) cvm-lock, [`gpu.md`](gpu.md) GPUEB,
[`logging.md`](logging.md) rings.

## Principle

1. **Same PA ⇒ same MemAttr.** Host physmap is Normal WB Inner
   Shareable. Guest S2 `kCached` of that PA is OK. Guest S2
   `kDevice` (nGnRE) of a PA that is also WB in physmap or
   another guest is an **alias**: stores can vanish or appear
   twice. lua `kDevice` vs `kCached` is that choice
   (`guest.cc` `cache_policy()`).

2. **IPA overlap is a map fail, not a merge.** `Guest::Init`
   `zx_vmar_map(SPECIFIC)` into the guest VMAR. WAVL children
   do not overlap. SOS `setVmemAuto("gz_nebula")` on top of
   identity `ram` returns -4 and SOS will not relaunch.

3. **Shared DRAM is three contracts stacked:** (a) cache type
   match on every alias, (b) a **barrier** so the producer’s
   payload is visible before the consumer is told, (c) a
   **doorbell** (IRQ / `processed` / virtio used.idx). Missing
   (b) looks like a stale occupancy row or a lost ioreq.

4. **Doorbell is not a cache flush.** Inner Shareable WB is
   coherent across CPUs. The barrier is for **order** (store
   buffer vs IRQ inject), not for flushing DRAM.

5. **Last writer wins** on a single slot (ramconsole snapshot
   4 KiB tail; occupancy row). That is not aliasing; it is
   protocol.

## Framework

```text
 same HPA
    host physmap     TTBR1  Normal WB ISH     always
    SOS S2           VTTBR  kCached / kDevice lua
    UOS S2           VTTBR  extra SPECIFIC map if outside UOS RAM
    tbox S2          VTTBR
    host EL0 Vmm     zx_vmar_map root  CACHED or UNCACHED_DEVICE
    guest Linux      ioremap_cache / ioremap  must match S2

 S1 AttrIndx × S2 MemAttr  (ARM D4.5.4)
    Device anywhere → Device
    else            → Normal, Inner Shareable (Savior CACHED)
```

`ARCH_MMU_FLAG_CACHED` → `MMU_PTE_ATTR_NORMAL_MEMORY |
SH_INNER_SHAREABLE` (`mmu.cpp`). UNCACHED → strongly ordered.
UNCACHED_DEVICE → Device-nGnRE.

**S1 × S2 combine** (ARM D4.5.4). Final type is the **worse** of
the two. Device anywhere wins.

| Guest S1 | Savior S2 | Result |
| --- | --- | --- |
| Normal WB | Normal WB ISH | Normal WB ISH |
| Normal WB | Device nGnRE | **Device** |
| Device | Normal WB | **Device** |
| `ioremap` (Device) | occupancy CACHED | **Device** — alias vs host WB |
| `ioremap_cache` | occupancy CACHED | WB ISH — OK |

Hint peek maps guest DRAM **CACHED RO** into the host kernel
(`SCHED_PARAM_TYPE_DRAM_INFO`). Same type as guest RAM. Peek
runs on the VCPU thread after a vmexit, so it sees that guest’s
stores; it does not DMA.

## Diagrams

### 1. Three kinds of memory stomping

```text
 A. IPA overlap          two SPECIFIC maps, same guest VMAR
    SOS ram  0x80000000 + 0x780000000
    gz_nebula 0x93400000 size 0x6800000   ⊂ ram
    setVmemAuto("gz_nebula") on SOS → ZX_ERR_NO_MEMORY -4

 B. Cache alias          same PA, different MemAttr
    physmap WB  +  guest kDevice nGnRE
    occupancy CACHED  +  accidental kDevice window
    GPU MMIO Device  +  leftover identity RAM  (must unmap on grant)

 C. Missing order        payload store after doorbell, or no release
    occupancy write, then IRQ, no dmb in update_vm_policy
      (steady `el2_hvc_resume` has no `dsb ish`)
    ioreq COMPLETE without smp_store_release
    virtio used.idx++ without dmb ishst
```

### 2. Shared-page recipe (this tree)

```text
 producer
    1. write payload  (Normal WB ISH)
    2. barrier        SEQ_CST / dmb ishst / stlxr / smp_wmb
    3. publish        processed= / used.idx++ / IRQ inject
 consumer
    1. see doorbell
    2. barrier        SEQ_CST / dmb ishld / ldar / smp_rmb
    3. read payload
```

Skip 2 and you debug “IRQ fired, occupancy still RUN”.

### 3. Who maps occupancy

```text
 reserved-memory  (DTB, same HPA for all VMs)
    SOS  already inside identity ram   InitSched skips extra S2 map
    UOS  SPECIFIC map of sched_vmo     CACHED
    tbox same as UOS if not in its RAM
 host  zx_vmar_map root  CACHED        sched_shm_base
 guest ioremap_cache                   nebula_sched_probe
```

All four **must** stay CACHED. Mixing `ioremap` (Device) on the
guest with host CACHED is alias B.

### 4. GPU grant vs identity

Mali MMIO is Device. Product `setVmem` may have put that PA in
a `kDevice` window; grant `MapDevice` uses the mapping’s
`arch_mmu_flags()` (Device). Ungrant `UnMapDevice` punches S2.
If identity RAM still covered the GPU PA, the guest would see
WB DRAM where Mali lives — that is why grant/unmap is exclusive
and STE is **not** retuned on ungrant (boot-time SMMU).

## Usage in this tree

| Page | Cache | Barrier | Publish |
| --- | --- | --- | --- |
| occupancy / RT / util | CACHED ISH (host + `ioremap_cache`) | **none** in `update_vm_policy`; bound VCPU IPI does `smp_mb` before `ICC_SGI1R` | VIRTUAL `sched_irq` |
| ioreq 4 KiB | CACHED | `ATOMIC_SEQ_CST` + Linux `smp_store_release(COMPLETE)` | kick SPI + `processed` |
| virtio rings | guest RAM = CACHED | `dmb ishld` before avail; `dmb ishst` before `used.idx++` | VIRTUAL IRQ |
| cvm-lock | CACHED VMO | `ldar` / `ldaxr`+`stlxr` acq/rel | VIRTUAL `cvm_lock_irq` on destroy |
| fwlog `log_rb` | CACHED | `smp_wmb` after `alloc+=`, before `put+=` | consumer polls `put` |
| ramconsole | CACHED ring | spinlock_irqsave only | kedump copies raw window |
| snapshot tail | same `gz_nebula` | memcpy, last RESET wins | not a doorbell |
| GPUEB magic | CACHED VMO | write 8 B @ `0x3FC0` | ALPS first ACTIVE |
| SPM 0x1c000000 | UNCACHED_DEVICE | MMIO | tbox s2idle |
| shared/audio IRQ pages | UNCACHED_DEVICE | MMIO | — |
| lk2 image | CACHED then `ZX_VMO_OP_CACHE_CLEAN` | cache op | `guest_ip_` |
| DTB in guest RAM | CACHED | `zx_cache_flush(DATA)` after fdt_pack | guest walks DTB |
| hint peek (guest RAM) | CACHED RO kernel map | vmexit on that VCPU | `AddSchedHint` loads |
| FF-A TX/RX + SHARE | guest pages CACHED; host copies to `ffa_tx` | `ffa_tx_buffer_lock` then SMC | EL3 / SP |
| rproc rings | reserved-mem, typically CACHED | virtio dmb on those rings | `INJECT_HW_IRQ` |
| GzFS vsock | virtio vsock = CACHED RAM | virtio dmb | port 5022 |
| ipc-mbox TX/RX | CACHED 4+4 KiB | **no dmb in C**; guest `smc` then host IRQ | `HC_IPC_SET_*` |
| GPU cmdbuf IPA `0x88000b000` | guest map default CACHED; kernel `SetVmo` **UNCACHED** | none | debug stamps |

`Guest::Init`: `kDevice` → `ZX_CACHE_POLICY_UNCACHED_DEVICE`,
else `CACHED`. Host physmap stays WB; do not `kDevice` a RAM PA.

**GPU cmdbuf (verified).** `zx::vmo::create(4K)` (paged),
`SPECIFIC` at `0x88000b000` into the **guest** VMAR, then
`SetMappingCachePolicy(UNCACHED)` + kernel `MapObjectInternal`
UNCACHED. IPA sits inside SOS identity `ram` (`0x80000000` +
`0x780000000`). If that IPA is still mapped, SPECIFIC is overlap
**A** (`-4`). If create succeeds, that 4K was a hole. Kernel
UNCACHED vs guest CACHED on the **same VMO** is alias **B**.

## Barriers (what the code actually emits)

**Virtio** (`virtio_queue.cc`) — Virtio 1.0 §2.4:

```text
 NextAvail   dmb ishld     then read avail.ring[i]
             dmb ish       after writing avail_event
 Return      write used.elem
             dmb ishst     then used.idx++
             dmb ish       before comparing used_event
```

`ish` = Inner Shareable. Guest and Vmm are different CPUs;
without `ishst` the guest can see `idx++` before `id/len`.

**ioreq** (`acrn/atomic.h` + `ioreq.c`):

```text
 UOS  ATOMIC_STORE PENDING     __ATOMIC_SEQ_CST
 SOS  emulate BAR
      smp_store_release COMPLETE
 SOS  HC_NOTIFY
 SOS  ATOMIC_LOAD COMPLETE then STORE FREE   SEQ_CST
```

SEQ_CST is stronger than needed; the Linux `store_release` is
the real protocol (payload before `processed`).

**cvm-lock** (`cross_vm_lock.cpp`): `ldar` acquire, `ldaxr`+
`stlxr` acq/rel. No IRQ on the live take path.

**occupancy** (`pelt.c` `update_vm_policy`): plain stores, then
`Vcpu::Interrupt(sched_irq)` → `InterruptCpu` → `mp_interrupt`
→ `arm_gic_sgi_mtk` / `arm_gic_sgi`. Those two **do**
`smp_mb()` immediately before `gic_write_sgi1r` (`arm_gicv3.c`).
On a **bound** VCPU (`bind_cpu_num_ >= 0`) `send_ipi` is always
true, so the producer PE’s occupancy stores are ordered before
the host SGI that pulls the target pCPU out of guest.

That is **not** `el2_hvc_resume`. Steady resume still has no
`dsb ish`. The barrier is the **IPI SGI path**. If someone
injects `sched_irq` without `InterruptCpu` / SGI (bitmap only),
there is no `smp_mb`. `update_vm_policy` itself still has no
`dmb`.

**fwlog**: `alloc += len`; `smp_wmb`; fill `data[]`; `smp_wmb`;
`put += len`. Consumer uses `put` as the published index.

**lk2**: `ZX_VMO_OP_CACHE_CLEAN` the image so I-cache/D-cache
do not disagree after a file read into a WB mapping.

**ipc-mbox.** Guest stores into TX (CACHED), then `smc`
`HC_IPC_*`. No `dmb` in `ipc_channel.cc`. The producing PE’s
SMC is an exception to EL2; that is **not** `dmb ish` to other
PEs. Host then injects a VIRTUAL IRQ. Same class as occupancy:
visibility leans on GIC/EL2, not an explicit ISH barrier.

**ioreq two producers.** UOS Vmm `ATOMIC_STORE(..., SEQ_CST)`
and SOS Linux `smp_store_release(COMPLETE)` hit the **same**
`processed` word. SEQ_CST is a full fence; `store_release` only
orders before the store. Both are CACHED ISH. Compatible: the
Linux release still publishes payload before COMPLETE; the Vmm
SEQ_CST load of COMPLETE sees it. Do not weaken the Vmm side to
relaxed.

**EL2 is not `dsb ish`.** Steady `el2_hvc_resume` has **no**
DSB before `eret` (only `dsb nshst` on the first VMID TLBI).
`nshst` does not drain Inner Shareable stores. Occupancy /
ipc-mbox must not be documented as “EL2 implies ISH DSB”.

**DTB.** `flush_dtb` → `zx_cache_flush(..., ZX_CACHE_FLUSH_DATA)`
(`dc cvac` via physmap) after `fdt_pack`, so the guest CPU does
not fetch a stale D-cache line.

**PoisonCache.** `system.nbl_cache_protection.enable` (off by
default): context-switch poisons the outgoing **host user**
aspace. Not guest S2. Irrelevant to occupancy/ioreq.

## DMA / SMMU

CPU ISH does **not** cover a device. A master that DMAs into a
CPU-cached page without a cache op is stomping.

```text
 Device  IOVA
    SMMU S1?  → IPA → same VTTBR S2 as CPU
    MemAttr of the STE/S2 walk
         │
         ▼
    DRAM  (snoop if Normal ISH; not if Device)
 CPU  WB line of that PA may be dirty
```

| Buffer | DMA? | Rule |
| --- | --- | --- |
| occupancy / ioreq / cvm-lock | no | CPU only |
| virtio rings + desc buffers | **yes** (backend in Vmm, not SoC DMA) | Vmm is CPU; `dmb` enough |
| rproc / cam dmabuf | SoC DMA possible | CACHED + `ZX_CACHE_FLUSH` or UNCACHED; `cam_dmabuf.cc` uses CACHED |
| GPU Mali | SoC DMA | grant MapDevice **Device**; STE boot-time |
| QEMU DummyIommu | IOVA := HPA | no STE; DMA is physical |

Ungrant unmaps **CPU** S2 only. STE still translates. DMA into
ungranted Mali is SMMU/SMPU, not a CPU DABT ([`memory.md`](memory.md)
SMMU).

## I-cache

Guest identity RAM is **RWX** at S2 (`PERM_EXECUTE` in `Init`).
W^X is not enforced. After the Vmm writes a payload into guest
RAM (bootloader, DTB, lk2):

| Write | D-cache | I-cache |
| --- | --- | --- |
| lk2 | `ZX_VMO_OP_CACHE_CLEAN` | guest `ic iallu` on its first fetch still needed if that PE had old lines; CLEAN is D-side |
| DTB | `zx_cache_flush(DATA)` | DTB is data, not exec |
| Linux Image | loaded into CACHED RAM | guest CPU `eret` to `guest_ip_`; first I-fetch may need `ic ivau` — this tree relies on the guest not having executed that IPA yet |

Host `ZX_CACHE_FLUSH_INSN` is `ic ivau` on the **host** VA, not
the guest. It does not shoot down guest I-cache.

## Overlap map (product)

```text
 SOS identity ram     0x80000000  size 0x780000000
 gz_nebula            0x93400000  size 0x6800000     ⊂ ram
    SOS: do not setVmemAuto
    UOS/tbox: setVmemAuto OK (outside their RAM mblock)
 occupancy            reserved-memory, CACHED everywhere
 ioreq                SOS GPA 4 KiB, mapped into UOS Vmm + SOS HSM
 snapshot             gz_nebula tail 4 KiB, shared one slot
```

`SPECIFIC` without `SPECIFIC_OVERWRITE` cannot replace. There is
no silent remap.

## How to analyse memory stomping

| Symptom | First check |
| --- | --- |
| `Failed to map guest physical memory -4` `gz_nebula` | A: SOS double map ([`logging.md`](logging.md) patches README) |
| Guest sees old occupancy after doorbell | C: barrier; confirm `ioremap_cache` not `ioremap` |
| UOS BAR hang, slot PENDING forever | C: `processed` not COMPLETE; or kick SPI missing |
| Virtio used but guest misses | C: `dmb ishst` around `used.idx`; HW bit on virtio IRQ |
| GPU MMIO looks like RAM | B: grant did not unmap identity; S2 still WB |
| Black screen after idle | GPUEB powered off ([`pm.md`](pm.md)), not alias |
| Snapshot not the panic log | last RESET overwrote the 4 KiB tail |
| SOS/UOS both DMA same PA | SMMU STE + S2; SMPU last ([`security.md`](security.md)) |

Dump: `readelf`/`vmar` is not in the guest. Use lua `setVmem` /
`setVmemAuto` list vs `Guest::Init` log (`Creating first/last VMO`
prints gpa/hpa/size/policy). `-4` is overlap.

## Pitfalls

- **`kDevice` on DRAM.** Physmap is WB. Device alias is undefined.
- **Guest `ioremap` on occupancy.** Must be `ioremap_cache`.
- **Second SPECIFIC.** Never “updates” the PTE; it fails.
- **Occupancy has no `dmb` next to the stores.** Bound-VCPU IPI
  inserts `smp_mb` before SGI. Do not copy the stores without
  that Interrupt/IPI path. Steady resume still has no `dsb ish`.
- **SMMU STE vs CPU S2.** Grant unmaps CPU S2; STE still boot-time.
  DMA to ungranted Mali is an SMMU/SMPU problem, not a CPU DABT.
- **Do not cite EL2 as ISH DSB.** Steady resume has no `dsb ish`.
- **GPU cmdbuf.** Guest CACHED `SPECIFIC` at `0x88000b000` vs kernel
  `SetVmo` UNCACHED on the **same paged VMO** is alias B. That IPA
  lies in SOS identity `ram`; if the 4K is not already a hole,
  SPECIFIC is overlap A (`-4`).

## Terminology

| Term | Meaning here |
| --- | --- |
| Memory stomping | 踩内存: unintended PA reuse (overlap, alias, or unordered share) |
| Alias | Same PA, different MemAttr |
| Overlap | Two SPECIFIC IPAs in one guest VMAR |
| ISH | Inner Shareable; Savior CACHED PTEs set `SH_INNER_SHAREABLE` |
| Doorbell | IRQ or index publish **after** payload + barrier |
| `SEQ_CST` | ioreq `ATOMIC_*`; stronger than the protocol needs |
| `dmb ishst` | Store-store; virtio used ring |

## Code

| Step | File |
| --- | --- |
| lua → cache policy | `garnet/lib/machina/guest.cc` `cache_policy` / `Init` |
| S1/S2 MemAttr | `zircon/kernel/arch/arm64/mmu.cpp` |
| Occupancy map | `guest.cc` `InitDump` / `InitSched` |
| Occupancy write | `zircon/kernel/kernel/pelt.c` `update_vm_policy` |
| ioreq atomics | `libnbl_vhm/include/acrn/atomic.h`, `linux/.../ioreq.c` |
| virtio dmb | `garnet/lib/machina/virtio_queue.cc` |
| cvm-lock | `cross_vm_lock.cpp` |
| gz_nebula overlap | `patches-kedump-nebula-ramconsole/patches/README.md` |
| VMO cache ops | `vm_object_dispatcher.cpp` `ZX_VMO_OP_CACHE_*` |
| DTB flush | `vmm.cc` `flush_dtb` |
| PoisonCache | `mmu.cpp` `ArmArchVmAspace::PoisonCache` |
| GPU cmdbuf | `gpu_scheduler.cc` `kShareMemBase`, `SetVmo` UNCACHED |
| hint peek | `board_mtk_comm.cpp` `SCHED_PARAM_TYPE_DRAM_INFO` |
| EL2 DSB | `el2.S` `el2_hvc_resume` (`dsb nshst` first enter only) |
| Occupancy IPI barrier | `arm_gicv3.c` `arm_gic_sgi_mtk` / `arm_gic_sgi` `smp_mb` then `gic_write_sgi1r` |

---

Prev: [`memory.md`](memory.md) · Next: [`interrupt.md`](interrupt.md) · [`README.md`](README.md)

Everyday's a Hustle.
