# Devices

**After this:** you can classify an IPA as map, trap, or hole.
**First read:** Diagrams 1–7. **Next:** [`vhm.md`](vhm.md).

A guest device is one of four things: **identity-mapped MMIO**
(passthrough), **a trap** (VMM or VHM emulates), **virtio**
(machina backend + guest frontend), or **not in stage-2** (SMPU /
TEE hole). Lua plus `Guest::Init` decide which.

Read [`system.md`](system.md) for vsock / ipc-mbox / event IRQ,
[`memory.md`](memory.md) for `setVmem` / traps / SMMU,
[`interrupt.md`](interrupt.md) for which SPIs are forwarded,
[`vhm.md`](vhm.md) for UOS trapped BARs, [`gpu.md`](gpu.md) for
Mali (not a virtio device).

This note is the device set SOS / tbox / UOS actually get, how
lua turns into maps and traps, and virtio vs passthrough.

## Principle

1. **Identity is the default for SOS MMIO on QEMU.** QEMU SOS
   `setVmem("IO", 0, 1 GiB, kCached, 0)` is 5-arg (GPA = HPA).
   Product SoC has **no** 0–1 GiB IO window: MMIO comes from
   `product_config:setVmem` (6-arg) per device. Isolation is
   the SPI list (`setVgicIrq`) plus later unmaps (`kMpu`, GPU
   ungrant, trap holes).

2. **A trap is an unmap plus a handler.** `zx_guest_set_trap`
   punches a hole; the next access is a DABT. `GUEST_TRAP_MEM`
   goes to the VMM (`HandleMem`). `GUEST_TRAP_BELL` is posted to
   a port. `GUEST_TRAP_VHM_*` is [`vhm.md`](vhm.md).

3. **Virtio is always emulated.** Frontends in guest Linux,
   backends in that guest's Vmm (or SOS for UOS virtio-be).
   IRQs are virtual (no HW bit).

4. **tbox is a small set.** Reserved RAM, same VGIC base, virtio
   console (`hvc0`), no SOS SPI range, no 0–1 GiB IO window.

## Framework

```text
 lua  sos.lua / tbox.lua / product.lua
    setCpus / setVgicPaddr / setVgicIrq
    setVmem / setVmemAuto
    setVhmIrq / setVsockIrqs / setTipcVqNotifierIrq / setVteeNotifierIrq
              │
              ▼
 Vmm::Initialize
    ioctl guest_memory_allocator     RAM windows
    guest_.Init(vmem)                zx_guest_create_new + maps
    interrupt_controller.Init        VGIC traps + passthrough bind
    virtio-*  (feature bits)
    vSMMU / vtee / VHM / GPU sched
              │
              ▼
 Guest IPA
    mapped   identity or allocated RAM
    trapped  GICD/GICR, virtio MMIO, VHM BARs, SMMU
    absent   SMPU / TEE / GPU while not granted
```

## Diagrams

### 1. Four kinds of IPA

```text
 Guest IPA
   ┌─ mapped, kCached ── RAM  (allocator or reserved)
   ├─ mapped, kDevice ── passthrough MMIO  (SoC product_config; QEMU 1 GiB IO is kCached)
   ├─ unmapped trap ──── VMM / VHM / VGIC / virtio
   └─ unmapped hole ──── SMPU kMpu, GPU ungrant, missing lua
```

### 2. SOS QEMU device set (`sos.lua`)

```text
 cpus            4
 GICD/GICR       0x08000000 / 0x080A0000   trapped + emulated
 passthrough SPI 36–251, 253–512           (QEMU lua; 252 is the hole)
 percpu IRQ      15
 IO window       GPA 0 + 1 GiB identity    kCached  (5-arg setVmem)
 VHM kick        SPI 254
 TIPC notifier   245
 vtee notifier   513
 vsock           380, 382
 ipc-mbox        150, 151
 guest-reserved  256 MiB advertised, not for SOS
```

252 is the QEMU hole between the two ranges. 150–151, 245, 254,
380, 382 are still listed as passthrough *and* used as virtual
notifiers — the lua list is dense, not a sparse “minus holes”
table.

### 3. SoC 3OS (`sos_mt8678.lua` + `product_mt8678.lua`)

Product image. No 0–1 GiB IO window. MMIO is per-device
`product_config:setVmem(name, gpa, hpa, size, policy, is_phy)`
(GPA = HPA, policy 1 = cached, 2 = `kDevice`).

