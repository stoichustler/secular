# Devices

A guest device is one of four things: **identity-mapped MMIO**
(passthrough), **a trap** (VMM or VHM emulates), **virtio**
(machina backend + guest frontend), or **not in stage-2** (SMPU /
TEE hole). Lua plus `Guest::Init` decide which.

Read [`memory.md`](memory.md) for `setVmem` / traps / SMMU,
[`interrupt.md`](interrupt.md) for which SPIs are forwarded,
[`vhm.md`](vhm.md) for UOS trapped BARs, [`gpu.md`](gpu.md) for
Mali (not a virtio device).

This note is the device set SOS / tbox / UOS actually get, how
lua turns into maps and traps, and virtio vs passthrough.

## Principle

1. **Identity is the default for SOS MMIO.** QEMU SOS
   `setVmem("IO", 0, 0, 1 GiB, kDevice)` maps GPA = HPA. GIC,
   UART, and most SoC blocks are then **visible**. Isolation is
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
   ├─ mapped, kDevice ── passthrough MMIO  (SOS 0–1 GiB)
   ├─ unmapped trap ──── VMM / VHM / VGIC / virtio
   └─ unmapped hole ──── SMPU kMpu, GPU ungrant, missing lua
```

### 2. SOS QEMU device set (`sos.lua`)

```text
 cpus            4
 GICD/GICR       0x08000000 / 0x080A0000   trapped + emulated
 passthrough SPI 36–512 minus holes        (see lua list)
 percpu IRQ      15
 IO window       GPA 0 + 1 GiB identity    kDevice
 VHM kick        SPI 254
 TIPC notifier   245
 vtee notifier   513
 vsock           380, 382
 ipc-mbox        150, 151
 guest-reserved  256 MiB advertised, not for SOS
 console         ttyAMA0  (PL011 at 0xffff32000 if vmlog)
```

Holes in the SPI list (70, 150–151 used as mbox, 178, 180,
191–200, 221–224, 245 TIPC, 252, 254 VHM, 256, 380 vsock, …)
are on purpose: those vectors are virtual or owned by Savior.

mt8678 `sos.lua` is denser (`36–251`, `253–512`) and `setVmem`
takes five args. This tree’s sparse list + six-arg `setVmem` +
`earlycon` cmdline are QEMU/Savior deltas, not SWA.

### 3. tbox QEMU (`tbox.lua`)

```text
 cpus            4
 GICD/GICR       same base, no SOS SPI range
 RAM             grt,tbox-reserved  identity
 extra           setVmemAuto("vmlog_sink_tbox")
 console         hvc0  virtio-console
 cmdline         INITTAB=/etc/inittab.tbox
```

No VHM, no vsock IRQs, no 0–1 GiB IO.

### 4. Virtio vs passthrough vs SWA

```text
 passthrough     identity S2 + PHYSICAL IRQ     UART, most SoC, Mali when granted
 virtio          trap MMIO + VIRTUAL IRQ        console, vsock, block, RPMB, …
 VHM trap        VHM_MEM / BELL                 UOS BARs SOS must emulate
 SWA             MapDevice on grant             Mali  (gpu.md)
```

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
| virtio-rpmb | `VirtioRpmb` | `ufs_enabled` |
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
S1 fields only. Four SoC bases (MM/APU/SOC/GPU) in
[`memory.md`](memory.md) **SMMU**. DummyIommu if no hardware
(QEMU): `Map` returns the VMO HPA as “IOVA”.

## Passthrough wiring

Three legs, from [`memory.md`](memory.md):

```text
 CPU MMIO   identity S2  (or MapDevice)
 IRQ        InterruptEventDispatcher  PHYSICAL  HW LR
 DMA        SMMU STE → same S2 TTB
```

Missing any leg is the usual “device works until DMA / until
IRQ” bug. GPU grant installs all three; ungrant removes MMIO
and stops dispatching Mali IRQs.

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
| `setVmem` | lua: name, GPA, HPA, size, policy, is_phy |
| `kDevice` / `kCached` / `kDemand` / `kMpu` | map policy (memory.md) |
| vSMMU | Emulated SMMU; nests guest CD onto host S2 |

## Code

| Step | File |
| --- | --- |
| SOS lua | `garnet/bin/guest/sos/qemu/sos.lua` |
| tbox lua | `garnet/bin/guest/sos/qemu/tbox.lua` |
| Vmm init | `garnet/bin/guest/vmm.cc` `Vmm::Initialize` |
| VGIC emulate | `garnet/lib/machina/arch/arm64/gic_distributor.cc` |
| Virtio devices | `garnet/lib/machina/virtio_*.cc` |
| vSMMU | `garnet/lib/machina/smmuv3.cc`, [`memory.md`](memory.md) SMMU |
| Map vs trap | `zircon/kernel/syscalls/hypervisor.cpp` `zx_guest_set_trap` |
| Stage-2 | [`memory.md`](memory.md) |

---

Everyday's a Hustle.
