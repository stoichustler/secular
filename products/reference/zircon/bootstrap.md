# Bootstrap

Secular boots a Zircon kernel on `qemu-system-aarch64` (virt, arm64). QEMU
supplies ATF as `-bios`, a Linux `Image` as `-kernel`, and `secular.zbi` as
`-initrd`. This note is the map of that path: exception-level and MMU
switches from firmware to `zxsh`, and the **physical** layouts the linker
scripts publish so a bootloader can place the images.

The product stays:

```text
zircon kernel + linux-arm64-boot-shim + BOOTFS + serial console + zxsh
```

Read [`memory.md`](memory.md) for TTBR/physmap, [`cpu.md`](cpu.md) for
secondary CPUs / idle, [`interrupt.md`](interrupt.md) for GICv3. First
user process and the bootstrap channel are also [`ipc.md`](ipc.md).
This note stops at the first `zxsh` prompt.

## Bootstrap

### Framework

```text
 QEMU virt
   ATF  qemu_fw.bios          EL3 → EL2 (Linux boot protocol)
   Image  linux-arm64-boot-shim.bin     x0 = DTB
   initrd secular.zbi                   kernel + BOOTFS
              │
              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ linux-arm64-boot-shim   (phys, PIC, MMU off at entry)          │
 │  LinuxbootEntry → _start → PhysMain                            │
 │  DTB → ZBI items (RAM, GIC, UART, PSCI)                        │
 │  BootZbi: SCTLR.M=0, br physboot, x0 = data ZBI                │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ physboot   (ZBI kernel item; MMU on; still phys executable)    │
 │  STORAGE_KERNEL BOOTFS → ELF "vmzircon"                        │
 │  load + relocate at kHandoffVirtualAddress                     │
 │  HandoffPrep: physmap, hand-off VMOs, kernel image maps        │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ vmzircon   EL1, high VA                                        │
 │  lk_main → bootstrap2 → userboot_init                          │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
 ┌────────────────────────────────────────────────────────────────┐
 │ userboot   first user process (static PIE + vDSO)              │
 │  unpack STORAGE_BOOTFS                                         │
 │  userboot.next = bin/component_manager+--boot                  │
 └────────────────────────────┬───────────────────────────────────┘
                              ▼
   component_manager → console-launcher → zxsh
```

Three things change along this path. Each **switch node** below is one
of them (sometimes two at once).

| Dimension | Starts as | Ends as |
| --- | --- | --- |
| Exception level | EL3 (ATF) | EL0 (`zxsh`) via EL2 then EL1 |
| MMU | off | TTBR1 kernel + TTBR0 user ([`memory.md`](memory.md)) |
| Address space | physical / identity | kernel high VA, then per-process user VA |

### Flow

Each box is a **switch node**: exception level, MMU, or address space
changes here.

```text
 qemu-system-aarch64
   -bios qemu_fw.bios
   -kernel linux-arm64-boot-shim.bin
   -initrd secular.zbi
         │
         ▼  (1) ATF  EL3 → EL2
 Linux protocol: x0=FDT, Image anywhere 2 MiB-aligned
         │
         ▼  (2) LinuxbootEntry  ("ARM\\x64" header)
 _start   DAIF masked, VBAR=0, .bss cleared, TPIDR/SP
         │
         ▼  (3) PhysMain (shim)
 DTB matchers → append ZBI items onto initrd
         │
         ▼  (4) ZbiBoot
 SCTLR.M/C/I = 0,  br physboot,  x0 = data ZBI
         │
         ▼  (5) physboot  MMU on (EL1 or EL2 VHE)
 identity RAM+UART (TTBR0), then kernel VA (TTBR1)
 load vmzircon at 0xffffffff00000000
         │
         ▼  (6) kernel handoff
 drop identity TTBR0, lk_main(PhysHandoff*)
         │
         ▼  (7) lk_main  (still CPU0, not idle yet)
 thread_init_early … kernel_init
 BecomeIdle; bootstrap2 on CPU0
         │
         ▼  (8) userboot_init
 Process "userboot", two handle-only channel msgs, eret EL0
         │
         ▼  (9) userboot
 BOOTFS → launch component_manager --boot
         │
         ▼  (10) component_manager
 bootstrap realm: console-launcher, guest-manager, …
         │
         ▼
 zxsh on kernel serial
```