```text
 cpus            8, bind 0..7
 GICD/GICR       0x0c400000 / 0x0c440000
 passthrough SPI long explicit list + eSPI 4096–4315
                 (holes include Mali 636–640, GRANT 4316–4319)
 VHM kick        SPI 287
 TIPC / vtee     286 / 174
 sched_irq       283
 smc / cvm-lock  984 / 985
 vsock           181, 183, 185, 187, 189, 191
 ipc-mbox        284–285, 277–278, 279–280
 vSMMU           SOC 0x14900000 IRQ 863 only
 GPU SWA         region 0x48000000 / 0x480000, affinity VCPU 4
 guest-reserved  2560 MiB advertised
```

tbox product (`uos_tbox_pv8678.lua`): 8 CPUs, bind
`{3,1,2,0,4,5,6,7}`, `setSchedId(3)`, no arbiter, no VHM.

Full QEMU vs SoC table: [`README.md`](README.md) **Two configs**.

### 4. tbox QEMU (`tbox.lua`)

```text
 cpus            4
 GICD/GICR       same base, no SOS SPI range
 RAM             grt,tbox-reserved  identity
 extra           setVmemAuto("vmlog_sink_tbox")
 console         hvc0  virtio-console
 cmdline         INITTAB=/etc/inittab.tbox
```

No VHM, no vsock IRQs, no 0–1 GiB IO.

### 5. Virtio vs passthrough vs SWA

```text
 passthrough     identity S2 + PHYSICAL IRQ     UART, most SoC, Mali when granted
 virtio          trap MMIO + VIRTUAL IRQ        console, vsock, block, RPMB, …
 VHM trap        ZX_GUEST_TRAP_MEM / BELL       UOS BARs SOS must emulate
 SWA             MapDevice on grant             Mali  (gpu.md)
```

### 6. lua → maps and traps (init)

```text
 sos.lua / product.lua / tbox.lua
    setCpus / setBindcpus / setVgicPaddr / setVgicIrq
    setVmem / setVmemAuto / setVhmIrq / setSchedIrq
         │
         ▼
 Vmm::Initialize
    ioctl guest_memory_allocator          RAM windows
    append VmemSpec
    guest_.Init(vmem)
       zx_guest_create_new                Guest + guest VMAR
       for each spec
          kCached / kDevice  physical VMO, GPA=HPA, MapRange
          kDemand            anonymous VMO, fault later (F4)
          kMpu               skip, then UnmapPhysicalMemory
       zx_guest_set_trap  UnmapRange      VGIC / virtio / SMMU
    interrupt_controller.Init             GICD/GICR traps + passthrough bind
    virtio-* / vSMMU / VHM / GPU
```

A later `set_trap` always wins over an earlier identity map
for that IPA.

### 7. Three legs of passthrough

A device “works until DMA” or “works until IRQ” is a missing
leg.

```text
 lua  setVmem(name, gpa, hpa, size, kDevice)
      setVgicIrq(spi)
         │
         ├─ CPU   identity S2  (or MapDevice on GPU grant)
         ├─ IRQ   InterruptEventDispatcher  PHYSICAL  HW LR
         └─ DMA   SMMU STE → same S2 TTB   (boot-time; GPU grant
                                           does not retune STE)
```

GPU grant installs MMIO + Mali IRQs; ungrant removes them.
SMMU STE is not part of grant.

### 8. Virtio IRQ (always VIRTUAL)

```text
 guest  kick virtqueue
    str  virtio MMIO notify
    EL2  DABT  GUEST_TRAP_MEM
    VMM  VirtioQueue notify
    backend work
    zx_vcpu_interrupt(irq)              VIRTUAL, no HW bit
         │
         ▼
 guest  virtio ISR
```

UOS virtio-be that SOS emulates uses VHM SYNC/BELL for the
MMIO, then `HC_INJECT_MSI` for the IRQ ([`vhm.md`](vhm.md)).

### 9. vSMMU CMDQ (guest programs IOMMU)

```text
 guest  str  SMMU doorbell
    trap  kSmmuv3Size = 0x80000
    VMM  cmdq_consume
       CFGI_STE   nest or unnest host STE  (guest cannot set S2TTB)
       CFGI_CD    host HwptInvalidate
       TLBI_NH_*  zx_iommu_hwpt_invalidate
       CMD_SYNC   inject vSMMU IRQ
    Host EVTQ → guest EVTQ + SignalInterrupt
```

Host STE S2 is always Savior’s `VTTBR`. This product only
enables SOC (`0x14900000`, IRQ 863). QEMU: DummyIommu.

## VMM device objects

`Vmm::Initialize` (feature bits / lua) may create:

| Backend | Typical guest | IRQ type |
| --- | --- | --- |
| VGIC `GicDistributor` | all | trap GICD/GICR; passthrough SPIs |
| `GicIts` | MSI guests | LPI virtual |
| `VirtioConsole` | tbox `hvc0` | virtual |
| `VirtioVsock` | SOS | virtual, lua vsock IRQs |
| `VirtioBlock` MMIO or PCI | optional | virtual |
| `VirtioRpmb` | SOS | virtual |
| `VirtioCluster` / `EINT` / `RTC` / `SPI` / `I2C` | SoC | virtual |
| vSMMU | SOS | trap SMMU MMIO, nest guest S1 |
| `VhmServiceImpl` | SOS | kick SPI 254 |
| `GpuSchedulerServiceImpl` | SOS then UOS | GRANT/STOP virtual; Mali physical |

UOS gets a subset plus `VhmDevice` mappings SOS pushed.

`Vmm::Initialize` order on this tree (`vmm.cc`):

```text
 guest_.Init(vmem)
 TeeVmCreate
 bootloader Setup → guest_ip_
 SetSchedIRQ / DumpIRQ / SchedId / affinity / wakeup / smc / cvm-lock
 interrupt_controller.Init
 SoC: UFS VmIrqNotifier, pci_global_irqs
 PciBus::Init
 cluster + spi client  (PCI)
 eint                  (PCI)
 SoC: rtc              (PCI; skip if Init fails)
 virtio-console        socket pair, PCI
 SoC: virtio-block, virtio-rpmb
 virtio-vsock + HostVsockEndpoint + GzFsVsockService
 …
```

QEMU (`is_qemu_platform()`) skips UFS IRQ, PCI global IRQs,
block, RPMB, RTC. tbox still gets console + VGIC.

### Virtio PCI BARs (`virtio_pci.cc`)

Virtio 1.0 layout. Vendor `0x1af4`. BAR0 strongly ordered MEM
trap; BAR1 notify is sequential 4-byte bells.

```text
 BAR0
   00h  common cfg   0x38 bytes  (queue select, features, status)
   38h  ISR
   3ch  device-specific config
 BAR1  notify
   cap.offset + n*4  →  Queue n  (multiplier 4)
```

Guest kick = store to BAR1 + 4×qid → `NotifyBarWrite` →
`VirtioQueue` work. IRQ back is VIRTUAL (`zx_vcpu_interrupt`).
MSI-X optional (`EnableMsixCapability`). PCI BAR realloc
(`ZX_ERR_PCI_BAR_REALLOC`) moves the MEM trap
([`vmm.md`](vmm.md) `HandleMem`).

Feature bits (`guest_features.h` / product config) gate each
backend. QEMU (`is_qemu_platform()`) skips PVBlock, MMIO/PCI
block, RPMB, virtio-rtc. tbox typically console + VGIC only.

| Feature | Backend | Notes |
| --- | --- | --- |
| PCI bus | `PciBus` | required by most virtio-* below |
| virtio-console | `VirtioConsole` | tbox `hvc0`; socket to Vmm |
| virtio-vsock | `VirtioVsock` + `GzFsVsockService` | SOS; lua 380/382 |
| MMIO/PCI virtio-block | `VirtioBlock` | SoC; not QEMU |
| PVBlock | UFS LUNs 0/1/2 | per-guest visibility |
| virtio-rpmb | `VirtioRpmb` | SoC only; pipe to UFS LU. [`tee.md`](tee.md) **RPMB**, F19 |
| virtio-cluster / eint / rtc / spi / i2c | SoC BEs | eint/rtc need PCI |
| vSMMU | `machina::Smmuv3` per `VsmmuSpec` | SOS; UOS `#if 0` |
| monitor-virtio | DTB carve-out | IPC debug |

### vSMMU (guest programs IOMMU)

Guest SMMU MMIO is trapped (`kSmmuv3Size = 0x80000`). Guest
CMDQ in guest RAM; VMM `cmdq_consume` on doorbell:

```text
 CFGI_STE   nest or unnest host STE  (guest cannot set S2TTB)
 CFGI_CD    host HwptInvalidate
 TLBI_NH_*  zx_iommu_hwpt_invalidate
 CMD_SYNC   inject vSMMU IRQ
 Host EVTQ  → guest EVTQ + SignalInterrupt
```

Host STE S2 is always Savior’s `VTTBR`. Nested S12 copies guest
S1 fields only. Four MMIO bases exist in code (MM/APU/SOC/GPU);
**this product only enables SOC** (`0x14900000`, IRQ 863). MM/APU/GPU
are commented out in `sos_mt8678.lua`. DummyIommu if no hardware
(QEMU): `Map` returns the VMO HPA as “IOVA”. Detail:
[`memory.md`](memory.md) **SMMU**.

## Passthrough wiring

Three legs, from [`memory.md`](memory.md):

```text
 CPU MMIO   identity S2  (or MapDevice)
 IRQ        InterruptEventDispatcher  PHYSICAL  HW LR
 DMA        SMMU STE → same S2 TTB
```

Missing any leg is the usual “device works until DMA / until
IRQ” bug. GPU grant installs MMIO + Mali IRQs; it does **not**
retune the SMMU STE (boot-time). Ungrant removes MMIO and
stops dispatching Mali IRQs.

## Lua API (`sos_config` / `product_config`)

`guest_config.cc` `addFunction`. QEMU `sos.lua` vs SoC
`product_config:setVmem` (6-arg). Unknown names are a lua error
at `Vmm::Initialize`.

| Function | Sets |
| --- | --- |
| `setCpus` / `setBindcpus` | VCPU count / pCPU pin |
| `setVcpuEntry` | unused if bootloader supplies Image+64KiB |
| `setPeriods` / `setBudgets` | occupancy period / slice |
| `setSchedPriority` / `Timeslice` / `Policy` / `VcpuPriority` | host hint knobs |
| `setVgicPaddr` / `setVgicIrq` / `setVgicPercpuIrq` | GICD/GICR + passthrough |
| `setVhmIrq` | SOS HSM kick SPI |
| `setSchedIrq` / `setSchedId` / `setDumpIrq` | occupancy doorbell / row |
| `setSmcIrq` / `setCvmLockIrq` | TEE-complete / lock IRQ |
| `setGpuIrq` | Mali PHYSICAL list |
| `setWakeupIrq` | s2idle wakeup list |
| `setVteeNotifierIrq` / `setTipcVqNotifierIrq` | [`tee.md`](tee.md) |
| `setVsockIrqs` / `setIpcMbox` / `setEventIrq` | [`system.md`](system.md) |
| `setVmem` / `setVmemAuto` | maps ([`memory.md`](memory.md)) |
| `setGuestReservedMemory` | advertised hole |
| `setVsmmu` / `addVsmmu` / `addSidToVsmmu` | vSMMU |
| `setArbiterGpuEnable` / `SchedVcpu` / `SchedMode` | SWA |
| `setCmdline` | extra guest cmdline |
| `setFlag` / `setCpufreq` / `setPciIrqs` / `setMmioIrqs` | misc |
| `setUfsNotifyIrqs` / `setApuIrq(s)` / `setCmdqIrq(s)` | SoC |
| `setNbltraceMemEnable` / `setMonitorSpec` | debug |
| `setDeviceTree` / `setProductSpec` | product.lua only |

## Pitfalls

- **SOS 0–1 GiB is a blunt instrument.** A device in that window
  is in the guest unless you punch a trap or `kMpu` hole.
- **SPI list ≠ MMIO map.** Forcing an IRQ without mapping MMIO
  (or the reverse) is a silent fail.
- **tbox `setVmemAuto` miss.** If the reserved node name does
  not match, tbox has no extra log sink, not a boot fail.

## Terminology

| Term | Meaning here |
| --- | --- |
| Passthrough | Identity S2 + physical IRQ (+ SMMU). Guest drives HW |
| Trap | Unmapped IPA → DABT → VMM / VHM |
| Virtio | Emulated MMIO + virtual IRQ |
| `setVmem` | QEMU `sos_config`: name, base, size, policy, is_physmem (GPA=HPA). `product_config`: 6-arg GPA/HPA |
| `kDevice` / `kCached` / `kDemand` / `kMpu` | map policy (memory.md) |
| vSMMU | Emulated SMMU; nests guest CD onto host S2 |

## Code

| Step | File |
| --- | --- |
| QEMU SOS lua | `garnet/bin/guest/sos/qemu/sos.lua` |
| QEMU tbox lua | `garnet/bin/guest/sos/qemu/tbox.lua` |
| SoC SOS / product / UOS / tbox | `yocto/prebuilt/hypervisor/grt/auto8678p1_64_sos_indep/{sos,product,uos_alps_pv,uos_tbox_pv}8678.lua` |
| Vmm init | `garnet/bin/guest/vmm.cc` `Vmm::Initialize` |
| VGIC emulate | `garnet/lib/machina/arch/arm64/gic_distributor.cc` |
| Virtio devices | `garnet/lib/machina/virtio_*.cc` |
| vSMMU | `garnet/lib/machina/smmuv3.cc`, [`memory.md`](memory.md) SMMU |
| Map vs trap | `zircon/kernel/syscalls/hypervisor.cpp` `zx_guest_set_trap` |
| Stage-2 | [`memory.md`](memory.md) |

---

Prev: [`pm.md`](pm.md) · Next: [`vhm.md`](vhm.md) · [`README.md`](README.md)

Everyday's a Hustle.