| Node | EL | MMU | Who runs |
| --- | --- | --- | --- |
| (1) ATF | EL3 → EL2 | off | firmware |
| (2)–(4) shim | EL1 or EL2 | off | PIC phys, identity later unused |
| (5) physboot | EL1 (or EL2 VHE) | on: identity TTBR0, then TTBR1 | still phys-executable |
| (6)–(7) `lk_main` | EL1 | TTBR1; TTBR0 `EPD0` | kernel, CPU0 then idle + `bootstrap2` |
| (8) `userboot_init` | EL1 → first `eret` EL0 | TTBR0 = userboot | first process |
| (9)–(10) | EL0 | that process's TTBR0 | `component_manager` → `zxsh` |

### Switch nodes

#### (1) ATF → Linux Image

`make run` ([`tools/secular-boot/run-qemu.sh`](../../../tools/secular-boot/run-qemu.sh)) execs `run-zircon` with:

```text
-bios  $SECULAR_SDK/prebuilt/firmware/arm-trusted-firmware-qemu-bios/qemu_fw.bios
-kernel linux-arm64-boot-shim.bin
-initrd secular.zbi
-m 1024  -s 8  --gic=3
```

QEMU's ATF blob is EL3 firmware. It programs GIC / UART / DRAM, then
drops to **EL2** (or EL1 if the firmware is built that way) and jumps to
the Image entry with the Linux ARM64 protocol:

| Register / fact | Value |
| --- | --- |
| `x0` | physical address of the QEMU virt DTB |
| `x1`–`x3` | 0 |
| PC | Image entry (`LinuxbootEntry`) |
| MMU / caches | off |
| DRAM | `0x40000000`, 1 GiB default |

Zircon phys does **not** run at EL3 (`AddressSpace::ArchInstall` panics).
It accepts EL1 or EL2 (VHE `HCR_EL2.E2H` uses the EL1 register aliases).

#### (2) Linux Image header → `_start`

`linuxboot-header.S` sits in `.boot.header`, first bytes of the file
(`phys.ld`). Layout is the [Linux ARM64 Image header](
https://www.kernel.org/doc/html/latest/arm64/booting.html):

```text
offset 0     ccmp … / "MZ"   EFI magic lookalike, then b _start
offset 8     load offset     0
offset 16    image size      LINUXBOOT_SIZE  (_end − load, includes .bss)
offset 24    flags           0x4  (place anywhere, 2 MiB aligned)
offset 56    magic           "ARM\x64"
```

`_start` (`arch/arm64/phys/start.S`): sample ticks, `DAIFSet` all, zero
`VBAR_ELx`, invalidate D-cache over `.data`…`_end`, clear `.bss`
(`_edata`…`_end`, 16-byte), `TPIDR_EL1` = boot thread pointer, `SPSel=1`,
`sp` = `boot_stack + BOOT_STACK_SIZE`, `bl PhysMain`. MMU still off.

#### (3) Shim `PhysMain`: DTB → ZBI

`linux-arm64-boot-shim.cc`: `BootShimHelper` walks the FDT (`x0`) and
appends ZBI driver items the kernel cannot discover by itself: RAM,
GICv3 (`0x08000000`), PL011, PSCI. The data ZBI is the QEMU initrd
(`secular.zbi`) plus those items.

#### (4) `ZbiBoot`: MMU off, jump into physboot

`arch-boot-zbi.cc` `BootZbi::ZbiBoot`:

- clean D-cache over the next kernel image, invalidate I-cache
- `SCTLR_ELx.M/C/I = 0` (EL1 or EL2 matching `CurrentEL`)
- `ISB`, clear `fp`/`lr`/`sp`
- `x0` = data ZBI physical address
- `br entry` into the ZBI kernel (physboot)

This is the **ZBI boot protocol**. The next image is itself a phys
executable (same `_start`).

#### (5) physboot: MMU on, load `vmzircon`

`physboot.cc` `BootZircon`:

- open `STORAGE_KERNEL` BOOTFS, ELF name `"vmzircon"`
- `kernel.Load(..., kHandoffVirtualAddress)` with
  `kHandoffVirtualAddress = 0xffff'ffff'0000'0000`
- code-patch + PIE relocate
- `HandoffPrep::DoHandoff` builds `PhysHandoff` and the first page tables

MMU enable (`phys/address-space.cc`): identity-map RAM and UART on
**TTBR0**, then kernel image / physmap on **TTBR1** ([`memory.md`](memory.md)). Physboot
itself still runs at a physical (or identity) address until the final jump.

#### (6) Kernel handoff

`HandoffPrep` maps (`handoff-prep-vm.cc`):

```text
KERNEL_ASPACE_BASE + KERNEL_ASPACE_SIZE
        other first-class maps          ↑
        hole (1 page)
        kernel memory image             (load VA 0xffffffff00000000)
        hole (1 page)
        permanent hand-off              ↓
        kernel load − 1 GiB
        temporary hand-off              ↓
KERNEL_ASPACE_BASE + PhysmapSize()
        physmap  PA [0, last RAM)
KERNEL_ASPACE_BASE  (0xffff000000000000)
```

Then jump to the kernel ELF entry at the **high VA**. `lk_main(PhysHandoff*)`
(`top/main.cc`) is now EL1 (or EL2-VHE looking like EL1), TTBR1 live,
identity TTBR0 later `EPD0`'d ([`cpu.md`](cpu.md) secondary.S does the same on APs).

#### (7) `lk_main` → idle + `bootstrap2`

Single-threaded on CPU0 until:

```text
thread_init_early
dlog / constructors / "printing enabled"  (serial: `[cpu N] printing enabled`)
arch_early_init, platform_early_init, DriverHandoffEarly   (UART, GIC early)
vm_init_preheap, heap_init, vm_init
DriverHandoffPostVm                                        (map GICD/GICR)
topology_init, kernel_init
Scheduler::SetCurrCpuActive(true)
Thread::Create("bootstrap2")  affinity CPU0
Thread::Current::BecomeIdle()  ← CPU0 is now IdlePowerThread (cpu.md)
```

`bootstrap2` finishes arch/platform, `mp_signal_curr_cpu_ready`,
`EndHandoff`, `userboot_init`. Secondary CPUs are PSCI `CPU_ON`'d from
platform init and enter as idle ([`cpu.md`](cpu.md) SMP).

#### (8) `userboot_init`: first EL0 process

`lib/userabi/userabi.rs`:

- `ProcessDispatcher::create` name `"userboot"` under the root job
- root-job observer: if this process dies with no siblings, reboot
- map userboot ELF (static PIE, vDSO only) into its VMAR
- channel with **two handle-only messages**
  1. process capability: log, process, root VMAR, thread, loaded VMAR
  2. system capability: root job, MMIO/IRQ/SMC/SYSTEM resources, ZBI VMO,
     vDSO VMOs, extra hand-off VMOs
- `userboot.start` → `eret` to EL0 ([`cpu.md`](cpu.md) uspace_entry)

Userboot is packed in `STORAGE_KERNEL`, **not** in BOOTFS (it is the thing
that unpacks BOOTFS).

#### (9)–(10) userboot → `component_manager` → `zxsh`

[`userspace/secular/bin/userboot/main.rs`](../../../userspace/secular/bin/userboot/main.rs):

- read ZBI `CMDLINE`; default `userboot.next = bin/component_manager+--boot`
- extract `STORAGE_BOOTFS`
- `launch_program` of `component_manager --boot`

`component_manager` starts the bootstrap realm from the boot config:
`console-launcher` (kernel serial → `zxsh`), `guest-manager` (eager),
`vmm_launcher` (lazy). Console I/O is kernel serial syscalls
(`kernel.enable-serial-syscalls=true`), not a userspace UART driver.
The two userboot channel messages are capability IPC ([`ipc.md`](ipc.md)).

### Linker scripts: physical load image

Two families. **Phys** images are one RWX segment, PIC, link address 0.
**vmzircon** is a PIE ELF linked at 0 and **relocated** to
`kHandoffVirtualAddress`; its script describes **virtual** layout after
handoff, not the QEMU load address.

#### QEMU virt physical map (hardware, not a linker script)

```text
PA
0x00000000  flash / ATF BIOS
0x08000000  GICv3 GICD     (0x10000)
0x080A0000  GICv3 GICR     (0x20000 × NCPU)
0x09000000  PL011 UART
0x40000000  DRAM  (default 1 GiB, -m 1024)
              │
              ├─ DTB            x0; typically DRAM base
              ├─ Image (shim)   2 MiB-aligned (typical DRAM+0x80000)
              ├─ initrd ZBI     secular.zbi, padded to 4 KiB
              └─ rest free RAM  → PMM after handoff
```

The initrd is padded so its last page is only ZBI (`kDataZbi`); see
[`memory.md`](memory.md) **Overall layout**.
`paddr_to_physmap(0x40000000) = 0xffff000040000000` once TTBR1 is on.

#### Phys executables — `phys.ld` + friends

Used by the **shim** and by **physboot**. Invocation order for BFD ld:
`-T phys-end.ld -T phys.ld` (and `-T zbi-header.ld` for ZBI kernels).
`linuxboot-pic.ld` sets `PHYS_LOAD_ADDRESS = 0`.

```text
PHYS_LOAD_ADDRESS = 0          (PIC; debug file only)
__executable_start
┌─────────────────────────────────────────────────────────────┐
│ .boot.header                                                │
│   shim:  Linux Image header  (linuxboot-header.S)           │
│   ZBI:   container + item headers  (zbi-header.S)           │
│          then zbi_kernel_t:                                 │
│            entry  = _start − __executable_start             │
│            reserve_memory_size = PHYS_RESERVE_MEMORY_SIZE   │
├─────────────────────────────────────────────────────────────┤
│ .text                                                       │
│ .rodata   kLinkTimeLoadAddress = PHYS_LOAD_ADDRESS          │
│ .dynamic / .rela.dyn / GOT (≤ 4 slots)                      │
│ .data     __data_start                                      │
│ PHYS_LOAD_END                 ← end of the file on disk     │
│                               (phys-end.ld INSERT BEFORE    │
│                                .bss; 8-byte ZBI pad)        │
├─────────────────────────────────────────────────────────────┤
│ .bss      _edata … ALIGN(16) _end                           │
│           start.S zeros this; bootloader must reserve it    │
└─────────────────────────────────────────────────────────────┘
LINUXBOOT_SIZE            = _end − PHYS_LOAD_ADDRESS
PHYS_RESERVE_MEMORY_SIZE  = _end − PHYS_LOAD_END
PHDRS  load  PT_LOAD  PF_R|PF_W|PF_X     (no MMU yet)
ENTRY(_start)
```

No static constructors (`ASSERT .init_array empty`). One segment, RWX:
phys runs with the MMU off, or with an identity map.

The Linux header's size field is `LINUXBOOT_SIZE`, **including `.bss`**, so
QEMU/U-Boot leave headroom after the file. The ZBI `reserve_memory_size` is
the same idea for the ZBI protocol.

#### Kernel proper — `kernel.ld`

Linked at 0; physboot relocates every segment to
`0xffff'ffff'0000'0000 + offset`. Page-aligned **virtual** segments:

```text
VA  kHandoffVirtualAddress + offset
0
__executable_start / __rodata_start
  ELF headers, .note.gnu.build-id, .interp, .dynamic, .rodata
  .kcounter.desc          page-aligned VMO for user kcounter
__rodata_end / __code_start          page
  .text
__code_end
__relro_start                        page
  .init_array, .data.rel.ro (lk_init, commands)
__relro_end
__data_start                         page
  .data
.bss  __bss_start                    16-byte
  kcounters_arena                    page-aligned VMO
  remaining .bss
_end                                 page
```

`PT_GNU_RELRO` covers the “writable until relocated” bits; after physboot
relocates, those pages are RO. `__code_start`…`__code_end` is the only
executable span. Physboot refuses a memory image larger than
`KERNEL_IMAGE_MAX_SIZE`.

### Where in the tree

Read in this order.

| File | Why |
| --- | --- |
| [`tools/secular-boot/run-qemu.sh`](../../../tools/secular-boot/run-qemu.sh) | ATF BIOS, Image, ZBI, 1 GiB, 8 CPUs, GICv3 |
| [`zircon/kernel/arch/arm64/phys/linuxboot-header.S`](../../../zircon/kernel/arch/arm64/phys/linuxboot-header.S) | Linux Image header |
| [`zircon/kernel/phys/linuxboot-pic.ld`](../../../zircon/kernel/phys/linuxboot-pic.ld) | `PHYS_LOAD_ADDRESS=0`, `LINUXBOOT_SIZE` |
| [`zircon/kernel/phys/phys.ld`](../../../zircon/kernel/phys/phys.ld) | phys segment order, `_end`, reserve |
| [`zircon/kernel/phys/phys-end.ld`](../../../zircon/kernel/phys/phys-end.ld) | `PHYS_LOAD_END` before `.bss` |
| [`zircon/kernel/phys/zbi-header.ld`](../../../zircon/kernel/phys/zbi-header.ld) | `zbi_kernel_t` entry + reserve |
| [`zircon/kernel/arch/arm64/phys/start.S`](../../../zircon/kernel/arch/arm64/phys/start.S) | `_start`, DAIF, `.bss`, `PhysMain` |
| [`zircon/kernel/arch/arm64/phys/boot-shim/linux-arm64-boot-shim.cc`](../../../zircon/kernel/arch/arm64/phys/boot-shim/linux-arm64-boot-shim.cc) | DTB → ZBI |
| [`zircon/kernel/arch/arm64/phys/arch-boot-zbi.cc`](../../../zircon/kernel/arch/arm64/phys/arch-boot-zbi.cc) | MMU-off jump, `x0` = ZBI |
| [`zircon/kernel/phys/physboot.cc`](../../../zircon/kernel/phys/physboot.cc) | load `vmzircon` at handoff VA |
| [`zircon/kernel/phys/handoff-prep-vm.cc`](../../../zircon/kernel/phys/handoff-prep-vm.cc) | physmap / kernel / hand-off maps |
| [`zircon/kernel/kernel.ld`](../../../zircon/kernel/kernel.ld) | vmzircon VA segments |
| [`zircon/kernel/top/main.cc`](../../../zircon/kernel/top/main.cc) | `lk_main`, `bootstrap2`, `userboot_init` |
| [`zircon/kernel/lib/userabi/userabi.rs`](../../../zircon/kernel/lib/userabi/userabi.rs) | first process + two channel msgs |
| [`userspace/secular/lib/userboot/README.md`](../../../userspace/secular/lib/userboot/README.md) | userboot protocol |
| [`userspace/secular/bin/userboot/main.rs`](../../../userspace/secular/bin/userboot/main.rs) | BOOTFS → `component_manager --boot` |

Suggested first pass: **`run-qemu.sh` → Linux header → `phys.ld` picture →
`ZbiBoot` → `physboot.cc` Load → `lk_main` → `userboot` `next`**. That is
enough to see serial print `"printing enabled"` (prefixed `[cpu N]`) then `"welcome to secular"`,
then a `zxsh` prompt.

### Terminology

| Term | Meaning in this note |
| --- | --- |
| ATF | ARM Trusted Firmware. QEMU `-bios`; EL3, then drops to EL2 with the Linux boot protocol. |
| Linux Image | `linux-arm64-boot-shim.bin`. File with an ARM64 Image header; QEMU `-kernel`. |
| ZBI | Zircon Boot Image. `secular.zbi` is `-initrd`: kernel item + BOOTFS. |
| Phys executable | PIC, one RWX segment, link address 0. Shim and physboot. MMU off or identity. |
| physboot | ZBI kernel item. Turns the MMU on, loads ELF `vmzircon` at the handoff VA. |
| `vmzircon` | The kernel ELF. Relocated to `0xffffffff00000000`. |
| Handoff | `PhysHandoff` plus the first TTBR1 maps (physmap, image, hand-off VMOs). |
| `lk_main` | Kernel C entry. CPU0 until `BecomeIdle`; `bootstrap2` finishes bring-up. |
| userboot | First user process. Static PIE + vDSO. Unpacks BOOTFS. Not itself in BOOTFS. |
| BOOTFS | `STORAGE_BOOTFS` in the ZBI. `component_manager`, `zxsh`, drivers, … |
| `userboot.next` | CMDLINE: default `bin/component_manager+--boot`. |
| Switch node | A step where EL, MMU, or aspace changes. Numbered (1)–(10) in **Flow**. |

---

Everyday's a Hustle.
